//! Isolate each UI-managed provider in its own app-server. Keys stay in the child
//! environment; native CLI configuration and other active providers stay independent.
use std::{collections::HashMap, process::Stdio, sync::{Arc, LazyLock, Mutex}, time::Duration};
use anyhow::Context;
use firefly_codex_transport::{app_server, CodexAppServer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{process::Command, sync::Mutex as AsyncMutex};
use crate::agent_settings::ModelProfile;

const PROVIDER: &str = "firefly_ui";
type Cached = HashMap<String, (String, Arc<CodexAppServer>)>;
static SERVERS: LazyLock<AsyncMutex<Cached>> = LazyLock::new(|| AsyncMutex::new(HashMap::new()));
static THREADS: LazyLock<Mutex<HashMap<String, Arc<CodexAppServer>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn command(profile: &ModelProfile) -> Command {
    let mut command = Command::new("codex");
    let value = |name: &str, value: Value| format!("{name}={}", toml::Value::try_from(value).expect("serializable CLI setting"));
    command.arg("app-server").args(["-c", "thread_unload_delay_secs=0"])
        .args(["-c", &value("model_provider", PROVIDER.into())])
        .args(["-c", &value("model", profile.model.clone().into())]);
    for (key, val) in [
        ("name", Value::from("Firefly model profile")),
        ("base_url", profile.base_url.clone().into()),
        ("wire_api", "responses".into()),
        ("requires_openai_auth", false.into()),
        ("supports_websockets", false.into()),
    ] { command.args(["-c", &value(&format!("model_providers.{PROVIDER}.{key}"), val)]); }
    if profile.api_key.as_deref().is_some_and(|key| !key.trim().is_empty()) {
        command.args(["-c", &value(&format!("model_providers.{PROVIDER}.env_key"), "FIREFLY_CODEX_PROFILE_KEY".into())]);
    }
    command.env("FIREFLY_CODEX_PROFILE_KEY", crate::llm::normalize_bearer_secret(profile.api_key.as_deref().unwrap_or_default()))
        .env("RUST_LOG", "warn").env("NO_COLOR", "1")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    command
}

pub async fn client(provider: Option<&str>) -> anyhow::Result<(Arc<CodexAppServer>, Option<String>)> {
    let Some(profile) = crate::cli_profiles::resolve(provider).await? else {
        return Ok((app_server().await?, provider.map(str::to_owned)));
    };
    let signature = format!("{:x}", Sha256::digest(serde_json::to_vec(&profile)?));
    let mut servers = SERVERS.lock().await;
    if let Some((_, server)) = servers.get(&profile.id).filter(|(sig, server)| sig == &signature && !server.is_closed()) {
        return Ok((server.clone(), Some(PROVIDER.into())));
    }
    let server = CodexAppServer::spawn_command(command(&profile), Duration::from_secs(60)).await
        .context("无法启动界面模型配置对应的 Codex 服务")?;
    servers.insert(profile.id, (signature, server.clone()));
    Ok((server, Some(PROVIDER.into())))
}

pub fn bind(thread: &str, client: Arc<CodexAppServer>) { THREADS.lock().unwrap().insert(thread.into(), client); }
pub async fn for_thread(thread: &str) -> anyhow::Result<Arc<CodexAppServer>> {
    let client = THREADS.lock().unwrap().get(thread).cloned();
    match client { Some(client) => Ok(client), None => app_server().await }
}
pub struct Binding(pub String);
impl Drop for Binding { fn drop(&mut self) { THREADS.lock().unwrap().remove(&self.0); } }

pub async fn refresh_profiles(profiles: &[ModelProfile]) {
    let signatures: HashMap<_, _> = profiles.iter().filter_map(|p| {
        serde_json::to_vec(p).ok().map(|bytes| (p.id.as_str(), format!("{:x}", Sha256::digest(bytes))))
    }).collect();
    // Active turns keep their own Arc through THREADS. Idle stale clients can
    // drop their credentials immediately when a profile is changed or removed.
    SERVERS.lock().await.retain(|id, (signature, _)| signatures.get(id.as_str()) == Some(signature));
}

pub async fn interrupt(thread: &str, turn: &str) -> anyhow::Result<()> {
    let client = THREADS.lock().unwrap().get(thread).cloned();
    if let Some(client) = client {
        client.request_with_timeout("turn/interrupt", serde_json::json!({"threadId":thread,"turnId":turn}), Duration::from_secs(15)).await?;
        Ok(())
    } else { firefly_codex_transport::interrupt_existing_turn(thread, turn).await }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires the installed Codex CLI; performs initialization only"]
    async fn native_managed_client_initializes_with_profile_configuration() {
        let p = ModelProfile { id:"native-smoke".into(), name:"Native smoke".into(),
            base_url:"http://127.0.0.1:9/v1".into(), claude_base_url:None,
            model:"fixture-model".into(), api_key:Some("fixture-key".into()),
            models:vec![], context_tokens:None, _key_configured:false };
        let server = CodexAppServer::spawn_command(command(&p), Duration::from_secs(20)).await.unwrap();
        let configuration = server.request("config/read", serde_json::json!({"includeLayers":false})).await.unwrap();
        assert_eq!(configuration["config"]["model_provider"], PROVIDER);
        assert_eq!(configuration["config"]["model"], "fixture-model");
        assert_eq!(configuration["config"]["model_providers"][PROVIDER]["base_url"], p.base_url);
    }
    #[test]
    fn key_is_only_in_the_child_environment_and_cli_uses_the_selected_endpoint() {
        let p = ModelProfile { id:"local".into(), name:"Local".into(), base_url:"http://127.0.0.1:1234/v1".into(),
            claude_base_url:None, model:"fixture".into(), api_key:Some("secret-fixture".into()), models:vec![], context_tokens:None, _key_configured:false };
        let command = command(&p); let command = command.as_std();
        let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(!args.join(" ").contains("secret-fixture"));
        assert!(args.iter().any(|v| v.contains("http://127.0.0.1:1234/v1")));
        assert!(args.iter().any(|v| v.contains("wire_api") && v.contains("responses")));
        assert_eq!(command.get_envs().find(|(key, _)| *key == "FIREFLY_CODEX_PROFILE_KEY").unwrap().1.unwrap(), "secret-fixture");
    }
}
