use super::*;
use std::sync::Mutex;

fn fixture() -> Settings {
    let mut settings = Settings::defaults();
    settings.profiles.push(ModelProfile {
        id: "default".into(), name: "测试模型".into(),
        base_url: "http://127.0.0.1:1234/v1".into(), claude_base_url: None, model: "fixture-model".into(),
        api_key: Some("private-fixture-key".into()), models: vec![], context_tokens: Some(32768), _key_configured: false,
    });
    settings.firefly.model_profile = Some("default".into());
    settings
}

fn child(id: &str) -> AgentDefinition {
    AgentDefinition {
        id: id.into(),
        name: "自定义助手".into(),
        description: "整理指定信息".into(),
        instruction: "CUSTOM_CHILD_INSTRUCTION".into(),
        model_profile: None,
        model: None,
        reasoning_level: None,
        tools: vec!["fetch_activity_records".into()],
        enabled: true,
        timeout_secs: 60,
    }
}

fn effort_settings() -> Settings {
    let mut settings = fixture();
    settings.profiles[0].models = vec![
        ProfileModel { id:"fixture-model".into(), reasoning_levels:Some(vec!["low".into(),"high".into(),"max".into()]),
            default_reasoning_level:Some("high".into()), ..Default::default() },
        ProfileModel { id:"other-model".into(), reasoning_levels:Some(vec!["low".into(),"high".into()]),
            default_reasoning_level:Some("low".into()), ..Default::default() },
    ];
    settings.subagents.push(child("helper"));
    settings
}

#[tokio::test]
async fn agent_effort_defaults_inherits_overrides_and_survives_reload() {
    let pool = crate::db::test_pool().await;
    let initial = Settings::load(&pool).await.unwrap();
    let mut settings = effort_settings(); settings.revision = initial.revision;
    assert_eq!(settings.reasoning_level(&settings.firefly).unwrap().as_deref(), Some("high"));
    settings.firefly.reasoning_level = Some("max".into());
    assert_eq!(settings.reasoning_level(&settings.subagents[0]).unwrap().as_deref(), Some("max"));
    settings.subagents[0].reasoning_level = Some("low".into());
    settings.save(&pool).await.unwrap().unwrap();
    let mut loaded = Settings::load(&pool).await.unwrap();
    assert_eq!(loaded.firefly.reasoning_level.as_deref(), Some("max"));
    assert_eq!(loaded.reasoning_level(&loaded.subagents[0]).unwrap().as_deref(), Some("low"));
    loaded.subagents[0].reasoning_level = None;
    loaded.subagents[0].model = Some("other-model".into());
    assert_eq!(loaded.reasoning_level(&loaded.subagents[0]).unwrap().as_deref(), Some("low"));
    loaded.subagents[0].model_profile = Some("separate".into());
    loaded.subagents[0].model = None;
    let mut profile = loaded.profiles[0].clone(); profile.id = "separate".into(); loaded.profiles.push(profile);
    assert_eq!(loaded.reasoning_level(&loaded.subagents[0]).unwrap().as_deref(), Some("high"));
    loaded.firefly.reasoning_level = Some("medium".into());
    assert_eq!(loaded.reasoning_level(&loaded.firefly).unwrap().as_deref(), Some("high"));
    loaded.firefly.reasoning_level = Some("high\ninvalid".into());
    assert!(loaded.validate().is_err());
    let mut legacy = serde_json::to_value(effort_settings()).unwrap();
    legacy["firefly"].as_object_mut().unwrap().remove("reasoning_level");
    let legacy: Settings = serde_json::from_value(legacy).unwrap();
    assert!(legacy.firefly.reasoning_level.is_none());
}

#[tokio::test]
async fn configured_effort_reaches_actual_responses_requests_including_streams_and_children() {
    use axum::{body::Body, extract::State, response::Response, routing::{get, post}, Json, Router};
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let app = Router::new()
        .route("/models/{id}", get(|| async { Json(json!({"context_window":32000})) }))
        .route("/responses", post(|State(captured): State<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>| async move {
            captured.lock().unwrap().push(body.clone());
            let response = json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}]});
            if body["stream"] == true {
                let event = json!({"type":"response.completed","response":response});
                Response::builder().header("content-type", "text/event-stream").body(Body::from(format!("data: {event}\n\n"))).unwrap()
            } else {
                Response::builder().header("content-type", "application/json").body(Body::from(response.to_string())).unwrap()
            }
        })).with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut settings = effort_settings();
    settings.profiles[0].base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let request = || g::ModelRequest::new(vec![g::Message::user("hello")], vec![]);
    settings.model(&settings.firefly).unwrap().generate(request()).await.unwrap();
    settings.firefly.reasoning_level = Some("max".into());
    struct Sink;
    #[async_trait::async_trait]
    impl g::ModelEventSink for Sink { async fn emit(&self, _: g::ModelEvent) {} }
    settings.model(&settings.firefly).unwrap().generate_stream(request(), &Sink).await.unwrap();
    settings.model(&settings.subagents[0]).unwrap().with_context_compaction().generate(request()).await.unwrap();
    settings.subagents[0].reasoning_level = Some("low".into());
    settings.model(&settings.subagents[0]).unwrap().generate(request()).await.unwrap();
    settings.subagents[0].reasoning_level = None;
    settings.subagents[0].model = Some("other-model".into());
    settings.model(&settings.subagents[0]).unwrap().generate(request()).await.unwrap();
    settings.profiles[0].models.clear();
    settings.model(&settings.firefly).unwrap().generate(request()).await.unwrap();
    let requests = captured.lock().unwrap();
    let efforts: Vec<_> = requests.iter().map(|r| r.pointer("/reasoning/effort").and_then(Value::as_str)).collect();
    assert_eq!(efforts, [Some("high"),Some("max"),Some("max"),Some("low"),Some("low"),None]);
    assert_eq!(requests[4]["model"], "other-model");
    assert!(requests[5].get("reasoning").is_none());
    assert!(requests.iter().all(|r| r.get("max_output_tokens").is_none()));
    server.abort();
}

#[tokio::test]
async fn configured_models_persist_provider_limits_in_the_application_pool() {
    use axum::{routing::get, Json, Router};
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let app = Router::new().route("/models/fixture-model", get(|| async {
        Json(json!({"id":"fixture-model","context_window":100000,"max_output_tokens":10000}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut settings = fixture();
    settings.profiles[0].base_url = base;
    settings.profiles[0].context_tokens = None;

    let model = settings.model(&settings.firefly).unwrap();
    assert_eq!(model.budget().resolve().await.unwrap(), 85500);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM model_limits_cache")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(count, 1);
    server.abort();
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

#[test]
fn initial_model_profile_ignores_openai_environment() {
    const CHILD: &str = "FIREFLY_TEST_MODEL_ENV_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let settings = Settings::defaults();
        assert!(settings.profiles.is_empty());
        assert!(settings.firefly.model_profile.is_none());
        settings.validate().unwrap();
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact").arg("agent_settings::tests::initial_model_profile_ignores_openai_environment")
        .env(CHILD, "1")
        .env("OPENAI_BASE_URL", "https://environment.invalid/v1")
        .env("OPENAI_MODEL", "environment-model")
        .env("OPENAI_API_KEY", "environment-key")
        .status().unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn empty_model_configuration_can_be_saved_and_reloaded() {
    let pool = crate::db::test_pool().await;
    let mut initial = Settings::load(&pool).await.unwrap();
    assert!(initial.profiles.is_empty());
    assert!(initial.firefly.model_profile.is_none());
    assert!(initial.model(&initial.firefly).is_err());
    initial.firefly.instruction = "saved without a model".into();
    initial.save(&pool).await.unwrap().unwrap();
    let loaded = Settings::load(&pool).await.unwrap();
    assert!(loaded.profiles.is_empty());
    assert!(loaded.firefly.model_profile.is_none());
    assert_eq!(loaded.firefly.instruction, "saved without a model");
}

#[test]
fn disabled_new_agent_can_be_saved_before_its_description_is_written() {
    let mut settings = fixture();
    let mut draft = child("new-agent");
    draft.description.clear();
    draft.enabled = false;
    settings.subagents.push(draft);
    assert!(settings.validate().is_ok());
    settings.subagents[0].enabled = true;
    assert!(settings.validate().is_err());
}

#[tokio::test]
async fn all_catalog_models_persist_with_independent_agent_selection_and_budgets() {
    let pool = crate::db::test_pool().await;
    let initial = Settings::load(&pool).await.unwrap();
    let mut settings = fixture(); settings.revision = initial.revision;
    settings.profiles[0].models = vec![
        ProfileModel { id:"fixture-model".into(), context_length:Some(65536), input_budget:Some(60000),
            reasoning_levels:Some(vec!["low".into(),"high".into(),"max".into()]), default_reasoning_level:Some("high".into()) },
        ProfileModel { id:"smaller-model".into(), context_length:Some(16384), input_budget:Some(15000), ..Default::default() },
    ];
    settings.firefly.model = Some("smaller-model".into());
    settings.subagents.push(child("helper"));
    settings.validate().unwrap();
    settings.save(&pool).await.unwrap().unwrap();
    let mut loaded = Settings::load(&pool).await.unwrap();
    assert_eq!(loaded.profiles[0].model_ids(), ["fixture-model", "smaller-model"]);
    assert_eq!(loaded.model_name(&loaded.firefly).unwrap(), "smaller-model");
    assert_eq!(loaded.model_name(&loaded.subagents[0]).unwrap(), "smaller-model");
    assert_eq!(loaded.profiles[0].input_budget("smaller-model"), Some(15000));
    assert_eq!(loaded.profiles[0].input_budget("fixture-model"), Some(32768));
    let profile = &loaded.profiles[0];
    assert_eq!(profile.reasoning_level("fixture-model", Some("max"), None).as_deref(), Some("max"));
    assert_eq!(profile.reasoning_level("fixture-model", Some("medium"), None).as_deref(), Some("high"));
    assert_eq!(profile.reasoning_level("fixture-model", None, None).as_deref(), Some("high"));
    assert_eq!(profile.reasoning_level("fixture-model", Some("max"), Some(&["low", "high"])).as_deref(), Some("high"));
    assert_eq!(profile.reasoning_level("smaller-model", Some("high"), None), None);
    loaded.subagents[0].model = Some("fixture-model".into());
    assert_eq!(loaded.model_name(&loaded.subagents[0]).unwrap(), "fixture-model");
    assert_eq!(loaded.model_name(&loaded.firefly).unwrap(), "smaller-model");
    let duplicate = loaded.profiles[0].models[0].clone();
    loaded.profiles[0].models.push(duplicate);
    assert!(loaded.validate().is_err());
}
