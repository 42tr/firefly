use super::*;
use axum::{body::to_bytes, http::Request, routing::{get, post, put}, Router};
use tower::ServiceExt;

#[tokio::test]
async fn schedules_api_auth_cron_validation_and_crud() {
    config::init_test_config();
    crate::db::set_test_pool(crate::db::test_pool().await);
    let app = Router::new().route("/schedules", get(list).post(create))
        .route("/schedules/preview", post(preview))
        .route("/schedules/{id}", put(update).patch(toggle).delete(delete));
    let token = crate::api::jwt::create(&crate::api::jwt::Claims::new(1)).unwrap();
    let request = |method: &str, path: &str, body: Value, auth: bool| {
        let mut request = Request::builder().method(method).uri(path).header("content-type", "application/json");
        if auth { request = request.header("cookie", format!("firefly_session={token}")); }
        request.body(Body::from(body.to_string())).unwrap()
    };
    let input = json!({"title":"每日巡检","prompt":"检查并总结","cron":"0 9 * * *","enabled":true,
        "execution":{"agent":"firefly","workspace_mode":"shared","cwd":null,"provider":null,"model":null,"reasoning_level":null}});
    for (method, path) in [("GET", "/schedules"), ("POST", "/schedules"), ("PUT", "/schedules/1"), ("PATCH", "/schedules/1"), ("DELETE", "/schedules/1"), ("POST", "/schedules/preview")] {
        let response = app.clone().oneshot(request(method, path, input.clone(), false)).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    for field in ["cron", "prompt", "title"] {
        let mut invalid = input.clone(); invalid[field] = json!("");
        assert_eq!(app.clone().oneshot(request("POST", "/schedules", invalid, true)).await.unwrap().status(), StatusCode::BAD_REQUEST);
    }
    let mut invalid = input.clone(); invalid["execution"]["agent"] = json!("unknown");
    assert_eq!(app.clone().oneshot(request("POST", "/schedules", invalid, true)).await.unwrap().status(), StatusCode::BAD_REQUEST);
    let response = app.clone().oneshot(request("POST", "/schedules/preview", json!({"cron":"0 9 * * 1-5"}), true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let preview: Value = serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
    assert_eq!(preview["times"].as_array().unwrap().len(), 3);
    assert_eq!(preview["timezone"], "Asia/Shanghai");
    let response = app.clone().oneshot(request("POST", "/schedules", input.clone(), true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let created: Value = serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
    assert!(created["next_run_at"].is_string());
    assert_eq!(created["execution"]["agent"], "firefly");
    assert!(created.get("user_id").is_none());
    let path = format!("/schedules/{}", created["id"]);
    let response = app.clone().oneshot(request("PATCH", &path, json!({"enabled":false}), true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let disabled: Value = serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
    assert!(disabled["next_run_at"].is_null());
    let mut edited = input.clone(); edited["prompt"] = json!("更新后的任务");
    assert_eq!(app.clone().oneshot(request("PUT", &path, edited, true)).await.unwrap().status(), StatusCode::OK);
    let listed = app.clone().oneshot(request("GET", "/schedules", json!(null), true)).await.unwrap();
    let listed: Value = serde_json::from_slice(&to_bytes(listed.into_body(), 10000).await.unwrap()).unwrap();
    assert_eq!(listed["schedules"][0]["prompt"], "更新后的任务");
    assert_eq!(listed["schedules"][0]["enabled"], true);
    assert_eq!(app.clone().oneshot(request("DELETE", &path, json!(null), true)).await.unwrap().status(), StatusCode::OK);
    assert_eq!(app.clone().oneshot(request("PUT", &path, input, true)).await.unwrap().status(), StatusCode::NOT_FOUND);
    assert_eq!(app.oneshot(request("DELETE", &path, json!(null), true)).await.unwrap().status(), StatusCode::NOT_FOUND);
}
