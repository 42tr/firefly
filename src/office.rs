//! 文件下载响应辅助函数（Content-Type / Content-Disposition）。

use axum::http::HeaderValue;

/// 把字符串解析为 HTTP HeaderValue，失败时使用默认值。
pub fn header_value_or_default(raw: &str, default: &'static str) -> HeaderValue {
    HeaderValue::from_str(raw).unwrap_or_else(|_| HeaderValue::from_static(default))
}

/// 构建 Content-Disposition 头（inline / attachment）。
pub fn build_disposition_header(kind: &str, file_name: &str) -> HeaderValue {
    let safe = file_name.replace('"', "");
    let value =
        if safe.is_empty() { format!("{kind}; filename=\"file\"") } else { format!("{kind}; filename=\"{}\"", safe) };
    HeaderValue::from_str(&value).unwrap_or_else(|_| match kind {
        "attachment" => HeaderValue::from_static("attachment"),
        _ => HeaderValue::from_static("inline"),
    })
}
