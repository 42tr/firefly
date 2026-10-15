use std::collections::HashMap;

use anyhow::anyhow;
use async_trait::async_trait;
use futures_util::StreamExt;
use g::{Tool, ToolBehavior, ToolError, ToolSpec};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::fs;
use uuid::Uuid;

use crate::{assistant_store, config, pdf, storage};

/// 生成 OpenAI 兼容的 tool parameters schema。
/// 部分 provider（如 DashScope）要求 object 类型必须带 properties 字段，
/// 且不接受 $schema 等 JSON Schema 元字段，这里统一清洗。
fn openai_tool_schema<T: JsonSchema>() -> Value {
    let mut schema = serde_json::to_value(schema_for!(T)).unwrap();
    if let Value::Object(map) = &mut schema {
        map.remove("$schema");
        map.remove("title");
        map.entry("properties".to_string()).or_insert_with(|| json!({}));
    }
    schema
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub title: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolAttachment {
    pub file_name: String,
    pub content_type: String,
    pub file_size: i64,
    pub storage_path: String,
}

#[derive(Clone, Debug)]
pub struct ToolResponse {
    pub value: Value,
    pub attachments: Vec<ToolAttachment>,
}

impl ToolResponse {
    pub fn new(value: Value) -> Self {
        Self { value, attachments: Vec::new() }
    }

    pub fn with_attachments(value: Value, attachments: Vec<ToolAttachment>) -> Self {
        Self { value, attachments }
    }
}

#[derive(Clone, Debug)]
pub struct ToolContext {
    pub user_id: String,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
struct MergePdfAttachmentsArgs {
    #[schemars(description = "需要合并的 PDF 附件 ID 列表，会按给定顺序拼接。")]
    attachment_ids: Vec<i64>,
    #[schemars(description = "可选，自定义生成文件名，自动补充 .pdf 后缀。")]
    filename: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
struct HttpRequestArgs {
    #[schemars(description = "HTTP 方法：GET、POST、PUT、PATCH、DELETE、HEAD，默认 GET")]
    method: Option<String>,
    #[schemars(description = "完整请求 URL，仅支持 http/https")]
    url: String,
    #[schemars(description = "可选，请求头键值对，如 {\"Authorization\": \"Bearer xxx\"}")]
    headers: Option<HashMap<String, String>>,
    #[schemars(description = "可选，请求体原文（如 JSON 字符串），仅 POST/PUT/PATCH 时有效")]
    body: Option<String>,
    #[schemars(description = "可选，超时秒数，默认 30，最大 120", range(min = 1, max = 120))]
    timeout_secs: Option<u64>,
}

fn wrap_tool_response(resp: ToolResponse) -> Value {
    let attachments = resp.attachments.iter().map(|a| serde_json::to_value(a).unwrap()).collect::<Vec<_>>();
    json!({ "response": resp.value, "attachments": attachments })
}

#[derive(Clone)]
struct McpAgentTool {
    definition: ToolDefinition,
    ctx: ToolContext,
}

#[async_trait]
impl Tool for McpAgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.definition.name.clone(),
            description: self.definition.description.clone(),
            input_schema: self.definition.input_schema.clone(),
            behavior: ToolBehavior::default(),
        }
    }

    async fn call(&self, _context: g::ToolContext, input: Value) -> Result<Value, ToolError> {
        let response = call_tool(&self.definition.name, input, Some(&self.ctx))
            .await
            .map_err(|err| ToolError::new(format!("{:#}", err)))?;
        Ok(wrap_tool_response(response))
    }
}

pub fn build_tool_set(ctx: ToolContext) -> Vec<std::sync::Arc<dyn Tool>> {
    list_tools()
        .into_iter()
        .map(|definition| {
            std::sync::Arc::new(McpAgentTool { definition, ctx: ctx.clone() }) as std::sync::Arc<dyn Tool>
        })
        .collect()
}

pub fn list_tools() -> Vec<ToolDefinition> {
    let mut tools = vec![
        ToolDefinition {
            name: "merge_pdf_attachments".to_string(),
            title: "合并 PDF 附件".to_string(),
            description: "将指定的 PDF 附件按照给定顺序合并成一个新的 PDF 文件。".to_string(),
            input_schema: openai_tool_schema::<MergePdfAttachmentsArgs>(),
        },
    ];
    if config::try_get().map(|cfg| cfg.assistant_http_tool_enabled).unwrap_or(false) {
        tools.push(ToolDefinition {
            name: "http_request".to_string(),
            title: "HTTP 请求".to_string(),
            description:
                "发起一次 HTTP 请求并返回状态码与响应体。按接口的 URL、方法、参数说明调用此工具执行。\
                3xx 重定向不会自动跟随，如需跳转请对 Location 再次调用。"
                    .to_string(),
            input_schema: openai_tool_schema::<HttpRequestArgs>(),
        });
    }
    tools
}

pub async fn call_tool(name: &str, arguments: Value, ctx: Option<&ToolContext>) -> anyhow::Result<ToolResponse> {
    match name {
        "merge_pdf_attachments" => {
            let ctx = ctx.ok_or_else(|| anyhow!("缺少用户上下文，无法执行该工具"))?;
            let attachment_ids = parse_attachment_ids(&arguments)?;
            if attachment_ids.len() < 2 {
                return Err(anyhow!("请至少提供 2 个 PDF 附件 ID"));
            }

            let mut handles = Vec::with_capacity(attachment_ids.len());
            for attachment_id in attachment_ids {
                let user_id = ctx.user_id.clone();
                handles.push(tokio::spawn(async move {
                    let attachment = assistant_store::get_attachment_for_user(attachment_id, &user_id)
                        .await?
                        .ok_or_else(|| anyhow!("附件 {} 不存在或无权访问", attachment_id))?;
                    if !is_pdf_content_type(&attachment.content_type) {
                        return Err(anyhow!("附件 {} 不是 PDF 文件", attachment.file_name));
                    }
                    let bytes = fs::read(&attachment.storage_path).await?;
                    let source = json!({
                        "id": attachment.id,
                        "file_name": attachment.file_name,
                        "file_size": attachment.file_size,
                    });
                    Ok::<_, anyhow::Error>((bytes, source))
                }));
            }

            let results = futures_util::future::join_all(handles).await;
            let mut pdf_buffers = Vec::with_capacity(results.len());
            let mut source_info = Vec::with_capacity(results.len());
            for result in results {
                let (bytes, source) = result.map_err(|e| anyhow!("读取 PDF 附件任务失败: {}", e))??;
                pdf_buffers.push(bytes);
                source_info.push(source);
            }

            let merged_bytes = pdf::merge_pdf_buffers(pdf_buffers).await?;
            let file_name = resolve_merge_filename(arguments.get("filename"));
            let storage_path = storage::save_tool_attachment(&merged_bytes).await?;
            let attachment = ToolAttachment {
                file_name: file_name.clone(),
                content_type: "application/pdf".to_string(),
                file_size: merged_bytes.len() as i64,
                storage_path,
            };

            let response = json!({
                "message": format!("已合并 {} 个 PDF 附件为 {}", source_info.len(), file_name),
                "source_attachments": source_info,
                "result_file_name": file_name,
                "result_file_size": attachment.file_size,
                "generated_attachments": [serde_json::to_value(&attachment)?],
            });
            Ok(ToolResponse::with_attachments(response, vec![attachment]))
        }
        "http_request" => {
            let response = execute_http_request(arguments).await?;
            Ok(ToolResponse::new(response))
        }
        _ => Err(anyhow!("Unknown tool: {}", name)),
    }
}

/// http_request 工具响应体的最大返回长度（超出部分截断）。
const HTTP_TOOL_MAX_BODY_CHARS: usize = 16_000;
/// 最多从远端响应读取的字节数。按 UTF-8 最多 4 字节/字符留出余量，
/// 防止为了截断响应而把完整的大响应读进内存。
const HTTP_TOOL_MAX_BODY_BYTES: usize = HTTP_TOOL_MAX_BODY_CHARS * 4 + 4;

/// 工具专用 client：不跟随 3xx 重定向，避免跳转目标绕过域名白名单校验。
fn http_tool_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to create http tool client")
    })
}

/// 未配置白名单时使用：连接前过滤掉内网/保留地址，防止模型被提示注入后访问
/// 本机服务、局域网或云厂商元数据接口（在连接时校验，也能挡住 DNS rebinding）。
/// 不走系统代理，否则解析的是代理地址而不是目标地址。
fn http_tool_public_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .no_proxy()
            .dns_resolver(std::sync::Arc::new(PublicOnlyResolver))
            .build()
            .expect("Failed to create http tool client")
    })
}

struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<std::net::SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.filter(|addr| is_public_ip(addr.ip())).collect();
            if addrs.is_empty() {
                return Err(format!("目标 {} 解析到内网或保留地址，已拒绝", host).into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => is_public_ipv4(ip),
        std::net::IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_ipv4(mapped);
            }
            let segments = ip.segments();
            !(ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (segments[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (segments[0] == 0x2001 && segments[1] == 0x0db8) // documentation
                || (segments[0] == 0x0064 && segments[1] == 0xff9b)) // NAT64 can reach IPv4 internals
        }
    }
}

fn is_public_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // includes 169.254.169.254 metadata
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // CGNAT, includes Aliyun metadata 100.100.100.200
        || (a == 192 && b == 0 && ip.octets()[2] == 0)
        || (a == 198 && (18..20).contains(&b))
        || a >= 240)
}

fn http_tool_allowed_hosts() -> Vec<String> {
    config::get()
        .assistant_http_tool_allowed_hosts
        .split(',')
        .map(|host| host.trim().to_ascii_lowercase())
        .filter(|host| !host.is_empty())
        .collect()
}

/// 返回校验后的 URL 以及它是否命中白名单（命中白名单的目标允许是内网地址）。
fn validate_http_tool_url(raw_url: &str) -> anyhow::Result<(reqwest::Url, bool)> {
    validate_http_tool_url_with_hosts(raw_url, &http_tool_allowed_hosts())
}

fn validate_http_tool_url_with_hosts(
    raw_url: &str, allowed_hosts: &[String],
) -> anyhow::Result<(reqwest::Url, bool)> {
    let url = reqwest::Url::parse(raw_url).map_err(|err| anyhow!("URL 无法解析: {}", err))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(anyhow!("仅支持 http/https URL"));
    }
    if !allowed_hosts.is_empty() {
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if !allowed_hosts.iter().any(|allowed| host == *allowed || host.ends_with(&format!(".{}", allowed))) {
            return Err(anyhow!("目标域名 {} 不在白名单内", host));
        }
        return Ok((url, true));
    }
    // IP literals skip DNS resolution, so the public-only resolver never sees them.
    let literal = url
        .host_str()
        .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
        .and_then(|host| host.parse::<std::net::IpAddr>().ok());
    if literal.is_some_and(|ip| !is_public_ip(ip)) {
        return Err(anyhow!("未配置白名单时不允许访问内网或保留地址"));
    }
    Ok((url, false))
}

/// 执行 http_request 工具：手动校验白名单、不跟随 3xx 重定向（避免跳转绕过白名单）。
async fn execute_http_request(arguments: Value) -> anyhow::Result<Value> {
    if !config::get().assistant_http_tool_enabled {
        return Err(anyhow!("http_request 工具未启用（ASSISTANT_HTTP_TOOL_ENABLED=false）"));
    }
    let args: HttpRequestArgs = serde_json::from_value(arguments).map_err(|err| anyhow!("参数格式错误: {}", err))?;
    let method = args.method.as_deref().unwrap_or("GET").to_ascii_uppercase();
    let method =
        reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| anyhow!("不支持的 HTTP 方法: {}", method))?;
    if matches!(method, reqwest::Method::TRACE | reqwest::Method::CONNECT) {
        return Err(anyhow!("不支持的 HTTP 方法: {}", method));
    }
    let (url, allowlisted) = validate_http_tool_url(args.url.trim())?;
    let timeout_secs = args.timeout_secs.unwrap_or(30).clamp(1, 120);

    let client = if allowlisted { http_tool_client() } else { http_tool_public_client() };
    let mut request = client.request(method.clone(), url).timeout(std::time::Duration::from_secs(timeout_secs));
    for (name, value) in args.headers.unwrap_or_default() {
        let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| anyhow!("非法请求头名称: {}", name))?;
        let header_value =
            reqwest::header::HeaderValue::from_str(&value).map_err(|_| anyhow!("非法请求头值: {}", name))?;
        request = request.header(header_name, header_value);
    }
    if let Some(body) = args.body.filter(|body| !body.is_empty()) {
        request = request.body(body);
    }

    let response = request.send().await.map_err(|err| anyhow!("请求失败: {}", err))?;
    let status = response.status();
    let content_type =
        response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let location =
        response.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).map(|s| s.to_string());
    let (body, truncated) = read_http_body_limited(response).await?;

    Ok(json!({
        "status": status.as_u16(),
        "content_type": content_type,
        "location": location,
        "truncated": truncated,
        "body": body,
    }))
}

/// 流式读取响应体，只保留有限字节，再按字符数截断返回给模型。
///
/// 响应可能没有 Content-Length，不能依赖 header 判断大小；读取到上限后
/// 立即停止消费流，避免恶意或异常上游响应造成无界内存增长。
async fn read_http_body_limited(response: reqwest::Response) -> anyhow::Result<(String, bool)> {
    let mut bytes = Vec::with_capacity(HTTP_TOOL_MAX_BODY_BYTES);
    let mut truncated_by_bytes = false;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| anyhow!("读取响应体失败: {}", err))?;
        let remaining = HTTP_TOOL_MAX_BODY_BYTES.saturating_sub(bytes.len());
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            truncated_by_bytes = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }

    let text = String::from_utf8_lossy(&bytes);
    let truncated = truncated_by_bytes || text.chars().count() > HTTP_TOOL_MAX_BODY_CHARS;
    let body = text.chars().take(HTTP_TOOL_MAX_BODY_CHARS).collect();
    Ok((body, truncated))
}

fn parse_attachment_ids(arguments: &Value) -> anyhow::Result<Vec<i64>> {
    let list = arguments
        .get("attachment_ids")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("attachment_ids 字段必须是整数数组"))?;
    let mut ids = Vec::with_capacity(list.len());
    for value in list {
        let id = value.as_i64().ok_or_else(|| anyhow!("attachment_ids 中存在非整数值"))?;
        ids.push(id);
    }
    Ok(ids)
}

fn is_pdf_content_type(content_type: &str) -> bool {
    let lowered = content_type.to_ascii_lowercase();
    lowered == "application/pdf" || lowered.ends_with("+pdf") || lowered.contains("pdf")
}

fn resolve_merge_filename(raw: Option<&Value>) -> String {
    let fallback = format!("merged-{}.pdf", Uuid::new_v4().simple());
    let Some(value) = raw else {
        return fallback;
    };
    let Some(text) = value.as_str() else {
        return fallback;
    };
    let cleaned_segment = text.split(['/', '\\']).next_back().unwrap_or(text).trim();
    if cleaned_segment.is_empty() {
        return fallback;
    }
    let mut sanitized: String = cleaned_segment.chars().filter(|c| !c.is_control()).collect();
    if !sanitized.to_ascii_lowercase().ends_with(".pdf") {
        sanitized.push_str(".pdf");
    }
    sanitized.chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_tool_set_matches_public_tool_definitions() {
        let ctx = ToolContext { user_id: "test-user".to_string() };
        let mut actual = build_tool_set(ctx).into_iter().map(|tool| tool.spec()).collect::<Vec<_>>();
        let mut expected = list_tools();
        actual.sort_by(|left, right| left.name.cmp(&right.name));
        expected.sort_by(|left, right| left.name.cmp(&right.name));

        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected.iter()) {
            assert_eq!(actual.name, expected.name);
            assert_eq!(actual.description, expected.description);
            assert_eq!(actual.input_schema, expected.input_schema);
        }
    }

    #[test]
    fn http_tool_url_rejects_non_http_schemes() {
        assert!(validate_http_tool_url_with_hosts("ftp://example.com/file", &[]).is_err());
        assert!(validate_http_tool_url_with_hosts("file:///etc/passwd", &[]).is_err());
        assert!(validate_http_tool_url_with_hosts("https://example.com/api", &[]).is_ok());
    }

    #[test]
    fn http_tool_blocks_internal_addresses_without_allowlist() {
        for url in [
            "http://127.0.0.1:3000/health",
            "http://0x7f.1/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.100.100.200/latest/meta-data/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[fd00::1]/",
        ] {
            assert!(validate_http_tool_url_with_hosts(url, &[]).is_err(), "{url}");
        }
        assert!(validate_http_tool_url_with_hosts("http://8.8.8.8/", &[]).is_ok());
        // Allowlisted hosts are trusted even when they are internal.
        let hosts = vec!["127.0.0.1".to_string()];
        assert!(validate_http_tool_url_with_hosts("http://127.0.0.1:3000/", &hosts).is_ok());
    }

    #[tokio::test]
    async fn http_tool_resolver_rejects_hostnames_resolving_internally() {
        use reqwest::dns::Resolve;
        let name: reqwest::dns::Name = "localhost".parse().unwrap();
        assert!(PublicOnlyResolver.resolve(name).await.is_err());
    }

    #[test]
    fn http_tool_url_enforces_host_allowlist() {
        let hosts = vec!["deploy.internal".to_string(), "example.com".to_string()];
        assert!(validate_http_tool_url_with_hosts("https://deploy.internal/api", &hosts).is_ok());
        // 子域名视为命中白名单
        assert!(validate_http_tool_url_with_hosts("https://a.example.com/api", &hosts).is_ok());
        assert!(validate_http_tool_url_with_hosts("https://evil.com/api", &hosts).is_err());
        // 相似后缀不算命中
        assert!(validate_http_tool_url_with_hosts("https://notexample.com/api", &hosts).is_err());
    }
}
