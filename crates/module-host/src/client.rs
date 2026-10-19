use super::{
    manifest::{self, Transport},
    Module,
};
use anyhow::{anyhow, ensure, Context};
use rmcp::{
    model::{ClientRequest, ServerResult},
    service::{PeerRequestOptions, RunningService},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
        TokioChildProcess,
    },
    RoleClient, ServiceExt,
};
use serde_json::{json, Value};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct Connection {
    pub(super) service: Option<RunningService<RoleClient, rmcp::model::ClientConfig>>,
    pub tools: Option<Vec<Value>>,
    pub error: Option<String>,
}

impl Module {
    async fn connect(
        &self,
    ) -> anyhow::Result<RunningService<RoleClient, rmcp::model::ClientConfig>> {
        let mut info = rmcp::model::ClientConfig::default();
        info.capabilities = serde_json::from_value(
            json!({"extensions":{"io.modelcontextprotocol/ui":{"mimeTypes":["text/html;profile=mcp-app"]}}}),
        )?;
        let result = match &self.manifest.mcp {
            Transport::Stdio {
                command,
                args,
                env,
                env_from,
            } => {
                let executable = if command.contains('/') {
                    self.root.join(command)
                } else {
                    command.into()
                };
                let mut process = tokio::process::Command::new(executable);
                process
                    .args(args)
                    .current_dir(&self.root)
                    .env_clear()
                    .envs(env)
                    .kill_on_drop(true);
                if let Some(path) = std::env::var_os("PATH") {
                    process.env("PATH", path);
                }
                for (name, source) in env_from {
                    process.env(
                        name,
                        std::env::var(source).map_err(|_| anyhow!("模块所需环境凭据未配置"))?,
                    );
                }
                process.env("FIREFLY_MODULE_TIMEZONE", self.timezone.name());
                process.env("FIREFLY_MODULE_DATA_DIR", &self.data_dir).env(
                    "FIREFLY_MODULE_CONFIG",
                    serde_json::to_string(&self.settings)?,
                );
                let log = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.data_dir.join("stderr.log"))?;
                let transport = TokioChildProcess::builder(process)
                    .stderr(log)
                    .spawn()
                    .map_err(|_| anyhow!("模块进程启动失败，请检查运行时和启动命令"))?
                    .0;
                info.serve(transport).await
            }
            Transport::StreamableHttp {
                url,
                bearer_token_env,
            } => {
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone());
                config.reinit_on_expired_session = false;
                if let Some(name) = bearer_token_env {
                    config = config.auth_header(
                        std::env::var(name).map_err(|_| anyhow!("模块认证环境变量未配置"))?,
                    );
                }
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()?;
                let transport = StreamableHttpClientTransport::with_client(client, config);
                info.serve(transport).await
            }
        };
        result.map_err(|_| anyhow!("MCP 初始化失败，请检查模块日志和服务地址"))
    }

    /// A single connection is shared by chat, UI and schedules. Requests are serialized
    /// per module; unknown writes are never retried or replayed after reconnecting.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<Value> {
        ensure!(
            self.enabled && !self.stop.is_cancelled(),
            "模块已停用或重载"
        );
        let mut state = tokio::select! {
            biased;
            _ = self.stop.cancelled() => return Err(anyhow!("模块已停用或重载")),
            _ = cancel.cancelled() => return Err(anyhow!("调用已取消，尚未提交")),
            state = self.connection.lock() => state,
        };
        if state.service.is_none() {
            let result = tokio::select! {
                _ = self.stop.cancelled() => Err(anyhow!("模块已停用")),
                _ = cancel.cancelled() => Err(anyhow!("调用已取消，尚未提交")),
                result = tokio::time::timeout(Duration::from_secs(10), self.connect()) => result.map_err(|_| anyhow!("MCP 连接超时")).and_then(|r| r),
            };
            match result {
                Ok(service) => {
                    state.service = Some(service);
                    state.error = None;
                }
                Err(err) => {
                    state.error = Some(err.to_string());
                    return Err(err);
                }
            }
        }
        let request: ClientRequest =
            serde_json::from_value(json!({"method":method,"params":params}))?;
        let peer = state.service.as_ref().unwrap().peer().clone();
        let operation = async {
            let handle = peer
                .send_cancellable_request(request, PeerRequestOptions::no_options())
                .await
                .map_err(|_| anyhow!("MCP 请求失败，执行结果未知；请检查实际状态"))?;
            handle
                .await_response()
                .await
                .map_err(|_| anyhow!("MCP 返回错误，执行结果未知；请查看模块日志"))
        };
        let result = tokio::select! {
            biased;
            _ = self.stop.cancelled() => Err(anyhow!("模块已停用，执行结果未知")),
            _ = cancel.cancelled() => Err(anyhow!("调用已取消，执行结果未知")),
            result = tokio::time::timeout(Duration::from_secs(self.manifest.call_timeout_secs), operation) => result
                .map_err(|_| anyhow!("模块调用超时，执行结果未知；请勿直接重试写入操作")).and_then(|r| r),
        };
        match result {
            Ok(result) => {
                let value = match result {
                    ServerResult::ListToolsResult(v) => serde_json::to_value(v)?,
                    ServerResult::CallToolResult(v) => serde_json::to_value(v)?,
                    ServerResult::ReadResourceResult(v) => serde_json::to_value(v)?,
                    _ => return Err(anyhow!("模块返回了不匹配的响应")),
                };
                ensure!(
                    serde_json::to_vec(&value)?.len() <= 16 * 1024 * 1024,
                    "模块结果超过 4 MiB"
                );
                state.error = None;
                Ok(value)
            }
            Err(error) => {
                state.error = Some(error.to_string());
                state.tools = None;
                if let Some(service) = state.service.take() {
                    let _ = tokio::time::timeout(Duration::from_secs(2), service.cancel()).await;
                }
                Err(error)
            }
        }
    }

    pub async fn tools(&self) -> anyhow::Result<Vec<Value>> {
        ensure!(
            self.enabled && !self.stop.is_cancelled(),
            "模块已停用或重载"
        );
        if let Some(tools) = self.connection.lock().await.tools.clone() {
            return Ok(tools);
        }
        let mut tools = Vec::new();
        let mut cursor = Value::Null;
        let mut cursors = HashSet::new();
        let mut names = HashSet::new();
        loop {
            let page = self
                .request(
                    "tools/list",
                    json!({"cursor":cursor}),
                    CancellationToken::new(),
                )
                .await?;
            for tool in page["tools"].as_array().context("MCP 工具列表格式无效")? {
                let name = tool["name"].as_str().context("工具缺少名称")?;
                ensure!(
                    !name.is_empty() && name.len() <= 128 && names.insert(name.to_owned()),
                    "工具名称无效或重复"
                );
                if self.manifest.allows(name) {
                    manifest::validate_schema(&tool["inputSchema"])?;
                    if let Some(schema) = tool.get("outputSchema") {
                        manifest::validate_schema(schema)?;
                    }
                    tools.push(tool.clone());
                }
            }
            ensure!(
                names.len() <= 256 && serde_json::to_vec(&tools)?.len() <= 1024 * 1024,
                "模块工具列表过大"
            );
            cursor = page["nextCursor"].clone();
            if cursor.is_null() {
                break;
            }
            ensure!(
                cursor.is_string() && cursors.insert(cursor.to_string()),
                "MCP 分页游标重复或无效"
            );
        }
        self.connection.lock().await.tools = Some(tools.clone());
        Ok(tools)
    }

    pub async fn call(
        &self,
        name: &str,
        args: Value,
        audience: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<Value> {
        ensure!(
            self.enabled && !self.stop.is_cancelled(),
            "模块已停用或重载"
        );
        let tool = self
            .tools()
            .await?
            .into_iter()
            .find(|t| t["name"] == name)
            .context("工具不存在或未授权")?;
        ensure!(visible(&tool, audience), "工具不允许从此入口调用");
        ensure!(
            args.is_object() && serde_json::to_vec(&args)?.len() <= 8 * 1024 * 1024,
            "工具参数无效或过大"
        );
        let validator = jsonschema::validator_for(&tool["inputSchema"])?;
        ensure!(validator.is_valid(&args), "工具参数不符合 Schema");
        let result = self
            .request("tools/call", json!({"name":name,"arguments":args}), cancel)
            .await?;
        if result["isError"] != true {
            if let Some(schema) = tool.get("outputSchema") {
                ensure!(
                    jsonschema::validator_for(schema)?.is_valid(&result["structuredContent"]),
                    "工具返回的数据不符合输出 Schema"
                );
            }
        }
        Ok(result)
    }

    pub async fn indicator(&self) -> anyhow::Result<Value> {
        let definition = self
            .manifest
            .indicator
            .as_ref()
            .context("模块未声明状态提示")?;
        let tools = self.tools().await?;
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == definition.tool)
            .context("状态工具不存在")?;
        ensure!(
            tool["annotations"]["readOnlyHint"] == true,
            "状态提示只允许调用声明为只读的工具"
        );
        let result = self
            .call(
                &definition.tool,
                definition.arguments.clone(),
                "app",
                CancellationToken::new(),
            )
            .await?;
        ensure!(result["isError"] != true, "模块状态暂时不可用");
        let value = &result["structuredContent"]["indicator"];
        ensure!(value.is_null() || value.is_object(), "状态提示格式无效");
        ensure!(
            serde_json::to_vec(value)?.len() <= 16 * 1024,
            "状态提示过大"
        );
        Ok(json!({"indicator":value}))
    }

    pub async fn page_resource(&self, page_id: &str) -> anyhow::Result<(String, String)> {
        let page = self
            .manifest
            .pages
            .iter()
            .find(|p| p.id == page_id)
            .context("模块页面不存在")?;
        let tools = self.tools().await?;
        let tool = tools
            .iter()
            .find(|t| t["name"] == page.open_tool)
            .context("页面入口工具不存在")?;
        ensure!(visible(tool, "app"), "页面入口工具不允许应用调用");
        let uri = tool
            .pointer("/_meta/ui/resourceUri")
            .and_then(Value::as_str)
            .context("工具没有 MCP Apps 页面资源")?;
        ensure!(
            uri.starts_with("ui://") && uri.len() <= 2048,
            "UI 资源 URI 无效"
        );
        let result = self
            .request(
                "resources/read",
                json!({"uri":uri}),
                CancellationToken::new(),
            )
            .await?;
        let resource = result["contents"]
            .as_array()
            .context("页面资源格式无效")?
            .iter()
            .find(|r| r["uri"] == uri)
            .context("页面资源缺失")?;
        ensure!(
            resource["mimeType"] == "text/html;profile=mcp-app",
            "模块页面必须是 text/html;profile=mcp-app"
        );
        let html = if let Some(text) = resource["text"].as_str() {
            text.to_string()
        } else {
            use base64::Engine;
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(resource["blob"].as_str().context("缺少 HTML 页面内容")?)?,
            )?
        };
        Ok((uri.into(), html))
    }

    pub async fn close(&self) {
        self.stop.cancel();
        let mut state = self.connection.lock().await;
        if let Some(service) = state.service.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), service.cancel()).await;
        }
    }
}

pub fn visible(tool: &Value, audience: &str) -> bool {
    tool.pointer("/_meta/ui/visibility")
        .and_then(Value::as_array)
        .is_none_or(|v| v.iter().any(|v| v == audience))
}

pub fn alias(id: &str, name: &str) -> String {
    use sha2::{Digest, Sha256};
    let full = format!("mcp__{id}__{name}");
    if full.len() <= 64
        && full
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        return full;
    }
    let prefix: String = full
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(47)
        .collect();
    format!(
        "{prefix}_{:.16}",
        format!("{:x}", Sha256::digest(full.as_bytes()))
    )
}

pub struct ModuleTool {
    pub module: Arc<Module>,
    pub definition: Value,
}
#[async_trait::async_trait]
impl g::Tool for ModuleTool {
    fn spec(&self) -> g::ToolSpec {
        let read_only = self.definition["annotations"]["readOnlyHint"] == true;
        g::ToolSpec {
            name: alias(
                &self.module.manifest.id,
                self.definition["name"].as_str().unwrap(),
            ),
            description: format!(
                "[{}] {}",
                self.module.manifest.name,
                self.definition["description"].as_str().unwrap_or_default()
            ),
            input_schema: self.definition["inputSchema"].clone(),
            behavior: g::ToolBehavior {
                read_only,
                idempotent: read_only || self.definition["annotations"]["idempotentHint"] == true,
                parallel_safe: read_only,
            },
        }
    }
    fn origin(&self) -> g::ToolOrigin {
        g::ToolOrigin::Mcp {
            server_id: self.module.manifest.id.clone(),
            tool_name: self.definition["name"].as_str().unwrap().into(),
        }
    }
    async fn call(&self, context: g::ToolContext, args: Value) -> Result<Value, g::ToolError> {
        let output = self.call_output(context, args).await?;
        if output.is_error {
            Err(g::ToolError::new(output.value.to_string()))
        } else {
            Ok(output.value)
        }
    }
    async fn call_output(
        &self,
        context: g::ToolContext,
        args: Value,
    ) -> Result<g::ToolOutput, g::ToolError> {
        let result = self
            .module
            .call(
                self.definition["name"].as_str().unwrap(),
                args,
                "model",
                context.cancellation_token,
            )
            .await
            .map_err(|e| g::ToolError::new(e.to_string()))?;
        let mut result = result;
        if let Some(value) = result.as_object_mut() {
            value.remove("_meta");
        }
        Ok(g::ToolOutput {
            is_error: result["isError"] == true,
            value: result,
        })
    }
}
