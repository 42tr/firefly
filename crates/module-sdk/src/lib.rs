//! Generic stdio MCP adapter. Business routes execute inside the module process;
//! no listening socket, host database, session cookie, or host API proxy exists.
pub mod auth;
pub mod db;
pub mod diagnostics;
pub mod error;
pub mod http_client;
pub mod migration;
pub mod utils;
use anyhow::{ensure, Context};
use axum::{
    body::{to_bytes, Body},
    http::{Method, Request},
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tower::ServiceExt;

pub const MAX_BODY: usize = 8 * 1024 * 1024;
pub fn data_dir() -> anyhow::Result<PathBuf> {
    let path = PathBuf::from(
        std::env::var("FIREFLY_MODULE_DATA_DIR").context("FIREFLY_MODULE_DATA_DIR is required")?,
    );
    ensure!(path.is_absolute(), "module data directory must be absolute");
    std::fs::create_dir_all(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path)
}
pub fn config<T: DeserializeOwned>() -> anyhow::Result<T> {
    Ok(serde_json::from_str(
        &std::env::var("FIREFLY_MODULE_CONFIG").unwrap_or_else(|_| "{}".into()),
    )?)
}
pub fn config_with_env<T: DeserializeOwned>(fields: &[(&str, &str)]) -> anyhow::Result<T> {
    let mut value: Value = config()?;
    let object = value
        .as_object_mut()
        .context("module config must be an object")?;
    for (name, kind) in fields {
        if object
            .get(*name)
            .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
        {
            continue;
        }
        if let Ok(raw) = std::env::var(name.to_uppercase()) {
            if raw.is_empty() {
                continue;
            }
            object.insert(
                (*name).into(),
                if *kind == "string" {
                    Value::String(raw)
                } else {
                    serde_json::from_str(&raw).with_context(|| format!("invalid {}", name))?
                },
            );
        }
    }
    Ok(serde_json::from_value(value)?)
}
pub fn init_logging() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter("warn")
        .init();
}
pub struct App {
    pub id: &'static str,
    pub title: &'static str,
    /// Document module-owned routes for model callers. No arbitrary host requests.
    pub description: &'static str,
    pub router: Router,
    pub public_router: Option<Router>,
    pub jobs: &'static [(&'static str, &'static str)],
    pub has_indicator: bool,
}
fn tool(name: &str, description: &str, read: bool, app_only: bool, schema: Value) -> Value {
    json!({"name":name,"description":description,"inputSchema":schema,
        "annotations":{"readOnlyHint":read,"destructiveHint":!read,"idempotentHint":read,"openWorldHint":true},
        "_meta":{"ui":{"visibility":if app_only {vec!["app"]}else{vec!["app","model"]}}}})
}
impl App {
    fn tools(&self) -> Vec<Value> {
        let empty = json!({"type":"object","properties":{},"additionalProperties":false});
        let mut open = tool("open_app", self.title, true, true, empty.clone());
        open["_meta"]["ui"]["resourceUri"] = json!(format!("ui://{}/app", self.id));
        let mut tools = vec![
            open,
            tool(
                "read",
                self.description,
                true,
                false,
                json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
            ),
            tool(
                "request",
                self.description,
                false,
                false,
                json!({"type":"object","properties":{"method":{"enum":["POST","PUT","PATCH","DELETE"]},"path":{"type":"string"},"body":{}},"required":["method","path"],"additionalProperties":false}),
            ),
        ];
        if self.has_indicator {
            tools.push(tool(
                "status",
                "Module status indicator",
                true,
                true,
                empty.clone(),
            ));
        }
        if self.public_router.is_some() {
            tools.push(tool("read_public","Public download (explicitly declared in manifest)",true,true,json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})));
        }
        for (name, _) in self.jobs {
            tools.push(tool(
                name,
                "Run the module's scheduled job",
                false,
                false, // The host scheduler invokes tools through the model audience.
                empty.clone(),
            ));
        }
        tools
    }
    async fn dispatch(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        Ok(match method {
            "initialize" => {
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{},"resources":{}},"serverInfo":{"name":self.id,"version":"1.0.0"},"instructions":self.description})
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools":self.tools()}),
            "resources/list" => {
                json!({"resources":[{"uri":format!("ui://{}/app",self.id),"name":self.title,"mimeType":"text/html;profile=mcp-app"}]})
            }
            "resources/read" => {
                ensure!(
                    params["uri"].as_str() == Some(format!("ui://{}/app", self.id).as_str()),
                    "unknown resource"
                );
                let html = tokio::fs::read_to_string("ui.html")
                    .await
                    .context("Build/install ui.html with the module binary")?;
                json!({"contents":[{"uri":params["uri"],"mimeType":"text/html;profile=mcp-app","text":html}]})
            }
            "tools/call" => {
                let name = params["name"].as_str().context("tool name required")?;
                let args = &params["arguments"];
                let result = match name {
                    "open_app" => Ok(json!({"module":self.id})),
                    "status" if self.has_indicator => self
                        .route(&self.router, Method::GET, "/status", Value::Null)
                        .await
                        .map(|v| v["body"].clone()),
                    "read_public" => {
                        self.route_encoded(
                            self.public_router.as_ref().context("no public resources")?,
                            Method::GET,
                            args["path"].as_str().context("path required")?,
                            Value::Null,
                            true,
                        )
                        .await
                    }
                    "read" => {
                        self.route(
                            &self.router,
                            Method::GET,
                            args["path"].as_str().context("path required")?,
                            Value::Null,
                        )
                        .await
                    }
                    "request" => {
                        let method = args["method"].as_str().context("method required")?;
                        ensure!(
                            ["POST", "PUT", "PATCH", "DELETE"].contains(&method),
                            "write method required"
                        );
                        self.route(
                            &self.router,
                            method.parse()?,
                            args["path"].as_str().context("path required")?,
                            args["body"].clone(),
                        )
                        .await
                    }
                    name => {
                        let (_, path) = self
                            .jobs
                            .iter()
                            .find(|(job, _)| *job == name)
                            .context("unknown tool")?;
                        self.route(&self.router, Method::POST, path, Value::Null)
                            .await
                    }
                };
                match result {
                    Ok(body) => {
                        let failed_job = self.jobs.iter().any(|(job, _)| *job == name)
                            && body["status"].as_u64().is_some_and(|status| !(200..300).contains(&status));
                        // Binary bodies belong in structuredContent only. Duplicating base64
                        // in text doubles large image/download responses beyond host limits.
                        let text = if body.get("base64").is_some() {
                            json!({"status":body["status"],"headers":body["headers"],"binary":"See structuredContent.base64"}).to_string()
                        } else {
                            body.to_string()
                        };
                        json!({"content":[{"type":"text","text":text}],"structuredContent":body,"isError":failed_job})
                    }
                    Err(err) => {
                        json!({"content":[{"type":"text","text":format!("{err:#}")}],"isError":true})
                    }
                }
            }
            _ => anyhow::bail!("unsupported method"),
        })
    }
    async fn route(
        &self,
        router: &Router,
        method: Method,
        path: &str,
        body: Value,
    ) -> anyhow::Result<Value> {
        self.route_encoded(router, method, path, body, false).await
    }
    async fn route_encoded(
        &self,
        router: &Router,
        method: Method,
        path: &str,
        body: Value,
        binary: bool,
    ) -> anyhow::Result<Value> {
        ensure!(
            path.starts_with('/') && !path.starts_with("//") && path.len() <= 4096,
            "module-relative path required"
        );
        let raw = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&body)?
        };
        ensure!(raw.len() <= MAX_BODY, "request too large");
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(raw))?;
        let response = router.clone().oneshot(request).await?;
        let status = response.status().as_u16();
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let headers = response
            .headers()
            .iter()
            .filter(|(k, _)| {
                matches!(
                    k.as_str(),
                    "content-type" | "content-disposition" | "etag" | "cache-control"
                )
            })
            .filter_map(|(k, v)| Some((k.to_string(), Value::String(v.to_str().ok()?.into()))))
            .collect::<serde_json::Map<_, _>>();
        let bytes = to_bytes(response.into_body(), MAX_BODY).await?;
        if !binary && mime.starts_with("application/json") {
            Ok(
                json!({"status":status,"headers":headers,"body":serde_json::from_slice::<Value>(&bytes)?}),
            )
        } else {
            Ok(json!({"status":status,"headers":headers,"base64":STANDARD.encode(bytes)}))
        }
    }
    pub async fn serve(self) -> anyhow::Result<()> {
        let mut input = BufReader::new(tokio::io::stdin());
        let mut output = tokio::io::stdout();
        loop {
            let mut line = Vec::new();
            let n = (&mut input)
                .take((MAX_BODY + 1) as u64)
                .read_until(b'\n', &mut line)
                .await?;
            if n == 0 {
                break;
            }
            ensure!(n <= MAX_BODY, "MCP frame too large");
            let msg: Value = serde_json::from_slice(&line)?;
            let Some(id) = msg.get("id") else { continue };
            let result = self
                .dispatch(
                    msg["method"].as_str().unwrap_or_default(),
                    msg["params"].clone(),
                )
                .await;
            let reply = match result {
                Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                Err(err) => {
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":format!("{err:#}")}})
                }
            };
            let mut bytes = serde_json::to_vec(&reply)?;
            bytes.push(b'\n');
            output.write_all(&bytes).await?;
            output.flush().await?;
        }
        Ok(())
    }
}
use tokio::io::AsyncReadExt;

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> App {
        App {
            id: "example",
            title: "Example",
            description: "test",
            router: Router::new().route(
                "/private",
                axum::routing::get(|| async { axum::Json(json!({"secret":"owned"})) })
                    .post(|| async { axum::Json(json!({"written":true})) }),
            ),
            public_router: Some(
                Router::new().route("/download", axum::routing::get(|| async { "public" }))
                    .route("/json", axum::routing::get(|| async { ([("content-type", "application/json")], "{ \"exact\": true }\n") })),
            ),
            jobs: &[],
            has_indicator: false,
        }
    }
    #[tokio::test]
    async fn public_tools_cannot_reach_private_routes_and_read_never_writes() {
        let app = app();
        let public = app
            .dispatch(
                "tools/call",
                json!({"name":"read_public","arguments":{"path":"/private"}}),
            )
            .await
            .unwrap();
        assert_eq!(public["structuredContent"]["status"], 404);
        let download = app.dispatch("tools/call", json!({"name":"read_public","arguments":{"path":"/json"}})).await.unwrap();
        assert_eq!(STANDARD.decode(download["structuredContent"]["base64"].as_str().unwrap()).unwrap(), b"{ \"exact\": true }\n");
        let read = app
            .dispatch(
                "tools/call",
                json!({"name":"read","arguments":{"path":"/private","method":"POST"}}),
            )
            .await
            .unwrap();
        assert_eq!(read["structuredContent"]["body"], json!({"secret":"owned"}));
        assert!(app
            .dispatch(
                "tools/call",
                json!({"name":"request","arguments":{"path":"/private","method":"GET"}})
            )
            .await
            .is_err());
        assert!(app
            .route(&app.router, Method::GET, "https://example.com", Value::Null)
            .await
            .is_err());
    }
    #[test]
    fn open_tool_is_app_only_and_public_download_is_read_only() {
        let tools = app().tools();
        assert_eq!(tools[0]["_meta"]["ui"]["visibility"], json!(["app"]));
        let public = tools.iter().find(|t| t["name"] == "read_public").unwrap();
        assert_eq!(public["annotations"]["readOnlyHint"], true);
        assert_eq!(tools[0]["_meta"]["ui"]["resourceUri"], "ui://example/app");
    }

    #[tokio::test]
    async fn scheduled_http_errors_are_tool_errors_and_successful_jobs_remain_successful() {
        let mut app = app();
        app.router = app.router.route("/failed", axum::routing::post(|| async {
            (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({"error":"cleanup refused"})))
        }));
        app.jobs = &[("run_failed", "/failed"), ("run_ok", "/private")];
        let failed = app.dispatch("tools/call", json!({"name":"run_failed","arguments":{}})).await.unwrap();
        assert_eq!(failed["isError"], true);
        assert_eq!(failed["structuredContent"]["status"], 400);
        let success = app.dispatch("tools/call", json!({"name":"run_ok","arguments":{}})).await.unwrap();
        assert_eq!(success["isError"], false);
        assert_eq!(success["structuredContent"]["body"]["written"], true);
        let ordinary = app.dispatch("tools/call", json!({"name":"request","arguments":{"method":"POST","path":"/failed"}})).await.unwrap();
        assert_eq!(ordinary["isError"], false);
        assert_eq!(ordinary["structuredContent"]["status"], 400);
    }
}
