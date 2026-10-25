use super::Host;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(sqlx::FromRow)]
struct ModuleRun {
    id: String,
    job_id: String,
    scheduled_at: i64,
    status: String,
    result: Option<String>,
    error: Option<String>,
    ended_at: Option<i64>,
}

impl Host {
    pub(super) async fn sync_jobs(&self) -> anyhow::Result<()> {
        let modules: Vec<_> = self.registry.read().await.values().cloned().collect();
        let mut transaction = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let mut active = Vec::new();
        for module in modules.iter().filter(|m| m.enabled) {
            for job in module
                .manifest
                .jobs
                .iter()
                .filter(|j| module.enabled_jobs.contains(&j.id))
            {
                let definition = serde_json::to_string(job)?;
                let next = job.next(Utc::now())?.timestamp_millis();
                sqlx::query("INSERT INTO external_module_jobs(module_id,job_id,definition,next_run_at) VALUES (?,?,?,?) ON CONFLICT(module_id,job_id) DO UPDATE SET next_run_at=CASE WHEN definition=excluded.definition THEN next_run_at ELSE excluded.next_run_at END,definition=excluded.definition")
                    .bind(&module.manifest.id).bind(&job.id).bind(&definition).bind(next).execute(&mut *transaction).await?;
                active.push((module.manifest.id.clone(), job.id.clone()));
            }
        }
        let existing: Vec<(String, String)> =
            sqlx::query_as("SELECT module_id,job_id FROM external_module_jobs")
                .fetch_all(&mut *transaction)
                .await?;
        for (module, job) in existing.into_iter().filter(|key| !active.contains(key)) {
            sqlx::query("DELETE FROM external_module_jobs WHERE module_id=? AND job_id=?")
                .bind(module)
                .bind(job)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn tick(self: &Arc<Self>, now: DateTime<Utc>) -> anyhow::Result<()> {
        let _guard = self.mutation.lock().await;
        let due: Vec<(String, String, String, i64)> = sqlx::query_as("SELECT module_id,job_id,definition,next_run_at FROM external_module_jobs WHERE next_run_at<=? ORDER BY next_run_at LIMIT 100")
            .bind(now.timestamp_millis()).fetch_all(&self.pool).await?;
        for (module_id, job_id, definition, at) in due {
            let Ok(permit) = self.job_slots.clone().try_acquire_owned() else {
                break;
            };
            let Ok(module) = self.module(&module_id).await else {
                continue;
            };
            if !module.enabled || !module.enabled_jobs.contains(&job_id) {
                continue;
            }
            let job: super::manifest::Job = serde_json::from_str(&definition)?;
            let next = job.next(now)?.timestamp_millis();
            let run = uuid::Uuid::new_v4().to_string();
            let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
            let changed = sqlx::query("UPDATE external_module_jobs SET next_run_at=? WHERE module_id=? AND job_id=? AND next_run_at=? AND NOT EXISTS(SELECT 1 FROM external_module_runs WHERE module_id=? AND job_id=? AND status='running')")
                .bind(next).bind(&module_id).bind(&job_id).bind(at).bind(&module_id).bind(&job_id).execute(&mut *tx).await?.rows_affected();
            if changed == 0 {
                continue;
            }
            sqlx::query("INSERT INTO external_module_runs(id,module_id,job_id,scheduled_at,status) VALUES (?,?,?,?,'running')")
                .bind(&run).bind(&module_id).bind(&job_id).bind(at).execute(&mut *tx).await?;
            tx.commit().await?;
            let pool = self.pool.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let result = module
                    .call(&job.tool, job.arguments, "model", CancellationToken::new())
                    .await;
                let (status, output, error) = match result {
                    Ok(value) => (
                        if value["isError"] == true {
                            "failed"
                        } else {
                            "completed"
                        },
                        Some(value.to_string()),
                        None,
                    ),
                    Err(error) => ("unknown", None, Some(error.to_string())),
                };
                // Persisting a completion may be retried with backoff; the
                // module operation itself never is. Giving up keeps the
                // semaphore permit free instead of stalling every job.
                let mut saved = false;
                for delay in [1, 3, 10] {
                    let attempt = sqlx::query("UPDATE external_module_runs SET status=?,result=?,error=?,ended_at=? WHERE id=?")
                        .bind(status).bind(&output).bind(&error).bind(Utc::now().timestamp_millis()).bind(&run).execute(&pool).await;
                    if attempt.is_ok() {
                        saved = true;
                        break;
                    }
                    if let Err(err) = attempt {
                        tracing::warn!("module run {run}: persisting completion failed, retrying in {delay}s: {err:#}");
                        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    }
                }
                if !saved {
                    tracing::error!("module run {run}: dropping completion record (status {status}); persistence kept failing");
                }
                let _ = sqlx::query("DELETE FROM external_module_runs WHERE module_id=? AND status!='running' AND id NOT IN (SELECT id FROM external_module_runs WHERE module_id=? ORDER BY scheduled_at DESC LIMIT 100)")
                    .bind(&module_id).bind(&module_id).execute(&pool).await;
            });
        }
        Ok(())
    }

    pub async fn runs(&self, id: &str) -> anyhow::Result<Value> {
        self.module(id).await?;
        let rows: Vec<ModuleRun> = sqlx::query_as(
            "SELECT id,job_id,scheduled_at,status,result,error,ended_at FROM external_module_runs WHERE module_id=? ORDER BY scheduled_at DESC LIMIT 30")
            .bind(id).fetch_all(&self.pool).await?;
        Ok(
            json!({"runs":rows.into_iter().map(|run| json!({
            "id":run.id,"job_id":run.job_id,"scheduled_at":run.scheduled_at,"status":run.status,
            "result":run.result.and_then(|r|serde_json::from_str::<Value>(&r).ok()),"error":run.error,"ended_at":run.ended_at
        })).collect::<Vec<_>>()}),
        )
    }
}
