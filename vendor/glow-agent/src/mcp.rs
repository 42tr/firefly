//! MCP clients configured per request. No global config, filesystem discovery or tool replay.
use crate::extensions::{fingerprint, invalid, unavailable};
use crate::{
    AgentError, InvocationScope, Tool, ToolBehavior, ToolContext, ToolError, ToolOrigin,
    ToolOutput, ToolSpec,
};
use async_trait::async_trait;
use rmcp::{
    Peer, RoleClient, ServiceExt,
    model::{CallToolRequestParams, ClientRequest, PaginatedRequestParams, ServerResult},
    service::PeerRequestOptions,
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        cwd: String,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    StreamableHttp {
        url: String,
    },
}
impl std::fmt::Debug for McpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stdio { .. } => "Stdio([redacted])",
            Self::StreamableHttp { .. } => "StreamableHttp([redacted])",
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpServerConfig {
    pub id: String,
    pub transport: McpTransport,
    pub credential_ref: Option<String>,
    pub required: bool,
    pub allowed_tools: Vec<String>,
    pub allow_all_tools: bool,
    pub max_in_flight: usize,
    pub connect_timeout_ms: u64,
    pub call_timeout_ms: u64,
}
impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            transport: McpTransport::StreamableHttp { url: String::new() },
            credential_ref: None,
            required: true,
            allowed_tools: vec![],
            allow_all_tools: false,
            max_in_flight: 4,
            connect_timeout_ms: 10000,
            call_timeout_ms: 30000,
        }
    }
}
impl McpServerConfig {
    pub fn http(id: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            transport: McpTransport::StreamableHttp { url: url.into() },
            ..Self::default()
        }
    }
    pub(crate) fn validate(&self) -> Result<(), AgentError> {
        if self.id.is_empty()
            || self.id.len() > 128
            || self.max_in_flight == 0
            || self.max_in_flight > 1024
            || self.connect_timeout_ms == 0
            || self.connect_timeout_ms > 300000
            || self.call_timeout_ms == 0
            || self.call_timeout_ms > 3600000
            || self.allowed_tools.len() > 1024
            || (self.allow_all_tools && !self.allowed_tools.is_empty())
        {
            return Err(invalid("invalid MCP server configuration"));
        }
        match &self.transport {
            McpTransport::Stdio { command, cwd, .. } => {
                if !std::path::Path::new(command).is_absolute()
                    || !std::path::Path::new(cwd).is_absolute()
                {
                    return Err(invalid("MCP command and cwd must be absolute"));
                }
            }
            McpTransport::StreamableHttp { url } => {
                let u = reqwest::Url::parse(url).map_err(|_| invalid("invalid MCP URL"))?;
                if !matches!(u.scheme(), "https" | "http")
                    || !u.username().is_empty()
                    || u.password().is_some()
                    || u.fragment().is_some()
                {
                    return Err(invalid("invalid MCP URL"));
                }
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| invalid("invalid MCP configuration"))?
            .len()
            > 65536
        {
            return Err(invalid("MCP configuration too large"));
        }
        Ok(())
    }
}

/// Credentials are intentionally not Debug or Serialize. Providers must not log secrets.
#[derive(Clone, Default)]
pub struct McpCredentials {
    pub generation: String,
    pub bearer_token: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
}
#[async_trait]
pub trait CredentialProvider: Send + Sync {
    async fn resolve(
        &self,
        scope: &InvocationScope,
        reference: Option<&str>,
    ) -> Result<McpCredentials, AgentError>;
}
pub struct AnonymousCredentials;
#[async_trait]
impl CredentialProvider for AnonymousCredentials {
    async fn resolve(
        &self,
        _: &InvocationScope,
        reference: Option<&str>,
    ) -> Result<McpCredentials, AgentError> {
        if reference.is_some() {
            return Err(invalid("credentials_required"));
        }
        Ok(McpCredentials::default())
    }
}
#[derive(Clone, Debug)]
pub struct McpPoolOptions {
    pub max_connections: usize,
    pub max_in_flight: usize,
    pub idle_ttl: Duration,
    pub shutdown_timeout: Duration,
    pub max_tools: usize,
    pub max_discovery_bytes: usize,
    pub max_result_bytes: usize,
}
impl Default for McpPoolOptions {
    fn default() -> Self {
        Self {
            max_connections: 64,
            max_in_flight: 64,
            idle_ttl: Duration::from_secs(60),
            shutdown_timeout: Duration::from_secs(10),
            max_tools: 1024,
            max_discovery_bytes: 2 * 1024 * 1024,
            max_result_bytes: 1024 * 1024,
        }
    }
}
struct Entry {
    result: watch::Receiver<ConnectResult>,
    stop: CancellationToken,
    last_used: Instant,
    task: Option<tokio::task::JoinHandle<()>>,
}
struct Connection {
    peer: Peer<RoleClient>,
    stop: CancellationToken,
    permits: Arc<Semaphore>,
}
impl Entry {
    fn idle(&self) -> bool {
        match self.result.borrow().as_ref() {
            Some(Ok(c)) => Arc::strong_count(c) == 1,
            Some(Err(_)) => true,
            None => false,
        }
    }
}
pub struct McpManager {
    options: McpPoolOptions,
    credentials: Arc<dyn CredentialProvider>,
    entries: Mutex<HashMap<String, Entry>>,
    retired: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    connections: Arc<Semaphore>,
    calls: Arc<Semaphore>,
    closed: AtomicBool,
    shutdown: CancellationToken,
    close_lock: Mutex<()>,
    /// Compiled schema validators keyed by schema fingerprint, reused across runs.
    validators: std::sync::Mutex<HashMap<String, Arc<jsonschema::Validator>>>,
}
/// Bound on cached validators; the cache is cleared when it fills up.
const MAX_CACHED_VALIDATORS: usize = 4096;
type ConnectResult = Option<Result<Arc<Connection>, String>>;
impl std::fmt::Debug for McpManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpManager")
            .field("closed", &self.closed.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}
impl McpManager {
    pub fn new(
        options: McpPoolOptions,
        credentials: Arc<dyn CredentialProvider>,
    ) -> Result<Self, AgentError> {
        if options.max_connections == 0
            || options.max_connections > 4096
            || options.max_in_flight == 0
            || options.max_in_flight > 65536
            || options.idle_ttl.is_zero()
            || options.shutdown_timeout.is_zero()
            || options.max_tools == 0
            || options.max_discovery_bytes == 0
            || options.max_result_bytes == 0
        {
            return Err(invalid("invalid MCP pool budget"));
        }
        Ok(Self {
            connections: Arc::new(Semaphore::new(options.max_connections)),
            calls: Arc::new(Semaphore::new(options.max_in_flight)),
            options,
            credentials,
            entries: Mutex::new(HashMap::new()),
            retired: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
            close_lock: Mutex::new(()),
            validators: std::sync::Mutex::new(HashMap::new()),
        })
    }
    pub fn anonymous() -> Self {
        Self::new(McpPoolOptions::default(), Arc::new(AnonymousCredentials))
            .expect("default pool options")
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    async fn acquire(
        &self,
        config: &McpServerConfig,
        scope: &InvocationScope,
    ) -> Result<Arc<Connection>, AgentError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(unavailable("manager_closed"));
        }
        // Errors from an application credential provider may contain secrets.
        let credentials = tokio::time::timeout(
            Duration::from_millis(config.connect_timeout_ms),
            self.credentials
                .resolve(scope, config.credential_ref.as_deref()),
        )
        .await
        .map_err(|_| unavailable("credential_timeout"))?
        .map_err(|_| unavailable("credentials_unavailable"))?;
        if (config.credential_ref.is_some()
            || credentials.bearer_token.is_some()
            || !credentials.headers.is_empty()
            || !credentials.env.is_empty())
            && scope.id.is_empty()
        {
            return Err(invalid("credential scope id is required"));
        }
        // Include actual credential contents as well as generation so an incorrectly reused
        // generation cannot return a connection bearing stale credentials. Never expose key.
        let key = fingerprint(&(
            &config.id,
            &config.transport,
            &config.credential_ref,
            config.max_in_flight,
            scope,
            &credentials.generation,
            &credentials.bearer_token,
            &credentials.headers,
            &credentials.env,
        ));
        let mut entries = self.entries.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(unavailable("manager_closed"));
        }
        let mut retired = self.retired.lock().await;
        retired.retain(|task| !task.is_finished());
        entries.retain(|_, e| {
            let failed = matches!(e.result.borrow().as_ref(), Some(Err(_)))
                && e.last_used.elapsed() > Duration::from_secs(1);
            let dead = matches!(e.result.borrow().as_ref(), Some(Ok(c)) if c.stop.is_cancelled());
            let evict =
                failed || dead || (e.idle() && e.last_used.elapsed() >= self.options.idle_ttl);
            if evict {
                e.stop.cancel();
                if let Some(task) = e.task.take() {
                    retired.push(task);
                }
            }
            !evict
        });
        drop(retired);
        if !entries.contains_key(&key) {
            if entries.len() >= self.options.max_connections {
                return Err(unavailable("MCP connection capacity exceeded"));
            }
            let permit = self
                .connections
                .clone()
                .try_acquire_owned()
                .map_err(|_| unavailable("MCP connection capacity exceeded"))?;
            let (tx, rx) = watch::channel(None);
            let stop = self.shutdown.child_token();
            let task = tokio::spawn(run_connection(
                config.clone(),
                credentials,
                permit,
                tx,
                stop.clone(),
                self.options.idle_ttl,
            ));
            entries.insert(
                key.clone(),
                Entry {
                    result: rx,
                    stop,
                    last_used: Instant::now(),
                    task: Some(task),
                },
            );
        }
        let entry = entries.get_mut(&key).unwrap();
        entry.last_used = Instant::now();
        let mut rx = entry.result.clone();
        drop(entries);
        loop {
            if let Some(result) = rx.borrow().clone() {
                return result.map_err(unavailable);
            }
            rx.changed()
                .await
                .map_err(|_| unavailable("MCP initialization stopped"))?;
        }
    }

    pub(crate) async fn tools(
        &self,
        config: &McpServerConfig,
        scope: &InvocationScope,
    ) -> Result<Vec<Arc<dyn Tool>>, AgentError> {
        let connection = self.acquire(config, scope).await?;
        let discovery = async {
            let mut cursor = None;
            let mut cursors = HashSet::new();
            let mut names = HashSet::new();
            let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
            let mut count = 0;
            let mut bytes = 0;
            loop {
                // Raw list request avoids SDK cache; each run sees current server tools.
                let req = rmcp::model::ListToolsRequest {
                    method: Default::default(),
                    params: Some(PaginatedRequestParams::default().with_cursor(cursor)),
                    extensions: Default::default(),
                };
                let response = connection
                    .peer
                    .send_request(ClientRequest::ListToolsRequest(req))
                    .await
                    .map_err(|_| unavailable("MCP discovery failed"))?;
                let ServerResult::ListToolsResult(page) = response else {
                    return Err(unavailable("unexpected MCP discovery result"));
                };
                bytes += serde_json::to_vec(&page)
                    .map_err(|_| unavailable("MCP discovery encoding failed"))?
                    .len();
                count += page.tools.len();
                if count > self.options.max_tools || bytes > self.options.max_discovery_bytes {
                    return Err(unavailable("MCP discovery budget exceeded"));
                }
                for remote in page.tools {
                    if !names.insert(remote.name.to_string()) {
                        return Err(unavailable("duplicate remote tool name"));
                    }
                    if !config.allow_all_tools
                        && !config
                            .allowed_tools
                            .iter()
                            .any(|n| n == remote.name.as_ref())
                    {
                        continue;
                    }
                    let input = Value::Object((*remote.input_schema).clone());
                    let validator = self.validator(&input, "unsupported MCP input schema")?;
                    let output_validator = remote
                        .output_schema
                        .as_ref()
                        .map(|schema| {
                            let schema = Value::Object((**schema).clone());
                            self.validator(&schema, "unsupported MCP output schema")
                        })
                        .transpose()?;
                    let tool = McpTool {
                        spec: ToolSpec {
                            name: tool_alias(&config.id, &remote.name),
                            description: remote
                                .description
                                .map(|s| s.to_string())
                                .unwrap_or_default(),
                            input_schema: input,
                            behavior: ToolBehavior::default(),
                        },
                        server_id: config.id.clone(),
                        remote_name: remote.name.to_string(),
                        connection: connection.clone(),
                        global: self.calls.clone(),
                        timeout: Duration::from_millis(config.call_timeout_ms),
                        max_result: self.options.max_result_bytes,
                        validator,
                        output_validator,
                    };
                    tools.push(Arc::new(tool));
                }
                cursor = page.next_cursor;
                let Some(next) = &cursor else {
                    break;
                };
                if !cursors.insert(next.clone()) || cursors.len() > self.options.max_tools {
                    return Err(unavailable("invalid MCP pagination"));
                }
            }
            Ok(tools)
        };
        tokio::time::timeout(Duration::from_millis(config.connect_timeout_ms), discovery)
            .await
            .map_err(|_| unavailable("MCP discovery timeout"))?
    }

    /// Compile `schema`, reusing a cached validator for an identical schema.
    fn validator(
        &self,
        schema: &Value,
        error: &'static str,
    ) -> Result<Arc<jsonschema::Validator>, AgentError> {
        let key = fingerprint(schema);
        if let Some(validator) = self.validators.lock().unwrap().get(&key) {
            return Ok(validator.clone());
        }
        reject_external_refs(schema)?;
        let validator =
            Arc::new(jsonschema::validator_for(schema).map_err(|_| unavailable(error))?);
        let mut validators = self.validators.lock().unwrap();
        if validators.len() >= MAX_CACHED_VALIDATORS {
            validators.clear();
        }
        validators.insert(key, validator.clone());
        Ok(validator)
    }

    pub async fn close(&self) -> Result<(), AgentError> {
        let _close = self.close_lock.lock().await;
        self.closed.store(true, Ordering::SeqCst);
        self.shutdown.cancel();
        let entries = std::mem::take(&mut *self.entries.lock().await);
        let mut tasks: Vec<_> = entries.into_values().filter_map(|e| e.task).collect();
        tasks.append(&mut *self.retired.lock().await);
        let result = tokio::time::timeout(self.options.shutdown_timeout, async {
            for task in &mut tasks {
                let _ = task.await;
            }
        })
        .await;
        if result.is_err() {
            for task in tasks {
                task.abort();
            }
            return Err(AgentError::Timeout);
        }
        Ok(())
    }
}
impl Drop for McpManager {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Connect, publish the result through `tx`, then keep the connection open until it is
/// stopped or has been idle for `idle_ttl`. Idle eviction also runs when no new requests
/// arrive.
async fn run_connection(
    config: McpServerConfig,
    credentials: McpCredentials,
    _permit: OwnedSemaphorePermit,
    tx: watch::Sender<ConnectResult>,
    stop: CancellationToken,
    idle_ttl: Duration,
) {
    let connect_timeout = Duration::from_millis(config.connect_timeout_ms);
    let result = tokio::select! {
        biased;
        _ = stop.cancelled() => Err("manager_closed".to_owned()),
        result = tokio::time::timeout(connect_timeout, connect(&config, credentials)) => {
            result.unwrap_or_else(|_| Err("MCP connect timeout".into()))
        }
    };
    let mut service = match result {
        Ok(service) => service,
        Err(error) => {
            let _ = tx.send(Some(Err(error)));
            return;
        }
    };
    let connection = Arc::new(Connection {
        peer: service.peer().clone(),
        stop: stop.clone(),
        permits: Arc::new(Semaphore::new(config.max_in_flight)),
    });
    let weak = Arc::downgrade(&connection);
    if tx.send(Some(Ok(connection))).is_err() {
        stop.cancel();
    }
    let mut idle_since = Instant::now();
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(idle_ttl.min(Duration::from_secs(1))) => {
                // Only the watch channel holds a strong reference when idle.
                if weak.strong_count() > 1 {
                    idle_since = Instant::now();
                }
                if weak.strong_count() == 0 || idle_since.elapsed() >= idle_ttl {
                    break;
                }
            }
        }
    }
    stop.cancel();
    let _ = service.close().await;
}

async fn connect(
    config: &McpServerConfig,
    credentials: McpCredentials,
) -> Result<rmcp::service::RunningService<RoleClient, ()>, String> {
    match &config.transport {
        McpTransport::Stdio {
            command,
            args,
            cwd,
            env,
        } => {
            let mut cmd = tokio::process::Command::new(command);
            cmd.args(args)
                .current_dir(cwd)
                .env_clear()
                .envs(env)
                .envs(credentials.env)
                .kill_on_drop(true);
            let transport = TokioChildProcess::builder(cmd)
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|_| "MCP process start failed")?
                .0;
            ().serve(transport)
                .await
                .map_err(|_| "MCP stdio initialization failed".into())
        }
        McpTransport::StreamableHttp { url } => {
            let mut headers = HashMap::new();
            for (name, value) in credentials.headers {
                let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| "invalid credential header")?;
                // Protocol/session routing headers must remain SDK owned.
                if matches!(
                    name.as_str(),
                    "host" | "content-length" | "content-type" | "accept"
                ) || name.as_str().starts_with("mcp-")
                {
                    return Err("reserved credential header".into());
                }
                headers.insert(
                    name,
                    reqwest::header::HeaderValue::from_str(&value)
                        .map_err(|_| "invalid credential header")?,
                );
            }
            let mut transport_config =
                StreamableHttpClientTransportConfig::with_uri(url.clone()).custom_headers(headers);
            // Never let transport recovery replay an ordinary tools/call POST.
            transport_config.reinit_on_expired_session = false;
            transport_config.max_concurrent_requests = config.max_in_flight;
            if let Some(token) = credentials.bearer_token {
                transport_config = transport_config.auth_header(token);
            }
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "MCP HTTP client failed")?;
            ().serve(StreamableHttpClientTransport::with_client(
                client,
                transport_config,
            ))
            .await
            .map_err(|_| "MCP HTTP initialization failed".into())
        }
    }
}
fn reject_external_refs(value: &Value) -> Result<(), AgentError> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(key.as_str(), "$ref" | "$dynamicRef")
                    && value.as_str().is_some_and(|r| !r.starts_with('#'))
                {
                    return Err(unavailable("external schema references are unsupported"));
                }
                reject_external_refs(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                reject_external_refs(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}
/// Stable model-facing alias. Authorization also receives the original server/tool pair.
pub fn tool_alias(server: &str, tool: &str) -> String {
    let full = format!("mcp__{server}__{tool}");
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
    format!("{prefix}_{}", &fingerprint(&(server, tool))[..16])
}
struct McpTool {
    spec: ToolSpec,
    server_id: String,
    remote_name: String,
    connection: Arc<Connection>,
    global: Arc<Semaphore>,
    timeout: Duration,
    max_result: usize,
    validator: Arc<jsonschema::Validator>,
    output_validator: Option<Arc<jsonschema::Validator>>,
}
#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }
    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Mcp {
            server_id: self.server_id.clone(),
            tool_name: self.remote_name.clone(),
        }
    }
    async fn call(&self, context: ToolContext, input: Value) -> Result<Value, ToolError> {
        Ok(self.call_output(context, input).await?.value)
    }
    async fn call_output(
        &self,
        context: ToolContext,
        input: Value,
    ) -> Result<ToolOutput, ToolError> {
        if !self.validator.is_valid(&input) {
            return Ok(failure("invalid_arguments", "not_submitted"));
        }
        let Value::Object(args) = input else {
            return Err(ToolError::new("arguments must be an object"));
        };
        let stop = context.cancellation_token.child_token();
        let _guard = stop.clone().drop_guard();
        let params = CallToolRequestParams::new(self.remote_name.clone()).with_arguments(args);
        // Worker retains permits while cancelling/draining even if Runtime drops this future.
        let task = tokio::spawn(execute_call(
            self.connection.clone(),
            self.global.clone(),
            stop,
            self.timeout,
            params,
        ));
        let result = match task
            .await
            .map_err(|_| ToolError::new("MCP worker stopped"))?
        {
            Ok(r) => r,
            Err(output) => return Ok(output),
        };
        let ServerResult::CallToolResult(result) = result else {
            return Ok(failure("unsupported_result", "unknown"));
        };
        if serde_json::to_vec(&result)?.len() > self.max_result {
            return Ok(failure("result_too_large", "completed"));
        }
        if !result.is_error.unwrap_or(false)
            && let Some(validator) = &self.output_validator
            && !result
                .structured_content
                .as_ref()
                .is_some_and(|v| validator.is_valid(v))
        {
            return Ok(failure("invalid_output_schema", "completed"));
        }
        let content = serde_json::to_value(&result.content)?;
        let mut unsupported = 0;
        let text: Vec<_> = content
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| {
                if v["type"] == "text" {
                    Some(v.clone())
                } else {
                    unsupported += 1;
                    None
                }
            })
            .collect();
        let is_error = result.is_error.unwrap_or(false)
            || (unsupported > 0 && text.is_empty() && result.structured_content.is_none());
        Ok(ToolOutput {
            value: json!({"server_id":self.server_id,"tool_name":self.remote_name,"content":text,"structured_content":result.structured_content,"unsupported_content":unsupported}),
            is_error,
        })
    }
}
/// Run one `tools/call` under the global and per-connection budgets. `Err` carries the
/// failure output to return to the model.
async fn execute_call(
    connection: Arc<Connection>,
    global: Arc<Semaphore>,
    stop: CancellationToken,
    duration: Duration,
    params: CallToolRequestParams,
) -> Result<ServerResult, ToolOutput> {
    let deadline = tokio::time::Instant::now() + duration;
    let acquire = async {
        let global = global.acquire_owned().await;
        let local = connection.permits.clone().acquire_owned().await;
        (global, local)
    };
    let _permits = tokio::select! {
        biased;
        _ = stop.cancelled() => return Err(failure("cancelled", "not_submitted")),
        _ = connection.stop.cancelled() => {
            return Err(failure("connection_closed", "not_submitted"));
        }
        permits = tokio::time::timeout_at(deadline, acquire) => {
            permits.map_err(|_| failure("queue_timeout", "not_submitted"))?
        }
    };
    if stop.is_cancelled() || connection.stop.is_cancelled() {
        return Err(failure("cancelled", "not_submitted"));
    }

    let request = ClientRequest::CallToolRequest(rmcp::model::CallToolRequest::new(params));
    let send = connection
        .peer
        .send_cancellable_request(request, PeerRequestOptions::no_options());
    let handle = tokio::select! {
        biased;
        _ = stop.cancelled() => {
            connection.stop.cancel();
            return Err(failure("cancelled", "unknown"));
        }
        _ = connection.stop.cancelled() => return Err(failure("connection_closed", "unknown")),
        _ = tokio::time::sleep_until(deadline) => {
            connection.stop.cancel();
            return Err(failure("timeout", "unknown"));
        }
        handle = send => handle.map_err(|_| {
            connection.stop.cancel();
            failure("transport_error", "unknown")
        })?,
    };

    let id = handle.id.clone();
    let mut response = Box::pin(handle.await_response());
    tokio::select! {
        biased;
        _ = stop.cancelled() => {}
        _ = connection.stop.cancelled() => return Err(failure("connection_closed", "unknown")),
        result = &mut response => {
            return result.map_err(|error| match error {
                rmcp::ServiceError::McpError(_) => failure("protocol_error", "unknown"),
                _ => {
                    connection.stop.cancel();
                    failure("transport_error", "unknown")
                }
            });
        }
        _ = tokio::time::sleep_until(deadline) => {}
    }

    // Cancelled or timed out after submission: tell the server, then give it a moment
    // to acknowledge before dropping the connection.
    let notice = rmcp::model::CancelledNotificationParam::new(
        Some(id),
        Some("run cancelled or timed out".into()),
    );
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        connection.peer.notify_cancelled(notice),
    )
    .await;
    if tokio::time::timeout(Duration::from_secs(1), &mut response)
        .await
        .is_err()
    {
        connection.stop.cancel();
    }
    Err(failure("cancelled_or_timed_out", "unknown"))
}

fn failure(code: &str, state: &str) -> ToolOutput {
    ToolOutput {
        value: json!({"error":{"code":code,"execution_state":state}}),
        is_error: true,
    }
}
