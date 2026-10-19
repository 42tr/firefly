use std::{
    future::{Future, IntoFuture},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::Utc;
use firefly_db::backup as db_backup;
use firefly_llm as llm;
use firefly_module_host as modules;
use firefly_workspace::{files as workspace_files, prepare as workspace_prepare};
use tokio::{net::TcpListener, time::timeout};
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::{error, info};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod agent;
mod agent_settings;
mod api;
mod assistant_cleanup;
mod assistant_events;
mod assistant_job;
mod assistant_store;
mod assistant_tasks;
mod assistant_workspace;
mod claude;
mod codex;
mod config;
mod db;
mod http_client;
mod logx;
mod mcp;
mod office;
mod pdf;
mod process_control;
mod storage;
mod utils;

/// 检查运行环境（环境变量）
///
/// 通过 `config::load()` 一次性加载并校验所有必填配置。
fn check_runtime_environment() {
    if let Err(err) = config::load() {
        eprintln!("Failed to load configuration: {:#}", err);
        std::process::exit(1);
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    // Worker protocols use stdout exclusively for JSON events or file bytes.
    // Run before logging and configuration; they need neither a DB nor Python.
    if args.get(1).map(String::as_str) == Some("--workspace-worker") {
        anyhow::ensure!(args.len() == 3, "invalid workspace worker arguments");
        return workspace_prepare::run(&args[2]);
    }
    if args.get(1).map(String::as_str) == Some("--file-worker") {
        anyhow::ensure!(args.len() == 5, "invalid file worker arguments");
        return workspace_files::run(std::path::Path::new(&args[2]), &args[3], &args[4]);
    }
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(run(args))
}

async fn run(args: Vec<String>) -> anyhow::Result<()> {
    logx::init();
    if args.get(1).map(String::as_str) == Some("--init-db") {
        anyhow::ensure!(args.len() == 3, "usage: firefly --init-db sqlite:///absolute/path/firefly.sqlite3");
        return db::initialize(&args[2]).await;
    }
    if args.get(1).map(String::as_str) == Some("--backup-db") {
        anyhow::ensure!(args.len() == 4, "usage: firefly --backup-db source.sqlite3 new-backup.sqlite3");
        db_backup::backup(std::path::Path::new(&args[2]), std::path::Path::new(&args[3]))?;
        println!("{}", serde_json::json!({ "backup": args[3], "verified": true }));
        return Ok(());
    }
    anyhow::ensure!(args.len() == 1, "unknown command line arguments");
    check_runtime_environment();

    // Initialize database
    let database_url = config::get().database_url.clone();
    db::init(&database_url).await?;
    assistant_store::schedules::synchronize_timezone(db::pool(), Utc::now(), config::timezone()).await?;
    modules::start(
        db::pool().clone(),
        modules::Config {
            directory: config::get().modules_dir.clone().into(),
            data_directory: config::get().module_data_dir.clone().into(),
            timezone: config::timezone(),
        },
    ).await?;
    agent_settings::Settings::load(db::pool()).await?;
    assistant_tasks::start(db::pool().clone()).await?;

    // 恢复上次退出时被中断的生成任务，启动后自动继续处理，
    // 避免前端把中断的回复显示为“处理失败”。
    match assistant_store::requeue_interrupted_generations().await {
        Ok(recovery) => {
            if recovery.requeued == 0 && recovery.completed == 0 && recovery.failed == 0 {
                info!("No interrupted generation jobs to resume");
            } else {
                info!(
                    "Recovered interrupted generation jobs: {} requeued, {} completed, {} failed",
                    recovery.requeued, recovery.completed, recovery.failed
                );
            }
        }
        Err(err) => error!("Failed to requeue interrupted generation jobs: {}", err),
    }
    if let Err(err) = api::assistant::resume_queued_generations().await {
        error!("Failed to resume queued assistant generations: {}", err);
    }

    let sched = JobScheduler::new().await?;
    // Internal data retention runs independently of external recurring jobs.
    if config::get().conversation_retention_days > 0 {
        register_conversation_cleanup_job(&sched).await?;
    }
    register_db_maintenance_job(&sched).await?;
    // User-created schedules remain active independently of external recurring jobs.
    add_tz_job(&sched, "*/10 * * * * *", || async {
        if let Err(err) = api::assistant::schedules::tick().await {
            error!("Scheduled assistant tasks failed: {err:#}");
        }
    }).await?;
    sched.start().await?;

    let app = api::app();
    let bind_address = config::get().bind_address.clone();
    info!("Server listening on {}", bind_address);
    let listener = TcpListener::bind(&bind_address).await?;
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    );
    tokio::select! {
        result = server.into_future() => { result?; }
        _ = shutdown_signal() => {}
    }
    modules::shutdown().await;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

async fn run_guarded<F>(label: &'static str, fut: F, timeout_duration: Duration)
where
    F: Future<Output=anyhow::Result<()>>, {
    match timeout(timeout_duration, fut).await {
        Ok(Ok(_)) => info!("{} completed", label),
        Ok(Err(err)) => error!("{} failed: {}", label, err),
        Err(_) => error!("{} timed out after {:?}", label, timeout_duration),
    }
}

async fn register_conversation_cleanup_job(sched: &JobScheduler) -> anyhow::Result<()> {
    add_tz_job(sched, "0 0 0 * * *", || async {
        run_guarded(
            "inactive conversation cleanup",
            assistant_cleanup::clean_inactive_conversations(),
            Duration::from_secs(600),
        )
        .await;
    })
    .await
}

// 低峰期更新查询计划统计信息；与对话清理一样在启动时注册为内部维护任务。
async fn register_db_maintenance_job(sched: &JobScheduler) -> anyhow::Result<()> {
    add_tz_job(sched, "0 20 4 * * *", || async {
        run_guarded("SQLite optimize", db::optimize(), Duration::from_secs(120)).await;
    })
    .await
}

async fn add_tz_job<F, Fut>(sched: &JobScheduler, spec: &'static str, task: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut+Send+Sync+'static,
    Fut: Future<Output=()>+Send+'static, {
    let timezone = config::timezone();
    let task = Arc::new(tokio::sync::Mutex::new(task));
    // 防止上一次执行尚未结束时同一任务叠加运行
    let running = Arc::new(AtomicBool::new(false));
    sched
        .add(Job::new_async_tz(spec, timezone, move |_uuid, mut _l| {
            let task = task.clone();
            let running = running.clone();
            Box::pin(async move {
                if running.swap(true, Ordering::SeqCst) {
                    info!("cron job '{}' skipped: previous run still in progress", spec);
                    return;
                }
                let fut = {
                    let mut guard = task.lock().await;
                    (guard)()
                };
                // 在独立的 tokio 任务中执行：即使任务 panic 也只会体现为 JoinError，
                // 不会波及调度器线程导致后续调度丢失。
                let result = tokio::spawn(fut).await;
                running.store(false, Ordering::SeqCst);
                if let Err(err) = result {
                    error!("cron job '{}' terminated abnormally: {}", spec, err);
                }
            })
        })?)
        .await?;
    Ok(())
}
