use std::sync::{Arc, OnceLock};

use anyhow::Context;
use axum::http::HeaderValue;
use serde::Deserialize;
use serde_inline_default::serde_inline_default;

/// 默认 system prompt 原文，独立存放在 prompts/ 目录下便于维护。
static DEFAULT_ASSISTANT_SYSTEM_PROMPT: &str = include_str!("../prompts/assistant.md");

/// 全局配置实例。
///
/// 在 `main.rs` 启动时调用 `config::load()` 初始化，之后所有模块通过 `config::get()`
/// 读取字段。
#[serde_inline_default]
#[derive(Debug, Deserialize)]
pub struct Config {
    pub jwt_secret: String,
    /// Single-instance access password; there are no accounts or registration.
    pub auth_password: String,
    /// Defaults to firefly.sqlite3 in the process working directory.
    #[serde_inline_default("sqlite://firefly.sqlite3".into())]
    pub database_url: String,
    #[serde_inline_default("0.0.0.0:3003".into())]
    pub bind_address: String,
    /// IANA timezone for core schedules, date statistics and logs.
    #[serde_inline_default("Asia/Shanghai".into())]
    pub app_timezone: String,
    /// Delete conversations inactive for this many days; zero disables cleanup.
    #[serde_inline_default(30)]
    pub conversation_retention_days: u32,
    /// 浏览器标签页标题；运行时应用于首页和登录入口。
    #[serde_inline_default("流萤".into())]
    pub site_title: String,
    #[serde_inline_default(!cfg!(debug_assertions))]
    pub cookie_secure: bool,
    #[serde_inline_default("http://localhost:5173".into())]
    pub cors_allowed_origins: String,
    #[serde_inline_default(4)]
    pub db_max_connections: u32,
    #[serde_inline_default(10)]
    pub db_acquire_timeout_secs: u64,
    #[serde_inline_default(600)]
    pub db_idle_timeout_secs: u64,
    #[serde_inline_default("storage/assistant_uploads".into())]
    pub assistant_upload_dir: String,
    /// Maximum size of each uploaded attachment, including videos, in MiB.
    #[serde_inline_default(100)]
    pub assistant_max_attachment_mb: u32,
    #[serde_inline_default("workspace".into())]
    pub workspace_root: String,
    /// External module packages and standalone MCP TOML definitions.
    #[serde_inline_default("modules".into())]
    pub modules_dir: String,
    #[serde_inline_default("storage/modules".into())]
    pub module_data_dir: String,
    #[serde_inline_default(DEFAULT_ASSISTANT_SYSTEM_PROMPT.into())]
    pub assistant_system_prompt: String,
    #[serde_inline_default(true)]
    pub assistant_enable_tools: bool,
    /// 是否启用 firefly 的通用 http_request 工具。
    #[serde_inline_default(false)]
    pub assistant_http_tool_enabled: bool,
    /// http_request 工具允许访问的域名白名单，英文逗号分隔；留空表示不限制。
    #[serde_inline_default(String::new())]
    pub assistant_http_tool_allowed_hosts: String,
    /// Optional input cap/fallback; omitted means query the model's context window.
    #[serde(default)]
    pub assistant_context_tokens: Option<usize>,
    #[serde_inline_default(4)]
    pub assistant_max_concurrent_jobs: usize,
    #[serde_inline_default(32)]
    pub assistant_max_queued_jobs: usize,
    #[serde_inline_default(String::new())]
    pub codex_model: String,
    /// Codex 各 API 平台（~/.codex/config.toml 的 model_providers）的可选模型，
    /// 格式 `provider=m1,m2;provider2=m3`；未配置的平台前端不展示模型下拉。
    #[serde_inline_default(String::new())]
    pub codex_provider_models: String,
    #[serde_inline_default(String::new())]
    pub claude_model: String,
    /// Optional comma-separated model choices; empty uses Claude's standard aliases.
    #[serde_inline_default(String::new())]
    pub claude_models: String,
    #[serde_inline_default("dontAsk".into())]
    pub claude_permission_mode: String,
    #[serde_inline_default("Bash,Edit,Write,Read,Glob,Grep,WebFetch,WebSearch,NotebookEdit".into())]
    pub claude_allowed_tools: String,
}

impl Config {
    pub fn assistant_max_attachment_bytes(&self) -> i64 {
        i64::from(self.assistant_max_attachment_mb) * 1024 * 1024
    }

    pub fn assistant_max_chat_upload_bytes(&self) -> u64 {
        // Keep the existing request budget, with room for a larger configured file and multipart metadata.
        (256 * 1024 * 1024).max(self.assistant_max_attachment_bytes() as u64 + 1024 * 1024)
    }
}

static CONFIG: OnceLock<Arc<Config>> = OnceLock::new();

/// 自动加载 .env 文件（如果存在），不报错。
pub fn load_dotenv() {
    // Read only this application's .env; never inherit a parent application's file.
    dotenvy::from_path(".env").ok();
}

/// 从环境变量加载配置并初始化全局配置实例。
pub fn load() -> anyhow::Result<()> {
    load_dotenv();
    let config: Config = envy::from_env().context("failed to load configuration from environment")?;
    validate_auth_config(&config)?;
    validate_timezone(&config)?;
    validate_upload_config(&config)?;
    CONFIG
        .set(Arc::new(config))
        .map_err(|_| anyhow::anyhow!("config already loaded"))?;
    Ok(())
}

fn validate_auth_config(config: &Config) -> anyhow::Result<()> {
    anyhow::ensure!(config.auth_password.trim().chars().count() >= 9 && !config.auth_password.starts_with("change-me"),
        "AUTH_PASSWORD must be a custom access password of at least 9 characters");
    anyhow::ensure!(config.jwt_secret.len() >= 32 && !config.jwt_secret.starts_with("change-me"),
        "JWT_SECRET must be a custom random secret of at least 32 bytes");
    Ok(())
}

fn validate_timezone(config: &Config) -> anyhow::Result<()> {
    config.app_timezone.parse::<chrono_tz::Tz>()
        .context("APP_TIMEZONE must be an IANA timezone, such as Asia/Shanghai or America/New_York")?;
    Ok(())
}

fn validate_upload_config(config: &Config) -> anyhow::Result<()> {
    anyhow::ensure!(config.assistant_max_attachment_mb > 0, "ASSISTANT_MAX_ATTACHMENT_MB must be a positive integer");
    usize::try_from(config.assistant_max_chat_upload_bytes())
        .context("ASSISTANT_MAX_ATTACHMENT_MB is too large for this platform")?;
    Ok(())
}

pub fn timezone() -> chrono_tz::Tz {
    try_get().map(|config| config.app_timezone.parse().expect("validated APP_TIMEZONE"))
        .unwrap_or(chrono_tz::Asia::Shanghai)
}

/// 获取全局配置的只读引用。
pub fn get() -> Arc<Config> {
    CONFIG
        .get()
        .expect("config not loaded. Call config::load() in main() first")
        .clone()
}

/// 尝试获取全局配置；未初始化（如单元测试）时返回 None。
pub fn try_get() -> Option<Arc<Config>> {
    CONFIG.get().cloned()
}

/// Deterministic router-test configuration without reading or mutating environment variables.
#[cfg(test)]
pub(crate) fn init_test_config() {
    CONFIG.get_or_init(|| {
        Arc::new(
            serde_json::from_value(serde_json::json!({
                "jwt_secret": "local-test-secret",
                "auth_password": "local-test-password",
                "database_url": "sqlite::memory:"
            }))
            .expect("valid test configuration"),
        )
    });
}

/// 将逗号分隔的 CORS 源字符串解析为 `HeaderValue` 列表。
///
/// 这是唯一带转换逻辑的兼容函数，保留它以避免每个调用方重复解析。
pub fn cors_allowed_origins() -> anyhow::Result<Vec<HeaderValue>> {
    let raw = &get().cors_allowed_origins;
    raw.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| HeaderValue::from_str(s).with_context(|| format!("Invalid CORS origin: {s}")))
        .collect()
}

#[cfg(test)]
mod runtime_tests {
    #[test]
    fn attachment_upload_limits_default_and_environment_override() {
        let default: super::Config = envy::from_iter([
            ("JWT_SECRET".to_owned(), "test".to_owned()),
            ("AUTH_PASSWORD".to_owned(), "test".to_owned()),
        ]).unwrap();
        assert_eq!(default.assistant_max_attachment_mb, 100);
        assert_eq!(default.assistant_max_attachment_bytes(), 100 * 1024 * 1024);
        assert_eq!(default.assistant_max_chat_upload_bytes(), 256 * 1024 * 1024);
        assert!(super::validate_upload_config(&default).is_ok());

        for (value, expected_request_mb) in [("1", 256), ("300", 301)] {
            let config: super::Config = envy::from_iter([
                ("JWT_SECRET".to_owned(), "test".to_owned()),
                ("AUTH_PASSWORD".to_owned(), "test".to_owned()),
                ("ASSISTANT_MAX_ATTACHMENT_MB".to_owned(), value.to_owned()),
            ]).unwrap();
            assert_eq!(config.assistant_max_attachment_bytes(), value.parse::<i64>().unwrap() * 1024 * 1024);
            assert_eq!(config.assistant_max_chat_upload_bytes(), expected_request_mb * 1024 * 1024);
            assert!(super::validate_upload_config(&config).is_ok());
        }

        for value in ["0", "-1", "1.5", "invalid", "4294967296"] {
            let config = envy::from_iter::<_, super::Config>([
                ("JWT_SECRET".to_owned(), "test".to_owned()),
                ("AUTH_PASSWORD".to_owned(), "test".to_owned()),
                ("ASSISTANT_MAX_ATTACHMENT_MB".to_owned(), value.to_owned()),
            ]);
            assert!(config.is_err() || super::validate_upload_config(&config.unwrap()).is_err(), "{value}");
        }
    }

    #[test]
    fn defaults_and_timezone_validation() {
        let mut config: super::Config = serde_json::from_value(serde_json::json!({
            "jwt_secret": "test", "auth_password": "test"
        })).unwrap();
        assert_eq!(config.conversation_retention_days, 30);
        assert_eq!(config.app_timezone, "Asia/Shanghai");
        config.app_timezone = "America/New_York".into();
        assert!(super::validate_timezone(&config).is_ok());
        config.app_timezone = "not-a-timezone".into();
        assert!(super::validate_timezone(&config).is_err());
    }
}
