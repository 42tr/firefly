use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use firefly_module_sdk::{self as sdk, error::AppError};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    label: String,
    step: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            label: "计数器".into(),
            step: 1,
        }
    }
}

#[derive(Clone)]
struct AppState {
    database: SqlitePool,
    config: Config,
}

#[derive(Serialize)]
struct Counter {
    label: String,
    value: i64,
    step: i64,
}

fn response(state: &AppState, value: i64) -> Json<Counter> {
    Json(Counter {
        label: state.config.label.clone(),
        value,
        step: state.config.step,
    })
}

async fn read_counter(State(state): State<AppState>) -> Result<Json<Counter>, AppError> {
    let value = sqlx::query_scalar("SELECT value FROM counter WHERE id=1")
        .fetch_one(&state.database)
        .await?;
    Ok(response(&state, value))
}

async fn increment(State(state): State<AppState>) -> Result<Json<Counter>, AppError> {
    // Update atomically and keep SQLite from promoting an overflowing integer to REAL.
    let value = sqlx::query_scalar(
        "UPDATE counter SET value=value+?1 WHERE id=1 AND value<=?2 RETURNING value",
    )
    .bind(state.config.step)
    .bind(i64::MAX - state.config.step)
    .fetch_optional(&state.database)
    .await?
    .ok_or_else(|| AppError::bad_request("计数器已达到最大值"))?;
    Ok(response(&state, value))
}

async fn reset(State(state): State<AppState>) -> Result<Json<Counter>, AppError> {
    let value = sqlx::query_scalar("UPDATE counter SET value=0 WHERE id=1 RETURNING value")
        .fetch_one(&state.database)
        .await?;
    Ok(response(&state, value))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // MCP uses stdout; the SDK directs application logs to stderr.
    sdk::init_logging();
    let config: Config = sdk::config()?;
    anyhow::ensure!(
        (1..=80).contains(&config.label.trim().chars().count()),
        "计数器名称需要 1–80 个字符"
    );
    anyhow::ensure!((1..=1000).contains(&config.step), "步长需要在 1–1000 之间");
    let path = sdk::data_dir()?.join("counter.sqlite3");
    let options = sdk::db::options("sqlite://counter.sqlite3", true)?.filename(path);
    let database = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS counter (id INTEGER PRIMARY KEY CHECK(id=1), value INTEGER NOT NULL CHECK(typeof(value)='integer' AND value>=0))")
        .execute(&database).await?;
    sqlx::query("INSERT OR IGNORE INTO counter(id,value) VALUES (1,0)")
        .execute(&database)
        .await?;
    let router = Router::new()
        .route("/counter", get(read_counter))
        .route("/increment", post(increment))
        .route("/reset", post(reset))
        .with_state(AppState { database, config });
    sdk::App {
        id: "rust-sdk-counter",
        title: "SDK 计数器",
        description: "计数器模块：read({path:'/counter'}) 查询数值；request({method:'POST',path:'/increment'}) 按配置步长增加；request({method:'POST',path:'/reset'}) 清零。修改操作会保存到模块自己的 SQLite。",
        router,
        public_router: None,
        jobs: &[],
        has_indicator: false,
    }.serve().await
}
