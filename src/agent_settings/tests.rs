use super::*;
use std::sync::Mutex;

fn fixture() -> Settings {
    let mut settings = Settings::defaults();
    settings.profiles[0].base_url = "http://127.0.0.1:1234/v1".into();
    settings.profiles[0].model = "fixture-model".into();
    settings.profiles[0].api_key = Some("private-fixture-key".into());
    settings.profiles[0].context_tokens = Some(32768);
    settings
}

fn child(id: &str) -> AgentDefinition {
    AgentDefinition {
        id: id.into(),
        name: "自定义助手".into(),
        description: "整理指定信息".into(),
        instruction: "CUSTOM_CHILD_INSTRUCTION".into(),
        model_profile: None,
        tools: vec!["fetch_activity_records".into()],
        enabled: true,
        timeout_secs: 60,
    }
}

#[test]
fn no_built_in_children_and_invalid_agent_definitions_are_rejected() {
    assert!(Settings::defaults().subagents.is_empty());
    let settings = fixture();
    settings.validate().unwrap();
    for invalid in ["", "Firefly", "with space", "firefly", "a/b"] {
        let mut invalid_settings = settings.clone();
        invalid_settings.subagents.push(child(invalid));
        assert!(invalid_settings.validate().is_err(), "accepted {invalid}");
    }
    let mut invalid = settings.clone();
    invalid.subagents = vec![child("helper"), child("helper")];
    assert!(invalid.validate().is_err());
    invalid = settings.clone();
    invalid.firefly.model_profile = Some("missing".into());
    assert!(invalid.validate().is_err());
    for tool in ["run_tools", "handoff_to_helper"] {
        invalid = settings.clone();
        invalid.firefly.tools.push(tool.into());
        assert!(invalid.validate().is_err());
    }
    invalid = settings.clone();
    invalid.profiles[0].base_url = "file:///etc/config".into();
    assert!(invalid.validate().is_err());
    invalid = settings;
    invalid.profiles[0].context_tokens = Some(100);
    assert!(invalid.validate().is_err());
}

#[tokio::test]
async fn saved_revisions_reload_without_restart_and_keys_never_leave_public_views() {
    let pool = crate::db::test_pool().await;
    let initial = Settings::load(&pool).await.unwrap();
    let mut next = fixture();
    next.revision = initial.revision;
    let saved = next.save(&pool).await.unwrap().unwrap();
    let public = saved.public();
    assert!(!public.to_string().contains("private-fixture-key"));
    assert!(public["profiles"][0]["api_key"].is_null());
    assert_eq!(public["profiles"][0]["key_configured"], true);
    let mut edit: Settings = serde_json::from_value(public).unwrap();
    edit.firefly.instruction = "UPDATED_MAIN_PROMPT".into();
    edit.subagents.push(child("helper"));
    let updated = edit.clone().save(&pool).await.unwrap().unwrap();
    assert!(
        edit.save(&pool).await.unwrap().is_none(),
        "stale overwrite accepted"
    );
    let reloaded = Settings::load(&pool).await.unwrap();
    assert_eq!(reloaded.revision, updated.revision);
    assert_eq!(reloaded.firefly.instruction, "UPDATED_MAIN_PROMPT");
    assert_eq!(reloaded.subagents[0].id, "helper");
    assert_eq!(
        reloaded.profiles[0].api_key.as_deref(),
        Some("private-fixture-key")
    );
    let mut clear = reloaded;
    clear.profiles[0].api_key = Some(String::new());
    let cleared = clear.save(&pool).await.unwrap().unwrap();
    assert_eq!(cleared.public()["profiles"][0]["key_configured"], false);
}

#[tokio::test]
async fn imported_profiles_cannot_send_saved_keys_to_another_service() {
    let pool = crate::db::test_pool().await;
    Settings::load(&pool).await.unwrap();
    let saved = fixture().save(&pool).await.unwrap().unwrap();
    let mut same_service: Settings = serde_json::from_value(saved.public()).unwrap();
    same_service.profiles[0].base_url = "http://127.0.0.1:1234/api/v1".into();
    let same_service = same_service.save(&pool).await.unwrap().unwrap();
    assert_eq!(
        same_service.profiles[0].api_key.as_deref(),
        Some("private-fixture-key")
    );
    let mut another_service: Settings = serde_json::from_value(same_service.public()).unwrap();
    another_service.profiles[0].base_url = "https://other-provider.example/v1".into();
    let another_service = another_service.save(&pool).await.unwrap().unwrap();
    assert_eq!(
        another_service.public()["profiles"][0]["key_configured"],
        false
    );
    let mut explicit = another_service;
    explicit.profiles[0].base_url = "https://new-provider.example/v1".into();
    explicit.profiles[0].api_key = Some("new-service-key".into());
    let explicit = explicit.save(&pool).await.unwrap().unwrap();
    assert_eq!(
        explicit.profiles[0].api_key.as_deref(),
        Some("new-service-key")
    );
}

struct Capture(Mutex<Vec<g::ModelRequest>>);
#[async_trait::async_trait]
impl Model for Capture {
    async fn generate(&self, request: g::ModelRequest) -> Result<g::ModelResponse, g::ModelError> {
        self.0.lock().unwrap().push(request);
        Ok(g::ModelResponse::new(g::Message::assistant("completed")))
    }
}

#[tokio::test]
async fn dynamic_children_receive_only_their_selected_tools_and_instruction() {
    let pool = crate::db::test_pool().await;
    let backend = Arc::new(assistant_tasks::Backend {
        manager: assistant_tasks::Manager::new(pool),
        user: "owner".into(),
        conversation: 0,
        job: uuid::Uuid::new_v4().to_string(),
        scope_id: None,
    });
    let tools = available_tools(mcp::ToolContext {
        user_id: "owner".into(),
    })
    .await;
    let mut settings = fixture();
    settings.subagents = vec![child("helper"), child("disabled"), child("missing-tool")];
    settings.subagents[1].enabled = false;
    settings.subagents[2].tools = vec!["mcp__not_installed__lookup".into()];
    let (children, errors) = settings.child_agents(&tools, backend.clone());
    assert_eq!(children.len(), 1);
    assert_eq!(errors.len(), 1);
    let model = Arc::new(Capture(Mutex::new(Vec::new())));
    let agent = build_agent(&settings.subagents[0], model.clone(), &tools, backend);
    agent.run("查看工作记录").await.unwrap();
    let requests = model.0.lock().unwrap();
    let names: Vec<_> = requests[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert!(names.contains(&"fetch_activity_records"));
    assert!(!names.contains(&"command"));
    assert!(!names.contains(&"todos"));
    assert!(requests[0]
        .messages
        .iter()
        .any(|message| message.text_content().contains("CUSTOM_CHILD_INSTRUCTION")));
    let parent = Agent::new(model.clone()).handoff(children);
    let names: Vec<_> = parent
        .tool_specs()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(names, ["handoff_to_helper"]);
    assert!(!routing_instruction(&[]).contains("handoff_to_programmer"));
    assert!(!routing_instruction(&[]).contains("handoff_to_chandao"));
}
