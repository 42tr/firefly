use super::*;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn config(base_url: &str) -> Config {
    serde_json::from_value(
        json!({"jwt_secret":"test", "auth_password":"test-password", "embedding_base_url":base_url}),
    )
    .unwrap()
}

fn memory(id: i64, title: &str, content: &str) -> MemoryRecord {
    MemoryRecord {
        id,
        user_id: "owner".into(),
        title: title.into(),
        content: content.into(),
        enabled: true,
        source: "manual".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

#[derive(Clone, Default)]
struct Calls {
    queries: Arc<AtomicUsize>,
    documents: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
}

async fn mock_server() -> (Config, Calls, tokio::task::JoinHandle<()>) {
    let calls = Calls::default();
    let app = Router::new().route("/v1/embeddings", post(|State(calls): State<Calls>, headers: HeaderMap,
        Json(body): Json<Value>| async move {
        calls.requests.fetch_add(1, Ordering::SeqCst);
        assert!(headers.get("authorization").is_none(), "must not inherit conversation credentials");
        assert_eq!(body["model"], "bge-m3");
        assert_eq!(body["encoding_format"], "float");
        let inputs = body["input"].as_array().unwrap();
        let data: Vec<_> = inputs.iter().enumerate().rev().map(|(index, text)| {
            let text = text.as_str().unwrap();
            if text == "部署怎么做" { calls.queries.fetch_add(1, Ordering::SeqCst); }
            if text.contains('\n') { calls.documents.fetch_add(1, Ordering::SeqCst); }
            let vector = if text.contains("部署") { vec![2.0, 0.0, 0.0] }
                else if text.contains("天气") { vec![0.0, 3.0, 0.0] }
                else { vec![0.0, 0.0, 4.0] };
            json!({"index":index,"embedding":vector})
        }).collect();
        Json(json!({"data":data,"model":"bge-m3"}))
    })).with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cfg = config(&format!("http://{}/v1", listener.local_addr().unwrap()));
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (cfg, calls, handle)
}

async fn semantic_mock_server() -> (Config, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/v1/embeddings",
        post(|Json(body): Json<Value>| async move {
            let data: Vec<_> = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(index, input)| {
                    let text = input.as_str().unwrap();
                    let vector = if text.contains("existing") || text.contains("duplicate") {
                        vec![1.0, 0.0]
                    } else if text.contains("similar") {
                        vec![0.8, 0.6]
                    } else {
                        vec![0.0, 1.0]
                    };
                    json!({"index": index, "embedding": vector})
                })
                .collect();
            Json(json!({"data": data}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cfg = config(&format!("http://{}/v1", listener.local_addr().unwrap()));
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (cfg, handle)
}

#[test]
fn validates_configuration_and_vectors() {
    let mut cfg = config("http://127.0.0.1:59801/v1/");
    validate_config(&cfg).unwrap();
    let a = EmbeddingService::from_config(&cfg).unwrap().unwrap();
    assert_eq!(a.endpoint.as_str(), "http://127.0.0.1:59801/v1/embeddings");
    cfg.embedding_model = "another-model".into();
    let b = EmbeddingService::from_config(&cfg).unwrap().unwrap();
    assert_ne!(a.space, b.space);
    cfg.embedding_base_url = "http://127.0.0.1:59802/v1".into();
    assert_ne!(
        b.space,
        EmbeddingService::from_config(&cfg).unwrap().unwrap().space
    );
    cfg.embedding_base_url = "http://user:secret@localhost/v1".into();
    assert!(validate_config(&cfg).is_err());
    cfg.embedding_base_url.clear();
    cfg.memory_similarity_threshold = f64::NAN;
    assert!(validate_config(&cfg).is_err());
    cfg.memory_similarity_threshold = 0.55;
    cfg.memory_top_k = 0;
    assert!(validate_config(&cfg).is_err());
    assert!(normalize(vec![]).is_err());
    assert!(normalize(vec![0.0, 0.0]).is_err());
    assert!(normalize(vec![f32::NAN]).is_err());
    assert!(normalize(vec![f32::INFINITY]).is_err());
    assert_eq!(normalize(vec![3.0, 4.0]).unwrap(), vec![0.6, 0.8]);
}

#[test]
fn semantic_nearest_uses_max_chunk_stable_ties_and_ignores_bad_vectors() {
    let memories = vec![
        (9, vec![vec![0.0, 1.0], vec![2.0, 0.0]]),
        (3, vec![vec![1.0, 0.0], vec![f32::NAN, 0.0]]),
    ];
    let nearest = nearest_enabled_memory(&[1.0, 0.0], &memories).unwrap();
    assert_eq!(
        nearest.memory_id, 3,
        "equal similarities use the smallest id"
    );
    assert_eq!(nearest.reason, SemanticMatchReason::Duplicate);
    assert!((nearest.similarity - 1.0).abs() < f64::EPSILON);
    assert!(nearest_enabled_memory(&[], &memories).is_none());
    assert!(nearest_enabled_memory(&[f32::NAN, 0.0], &memories).is_none());
    assert!(nearest_enabled_memory(&[1.0, 0.0], &[(1, vec![vec![1.0]])]).is_none());
    assert!(nearest_enabled_memory(&[1.0, 0.0], &[(1, vec![vec![f32::INFINITY, 0.0]])]).is_none());
}

#[test]
fn semantic_nearest_classifies_uncalibrated_thresholds() {
    let similar = nearest_enabled_memory(&[1.0, 0.0], &[(1, vec![vec![0.8, 0.6]])]).unwrap();
    assert_eq!(similar.reason, SemanticMatchReason::Similar);
    assert!((similar.similarity - 0.8).abs() < 1e-6);
    let below = nearest_enabled_memory(&[1.0, 0.0], &[(1, vec![vec![0.0, 1.0]])]).unwrap();
    assert_eq!(below.reason, SemanticMatchReason::BelowThreshold);
}

#[tokio::test]
async fn semantic_review_suppresses_duplicates_but_keeps_similar_candidates() {
    crate::db::set_test_pool(crate::db::test_pool().await);
    let (cfg, server) = semantic_mock_server().await;
    let existing = assistant_store::create_memory("owner", "基准", "existing", true)
        .await
        .unwrap();
    assistant_store::create_memory("owner", "未索引", "uncached", true)
        .await
        .unwrap();
    let service = EmbeddingService::from_config(&cfg).unwrap().unwrap();
    refresh_with(&service, std::slice::from_ref(&existing), None)
        .await
        .unwrap();
    let checked = assistant_store::list_memories("owner").await.unwrap();
    let evidence = "依据：「本轮事实」";
    let filtered = filter_review_changes(
        vec![
            MemoryReviewChange {
                id: None,
                title: "重复候选".into(),
                content: "duplicate".into(),
                evidence: evidence.into(),
            },
            MemoryReviewChange {
                id: None,
                title: "相似候选".into(),
                content: "similar".into(),
                evidence: evidence.into(),
            },
        ],
        &checked,
        "本轮事实",
        "已确认",
    );
    let mut filtered = filtered;
    apply_semantic_review(&cfg, "owner", &mut filtered, &checked)
        .await
        .unwrap();
    assert_eq!(filtered.changes.len(), 1);
    assert_eq!(filtered.changes[0].content, "similar");
    assert_eq!(filtered.rejected.semantic_duplicate, 1);
    assert_eq!(filtered.rejected.semantic_similar, 1);
    assert_eq!(filtered.rejected.skipped_no_vector, 1);
    assert_eq!(filtered.semantic_nearest.len(), 2);
    assert!(filtered
        .semantic_nearest
        .iter()
        .any(|(_, nearest)| nearest.memory_id == existing.id
            && nearest.reason == SemanticMatchReason::Duplicate));
    assert!(filtered
        .semantic_nearest
        .iter()
        .any(|(_, nearest)| nearest.memory_id == existing.id
            && nearest.reason == SemanticMatchReason::Similar));
    server.abort();
}

#[tokio::test]
async fn semantic_review_leaves_explicit_updates_and_skips_embedding_calls() {
    crate::db::set_test_pool(crate::db::test_pool().await);
    let (cfg, calls, server) = mock_server().await;
    let manual = assistant_store::create_memory("owner", "手动", "手动事实", true)
        .await
        .unwrap();
    let agent = assistant_store::create_reviewed_memory("owner", "自动", "部署旧事实", &[manual])
        .await
        .unwrap()
        .unwrap();
    let checked = assistant_store::list_memories("owner").await.unwrap();
    let mut filtered = filter_review_changes(
        vec![MemoryReviewChange {
            id: Some(agent.id),
            title: "自动修正".into(),
            content: "部署旧事实".into(),
            evidence: "依据：「本轮事实」".into(),
        }],
        &checked,
        "本轮事实",
        "已确认",
    );
    apply_semantic_review(&cfg, "owner", &mut filtered, &checked)
        .await
        .unwrap();
    assert_eq!(filtered.changes.len(), 1);
    assert_eq!(filtered.changes[0].id, Some(agent.id));
    assert_eq!(filtered.rejected.semantic_duplicate, 0);
    assert_eq!(calls.documents.load(Ordering::SeqCst), 0);
    assert_eq!(calls.queries.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn semantic_review_mixed_batch_keeps_updates_and_suppresses_only_new_candidates() {
    crate::db::set_test_pool(crate::db::test_pool().await);
    let (cfg, calls, server) = mock_server().await;
    let manual = assistant_store::create_memory("owner", "基准", "部署基准", true)
        .await
        .unwrap();
    let agent = assistant_store::create_reviewed_memory(
        "owner",
        "自动",
        "部署旧事实",
        std::slice::from_ref(&manual),
    )
    .await
    .unwrap()
    .unwrap();
    let service = EmbeddingService::from_config(&cfg).unwrap().unwrap();
    refresh_with(&service, std::slice::from_ref(&manual), None)
        .await
        .unwrap();
    let documents_before = calls.documents.load(Ordering::SeqCst);
    let checked = assistant_store::list_memories("owner").await.unwrap();
    let mut filtered = filter_review_changes(
        vec![
            MemoryReviewChange {
                id: Some(agent.id),
                title: "自动修正".into(),
                content: "部署更新事实".into(),
                evidence: "依据：「本轮事实」".into(),
            },
            MemoryReviewChange {
                id: None,
                title: "新增事实".into(),
                content: "部署新增事实".into(),
                evidence: "依据：「本轮事实」".into(),
            },
        ],
        &checked,
        "本轮事实",
        "已确认",
    );
    apply_semantic_review(&cfg, "owner", &mut filtered, &checked)
        .await
        .unwrap();
    assert_eq!(filtered.changes.len(), 1);
    assert_eq!(filtered.changes[0].id, Some(agent.id));
    assert_eq!(filtered.changes[0].content, "部署更新事实");
    assert_eq!(filtered.rejected.semantic_duplicate, 1);
    assert_eq!(filtered.semantic_nearest.len(), 1);
    assert_eq!(
        calls.documents.load(Ordering::SeqCst) - documents_before,
        1,
        "only the new candidate's one chunk is embedded"
    );
    let saved = assistant_store::apply_reviewed_memories_for_turn(
        "owner",
        &filtered.changes,
        &checked,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].id, agent.id);
    assert_eq!(saved[0].content, "部署更新事实");
    server.abort();
}

#[tokio::test]
async fn semantic_review_counts_embedding_failure_without_suppressing_new_memory() {
    crate::db::set_test_pool(crate::db::test_pool().await);
    let app = Router::new().route(
        "/v1/embeddings",
        post(|| async { StatusCode::SERVICE_UNAVAILABLE }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cfg = config(&format!("http://{}/v1", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let existing = assistant_store::create_memory("owner", "已有", "existing", true)
        .await
        .unwrap();
    let checked = vec![existing];
    let mut filtered = filter_review_changes(
        vec![MemoryReviewChange {
            id: None,
            title: "新候选".into(),
            content: "new fact".into(),
            evidence: "依据：「本轮事实」".into(),
        }],
        &checked,
        "本轮事实",
        "已确认",
    );
    apply_semantic_review(&cfg, "owner", &mut filtered, &checked)
        .await
        .unwrap();
    assert_eq!(filtered.changes.len(), 1);
    assert_eq!(filtered.rejected.semantic_unavailable, 1);
    assert_eq!(filtered.rejected.semantic_duplicate, 0);
    server.abort();
}

#[tokio::test]
async fn semantic_review_fails_open_when_cache_read_fails() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    assistant_store::create_memory("owner", "已有", "已有事实", true)
        .await
        .unwrap();
    let checked = assistant_store::list_memories("owner").await.unwrap();
    let mut filtered = filter_review_changes(
        vec![MemoryReviewChange {
            id: None,
            title: "缓存失败候选".into(),
            content: "缓存失败时仍保存".into(),
            evidence: "依据：「本轮事实」".into(),
        }],
        &checked,
        "本轮事实",
        "已确认",
    );
    pool.close().await;
    let (cfg, calls, server) = mock_server().await;
    apply_semantic_review(&cfg, "owner", &mut filtered, &checked)
        .await
        .unwrap();
    assert_eq!(filtered.changes.len(), 1);
    assert_eq!(filtered.changes[0].content, "缓存失败时仍保存");
    assert_eq!(filtered.rejected.semantic_unavailable, 1);
    assert!(filtered.semantic_nearest.is_empty());
    assert_eq!(calls.documents.load(Ordering::SeqCst), 0);
    assert_eq!(calls.requests.load(Ordering::SeqCst), 0, "a cache read failure must not call the embedding endpoint");
    server.abort();
}

#[tokio::test]
async fn semantic_review_fails_open_when_embedding_step_times_out() {
    crate::db::set_test_pool(crate::db::test_pool().await);
    let app = Router::new().route(
        "/v1/embeddings",
        post(|| async {
            tokio::time::sleep(Duration::from_secs(3)).await;
            Json(json!({"data": []}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = config(&format!("http://{}/v1", listener.local_addr().unwrap()));
    cfg.embedding_timeout_secs = 1;
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let existing = assistant_store::create_memory("owner", "已有", "已有事实", true)
        .await
        .unwrap();
    let checked = vec![existing];
    let mut filtered = filter_review_changes(
        vec![
            MemoryReviewChange {
                id: None,
                title: "超时候选一".into(),
                content: "超时后仍保存一".into(),
                evidence: "依据：「本轮事实」".into(),
            },
            MemoryReviewChange {
                id: None,
                title: "超时候选二".into(),
                content: "超时后仍保存二".into(),
                evidence: "依据：「本轮事实」".into(),
            },
            MemoryReviewChange {
                id: None,
                title: "超时候选三".into(),
                content: "超时后仍保存三".into(),
                evidence: "依据：「本轮事实」".into(),
            },
        ],
        &checked,
        "本轮事实",
        "已确认",
    );
    let started = std::time::Instant::now();
    apply_semantic_review(&cfg, "owner", &mut filtered, &checked)
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(filtered.changes.len(), 3);
    assert_eq!(
        filtered
            .changes
            .iter()
            .map(|change| change.content.as_str())
            .collect::<Vec<_>>(),
        ["超时后仍保存一", "超时后仍保存二", "超时后仍保存三"]
    );
    assert_eq!(filtered.rejected.semantic_unavailable, 3);
    assert!(filtered.semantic_nearest.is_empty());
    server.abort();
}

#[tokio::test]
async fn semantic_review_fails_open_when_cached_vector_dimension_mismatches() {
    crate::db::set_test_pool(crate::db::test_pool().await);
    let (cfg, _, server) = mock_server().await;
    let existing = assistant_store::create_memory("owner", "已有", "existing", true)
        .await
        .unwrap();
    let service = EmbeddingService::from_config(&cfg).unwrap().unwrap();
    assert!(
        assistant_store::save_memory_embedding(
            &existing,
            &service.space,
            &content_hash(&existing),
            2,
            r#"[{"text":"existing","vector":[1.0,0.0]}]"#,
        )
        .await
        .unwrap()
    );
    let checked = assistant_store::list_memories("owner").await.unwrap();
    let mut filtered = filter_review_changes(
        vec![MemoryReviewChange {
            id: None,
            title: "新候选".into(),
            content: "new fact".into(),
            evidence: "依据：「本轮事实」".into(),
        }],
        &checked,
        "本轮事实",
        "已确认",
    );
    apply_semantic_review(&cfg, "owner", &mut filtered, &checked)
        .await
        .unwrap();
    assert_eq!(filtered.changes.len(), 1);
    assert_eq!(filtered.rejected.semantic_duplicate, 0);
    assert_eq!(filtered.rejected.semantic_unavailable, 0);
    assert!(filtered.semantic_nearest.is_empty());
    let saved = assistant_store::apply_reviewed_memories_for_turn(
        "owner",
        &filtered.changes,
        &checked,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].content, "new fact");
    server.abort();
}

#[test]
fn parses_structured_memory_review_and_tolerates_code_fences() {
    let review = parse_memory_review(
        "```json\n{\"changes\":[{\"id\":null,\"title\":\"偏好\",\"content\":\"喜欢深色主题\",\"evidence\":\"用户说明\"}]}\n```",
    )
    .unwrap();
    assert_eq!(review.changes.len(), 1);
    assert_eq!(review.changes[0].title, "偏好");
    assert!(parse_memory_review("没有 JSON").is_err());
}

#[test]
fn memory_comparison_key_folds_rewrites_but_keeps_distinct_dates() {
    assert_eq!(
        assistant_store::memory_comparison_key("  喜欢，  深色模式。 "),
        assistant_store::memory_comparison_key("喜欢,深色模式.")
    );
    assert_ne!(
        assistant_store::memory_comparison_key("部署状态（截至 10-24）"),
        assistant_store::memory_comparison_key("部署状态 (截至 10-25)")
    );
    assert_eq!(
        assistant_store::memory_comparison_key("部署状态（截至 10/24）"),
        assistant_store::memory_comparison_key("部署状态 (截至 10／24)")
    );
}

#[test]
fn evidence_requires_a_verbatim_quoted_fragment_from_this_turn() {
    assert!(evidence_is_valid(
        "依据：「我喜欢，深色模式」",
        "我喜欢深色模式",
        "已确认"
    ));
    assert!(evidence_is_valid(
        "助手回复：“部署  已完成。”",
        "请部署",
        "部署已完成。"
    ));
    assert!(!evidence_is_valid(
        "本轮提到喜欢深色模式",
        "我喜欢深色模式",
        "已确认"
    ));
    assert!(!evidence_is_valid(
        "依据：「用户喜欢浅色模式」",
        "我喜欢深色模式",
        "已确认"
    ));
}

#[test]
fn review_filter_counts_rejections_and_caps_valid_changes() {
    let mut agent = memory(2, "自动", "旧事实");
    agent.source = "agent".into();
    let mut disabled = memory(3, "停用", "停用事实");
    disabled.source = "agent".into();
    disabled.enabled = false;
    let checked = vec![memory(1, "手动", "喜欢，深色模式"), agent, disabled];
    let evidence = "依据：「我喜欢深色模式」";
    let mut changes = vec![
        MemoryReviewChange {
            id: None,
            title: "重复".into(),
            content: "喜欢，深色模式".into(),
            evidence: evidence.into(),
        },
        MemoryReviewChange {
            id: None,
            title: "改写重复".into(),
            content: "喜欢,  深色模式".into(),
            evidence: evidence.into(),
        },
        MemoryReviewChange {
            id: Some(404),
            title: "不存在".into(),
            content: "事实".into(),
            evidence: evidence.into(),
        },
        MemoryReviewChange {
            id: Some(1),
            title: "覆盖手动".into(),
            content: "事实".into(),
            evidence: evidence.into(),
        },
        MemoryReviewChange {
            id: Some(3),
            title: "启用".into(),
            content: "事实".into(),
            evidence: evidence.into(),
        },
        MemoryReviewChange {
            id: None,
            title: "无证据".into(),
            content: "无证据事实".into(),
            evidence: "没有引文".into(),
        },
    ];
    for index in 0..9 {
        changes.push(MemoryReviewChange {
            id: None,
            title: format!("事实 {index}"),
            content: format!("新事实 {index}"),
            evidence: evidence.into(),
        });
    }
    let filtered = filter_review_changes(changes, &checked, "我喜欢深色模式", "已确认");
    assert_eq!(filtered.changes.len(), 8);
    assert_eq!(filtered.rejected.duplicate_exact, 1);
    assert_eq!(filtered.rejected.duplicate_normalized, 1);
    assert_eq!(filtered.rejected.invalid_id, 2);
    assert_eq!(filtered.rejected.manual_update, 1);
    assert_eq!(filtered.rejected.evidence_invalid, 1);
    assert_eq!(filtered.rejected.over_limit, 1);
}

#[test]
fn chunks_preserve_unicode_and_large_hits_fit_the_context_budget() {
    let content = "部署步骤".repeat(1000);
    let chunks = split_content(&content);
    assert_eq!(chunks.concat(), content);
    assert!(chunks
        .iter()
        .all(|chunk| !chunk.is_empty() && chunk.len() <= CHUNK_BYTES));
    let original = memory(1, "部署流程", &content);
    let candidates = vec![Candidate {
        memory: original,
        score: 0.9,
        chunks: chunks
            .into_iter()
            .enumerate()
            .map(|(i, text)| (i, 0.9, text))
            .collect(),
    }];
    let result = select(candidates, 5, 2400, "embedding");
    assert_eq!(result.memories.len(), 1);
    assert!(result.memories[0].content.ends_with(PARTIAL_NOTICE));
    assert!(memory_cost(&result.memories[0]) <= 2400);
    assert!(!result.memories[0].content.contains('\u{fffd}'));
}

#[test]
fn fallback_requires_matches_and_uses_english_word_boundaries() {
    let memories = vec![
        memory(1, "Django", "Python server"),
        memory(2, "Go", "Go server"),
        memory(3, "天气", "南京天气"),
    ];
    let result = keyword_search(memories.clone(), "go", 5, 8192, "keyword");
    assert_eq!(
        result.memories.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![2]
    );
    assert!(
        keyword_search(memories.clone(), "早餐吃什么", 5, 8192, "keyword")
            .memories
            .is_empty()
    );
    assert!(keyword_search(memories.clone(), "", 5, 8192, "keyword")
        .memories
        .is_empty());
    assert!(keyword_search(memories, "Go", 5, 0, "keyword")
        .memories
        .is_empty());
}

#[tokio::test]
async fn retrieves_only_current_owner_enabled_relevant_memories_and_reuses_the_index() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let (mut cfg, calls, server) = mock_server().await;
    let first = assistant_store::create_memory("owner", "部署流程", "先构建再部署", true)
        .await
        .unwrap();
    let second = assistant_store::create_memory("owner", "部署备用", "检查部署结果", true)
        .await
        .unwrap();
    let weather = assistant_store::create_memory("owner", "天气", "南京天气", true)
        .await
        .unwrap();
    let disabled = assistant_store::create_memory("owner", "部署停用", "旧部署方式", false)
        .await
        .unwrap();
    let outsider =
        assistant_store::create_memory("other-user", "部署秘密", "另一个用户的部署地址", true)
            .await
            .unwrap();
    let all = assistant_store::list_all_memories().await.unwrap();
    let service = EmbeddingService::from_config(&cfg).unwrap().unwrap();
    refresh_with(&service, &all, None).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM assistant_memory_embeddings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 5, "backfill indexes disabled entries too");
    let document_calls = calls.documents.load(Ordering::SeqCst);
    let found = search_with(&cfg, "owner", "部署怎么做", 8192)
        .await
        .unwrap();
    assert_eq!(found.method, "embedding");
    assert_eq!(
        found.memories.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![first.id, second.id]
    );
    assert!(!found
        .memories
        .iter()
        .any(|m| [weather.id, disabled.id, outsider.id].contains(&m.id)));
    assert_eq!(calls.queries.load(Ordering::SeqCst), 1);
    assert_eq!(calls.documents.load(Ordering::SeqCst), document_calls);
    cfg.memory_top_k = 1;
    let found = search_with(&cfg, "owner", "部署怎么做", 8192)
        .await
        .unwrap();
    assert_eq!(found.memories[0].id, first.id);
    assert_eq!(found.omitted, 1);
    assert_eq!(calls.queries.load(Ordering::SeqCst), 2);
    assert_eq!(calls.documents.load(Ordering::SeqCst), document_calls);
    assert!(search_with(&cfg, "owner", "早餐", 8192)
        .await
        .unwrap()
        .memories
        .is_empty());
    assert!(search_with(&cfg, "empty-user", "部署怎么做", 8192)
        .await
        .unwrap()
        .memories
        .is_empty());
    assistant_store::update_memory(first.id, "owner", None, None, Some(false))
        .await
        .unwrap();
    assert_eq!(
        search_with(&cfg, "owner", "部署怎么做", 8192)
            .await
            .unwrap()
            .memories[0]
            .id,
        second.id
    );
    server.abort();
}

#[tokio::test]
async fn content_updates_invalidate_vectors_old_writes_are_discarded_and_delete_cascades() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let (cfg, _, server) = mock_server().await;
    let service = EmbeddingService::from_config(&cfg).unwrap().unwrap();
    let original = assistant_store::create_memory("owner", "部署", "部署步骤", true)
        .await
        .unwrap();
    refresh_with(&service, std::slice::from_ref(&original), None)
        .await
        .unwrap();
    let old = assistant_store::list_memory_embeddings("owner", &service.space)
        .await
        .unwrap()
        .remove(0);
    let updated =
        assistant_store::update_memory(original.id, "owner", Some("天气"), Some("天气查询"), None)
            .await
            .unwrap()
            .unwrap();
    assert!(cached_chunks(&old, &updated, None).is_none());
    assert!(!assistant_store::save_memory_embedding(
        &original,
        &service.space,
        &old.content_hash,
        old.dimensions as usize,
        &old.chunks
    )
    .await
    .unwrap());
    let found = search_with(&cfg, "owner", "天气", 8192).await.unwrap();
    assert_eq!(found.method, "embedding");
    assert_eq!(found.memories[0].content, "天气查询");
    assert!(search_with(&cfg, "owner", "部署怎么做", 8192)
        .await
        .unwrap()
        .memories
        .is_empty());
    assistant_store::delete_memory(original.id, "owner")
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM assistant_memory_embeddings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(
        !assistant_store::save_memory_embedding(&updated, &service.space, "hash", 3, "[]")
            .await
            .unwrap()
    );
    server.abort();
}

#[tokio::test]
async fn cold_search_builds_vectors_and_endpoint_changes_rebuild_them() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool);
    let (cfg, calls, server) = mock_server().await;
    assistant_store::create_memory("owner", "部署", "部署步骤", true)
        .await
        .unwrap();
    let found = search_with(&cfg, "owner", "部署怎么做", 8192)
        .await
        .unwrap();
    assert_eq!(found.method, "embedding");
    assert_eq!(found.memories.len(), 1);
    assert_eq!(calls.queries.load(Ordering::SeqCst), 1);
    assert_eq!(calls.documents.load(Ordering::SeqCst), 1);
    let (other_cfg, other_calls, other_server) = mock_server().await;
    assert_eq!(
        search_with(&other_cfg, "owner", "部署怎么做", 8192)
            .await
            .unwrap()
            .memories
            .len(),
        1
    );
    assert_eq!(other_calls.documents.load(Ordering::SeqCst), 1);
    server.abort();
    other_server.abort();
}

#[tokio::test]
async fn failures_and_timeouts_fall_back_without_injecting_unrelated_memories() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool);
    let deployment = assistant_store::create_memory("owner", "部署", "部署步骤", true)
        .await
        .unwrap();
    assistant_store::create_memory("owner", "天气", "南京天气", true)
        .await
        .unwrap();
    let app = Router::new()
        .route(
            "/failed/embeddings",
            post(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        )
        .route(
            "/slow/embeddings",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                Json(json!({"data":[]}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut cfg = config(&format!("{base}/failed"));
    let found = search_with(&cfg, "owner", "部署怎么做", 8192)
        .await
        .unwrap();
    assert_eq!(found.method, "keyword_fallback");
    assert_eq!(
        found.memories.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![deployment.id]
    );
    assert!(search_with(&cfg, "owner", "早餐", 8192)
        .await
        .unwrap()
        .memories
        .is_empty());
    cfg.embedding_base_url = format!("{base}/slow");
    cfg.embedding_timeout_secs = 1;
    let start = std::time::Instant::now();
    let found = search_with(&cfg, "owner", "部署怎么做", 8192)
        .await
        .unwrap();
    assert_eq!(found.method, "keyword_fallback");
    assert_eq!(found.memories[0].id, deployment.id);
    assert!(start.elapsed() < Duration::from_secs(2));
    server.abort();
}

#[tokio::test]
async fn embedding_protocol_rejects_missing_duplicate_wrong_dimension_and_zero_vectors() {
    let app = Router::new().route(
        "/v1/embeddings",
        post(|Json(body): Json<Value>| async move {
            let data = match body["model"].as_str().unwrap() {
                "missing" => json!([]),
                "duplicate" => json!([{"index":0,"embedding":[1.0]},{"index":0,"embedding":[1.0]}]),
                "dimension" => {
                    json!([{"index":0,"embedding":[1.0]},{"index":1,"embedding":[1.0,0.0]}])
                }
                _ => json!([{"index":0,"embedding":[0.0]}]),
            };
            Json(json!({"data":data}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = config(&format!("http://{}/v1", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    for model in ["missing", "duplicate", "dimension", "zero"] {
        cfg.embedding_model = model.into();
        let service = EmbeddingService::from_config(&cfg).unwrap().unwrap();
        let inputs = if matches!(model, "duplicate" | "dimension") {
            vec!["a".into(), "b".into()]
        } else {
            vec!["a".into()]
        };
        assert!(service.embed(&inputs).await.is_err(), "{model}");
    }
    server.abort();
}

/// Opt-in end-to-end check against the configured service; ordinary tests stay offline.
#[tokio::test]
#[ignore = "requires the configured EMBEDDING_* service"]
async fn live_embedding_retrieval_smoke() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool);
    let mut cfg: Config =
        serde_json::from_value(json!({"jwt_secret":"test", "auth_password":"test-password"}))
            .unwrap();
    if let Ok(value) = std::env::var("EMBEDDING_BASE_URL") {
        cfg.embedding_base_url = value;
    }
    if let Ok(value) = std::env::var("EMBEDDING_MODEL") {
        cfg.embedding_model = value;
    }
    if let Ok(value) = std::env::var("EMBEDDING_API_KEY") {
        cfg.embedding_api_key = value;
    }
    let deployment = assistant_store::create_memory(
        "smoke-owner",
        "测试环境部署流程",
        "发布前先检查构建，调用部署接口更新测试环境的版本。",
        true,
    )
    .await
    .unwrap();
    assistant_store::create_memory("smoke-owner", "早餐", "我通常早餐喝牛奶。", true)
        .await
        .unwrap();
    let found = search_with(&cfg, "smoke-owner", "如何更新测试环境部署版本", 8192)
        .await
        .unwrap();
    assert_eq!(
        found.method, "embedding",
        "must use the real embedding service, without falling back"
    );
    assert_eq!(
        found
            .memories
            .iter()
            .map(|memory| memory.id)
            .collect::<Vec<_>>(),
        vec![deployment.id]
    );
    println!("live semantic retrieval: {:?}", found.scores);
    let unrelated = search_with(&cfg, "smoke-owner", "南京明天会下雨吗", 8192)
        .await
        .unwrap();
    assert_eq!(unrelated.method, "embedding");
    assert!(
        unrelated.memories.is_empty(),
        "an unrelated query must not inject memories"
    );
}
