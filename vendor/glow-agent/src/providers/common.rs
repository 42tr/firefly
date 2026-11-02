//! HTTP plumbing shared by the OpenAI-compatible providers.
use std::{pin::Pin, time::Duration};

use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::ModelError;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum silence between two reads, so a stalled stream fails instead of consuming
/// the whole run timeout. Reasoning models can be quiet for a while before output.
const READ_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) type SseStream = Pin<Box<dyn Stream<Item = Result<String, ModelError>> + Send>>;

#[derive(Clone)]
pub(crate) struct OpenAIClient {
    client: Client,
    api_key: String,
    pub(crate) model: String,
    base_url: String,
}

impl std::fmt::Debug for OpenAIClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAIClient")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("api_key", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl OpenAIClient {
    pub(crate) fn new(api_key: String, model: String) -> Self {
        Self {
            client: default_http_client(),
            api_key,
            model,
            base_url: DEFAULT_BASE_URL.into(),
        }
    }

    pub(crate) fn from_env(default_model: &str) -> Result<Self, ModelError> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| ModelError::new("OPENAI_API_KEY is not set"))?;
        let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| default_model.into());
        let mut client = Self::new(api_key, model);
        if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
            client.set_base_url(base_url);
        }
        Ok(client)
    }

    pub(crate) fn set_base_url(&mut self, base_url: String) {
        self.base_url = base_url.trim_end_matches('/').to_owned();
    }

    pub(crate) fn set_http_client(&mut self, client: Client) {
        self.client = client;
    }

    #[cfg(feature = "limits")]
    pub(crate) fn http_client(&self) -> &Client {
        &self.client
    }

    #[cfg(feature = "limits")]
    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }

    #[cfg(feature = "limits")]
    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    /// POST `body` with a request-level total timeout and fail unless success.
    /// The timeout covers connection, headers and the whole body.
    pub(crate) async fn post_timed(
        &self,
        path: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<reqwest::Response, ModelError> {
        let response = self
            .client
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(&self.api_key)
            .timeout(timeout)
            .json(body)
            .send()
            .await
            .map_err(|error| ModelError::retryable(error.to_string()))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        tracing::warn!(%status, model = %self.model, path, "OpenAI API request failed");
        let body = response
            .text()
            .await
            .map_err(|error| ModelError::retryable(error.to_string()))?;
        Err(api_status_error(status, &body))
    }

    /// POST `body` and fail with an API error unless the status is a success.
    async fn post(&self, path: &str, body: &Value) -> Result<reqwest::Response, ModelError> {
        let response = self
            .client
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|error| ModelError::retryable(error.to_string()))?;
        let status = response.status();
        tracing::debug!(%status, model = %self.model, path, "received OpenAI API response");
        if status.is_success() {
            return Ok(response);
        }
        tracing::warn!(%status, model = %self.model, path, "OpenAI API request failed");
        let body = response
            .text()
            .await
            .map_err(|error| ModelError::retryable(error.to_string()))?;
        Err(api_status_error(status, &body))
    }

    pub(crate) async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, ModelError> {
        let body = self
            .post(path, body)
            .await?
            .text()
            .await
            .map_err(|error| ModelError::retryable(error.to_string()))?;
        serde_json::from_str(&body)
            .map_err(|error| ModelError::new(format!("invalid OpenAI response: {error}")))
    }

    /// POST `body` and yield the `data` field of each server-sent event.
    pub(crate) async fn post_sse(&self, path: &str, body: &Value) -> Result<SseStream, ModelError> {
        let response = self.post(path, body).await?;
        Ok(Box::pin(response.bytes_stream().eventsource().map(
            |event| {
                event
                    .map(|event| event.data)
                    .map_err(|error| ModelError::retryable(format!("OpenAI stream error: {error}")))
            },
        )))
    }
}

pub(crate) fn default_http_client() -> Client {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .expect("HTTP client configuration is valid")
}

/// Decode tool call arguments. Some servers send `""` for tools without parameters.
pub(crate) fn parse_tool_arguments(name: &str, raw: &str) -> Result<Value, ModelError> {
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(raw)
        .map_err(|error| ModelError::new(format!("invalid arguments for tool `{name}`: {error}")))
}

pub(crate) fn encode_tool_arguments(arguments: &Value) -> Result<String, ModelError> {
    serde_json::to_string(arguments)
        .map_err(|error| ModelError::new(format!("failed to serialize tool arguments: {error}")))
}

/// Serialize a tool result. Neither API has an error flag for tool output, so failed
/// results that don't already say so are wrapped as `{"error": ...}`.
pub(crate) fn encode_tool_result(result: &Value, is_error: bool) -> Result<String, ModelError> {
    let encoded = if is_error && result.get("error").is_none() {
        serde_json::to_string(&json!({ "error": result }))
    } else {
        serde_json::to_string(result)
    };
    encoded.map_err(|error| ModelError::new(format!("failed to serialize tool result: {error}")))
}

fn api_status_error(status: StatusCode, body: &str) -> ModelError {
    let message = serde_json::from_str::<ApiErrorEnvelope>(body)
        .ok()
        .map(|error| error.error.message)
        .unwrap_or_else(|| body.to_owned());
    let message = format!("OpenAI API returned {status}: {message}");
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        ModelError::retryable(message)
    } else {
        ModelError::new(message)
    }
}

#[derive(Debug, Deserialize)]
struct ApiErrorEnvelope {
    error: ApiError,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiError {
    pub(crate) message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tool_arguments_are_an_empty_object() {
        assert_eq!(parse_tool_arguments("ping", "").unwrap(), json!({}));
        assert_eq!(parse_tool_arguments("ping", "  ").unwrap(), json!({}));
        assert!(parse_tool_arguments("ping", "{").is_err());
    }

    #[test]
    fn error_results_are_marked_for_the_model() {
        let plain = encode_tool_result(&json!({"content": []}), true).unwrap();
        assert_eq!(plain, r#"{"error":{"content":[]}}"#);
        let already = encode_tool_result(&json!({"error": "boom"}), true).unwrap();
        assert_eq!(already, r#"{"error":"boom"}"#);
        let ok = encode_tool_result(&json!({"sum": 42}), false).unwrap();
        assert_eq!(ok, r#"{"sum":42}"#);
    }

    #[test]
    fn debug_output_redacts_the_api_key() {
        let client = OpenAIClient::new("sk-secret".into(), "m".into());
        assert!(!format!("{client:?}").contains("sk-secret"));
    }
}
