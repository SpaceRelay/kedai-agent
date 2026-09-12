// 计划二 · 7 作用域变量 API 集成测试:
//   - PUT/GET /api/variables 读写回环(global 作用域)
//   - scope=chat 与旧 PUT /api/chat/sessions/{id}/assistant-vars 结果等价
//   - PATCH 应用 JSON Patch 子集(chat 作用域增量写)
//   - message 作用域 GET 缺镜像时返回空对象(不报错)
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kedai_server::build_test_app;
use serde_json::{json, Value};
use std::sync::OnceLock;
use tokio::sync::{Mutex, MutexGuard};
use tower::ServiceExt;

fn test_app() -> &'static axum::Router {
    static APP: OnceLock<axum::Router> = OnceLock::new();
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let dir = std::env::temp_dir().join("kedai-test-runtime-prompt-empty");
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("KEDAI_RUNTIME_PROMPT_DIR", &dir);
    });
    APP.get_or_init(|| build_test_app().expect("构建测试应用失败"))
}

/// 共享同一 app/DB,须串行执行(panic 毒化容错:取回锁继续)
async fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().await
}

async fn send_json(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// 上传最小角色卡并创建会话,返回 (sid, cid)
async fn new_session(app: &axum::Router) -> (String, String) {
    let card = json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "name": "变量测试角色",
        "description": "用于 7 作用域变量 API 测试。",
        "first_mes": "你好",
        "data": { "name": "变量测试角色", "description": "用于 7 作用域变量 API 测试。" }
    });
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"min.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        card
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/characters/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let char: Value = serde_json::from_slice(&bytes).unwrap();
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, sess) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = sess["id"].as_str().unwrap().to_string();
    (sid, cid)
}

/// global 作用域 PUT/GET 读写回环
#[tokio::test]
async fn global_scope_put_get_roundtrip() {
    let _guard = test_lock().await;
    let app = test_app();
    let (sid, _) = new_session(app).await;

    let (status, res) = send_json(
        app,
        "PUT",
        "/api/variables",
        json!({ "session_id": sid, "scope": "global", "data": { "好感度": 66, "flag": true } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "PUT global 应成功: {res}");

    let (status, res) = send_json(
        app,
        "GET",
        &format!("/api/variables?session_id={sid}&scope=global"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET global 应成功: {res}");
    assert_eq!(res["data"]["好感度"], json!(66));
    assert_eq!(res["data"]["flag"], json!(true));

    // 作用域隔离:global 不影响 chat 树(旧接口读回空树)
    let (_, chat) = send_json(
        app,
        "GET",
        &format!("/api/variables?session_id={sid}&scope=chat"),
        json!({}),
    )
    .await;
    assert_eq!(
        chat["data"],
        json!({}),
        "global 写入不应污染 chat 树: {chat}"
    );
}

/// scope=chat 与旧 PUT assistant-vars 等价(同一存储:旧接口写、新接口读回)
#[tokio::test]
async fn chat_scope_equivalent_to_legacy_endpoint() {
    let _guard = test_lock().await;
    let app = test_app();
    let (sid, _) = new_session(app).await;

    let tree = json!({ "心之所向": { "好感度": 150 } });
    let (status, res) = send_json(
        app,
        "PUT",
        "/api/variables",
        json!({ "session_id": sid, "scope": "chat", "data": tree }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "PUT chat 应成功: {res}");
    let (_, chat) = send_json(
        app,
        "GET",
        &format!("/api/variables?session_id={sid}&scope=chat"),
        json!({}),
    )
    .await;
    assert_eq!(chat["data"], tree, "新接口读回: {chat}");

    // 旧接口覆写后,新接口读回同一树
    let (_, res2) = send_json(
        app,
        "PUT",
        &format!("/api/chat/sessions/{sid}/assistant-vars"),
        json!({ "stat_data": { "心之所向": { "好感度": 8 } } }),
    )
    .await;
    assert_eq!(res2["ok"], json!(true));
    let (_, chat2) = send_json(
        app,
        "GET",
        &format!("/api/variables?session_id={sid}&scope=chat"),
        json!({}),
    )
    .await;
    assert_eq!(chat2["data"]["心之所向"]["好感度"], json!(8));
}

/// PATCH 对 chat 作用域应用 JSON Patch 子集(增量写)
#[tokio::test]
async fn patch_applies_jsonpatch_to_chat() {
    let _guard = test_lock().await;
    let app = test_app();
    let (sid, _) = new_session(app).await;

    // 先写基础树
    send_json(
        app,
        "PUT",
        "/api/variables",
        json!({ "session_id": sid, "scope": "chat", "data": { "好感度": 10, "flag": false } }),
    )
    .await;

    // PATCH:replace 好感度 + remove flag
    let (status, res) = send_json(
        app,
        "PATCH",
        "/api/variables",
        json!({
            "session_id": sid,
            "scope": "chat",
            "data": [
                { "op": "replace", "path": "/好感度", "value": 42 },
                { "op": "remove", "path": "/flag" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "PATCH chat 应成功: {res}");

    let (_, chat) = send_json(
        app,
        "GET",
        &format!("/api/variables?session_id={sid}&scope=chat"),
        json!({}),
    )
    .await;
    assert_eq!(chat["data"]["好感度"], json!(42));
    assert!(chat["data"].get("flag").is_none(), "remove 应生效: {chat}");

    // 非法 scope → 400
    let (status, _) = send_json(
        app,
        "GET",
        &format!("/api/variables?session_id={sid}&scope=bogus"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "非法 scope 应 400");
}

/// message 作用域 GET:缺镜像/缺消息时返回空对象(不报错)
#[tokio::test]
async fn message_scope_missing_returns_empty() {
    let _guard = test_lock().await;
    let app = test_app();
    let (sid, _) = new_session(app).await;

    let (status, res) = send_json(
        app,
        "GET",
        &format!("/api/variables?session_id={sid}&scope=message&scope_id=99999"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "缺镜像 message 应 200: {res}");
    assert_eq!(res["data"], json!({}), "缺数据返回空对象: {res}");
}
