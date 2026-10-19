//! Runtime-loaded, out-of-process modules. No module code is linked into Firefly.
pub mod client;
pub mod manifest;
mod schedules;
#[cfg(test)]
mod tests;

use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

/// Application-owned paths and timezone used by the module host.
#[derive(Clone, Debug)]
pub struct Config {
    pub directory: PathBuf,
    pub data_directory: PathBuf,
    /// Passed to module subprocesses as `FIREFLY_MODULE_TIMEZONE`.
    /// Scheduled jobs use the timezone in their own manifests.
    pub timezone: chrono_tz::Tz,
}

pub struct Module {
    pub manifest: manifest::Manifest,
    pub root: PathBuf,
    pub data_dir: PathBuf,
    timezone: chrono_tz::Tz,
    pub schema: Value,
    pub settings: Value,
    pub enabled: bool,
    pub enabled_jobs: Vec<String>,
    pub stop: CancellationToken,
    pub connection: Mutex<client::Connection>,
}

#[derive(Clone, Serialize)]
pub struct Diagnostic {
    pub file: String,
    pub error: String,
}
pub struct Host {
    pub pool: SqlitePool,
    pub directory: PathBuf,
    pub data_directory: PathBuf,
    timezone: chrono_tz::Tz,
    pub registry: RwLock<BTreeMap<String, Arc<Module>>>,
    diagnostics: RwLock<Vec<Diagnostic>>,
    mutation: Mutex<()>,
    pub stop: CancellationToken,
    pub job_slots: Arc<tokio::sync::Semaphore>,
}
static HOST: OnceLock<Arc<Host>> = OnceLock::new();
pub fn get() -> anyhow::Result<&'static Arc<Host>> {
    HOST.get().context("模块宿主尚未启动")
}

/// Start the process-wide host and its scheduler using an initialized database.
pub async fn start(pool: SqlitePool, config: Config) -> anyhow::Result<()> {
    let host = Host::new(pool, config).await?;
    HOST.set(host.clone())
        .map_err(|_| anyhow::anyhow!("模块宿主已启动"))?;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            tokio::select! {
                _ = host.stop.cancelled() => break,
                _ = interval.tick() => if let Err(error) = host.tick(chrono::Utc::now()).await {
                    tracing::error!("Module scheduler: {error}");
                },
            }
        }
    });
    Ok(())
}

pub async fn shutdown() {
    if let Ok(host) = get() {
        host.stop.cancel();
        let modules: Vec<_> = host.registry.read().await.values().cloned().collect();
        for module in modules {
            module.close().await;
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub enabled: bool,
    #[serde(default)]
    pub settings: Option<Value>,
    #[serde(default)]
    pub enabled_jobs: Option<Vec<String>>,
}

impl Host {
    /// Load modules from disk using a database already migrated by the application.
    /// Independent instances are driven by explicit calls to [`Self::tick`].
    pub async fn new(pool: SqlitePool, config: Config) -> anyhow::Result<Arc<Self>> {
        let Config {
            directory,
            data_directory,
            timezone,
        } = config;
        std::fs::create_dir_all(&directory)?;
        std::fs::create_dir_all(&data_directory)?;
        let host = Arc::new(Self {
            pool,
            directory: directory.canonicalize()?,
            data_directory: data_directory.canonicalize()?,
            timezone,
            registry: RwLock::new(BTreeMap::new()),
            diagnostics: RwLock::new(Vec::new()),
            mutation: Mutex::new(()),
            stop: CancellationToken::new(),
            job_slots: Arc::new(tokio::sync::Semaphore::new(4)),
        });
        sqlx::query("UPDATE external_module_runs SET status='unknown',error='服务重启，执行结果未知；未自动重试',ended_at=? WHERE status='running'")
            .bind(chrono::Utc::now().timestamp_millis()).execute(&host.pool).await?;
        host.reload().await?;
        Ok(host)
    }

    pub async fn module(&self, id: &str) -> anyhow::Result<Arc<Module>> {
        self.registry
            .read()
            .await
            .get(id)
            .cloned()
            .context("模块不存在，请重新扫描模块目录")
    }

    pub async fn reload(&self) -> anyhow::Result<()> {
        let _guard = self.mutation.lock().await;
        self.reload_locked().await
    }

    async fn reload_locked(&self) -> anyhow::Result<()> {
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if path.is_dir() && path.join("module.toml").is_file() {
                paths.push(path.join("module.toml"));
            } else if path.is_file() && path.extension().is_some_and(|e| e == "toml") {
                paths.push(path);
            }
        }
        ensure!(paths.len() <= 100, "最多安装 100 个模块");
        paths.sort();
        let mut loaded = Vec::new();
        let mut diagnostics = Vec::new();
        let mut counts = BTreeMap::new();
        for path in paths {
            let name = path
                .strip_prefix(&self.directory)
                .unwrap_or(&path)
                .display()
                .to_string();
            match manifest::Manifest::load(&path) {
                Ok(value) => {
                    *counts.entry(value.0.id.clone()).or_insert(0) += 1;
                    loaded.push((name, value));
                }
                Err(error) => diagnostics.push(Diagnostic {
                    file: name,
                    error: error.to_string(),
                }),
            }
        }
        let previous_registry = self.registry.read().await.clone();
        let mut registry = BTreeMap::new();
        for (name, (definition, root, schema)) in loaded {
            if counts[&definition.id] > 1 {
                diagnostics.push(Diagnostic {
                    file: name,
                    error: "模块 ID 重复；同 ID 模块均未加载".into(),
                });
                continue;
            }
            let defaults: serde_json::Map<String, Value> = schema["properties"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(k, v)| v.get("default").map(|value| (k.clone(), value.clone())))
                .collect();
            let jobs: Vec<_> = definition
                .jobs
                .iter()
                .filter(|j| j.enabled_by_default)
                .map(|j| j.id.clone())
                .collect();
            sqlx::query("INSERT OR IGNORE INTO external_module_settings(id,settings,enabled_jobs) VALUES (?,?,?)")
                .bind(&definition.id).bind(Value::Object(defaults).to_string()).bind(serde_json::to_string(&jobs)?).execute(&self.pool).await?;
            let row: (bool, String, String) = sqlx::query_as(
                "SELECT enabled,settings,enabled_jobs FROM external_module_settings WHERE id=?",
            )
            .bind(&definition.id)
            .fetch_one(&self.pool)
            .await?;
            let settings: Value = serde_json::from_str(&row.1)?;
            let valid = jsonschema::validator_for(&schema)?.is_valid(&settings);
            let data_dir = self.data_directory.join(&definition.id);
            std::fs::create_dir_all(&data_dir)?;
            let data_dir = data_dir.canonicalize()?;
            ensure!(
                data_dir.starts_with(&self.data_directory),
                "模块数据目录超出宿主数据根目录"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))?;
            }
            let error = (!valid).then(|| "请补全模块配置后启用".to_string());
            let module = Arc::new(Module {
                manifest: definition,
                root,
                data_dir,
                timezone: self.timezone,
                schema,
                settings,
                enabled: row.0 && valid,
                enabled_jobs: serde_json::from_str(&row.2)?,
                stop: CancellationToken::new(),
                connection: Mutex::new(client::Connection {
                    error,
                    ..Default::default()
                }),
            });
            let retained = previous_registry.get(&module.manifest.id).filter(|old| {
                old.enabled == module.enabled
                    && old.settings == module.settings
                    && old.enabled_jobs == module.enabled_jobs
                    && old.root == module.root
                    && old.schema == module.schema
                    && serde_json::to_value(&old.manifest).ok()
                        == serde_json::to_value(&module.manifest).ok()
            });
            registry.insert(
                module.manifest.id.clone(),
                retained.cloned().unwrap_or(module),
            );
        }
        // Revoke old handles before exposing new ones: stale queued/model calls cannot execute.
        let previous = {
            let mut current = self.registry.write().await;
            for (id, module) in current.iter() {
                if registry
                    .get(id)
                    .is_none_or(|next| !Arc::ptr_eq(module, next))
                {
                    module.stop.cancel();
                }
            }
            std::mem::replace(&mut *current, registry)
        };
        *self.diagnostics.write().await = diagnostics;
        for module in previous.values().filter(|m| m.stop.is_cancelled()) {
            module.close().await;
        }
        self.sync_jobs().await?;
        Ok(())
    }

    pub async fn update(&self, id: &str, input: Update) -> anyhow::Result<()> {
        let _guard = self.mutation.lock().await;
        let module = self.module(id).await?;
        let settings = match input.settings {
            Some(patch) => {
                ensure!(patch.is_object(), "配置必须为 JSON 对象");
                manifest::merge_settings(&module.schema, &module.settings, &patch)
            }
            None => module.settings.clone(),
        };
        ensure!(
            serde_json::to_vec(&settings)?.len() <= 64 * 1024,
            "配置超过 64 KiB"
        );
        if input.enabled {
            ensure!(
                jsonschema::validator_for(&module.schema)?.is_valid(&settings),
                "配置不符合模块 Schema，请补全必填项"
            );
        }
        let jobs = input
            .enabled_jobs
            .unwrap_or_else(|| module.enabled_jobs.clone());
        ensure!(
            jobs.len() <= 20
                && jobs
                    .iter()
                    .all(|id| module.manifest.jobs.iter().any(|j| &j.id == id)),
            "定时任务 ID 无效"
        );
        sqlx::query(
            "UPDATE external_module_settings SET enabled=?,settings=?,enabled_jobs=? WHERE id=?",
        )
        .bind(input.enabled)
        .bind(settings.to_string())
        .bind(serde_json::to_string(&jobs)?)
        .bind(id)
        .execute(&self.pool)
        .await?;
        self.reload_locked().await
    }

    pub async fn list(&self) -> Value {
        let modules: Vec<_> = self.registry.read().await.values().cloned().collect();
        let mut entries = Vec::new();
        for module in modules {
            let state = module.connection.try_lock().ok();
            let error = state.as_ref().and_then(|state| state.error.clone());
            entries.push(json!({"id":module.manifest.id,"name":module.manifest.name,"description":module.manifest.description,
                "version":module.manifest.version,"enabled":module.enabled,"status":if !module.enabled { "disabled" } else if state.is_none() { "running" } else if error.is_some() { "error" } else { "ready" },
                "error":error,"pages":module.manifest.pages,"indicator":module.manifest.indicator,"jobs":module.manifest.jobs,"enabled_jobs":module.enabled_jobs,
                "settings":manifest::public_settings(&module.schema,&module.settings),"config_schema":module.schema}));
        }
        json!({"modules":entries,"diagnostics":self.diagnostics.read().await.clone()})
    }
}

pub async fn agent_tools() -> Vec<Arc<dyn g::Tool>> {
    use futures_util::{stream, StreamExt};
    let Ok(host) = get() else {
        return Vec::new();
    };
    let modules: Vec<_> = host
        .registry
        .read()
        .await
        .values()
        .filter(|m| m.enabled)
        .cloned()
        .collect();
    stream::iter(modules)
        .map(|module| async move {
            match module.tools().await {
                Ok(tools) => tools
                    .into_iter()
                    .filter(|t| client::visible(t, "model"))
                    .map(|definition| {
                        Arc::new(client::ModuleTool {
                            module: module.clone(),
                            definition,
                        }) as Arc<dyn g::Tool>
                    })
                    .collect::<Vec<_>>(),
                Err(error) => {
                    tracing::warn!(module=%module.manifest.id, "模块工具暂不可用: {error}");
                    Vec::new()
                }
            }
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect()
}

pub async fn recovered_tool(name: &str) -> Option<Arc<dyn g::Tool>> {
    if !name.starts_with("mcp__") {
        return None;
    }
    agent_tools()
        .await
        .into_iter()
        .find(|tool| tool.spec().name == name)
}

/// Resolve only explicit public mounts. Ambiguous mounts and disabled modules never serve data.
pub async fn public_resource(path: &str) -> Option<axum::response::Response> {
    use base64::Engine;
    let host=get().ok()?;
    let candidates:Vec<_>=host.registry.read().await.values().filter(|module| module.enabled && module.manifest.public.as_ref().is_some_and(|p| path.starts_with(&format!("{}/",p.prefix)))).cloned().collect();
    if candidates.len()!=1 {return None;}
    let module=&candidates[0];let public=module.manifest.public.as_ref()?;
    let tools=module.tools().await.ok()?;
    let tool=tools.iter().find(|t|t["name"].as_str()==Some(&public.tool))?;
    if tool["annotations"]["readOnlyHint"] != true {return None;}
    let result=module.call(&public.tool,json!({"path":path}),"app",CancellationToken::new()).await.ok()?;
    if result["isError"]==true {return None;}
    let value=&result["structuredContent"];
    if value["status"]!=200{return None;}
    let bytes=base64::engine::general_purpose::STANDARD.decode(value["base64"].as_str()?).ok()?;
    if bytes.len()>8*1024*1024{return None;}
    let mut response=axum::response::Response::builder().header("content-type","application/octet-stream").header("cache-control","no-store");
    let disposition=value["headers"]["content-disposition"].as_str().filter(|s|s.starts_with("attachment;")).unwrap_or("attachment");
    response=response.header("content-disposition",disposition);
    response.body(axum::body::Body::from(bytes)).ok()
}
