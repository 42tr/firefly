//! SSRF-resistant, bounded HTTP requests for model-facing tools.
//!
//! Redirects are disabled. When no host allowlist is supplied, DNS results and
//! literal addresses are restricted to public IP ranges. Allowlisted hosts are
//! trusted explicitly and may point at private networks.

use std::{collections::HashMap, sync::OnceLock, time::Duration};

use anyhow::anyhow;
use futures_util::StreamExt;
use reqwest::dns::Resolve;
use serde::Deserialize;
use serde_json::{json, Value};

/// Maximum response size returned to a model-facing caller.
pub const MAX_BODY_CHARS: usize = 16_000;
const MAX_BODY_BYTES: usize = MAX_BODY_CHARS * 4 + 4;

/// The wire format of a tool request. Hosts derive their own JSON Schema for
/// the model (schemars versions differ across hosts); this type only
/// describes what execute accepts.
#[derive(Debug, Deserialize)]
pub struct Request {
    /// HTTP method; defaults to GET.
    pub method: Option<String>,
    /// Full HTTP(S) URL.
    pub url: String,
    /// Optional request headers.
    pub headers: Option<HashMap<String, String>>,
    /// Optional request body.
    pub body: Option<String>,
    /// Request timeout, clamped to one through 120 seconds.
    pub timeout_secs: Option<u64>,
}

/// Execute one bounded request using an optional explicit host allowlist.
pub async fn execute(request: Request, allowed_hosts: &[String]) -> anyhow::Result<Value> {
    let method = request
        .method
        .as_deref()
        .unwrap_or("GET")
        .to_ascii_uppercase();
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| anyhow!("不支持的 HTTP 方法: {}", method))?;
    if matches!(method, reqwest::Method::TRACE | reqwest::Method::CONNECT) {
        return Err(anyhow!("不支持的 HTTP 方法: {}", method));
    }
    let (url, allowlisted) = validate_url(request.url.trim(), allowed_hosts)?;
    let timeout_secs = request.timeout_secs.unwrap_or(30).clamp(1, 120);

    let client = if allowlisted {
        http_client()
    } else {
        public_client()
    };
    let mut builder = client
        .request(method, url)
        .timeout(Duration::from_secs(timeout_secs));
    for (name, value) in request.headers.unwrap_or_default() {
        let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| anyhow!("非法请求头名称: {}", name))?;
        let header_value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| anyhow!("非法请求头值: {}", name))?;
        builder = builder.header(header_name, header_value);
    }
    if let Some(body) = request.body.filter(|body| !body.is_empty()) {
        builder = builder.body(body);
    }

    let response = builder
        .send()
        .await
        .map_err(|err| anyhow!("请求失败: {}", err))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let (body, truncated) = read_body_limited(response).await?;

    Ok(json!({
        "status": status.as_u16(),
        "content_type": content_type,
        "location": location,
        "truncated": truncated,
        "body": body,
    }))
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to create safe http client")
    })
}

fn public_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .no_proxy()
            .dns_resolver(std::sync::Arc::new(PublicOnlyResolver))
            .build()
            .expect("Failed to create public-only http client")
    })
}

struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|addr| is_public_ip(addr.ip()))
                .collect();
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
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
                || (segments[0] == 0x0064 && segments[1] == 0xff9b))
        }
    }
}

fn is_public_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && ip.octets()[2] == 0)
        || (a == 198 && (18..20).contains(&b))
        || a >= 240)
}

fn normalize_allowed_hosts(allowed_hosts: &[String]) -> Vec<String> {
    allowed_hosts
        .iter()
        .map(|host| host.trim().to_ascii_lowercase())
        .filter(|host| !host.is_empty())
        .collect()
}

fn validate_url(raw_url: &str, allowed_hosts: &[String]) -> anyhow::Result<(reqwest::Url, bool)> {
    let url = reqwest::Url::parse(raw_url).map_err(|err| anyhow!("URL 无法解析: {}", err))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(anyhow!("仅支持 http/https URL"));
    }
    let allowed_hosts = normalize_allowed_hosts(allowed_hosts);
    if !allowed_hosts.is_empty() {
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if !allowed_hosts
            .iter()
            .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
        {
            return Err(anyhow!("目标域名 {} 不在白名单内", host));
        }
        return Ok((url, true));
    }

    let literal = url
        .host_str()
        .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
        .and_then(|host| host.parse::<std::net::IpAddr>().ok());
    if literal.is_some_and(|ip| !is_public_ip(ip)) {
        return Err(anyhow!("未配置白名单时不允许访问内网或保留地址"));
    }
    Ok((url, false))
}

async fn read_body_limited(response: reqwest::Response) -> anyhow::Result<(String, bool)> {
    let mut bytes = Vec::with_capacity(MAX_BODY_BYTES);
    let mut truncated_by_bytes = false;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| anyhow!("读取响应体失败: {}", err))?;
        let remaining = MAX_BODY_BYTES.saturating_sub(bytes.len());
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            truncated_by_bytes = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&bytes);
    let truncated = truncated_by_bytes || text.chars().count() > MAX_BODY_CHARS;
    let body = text.chars().take(MAX_BODY_CHARS).collect();
    Ok((body, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn rejects_non_http_schemes() {
        assert!(validate_url("ftp://example.com/file", &[]).is_err());
        assert!(validate_url("file:///etc/passwd", &[]).is_err());
        assert!(validate_url("https://example.com/api", &[]).is_ok());
    }

    #[test]
    fn blocks_internal_addresses_without_allowlist() {
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
            assert!(validate_url(url, &[]).is_err(), "{url}");
        }
        assert!(validate_url("http://8.8.8.8/", &[]).is_ok());
        assert!(validate_url("http://127.0.0.1:3000/", &hosts(&["127.0.0.1"])).is_ok());
    }

    #[tokio::test]
    async fn resolver_rejects_hostnames_resolving_internally() {
        let name: reqwest::dns::Name = "localhost".parse().unwrap();
        assert!(PublicOnlyResolver.resolve(name).await.is_err());
    }

    #[test]
    fn enforces_host_allowlist() {
        let allowed = hosts(&["deploy.internal", "example.com"]);
        assert!(validate_url("https://deploy.internal/api", &allowed).is_ok());
        assert!(validate_url("https://a.example.com/api", &allowed).is_ok());
        assert!(validate_url("https://evil.com/api", &allowed).is_err());
        assert!(validate_url("https://notexample.com/api", &allowed).is_err());
        assert!(validate_url("https://EXAMPLE.COM/api", &hosts(&[" example.com "])).is_ok());
    }
}
