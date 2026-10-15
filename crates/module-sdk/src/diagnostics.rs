//! Bounded, single-line diagnostics for external responses and CLI failures.
use std::sync::LazyLock;

const EXCERPT_BYTES: usize = 4096;
const RESPONSE_BYTES: usize = 16 * 1024;

pub fn excerpt(raw: &str, secrets: &[&str]) -> String {
    static CREDENTIALS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"\b(?:sk-|gh[pousr]_|github_pat_)[A-Za-z0-9_-]+\b").unwrap()
    });
    static BEARER: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?i)\bBearer\s+[^\s"',;}]+"#).unwrap());
    static NAMED_SECRET: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?i)((?:authorization|api[_-]?key|access[_-]?token|refresh[_-]?token|password|secret)["']?\s*[:=]\s*)(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s,;}]+)"#).unwrap()
    });
    static URL_CREDENTIALS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?i)(https?://)[^/\s:@]+:[^/\s@]+@").unwrap());
    let mut text = raw.to_owned();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        text = text.replace(secret, "[redacted]");
    }
    text = CREDENTIALS.replace_all(&text, "[redacted]").into_owned();
    text = BEARER.replace_all(&text, "Bearer [redacted]").into_owned();
    text = NAMED_SECRET
        .replace_all(&text, "${1}[redacted]")
        .into_owned();
    text = URL_CREDENTIALS
        .replace_all(&text, "${1}[redacted]@")
        .into_owned();
    text = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.len() > EXCERPT_BYTES {
        let mut end = EXCERPT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(" [truncated]");
    }
    if text.is_empty() {
        "<empty>".into()
    } else {
        text
    }
}

pub async fn response_body(mut response: reqwest::Response, secrets: &[&str]) -> String {
    let mut bytes = Vec::new();
    while bytes.len() < RESPONSE_BYTES {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = RESPONSE_BYTES - bytes.len();
                bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
            Ok(None) => break,
            Err(error) => {
                return excerpt(
                    &format!(
                        "{} [body read failed: {error}]",
                        String::from_utf8_lossy(&bytes)
                    ),
                    secrets,
                );
            }
        }
    }
    let mut result = excerpt(&String::from_utf8_lossy(&bytes), secrets);
    if bytes.len() == RESPONSE_BYTES && !result.ends_with("[truncated]") {
        result.push_str(" [truncated]");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_redact_credentials_before_truncating_and_remain_single_line() {
        let raw = "{\"api_key\":\"custom-key\",\"password\":\"hidden words\",\"message\":\"Bearer opaque-token sk-example ghp_example github_pat_example https://user:pass@proxy.example\"}\nTLS handshake failed\r\x1b";
        let result = excerpt(raw, &["custom-key"]);
        for secret in [
            "custom-key",
            "hidden",
            "words",
            "opaque-token",
            "sk-example",
            "ghp_example",
            "github_pat_example",
            "user:pass",
        ] {
            assert!(!result.contains(secret), "{result}");
        }
        assert!(result.contains("TLS handshake failed"));
        assert!(!result.chars().any(char::is_control));
        let result = excerpt(&"中".repeat(3000), &[]);
        assert!(result.ends_with("[truncated]"));
        assert!(result.len() <= EXCERPT_BYTES + " [truncated]".len());
        assert_eq!(excerpt(" \n\r", &[]), "<empty>");
    }
}
