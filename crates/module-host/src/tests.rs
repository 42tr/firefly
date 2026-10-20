use super::*;
use g::Tool;

async fn test_pool() -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    // Exercise the application's actual schema without depending on its globals.
    sqlx::migrate!("../../migrations_sqlite")
        .run(&pool)
        .await
        .unwrap();
    pool
}

#[test]
fn public_download_prefix_cannot_capture_core_or_root_paths() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("module.toml");
    for prefix in [
        "/",
        "/api",
        "/auth",
        "/health",
        "/assets",
        "/one/two",
        "/downloads",
    ] {
        std::fs::write(&path, format!(
            "id='test'\n[mcp]\ntransport='stdio'\ncommand='./module'\n[public]\nprefix='{prefix}'\ntool='read_public'\n"
        )).unwrap();
        assert_eq!(
            manifest::Manifest::load(&path).is_ok(),
            prefix == "/downloads",
            "{prefix}"
        );
    }
}

async fn calendar_host() -> (tempfile::TempDir, Arc<Host>) {
    let temp = tempfile::tempdir().unwrap();
    let installed = temp.path().join("modules/calendar");
    std::fs::create_dir_all(&installed).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for file in ["module.toml", "config.schema.json", "server.py"] {
        std::fs::copy(
            root.join("examples/modules/calendar").join(file),
            installed.join(file),
        )
        .unwrap();
    }
    std::fs::copy(
        root.join("frontend/examples/calendar/calendar.html"),
        installed.join("calendar.html"),
    )
    .unwrap();
    let host = Host::new(
        test_pool().await,
        Config {
            directory: temp.path().join("modules"),
            data_directory: temp.path().join("data"),
            timezone: chrono_tz::Europe::London,
        },
    )
    .await
    .unwrap();
    (temp, host)
}

fn enabled() -> Update {
    Update {
        enabled: true,
        settings: None,
        enabled_jobs: None,
    }
}

#[tokio::test]
async fn configured_timezone_is_passed_to_module_processes() {
    let (temp, host) = calendar_host().await;
    let directory = temp.path().join("modules/calendar");
    std::fs::rename(
        directory.join("server.py"),
        directory.join("calendar_example.py"),
    )
    .unwrap();
    std::fs::write(
        directory.join("server.py"),
        r#"import os
import calendar_example as example
original_call = example.call
def call(name, arguments):
    result = original_call(name, arguments)
    result["structuredContent"]["host_timezone"] = os.environ["FIREFLY_MODULE_TIMEZONE"]
    return result
example.call = call
example.serve()
"#,
    )
    .unwrap();
    host.update("personal-calendar", enabled()).await.unwrap();
    let module = host.module("personal-calendar").await.unwrap();
    let result = module
        .call("list_dates", json!({}), "app", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        result["structuredContent"]["host_timezone"],
        "Europe/London"
    );
    module.close().await;
}

#[tokio::test]
async fn packaged_readonly_indicator_is_revoked_on_disable_and_restored_on_enable() {
    let (temp, host) = calendar_host().await;
    let directory = temp.path().join("modules/calendar");
    let path = directory.join("module.toml");
    let manifest = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!("{manifest}\n[indicator]\ntool='check_reminders'\nrefresh_secs=60\n"),
    )
    .unwrap();
    std::fs::rename(
        directory.join("server.py"),
        directory.join("calendar_example.py"),
    )
    .unwrap();
    std::fs::write(
        directory.join("server.py"),
        r#"import calendar_example as example
original_call = example.call
def call(name, arguments):
    result = original_call(name, arguments)
    if name == "check_reminders":
        result["structuredContent"]["indicator"] = {"label": "Example ready", "tone": "info"}
    return result
example.call = call
example.serve()
"#,
    )
    .unwrap();
    host.reload().await.unwrap();
    host.update("personal-calendar", enabled()).await.unwrap();
    let module = host.module("personal-calendar").await.unwrap();
    assert_eq!(
        module.indicator().await.unwrap()["indicator"]["label"],
        "Example ready"
    );
    host.update(
        "personal-calendar",
        Update {
            enabled: false,
            settings: None,
            enabled_jobs: None,
        },
    )
    .await
    .unwrap();
    assert!(module.indicator().await.is_err());
    host.update("personal-calendar", enabled()).await.unwrap();
    let fresh = host.module("personal-calendar").await.unwrap();
    assert_eq!(
        fresh.indicator().await.unwrap()["indicator"]["label"],
        "Example ready"
    );
    fresh.close().await;
}

#[tokio::test]
async fn indicators_never_automatically_invoke_mutating_tools() {
    let (temp, host) = calendar_host().await;
    let path = temp.path().join("modules/calendar/module.toml");
    let manifest = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{manifest}\n[indicator]\ntool='save_date'\nrefresh_secs=60\narguments={{id='must-not-save',name='test',date='10-13'}}\n")).unwrap();
    host.reload().await.unwrap();
    host.update("personal-calendar", enabled()).await.unwrap();
    let module = host.module("personal-calendar").await.unwrap();
    assert!(module
        .indicator()
        .await
        .unwrap_err()
        .to_string()
        .contains("只读"));
    assert!(!module.data_dir.join("calendar.sqlite3").exists());
    module.close().await;
}

#[tokio::test]
async fn external_package_tools_page_data_and_revocation_work_without_registration() {
    let (_temp, host) = calendar_host().await;
    let disabled = host.module("personal-calendar").await.unwrap();
    assert!(!disabled.enabled);
    assert!(disabled.tools().await.is_err());
    host.update("personal-calendar", enabled()).await.unwrap();
    let module = host.module("personal-calendar").await.unwrap();
    let tools = module.tools().await.unwrap();
    assert_eq!(tools.len(), 4);
    let result = module
        .call(
            "save_date",
            json!({"id":"test","name":"测试纪念日","date":"10-13"}),
            "app",
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        result["structuredContent"]["dates"][0]["name"],
        "测试纪念日"
    );
    let (_, html) = module.page_resource("calendar").await.unwrap();
    assert!(html.contains("ui/initialize"));
    assert!(module
        .call(
            "save_date",
            json!({"id":"bad"}),
            "model",
            CancellationToken::new()
        )
        .await
        .is_err());
    let tool = client::ModuleTool {
        module: module.clone(),
        definition: tools
            .iter()
            .find(|t| t["name"] == "list_dates")
            .unwrap()
            .clone(),
    };
    assert_eq!(tool.spec().name, "mcp__personal-calendar__list_dates");
    let output = tool
        .call_output(
            g::ToolContext {
                run_id: uuid::Uuid::new_v4(),
                cancellation_token: CancellationToken::new(),
            },
            json!({}),
        )
        .await
        .unwrap();
    assert!(!output.is_error);
    host.reload().await.unwrap();
    assert!(Arc::ptr_eq(
        &module,
        &host.module("personal-calendar").await.unwrap()
    ));
    host.update(
        "personal-calendar",
        Update {
            enabled: false,
            settings: None,
            enabled_jobs: None,
        },
    )
    .await
    .unwrap();
    assert!(module
        .call("list_dates", json!({}), "model", CancellationToken::new())
        .await
        .is_err());
    host.update("personal-calendar", enabled()).await.unwrap();
    let fresh = host.module("personal-calendar").await.unwrap();
    let result = fresh
        .call("list_dates", json!({}), "app", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        result["structuredContent"]["dates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fresh.close().await;
}

#[tokio::test]
async fn scheduler_claims_once_and_does_not_replay_unknown_runs() {
    let (_temp, host) = calendar_host().await;
    host.update(
        "personal-calendar",
        Update {
            enabled: true,
            settings: None,
            enabled_jobs: Some(vec!["daily-check".into()]),
        },
    )
    .await
    .unwrap();
    let now = chrono::Utc::now();
    sqlx::query("UPDATE external_module_jobs SET next_run_at=?")
        .bind(now.timestamp_millis() - 1000)
        .execute(&host.pool)
        .await
        .unwrap();
    host.tick(now).await.unwrap();
    host.tick(now).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let status: String = sqlx::query_scalar("SELECT status FROM external_module_runs")
                .fetch_one(&host.pool)
                .await
                .unwrap();
            if status != "running" {
                assert_eq!(status, "completed");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM external_module_runs")
        .fetch_one(&host.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    sqlx::query("UPDATE external_module_runs SET status='running'")
        .execute(&host.pool)
        .await
        .unwrap();
    host.module("personal-calendar")
        .await
        .unwrap()
        .close()
        .await;
    let restarted = Host::new(
        host.pool.clone(),
        Config {
            directory: host.directory.clone(),
            data_directory: host.data_directory.clone(),
            timezone: host.timezone,
        },
    )
    .await
    .unwrap();
    restarted.tick(now).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM external_module_runs")
        .fetch_one(&host.pool)
        .await
        .unwrap();
    assert_eq!(status, "unknown");
    restarted
        .module("personal-calendar")
        .await
        .unwrap()
        .close()
        .await;
}

#[tokio::test]
async fn malformed_and_duplicate_modules_do_not_load_or_start() {
    let (temp, host) = calendar_host().await;
    std::fs::write(
        temp.path().join("modules/bad.toml"),
        "id='secret-value'\ninvalid",
    )
    .unwrap();
    std::fs::copy(
        temp.path().join("modules/calendar/module.toml"),
        temp.path().join("modules/calendar/copy.toml"),
    )
    .unwrap();
    host.reload().await.unwrap();
    assert_eq!(
        host.list().await["diagnostics"].as_array().unwrap().len(),
        1
    );
    std::fs::write(
        temp.path().join("modules/duplicate.toml"),
        "id='personal-calendar'\n[mcp]\ntransport='stdio'\ncommand='never-execute'",
    )
    .unwrap();
    host.reload().await.unwrap();
    assert!(host.registry.read().await.is_empty());
    assert!(host.list().await["diagnostics"]
        .to_string()
        .contains("重复"));
    assert!(!host.list().await["diagnostics"]
        .to_string()
        .contains("secret-value"));
}

#[test]
fn secrets_visibility_schema_and_alias_boundaries() {
    let schema = json!({"type":"object","properties":{"token":{"type":"string","writeOnly":true},"nested":{"type":"object","properties":{"password":{"format":"password"}}}}});
    let values = json!({"token":"secret","nested":{"password":"private"}});
    let public = manifest::public_settings(&schema, &values);
    assert_eq!(public, json!({"token":null,"nested":{"password":null}}));
    assert_eq!(manifest::merge_settings(&schema, &values, &public), values);
    let array_schema = json!({"type":"array","items":{"type":"object","properties":{"token":{"type":"string","writeOnly":true}}}});
    let secrets = json!([{"token":"private"}]);
    assert_eq!(
        manifest::merge_settings(
            &array_schema,
            &secrets,
            &manifest::public_settings(&array_schema, &secrets)
        ),
        secrets
    );
    assert_eq!(
        manifest::merge_settings(&schema, &values, &json!({})),
        json!({})
    );
    assert!(
        manifest::validate_settings_schema(&json!({"properties":{"oneOf":{"type":"string"}}}))
            .is_ok()
    );
    assert!(
        manifest::validate_settings_schema(&json!({"prefixItems":[{"writeOnly":true}]})).is_err()
    );
    assert!(manifest::validate_schema(&json!({"$ref":"http://localhost/schema"})).is_err());
    assert!(manifest::validate_schema(&json!({"$ref":"file:///etc/passwd"})).is_err());
    assert!(!client::visible(
        &json!({"_meta":{"ui":{"visibility":["app"]}}}),
        "model"
    ));
    let alias = client::alias("calendar", &"中文工具".repeat(30));
    assert!(alias.len() <= 64 && alias.is_ascii());
    assert_ne!(alias, client::alias("other", &"中文工具".repeat(30)));
}

#[tokio::test]
async fn http_module_tools_and_errors_are_not_replayed() {
    use axum::{routing::post, Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route("/mcp",post(move |Json(body):Json<Value>| {
        let calls = observed.clone();
        async move {
            let result = match body["method"].as_str().unwrap_or_default() {
                "initialize" => json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1"}}),
                "tools/list" => json!({"tools":[{"name":"write","description":"write once","inputSchema":{"type":"object","properties":{}}}]}),
                "tools/call" => { calls.fetch_add(1,Ordering::SeqCst); json!({"isError":true,"content":[{"type":"text","text":"business failure"}]}) },
                _ => json!({}),
            };
            Json(json!({"jsonrpc":"2.0","id":body["id"],"result":result}))
        }
    }));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("modules");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join("http.toml"),
        format!("id='http-test'\n[mcp]\ntransport='streamable_http'\nurl='http://{address}/mcp'\n"),
    )
    .unwrap();
    let host = Host::new(
        test_pool().await,
        Config {
            directory,
            data_directory: temp.path().join("data"),
            timezone: chrono_tz::UTC,
        },
    )
    .await
    .unwrap();
    host.update("http-test", enabled()).await.unwrap();
    let module = host.module("http-test").await.unwrap();
    let result = module
        .call("write", json!({}), "app", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["isError"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    module.close().await;
    server.abort();
}
