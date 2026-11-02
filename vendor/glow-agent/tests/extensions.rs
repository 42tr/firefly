use async_trait::async_trait;
#[cfg(any(feature = "skills", feature = "mcp"))]
use glow_agent::ExtensionConfig;
use glow_agent::{
    Agent, Content, Message, Model, ModelError, ModelRequest, ModelResponse, Role, RunRequest,
    Runtime,
};
use serde_json::Value;
#[cfg(any(feature = "skills", feature = "mcp"))]
use serde_json::json;
use std::sync::{Arc, Mutex};

struct Script {
    calls: Vec<(String, Value)>,
    requests: Mutex<Vec<ModelRequest>>,
}
impl Script {
    fn new(calls: Vec<(String, Value)>) -> Arc<Self> {
        Arc::new(Self {
            calls,
            requests: Mutex::new(vec![]),
        })
    }
}
#[async_trait]
impl Model for Script {
    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let mut requests = self.requests.lock().unwrap();
        let turn = requests.len();
        requests.push(request);
        Ok(ModelResponse::new(
            if let Some((name, arguments)) = self.calls.get(turn) {
                Message::new(
                    Role::Assistant,
                    vec![Content::ToolCall {
                        id: format!("call-{turn}"),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    }],
                )
            } else {
                Message::assistant("done")
            },
        ))
    }
}
#[cfg(any(feature = "skills", feature = "mcp"))]
fn tool_result(output: &glow_agent::RunOutput, index: usize) -> (&Value, bool) {
    output
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|c| match c {
            Content::ToolResult {
                result, is_error, ..
            } => Some((result, *is_error)),
            _ => None,
        })
        .nth(index)
        .unwrap()
}

#[tokio::test]
async fn composed_history_can_be_resumed_without_duplicate_instructions() {
    let agent = Agent::new(Script::new(vec![])).instruction("one instruction");
    let out = Runtime::new()
        .run(&agent, RunRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap();
    assert_eq!(out.messages[0], Message::system("one instruction"));
    assert_eq!(out.conversation_messages[0], Message::user("hi"));
    let next = Runtime::new()
        .run(&agent, RunRequest::new(out.conversation_messages))
        .await
        .unwrap();
    assert_eq!(
        next.messages
            .iter()
            .filter(|m| m.role == Role::System)
            .count(),
        1
    );
}

#[cfg(feature = "skills")]
mod skills {
    use super::*;
    use glow_agent::{
        Policy, PolicyError, ToolContext, ToolSpec,
        skills::{SkillDefinition, SkillResource},
    };
    fn config(text: &str) -> Arc<ExtensionConfig> {
        let mut skill = SkillDefinition::new("analysis", "Analyze a task", text);
        skill.resources.insert(
            "references/state.md".into(),
            SkillResource::text("resource-v1"),
        );
        Arc::new(ExtensionConfig {
            skills: vec![skill],
            ..Default::default()
        })
    }
    #[tokio::test]
    async fn reads_explicit_body_and_resources_and_reports_activation() {
        let model = Script::new(vec![
            ("skills_read".into(), json!({"name":"analysis"})),
            (
                "skills_read".into(),
                json!({"name":"analysis","path":"references/state.md"}),
            ),
        ]);
        let out = Runtime::new()
            .run(
                &Agent::new(model.clone()),
                RunRequest::new(vec![Message::user("analyze")]).with_extensions(config("body-v1")),
            )
            .await
            .unwrap();
        assert_eq!(tool_result(&out, 0).0["content"], "body-v1");
        assert_eq!(tool_result(&out, 1).0["content"], "resource-v1");
        assert!(out.context_manifest.active_skills.contains_key("analysis"));
        let requests = model.requests.lock().unwrap();
        assert!(
            !requests[0]
                .messages
                .iter()
                .any(|m| m.text_content().contains("body-v1"))
        );
        assert!(requests[0].tools.iter().any(|t| t.name == "skills_read"));
    }
    #[tokio::test]
    async fn same_runtime_accepts_replacements_and_empty_config_without_inheritance() {
        let runtime = Runtime::new();
        for body in ["v1", "v2"] {
            let agent = Agent::new(Script::new(vec![(
                "skills_read".into(),
                json!({"name":"analysis"}),
            )]));
            let out = runtime
                .run(
                    &agent,
                    RunRequest::new(vec![]).with_extensions(config(body)),
                )
                .await
                .unwrap();
            assert_eq!(tool_result(&out, 0).0["content"], body);
        }
        let model = Script::new(vec![]);
        runtime
            .run(&Agent::new(model.clone()), RunRequest::new(vec![]))
            .await
            .unwrap();
        assert!(model.requests.lock().unwrap()[0].tools.is_empty());
    }
    #[tokio::test]
    async fn concurrent_runs_keep_separate_snapshots() {
        let runtime = Runtime::new();
        let a = Agent::new(Script::new(vec![(
            "skills_read".into(),
            json!({"name":"analysis"}),
        )]));
        let b = Agent::new(Script::new(vec![(
            "skills_read".into(),
            json!({"name":"analysis"}),
        )]));
        let (a, b) = tokio::join!(
            runtime.run(&a, RunRequest::new(vec![]).with_extensions(config("a"))),
            runtime.run(&b, RunRequest::new(vec![]).with_extensions(config("b")))
        );
        assert_eq!(tool_result(&a.unwrap(), 0).0["content"], "a");
        assert_eq!(tool_result(&b.unwrap(), 0).0["content"], "b");
    }

    struct ConcurrentModel(tokio::sync::Barrier);
    #[async_trait]
    impl Model for ConcurrentModel {
        async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
            if request.messages.iter().any(|m| m.role == Role::Tool) {
                return Ok(ModelResponse::new(Message::assistant("done")));
            }
            self.0.wait().await;
            Ok(ModelResponse::new(Message::new(
                Role::Assistant,
                vec![Content::ToolCall {
                    id: "read".into(),
                    name: "skills_read".into(),
                    arguments: json!({"name":"analysis"}),
                }],
            )))
        }
    }
    #[tokio::test]
    async fn same_agent_overlapping_runs_do_not_share_skill_activation_or_content() {
        let agent = Agent::new(Arc::new(ConcurrentModel(tokio::sync::Barrier::new(2))));
        let runtime = Runtime::new();
        let (a, b) = tokio::join!(
            runtime.run(
                &agent,
                RunRequest::new(vec![]).with_extensions(config("old"))
            ),
            runtime.run(
                &agent,
                RunRequest::new(vec![]).with_extensions(config("new"))
            )
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(tool_result(&a, 0).0["content"], "old");
        assert_eq!(tool_result(&b, 0).0["content"], "new");
        assert_ne!(
            a.context_manifest.active_skills,
            b.context_manifest.active_skills
        );
    }

    #[tokio::test]
    async fn reads_unicode_pages_and_rejects_stale_catalog_cursors() {
        let model = Script::new(vec![
            (
                "skills_read".into(),
                json!({"name":"analysis","offset":1,"limit":1}),
            ),
            ("skills_list".into(), json!({"cursor":"old:0"})),
        ]);
        let out = Runtime::new()
            .run(
                &Agent::new(model),
                RunRequest::new(vec![]).with_extensions(config("你好世界")),
            )
            .await
            .unwrap();
        assert_eq!(tool_result(&out, 0).0["content"], "好");
        assert_eq!(tool_result(&out, 0).0["next_offset"], 2);
        assert!(tool_result(&out, 1).1);
    }

    struct Deny;
    #[async_trait]
    impl Policy for Deny {
        async fn authorize(
            &self,
            _: &ToolContext,
            _: &ToolSpec,
            _: &Value,
        ) -> Result<(), PolicyError> {
            Err(PolicyError::new("no skills"))
        }
    }
    #[tokio::test]
    async fn explicit_and_model_activation_both_require_policy() {
        let agent = Agent::new(Script::new(vec![])).with_policy(Arc::new(Deny));
        assert!(matches!(
            Runtime::new()
                .run(
                    &agent,
                    RunRequest::new(vec![])
                        .with_extensions(config("secret"))
                        .with_selected_skills(["analysis"])
                )
                .await,
            Err(glow_agent::AgentError::Policy(_))
        ));
        let agent = Agent::new(Script::new(vec![(
            "skills_read".into(),
            json!({"name":"analysis"}),
        )]))
        .with_policy(Arc::new(Deny));
        assert!(matches!(
            Runtime::new()
                .run(
                    &agent,
                    RunRequest::new(vec![]).with_extensions(config("secret"))
                )
                .await,
            Err(glow_agent::AgentError::Policy(_))
        ));
    }
    #[tokio::test]
    async fn rejects_missing_resources_and_never_reads_files() {
        let agent = Agent::new(Script::new(vec![
            (
                "skills_read".into(),
                json!({"name":"analysis","path":"/etc/passwd"}),
            ),
            (
                "skills_read".into(),
                json!({"name":"analysis","path":"references/missing.md"}),
            ),
        ]));
        let out = Runtime::new()
            .run(
                &agent,
                RunRequest::new(vec![]).with_extensions(config("body")),
            )
            .await
            .unwrap();
        assert!(tool_result(&out, 0).1);
        assert_eq!(tool_result(&out, 1).0["error"], "resource_not_found");
    }
    #[tokio::test]
    async fn rejects_duplicate_or_invalid_config_before_model() {
        let model = Script::new(vec![]);
        let mut cfg = (*config("body")).clone();
        cfg.skills.push(cfg.skills[0].clone());
        assert!(
            Runtime::new()
                .run(
                    &Agent::new(model.clone()),
                    RunRequest::new(vec![]).with_extensions(Arc::new(cfg))
                )
                .await
                .is_err()
        );
        assert!(model.requests.lock().unwrap().is_empty());
    }
    #[test]
    fn parses_supplied_markdown_without_a_directory() {
        let skill = SkillDefinition::from_markdown(
            "---\nname: analysis\ndescription: Analyze\n---\nRead the task",
            Default::default(),
        )
        .unwrap();
        assert_eq!(skill.instructions, "Read the task");
    }
}

#[cfg(feature = "mcp")]
mod mcp {
    use super::*;
    use glow_agent::{
        InvocationScope,
        mcp::{CredentialProvider, McpCredentials, McpManager, McpServerConfig, McpTransport},
    };
    use std::{collections::BTreeMap, time::Duration};
    use tokio::io::{AsyncBufReadExt, BufReader};
    fn fixture() -> String {
        format!(
            "{}/tests/fixtures/mcp_server.py",
            env!("CARGO_MANIFEST_DIR")
        )
    }
    fn stdio(tag: &str) -> McpServerConfig {
        McpServerConfig {
            id: "fixture".into(),
            transport: McpTransport::Stdio {
                command: "/usr/bin/python3".into(),
                args: vec![fixture()],
                cwd: env!("CARGO_MANIFEST_DIR").into(),
                env: BTreeMap::from([("TAG".into(), tag.into())]),
            },
            allowed_tools: vec!["echo".into(), "fail".into()],
            ..Default::default()
        }
    }
    fn config(server: McpServerConfig) -> Arc<ExtensionConfig> {
        Arc::new(ExtensionConfig {
            mcp: vec![server],
            ..Default::default()
        })
    }
    async fn invoke(
        runtime: &Runtime,
        server: McpServerConfig,
        scope: InvocationScope,
        tool: &str,
        args: Value,
    ) -> glow_agent::RunOutput {
        let model = Script::new(vec![(format!("mcp__fixture__{tool}"), args)]);
        runtime
            .run(
                &Agent::new(model),
                RunRequest::new(vec![Message::user("call")])
                    .with_extensions(config(server))
                    .with_scope(scope),
            )
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn stdio_reuses_connections_updates_config_and_preserves_error_results() {
        let manager = Arc::new(McpManager::anonymous());
        let runtime = Runtime::new().with_mcp_manager(manager.clone());
        let first = invoke(
            &runtime,
            stdio("v1"),
            Default::default(),
            "echo",
            json!({"value":"one"}),
        )
        .await;
        let second = invoke(
            &runtime,
            stdio("v1"),
            Default::default(),
            "fail",
            json!({"value":"two"}),
        )
        .await;
        assert_eq!(tool_result(&first, 0).0["structured_content"]["calls"], 1);
        assert_eq!(tool_result(&second, 0).0["structured_content"]["calls"], 2);
        assert!(tool_result(&second, 0).1);
        let third = invoke(
            &runtime,
            stdio("v2"),
            Default::default(),
            "echo",
            json!({"value":"three"}),
        )
        .await;
        assert_eq!(tool_result(&third, 0).0["structured_content"]["tag"], "v2");
        assert_eq!(tool_result(&third, 0).0["structured_content"]["calls"], 1);
        manager.close().await.unwrap();
        manager.close().await.unwrap();
    }
    #[tokio::test]
    async fn input_schema_failure_does_not_execute_and_tool_filters_do_not_stick() {
        let manager = Arc::new(McpManager::anonymous());
        let runtime = Runtime::new().with_mcp_manager(manager.clone());
        let bad = invoke(
            &runtime,
            stdio("v1"),
            Default::default(),
            "echo",
            json!({"value":5}),
        )
        .await;
        assert_eq!(
            tool_result(&bad, 0).0["error"]["execution_state"],
            "not_submitted"
        );
        let good = invoke(
            &runtime,
            stdio("v1"),
            Default::default(),
            "echo",
            json!({"value":"ok"}),
        )
        .await;
        assert_eq!(tool_result(&good, 0).0["structured_content"]["calls"], 1);
        let mut filtered = stdio("v1");
        filtered.allowed_tools.clear();
        let model = Script::new(vec![]);
        runtime
            .run(
                &Agent::new(model.clone()),
                RunRequest::new(vec![]).with_extensions(config(filtered)),
            )
            .await
            .unwrap();
        assert!(model.requests.lock().unwrap()[0].tools.is_empty());
        manager.close().await.unwrap();
    }
    struct Credentials;
    #[async_trait]
    impl CredentialProvider for Credentials {
        async fn resolve(
            &self,
            scope: &InvocationScope,
            _: Option<&str>,
        ) -> Result<McpCredentials, glow_agent::AgentError> {
            Ok(McpCredentials {
                bearer_token: Some(scope.id.clone()),
                ..Default::default()
            })
        }
    }
    async fn http_fixture() -> (tokio::process::Child, String) {
        let mut child = tokio::process::Command::new("/usr/bin/python3")
            .args([fixture(), "http".into()])
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut port = String::new();
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut port))
            .await
            .unwrap()
            .unwrap();
        (child, format!("http://127.0.0.1:{}/mcp", port.trim()))
    }
    #[tokio::test]
    async fn http_concurrent_user_credentials_are_isolated() {
        let (mut fixture, url) = http_fixture().await;
        let manager = Arc::new(McpManager::new(Default::default(), Arc::new(Credentials)).unwrap());
        let runtime = Runtime::new().with_mcp_manager(manager.clone());
        let mut server = McpServerConfig::http("fixture", url);
        server.allowed_tools = vec!["echo".into()];
        let (a, b) = tokio::join!(
            invoke(
                &runtime,
                server.clone(),
                InvocationScope {
                    id: "alice".into(),
                    ..Default::default()
                },
                "echo",
                json!({"value":"a","delay":0.1})
            ),
            invoke(
                &runtime,
                server,
                InvocationScope {
                    id: "bob".into(),
                    ..Default::default()
                },
                "echo",
                json!({"value":"b","delay":0.1})
            )
        );
        assert_eq!(
            tool_result(&a, 0).0["structured_content"]["auth"],
            "Bearer alice"
        );
        assert_eq!(
            tool_result(&b, 0).0["structured_content"]["auth"],
            "Bearer bob"
        );
        manager.close().await.unwrap();
        fixture.kill().await.unwrap();
    }
    #[tokio::test]
    async fn warmup_and_required_optional_failures_do_not_call_model() {
        let manager = Arc::new(McpManager::anonymous());
        let runtime = Runtime::new().with_mcp_manager(manager.clone());
        let model = Script::new(vec![]);
        let agent = Agent::new(model.clone());
        let report = runtime
            .warmup(
                &agent,
                RunRequest::new(vec![]).with_extensions(config(stdio("warm"))),
            )
            .await
            .unwrap();
        assert_eq!(report.tools.len(), 2);
        assert!(model.requests.lock().unwrap().is_empty());
        let out = invoke(
            &runtime,
            stdio("warm"),
            Default::default(),
            "echo",
            json!({"value":"ok"}),
        )
        .await;
        assert_eq!(
            tool_result(&out, 0).0["structured_content"]["initializations"],
            1
        );
        let mut bad = stdio("bad");
        if let McpTransport::Stdio { command, .. } = &mut bad.transport {
            *command = "/missing-g-mcp-fixture".into();
        }
        assert!(
            runtime
                .run(
                    &agent,
                    RunRequest::new(vec![]).with_extensions(config(bad.clone()))
                )
                .await
                .is_err()
        );
        bad.required = false;
        let out = runtime
            .run(&agent, RunRequest::new(vec![]).with_extensions(config(bad)))
            .await
            .unwrap();
        assert_eq!(out.context_manifest.diagnostics.len(), 1);
        manager.close().await.unwrap();
    }
    #[cfg(feature = "skills")]
    #[tokio::test]
    async fn reads_skill_then_calls_mcp_in_the_same_run() {
        let manager = Arc::new(McpManager::anonymous());
        let runtime = Runtime::new().with_mcp_manager(manager.clone());
        let mut skill = glow_agent::skills::SkillDefinition::new(
            "analysis",
            "Analyze",
            "Call the fixture echo tool",
        );
        skill.required_tools.push(glow_agent::ToolOrigin::Mcp {
            server_id: "fixture".into(),
            tool_name: "echo".into(),
        });
        let config = Arc::new(ExtensionConfig {
            mcp: vec![stdio("joint")],
            skills: vec![skill],
        });
        let model = Script::new(vec![
            ("skills_read".into(), json!({"name":"analysis"})),
            ("mcp__fixture__echo".into(), json!({"value":"joint"})),
        ]);
        let out = runtime
            .run(
                &Agent::new(model),
                RunRequest::new(vec![]).with_extensions(config),
            )
            .await
            .unwrap();
        assert_eq!(
            tool_result(&out, 1).0["structured_content"]["value"],
            "joint"
        );
        assert!(out.context_manifest.active_skills.contains_key("analysis"));
        manager.close().await.unwrap();
    }
    #[tokio::test]
    async fn cancelling_first_initializer_does_not_cancel_other_waiters() {
        let manager = Arc::new(McpManager::anonymous());
        let runtime = Runtime::new().with_mcp_manager(manager.clone());
        let mut server = stdio("cold");
        if let McpTransport::Stdio { env, .. } = &mut server.transport {
            env.insert("INIT_DELAY".into(), "0.2".into());
        }
        let token = tokio_util::sync::CancellationToken::new();
        let agent = Agent::new(Script::new(vec![]));
        let first = runtime.warmup(
            &agent,
            RunRequest::new(vec![])
                .with_extensions(config(server.clone()))
                .with_cancellation_token(token.clone()),
        );
        let second = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token.cancel();
            runtime
                .warmup(
                    &agent,
                    RunRequest::new(vec![]).with_extensions(config(server.clone())),
                )
                .await
        };
        let (first, second) = tokio::join!(first, second);
        assert!(matches!(first, Err(glow_agent::AgentError::Cancelled)));
        assert_eq!(second.unwrap().tools.len(), 2);
        let out = invoke(
            &runtime,
            server,
            Default::default(),
            "echo",
            json!({"value":"ok"}),
        )
        .await;
        assert_eq!(
            tool_result(&out, 0).0["structured_content"]["initializations"],
            1
        );
        manager.close().await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_queued_call_is_not_submitted_or_replayed() {
        let (mut fixture, url) = http_fixture().await;
        let manager = Arc::new(McpManager::anonymous());
        let runtime = Runtime::new().with_mcp_manager(manager.clone());
        let mut server = McpServerConfig::http("fixture", url.clone());
        server.allowed_tools = vec!["echo".into()];
        server.max_in_flight = 1;
        let token = tokio_util::sync::CancellationToken::new();
        let first = invoke(
            &runtime,
            server.clone(),
            Default::default(),
            "echo",
            json!({"value":"slow","delay":0.4}),
        );
        let second = async {
            let stats_url = url.replace("/mcp", "/stats");
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let stats: Value = reqwest::get(&stats_url)
                        .await
                        .unwrap()
                        .json()
                        .await
                        .unwrap();
                    if stats["calls"] == 1 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let agent = Agent::new(Script::new(vec![(
                "mcp__fixture__echo".into(),
                json!({"value":"must-not-run"}),
            )]));
            let request = RunRequest::new(vec![])
                .with_extensions(config(server.clone()))
                .with_cancellation_token(token.clone());
            let cancel = async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                token.cancel();
            };
            let (result, _) = tokio::join!(runtime.run(&agent, request), cancel);
            assert!(matches!(result, Err(glow_agent::AgentError::Cancelled)));
        };
        let (out, _) = tokio::join!(first, second);
        assert_eq!(tool_result(&out, 0).0["structured_content"]["calls"], 1);
        let out = invoke(
            &runtime,
            server,
            Default::default(),
            "echo",
            json!({"value":"after"}),
        )
        .await;
        assert_eq!(tool_result(&out, 0).0["structured_content"]["calls"], 2);
        manager.close().await.unwrap();
        fixture.kill().await.unwrap();
    }

    #[tokio::test]
    async fn updated_http_url_does_not_reuse_the_old_server() {
        let (mut a, url_a) = http_fixture().await;
        let (mut b, url_b) = http_fixture().await;
        let manager = Arc::new(McpManager::anonymous());
        let runtime = Runtime::new().with_mcp_manager(manager.clone());
        for url in [url_a, url_b] {
            let mut server = McpServerConfig::http("fixture", url);
            server.allowed_tools = vec!["echo".into()];
            let out = invoke(
                &runtime,
                server,
                Default::default(),
                "echo",
                json!({"value":"ok"}),
            )
            .await;
            assert_eq!(tool_result(&out, 0).0["structured_content"]["calls"], 1);
        }
        manager.close().await.unwrap();
        a.kill().await.unwrap();
        b.kill().await.unwrap();
    }
    #[tokio::test]
    async fn expired_session_does_not_replay_submitted_tools() {
        let (mut fixture, url) = http_fixture().await;
        let manager = Arc::new(McpManager::anonymous());
        let runtime = Runtime::new().with_mcp_manager(manager.clone());
        let mut server = McpServerConfig::http("fixture", url.replace("/mcp", "/session"));
        server.allowed_tools = vec!["echo".into()];
        let out = invoke(
            &runtime,
            server,
            Default::default(),
            "echo",
            json!({"value":"expire"}),
        )
        .await;
        assert!(tool_result(&out, 0).1);
        assert_eq!(
            tool_result(&out, 0).0["error"]["execution_state"],
            "unknown"
        );
        let stats: Value = reqwest::get(url.replace("/mcp", "/stats"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(stats["calls"], 1);
        assert_eq!(stats["initializations"], 1);
        manager.close().await.unwrap();
        fixture.kill().await.unwrap();
    }
}
