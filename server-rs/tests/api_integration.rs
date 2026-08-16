// API 集成测试:与 Node 版 server/tests/api.test.ts 对齐
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
    APP.get_or_init(|| build_test_app().expect("构建测试应用失败"))
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

async fn send_empty(app: &axum::Router, method: &str, path: &str) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    resp.status()
}

async fn upload_character(app: &axum::Router, name: &str) -> (StatusCode, Value) {
    // 构造 multipart 表单:字段 file
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        json!({
            "spec": "chara_card_v2",
            "spec_version": "1.0",
            "name": "测试角色",
            "description": "测试描述",
            "first_mes": "你好,我是测试角色",
            "custom_field": { "unknown": true }
        })
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/characters/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn health() {
    let app = test_app();
    let (status, json) = send_json(app, "GET", "/api/health", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], json!(true));
    assert!(json["ts"].is_number());
}

#[tokio::test]
async fn character_crud() {
    let app = test_app();
    // 上传
    let (status, char) = upload_character(app, "测试卡.json").await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let id = char["id"].as_str().unwrap().to_string();
    assert_eq!(char["chara_name"], json!("测试角色"));
    assert_eq!(char["name"], json!("测试卡"));
    assert_eq!(char["data_raw"]["custom_field"]["unknown"], json!(true));
    // 列表(不含 data_raw)
    let (_, list) = send_json(app, "GET", "/api/characters", json!({})).await;
    assert!(!list["characters"].as_array().unwrap().is_empty());
    assert!(list["characters"][0].get("data_raw").is_none());
    // 详情
    let (status, detail) = send_json(app, "GET", &format!("/api/characters/{id}"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(detail["data_raw"].is_object());
    // 更新
    let (status, updated) = send_json(
        app,
        "PUT",
        &format!("/api/characters/{id}"),
        json!({ "chara_name": "新名字", "description": "新描述" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["chara_name"], json!("新名字"));
    assert_eq!(updated["data_raw"]["name"], json!("新名字"));
    // 删除
    let status = send_empty(app, "DELETE", &format!("/api/characters/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // 删除后 404
    let (status, _) = send_json(app, "GET", &format!("/api/characters/{id}"), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tool_permissions_are_derived_from_valid_session_and_registered_tool() {
    let app = test_app();
    let (_, first_char) = upload_character(app, "权限角色一.json").await;
    let (_, second_char) = upload_character(app, "权限角色二.json").await;
    let first_cid = first_char["id"].as_str().unwrap();
    let second_cid = second_char["id"].as_str().unwrap();
    let (_, first_session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": first_cid }),
    )
    .await;
    let (_, second_session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": second_cid }),
    )
    .await;
    let first_sid = first_session["id"].as_str().unwrap();
    let second_sid = second_session["id"].as_str().unwrap();

    let (status, _) = send_json(
        app,
        "POST",
        "/api/agent/tool-permissions",
        json!({
            "session_id": "missing-session", "tool": "write", "scope": "session"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send_json(
        app,
        "POST",
        "/api/agent/tool-permissions",
        json!({
            "session_id": first_sid, "tool": "not-registered", "scope": "session"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = send_json(
        app,
        "POST",
        "/api/agent/tool-permissions",
        json!({
            "session_id": first_sid,
            "tool": "write",
            "scope": "role",
            "scope_id": second_cid
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, first_permissions) = send_json(
        app,
        "GET",
        &format!("/api/agent/tool-permissions?session_id={first_sid}&character_id={second_cid}"),
        json!({}),
    )
    .await;
    let first_write = first_permissions["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "write")
        .unwrap();
    assert_eq!(first_write["allowed"], json!(true));

    let (_, second_permissions) = send_json(
        app,
        "GET",
        &format!("/api/agent/tool-permissions?session_id={second_sid}&character_id={first_cid}"),
        json!({}),
    )
    .await;
    let second_write = second_permissions["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "write")
        .unwrap();
    assert_eq!(second_write["allowed"], json!(false));
}

#[tokio::test]
async fn session_and_messages() {
    let app = test_app();
    let (_, char) = upload_character(app, "会话测试.json").await;
    let cid = char["id"].as_str().unwrap().to_string();

    // 创建会话
    let (status, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid, "title": "我的会话" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let sid = session["id"].as_str().unwrap().to_string();
    assert_eq!(session["title"], json!("我的会话"));

    // 列表
    let (_, list) = send_json(
        app,
        "GET",
        &format!("/api/chat/sessions?character_id={cid}"),
        json!({}),
    )
    .await;
    assert_eq!(list["sessions"].as_array().unwrap().len(), 1);

    // 历史:角色 first_mes 作为首条 assistant 消息注入(开场白)
    let (_, history) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    let msgs = history["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], json!("assistant"));
    assert_eq!(msgs[0]["content"], json!("你好,我是测试角色"));
    assert_eq!(msgs[0]["extra"]["first_mes"], json!(true));

    // 导入消息
    let (status, imp) = send_json(
        app,
        "POST",
        "/api/import/chat",
        json!({ "session_id": sid, "messages": [
            { "role": "user", "content": "第一条" },
            { "role": "assistant", "content": "回复一" },
            { "role": "system", "content": "记忆", "extra": { "kind": "memory" } }
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(imp["imported"], json!(3));

    // 历史
    let (_, history) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    let msgs = history["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["role"], json!("user"));
    assert!(msgs[0]["id"].is_number());

    // 更新消息
    let mid = msgs[0]["id"].as_i64().unwrap();
    let (status, updated) = send_json(
        app,
        "PUT",
        &format!("/api/chat/messages/{mid}?session_id={sid}"),
        json!({ "content": "改过的" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["content"], json!("改过的"));
    assert_eq!(updated["extra"]["edited"], json!(true));

    // 删除消息
    let status = send_empty(
        app,
        "DELETE",
        &format!("/api/chat/messages/{mid}?session_id={sid}"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 导出
    let (status, exp) = send_json(
        app,
        "GET",
        &format!("/api/export/chat?session_id={sid}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(exp["session_id"], json!(sid));
    assert_eq!(exp["messages"].as_array().unwrap().len(), 2);

    // 清空
    let (status, clear) =
        send_json(app, "POST", "/api/chat/clear", json!({ "session_id": sid })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(clear["ok"], json!(true));
    let (_, history) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    assert_eq!(history["messages"].as_array().unwrap().len(), 0);

    // 删除会话
    let status = send_empty(app, "DELETE", &format!("/api/chat/sessions/{sid}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn settings_and_token() {
    let app = test_app();
    // settings/connect(mock)
    let (status, conn) = send_json(app, "POST", "/api/settings/connect", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(conn["ok"], json!(true));
    assert_eq!(conn["models"][0], json!("mock-demo"));
    // settings/info
    let (_, info) = send_json(app, "GET", "/api/settings/info", json!({})).await;
    assert_eq!(info["connector"], json!("mock"));
    assert!(info["availableConnectors"].as_array().unwrap().len() >= 2);
    // settings/model
    let (_, m) = send_json(app, "GET", "/api/settings/model", json!({})).await;
    assert!(m["model"].as_str().is_some());
    // 切换模型(mock 下不变更但返回 ok)
    let (status, sw) = send_json(
        app,
        "PUT",
        "/api/settings/model",
        json!({ "model": "gpt-4o" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sw["ok"], json!(true));
    // token/count
    let (status, tc) = send_json(
        app,
        "POST",
        "/api/token/count",
        json!({ "messages": [{ "role": "user", "content": "你好" }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(tc["total"].as_i64().unwrap() >= 4 + 2);
}

#[tokio::test]
async fn tool_permission_api_authorizes_session_without_executing_tool() {
    let app = test_app();
    let (_, character) = upload_character(app, "权限 API 角色.json").await;
    let character_id = character["id"].as_str().unwrap();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": character_id }),
    )
    .await;
    let session_id = session["id"].as_str().unwrap();
    let (status, before) = send_json(
        app,
        "GET",
        &format!("/api/agent/tool-permissions?session_id={session_id}&character_id={character_id}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let write_before = before["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "write")
        .unwrap();
    assert_eq!(write_before["risk"], json!("dangerous"));
    assert_eq!(write_before["allowed"], json!(false));

    let (status, _) = send_json(
        app,
        "POST",
        "/api/agent/tool-permissions",
        json!({ "session_id": session_id, "tool": "write", "scope": "session" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, after) = send_json(
        app,
        "GET",
        &format!("/api/agent/tool-permissions?session_id={session_id}&character_id={character_id}"),
        json!({}),
    )
    .await;
    let write_after = after["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "write")
        .unwrap();
    assert_eq!(write_after["allowed"], json!(true));
}

/// 最终提示词预览按层输出且聊天历史仅返回角色/长度/哈希，不泄露正文或 API Key。
#[tokio::test]
async fn prompt_preview_is_layered_and_redacts_history() {
    let app = test_app();
    let (_, char) = upload_character(app, "预览角色.json").await;
    let cid = char["id"].as_str().unwrap();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap();
    let secret = "CHAT_BODY_MUST_NOT_LEAK_92a1";
    let (_, _) = send_json(
        app,
        "POST",
        "/api/import/chat",
        json!({ "session_id": sid, "messages": [{ "role": "user", "content": secret }] }),
    )
    .await;

    let (status, preview) = send_json(
        app,
        "GET",
        &format!("/api/settings/prompt-preview?session_id={sid}&character_id={cid}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "预览失败: {preview}");
    let layers = preview["layers"].as_array().expect("layers 应为数组");
    assert!(layers.iter().all(|v| v["source"].is_string()
        && v["role"].is_string()
        && v["layer"].is_number()
        && v["order"].is_number()));
    let history = layers
        .iter()
        .find(|v| v["source"] == "history_summary")
        .expect("应有历史摘要层");
    assert!(history["content"].as_str().unwrap().contains("role=user"));
    assert!(history["content"].as_str().unwrap().contains("hash="));
    let text = preview.to_string();
    assert!(!text.contains(secret), "预览不得泄露聊天正文: {text}");
    assert!(
        !text.to_lowercase().contains("api_key"),
        "预览不得包含 API key 字段: {text}"
    );
}

#[tokio::test]
async fn agent_execute_is_explicitly_not_implemented() {
    let app = test_app();
    let (status, body) = send_json(
        app,
        "POST",
        "/api/agent/execute",
        json!({ "session_id": "unused" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body["error"], json!("该接口尚未实现,请使用 /api/chat/send"));
}

#[tokio::test]
async fn agent_plan() {
    let app = test_app();
    let (status, plan) = send_json(
        app,
        "POST",
        "/api/agent/plan",
        json!({ "message": "你好", "agent_mode": "fast" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(plan["summary"], json!("快速模式:直接生成回复"));
    assert!(plan["tools"]
        .as_array()
        .unwrap()
        .contains(&json!("calculator")));

    let (status, deep) = send_json(
        app,
        "POST",
        "/api/agent/plan",
        json!({ "message": "你好", "agent_mode": "deep" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // deep 模式:计划生成步(先写 ≤200 字计划再输出正文)+ 反思步(理解意图步已废弃)
    let steps = deep["plan"]["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["generates"], json!(true));
    assert_eq!(steps[1]["action"], json!("reflect"));
    assert!(
        steps[0]["system_prompt"]
            .as_str()
            .unwrap_or("")
            .contains("200 字"),
        "deep 生成步应先写 ≤200 字计划: {:?}",
        steps[0]["system_prompt"]
    );

    // 缺少 message → 400
    let (status, _) = send_json(app, "POST", "/api/agent/plan", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn chat_send_sse() {
    let app = test_app();
    let (_, char) = upload_character(app, "SSE测试.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/send")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "session_id": sid, "character_id": cid, "message": "你好" }).to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("text/event-stream"));

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    // 解析 SSE 事件
    let events: Vec<Value> = text
        .split("\n\n")
        .filter_map(|block| {
            let block = block.trim();
            if block.is_empty() {
                return None;
            }
            let data_line = block.lines().find(|l| l.starts_with("data: "))?;
            serde_json::from_str(&data_line[6..]).ok()
        })
        .collect();
    let types: Vec<&str> = events.iter().filter_map(|e| e["type"].as_str()).collect();
    // 至少包含 step(计划中…) 与 finish
    assert!(types.contains(&"step"), "事件缺 step: {types:?}");
    assert!(types.contains(&"finish"), "事件缺 finish: {types:?}");
    let finish = events.iter().find(|e| e["type"] == "finish").unwrap();
    assert!(!finish["content"].as_str().unwrap().is_empty());
    assert!(finish["usage"]["context_tokens"].is_number());
}

#[tokio::test]
async fn chat_send_validation() {
    let app = test_app();
    // 空消息 → 400
    let (status, _) = send_json(
        app,
        "POST",
        "/api/chat/send",
        json!({ "message": "  ", "character_id": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // 缺 session_id 和 character_id → 400
    let (status, _) = send_json(app, "POST", "/api/chat/send", json!({ "message": "hi" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // 会话不存在 → 404
    let (status, _) = send_json(
        app,
        "POST",
        "/api/chat/send",
        json!({ "message": "hi", "session_id": "no-such-session" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn concurrent_send_reserves_session_before_message_insert() {
    let app = test_app();
    let (_, character) = upload_character(app, "并发占位.json").await;
    let cid = character["id"].as_str().unwrap();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap();
    let make = || {
        Request::builder()
            .method("POST")
            .uri("/api/chat/send")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "session_id": sid, "message": "并发消息 [[sleep:200]]" }).to_string(),
            ))
            .unwrap()
    };
    let (first, second) = tokio::join!(app.clone().oneshot(make()), app.clone().oneshot(make()));
    let statuses = [first.unwrap().status(), second.unwrap().status()];
    assert!(statuses.contains(&StatusCode::OK));
    assert!(statuses.contains(&StatusCode::CONFLICT));
}

#[tokio::test]
async fn resend_requires_current_matching_user_message() {
    let app = test_app();
    let (_, character) = upload_character(app, "安全重发.json").await;
    let cid = character["id"].as_str().unwrap();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap();
    let (_, _) = send_json(
        app,
        "POST",
        "/api/import/chat",
        json!({ "session_id": sid, "messages": [{ "role": "user", "content": "原文" }] }),
    )
    .await;
    let (_, history) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    let id = history["messages"][0]["id"].as_i64().unwrap();
    let (status, _) = send_json(
        app,
        "POST",
        "/api/chat/send",
        json!({ "session_id": sid, "message": "篡改", "resend_message_id": id }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn concurrent_partial_settings_updates_preserve_both_fields() {
    let app = test_app();
    let first = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "preset_tail_role": "assistant" }),
    );
    let second = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "render_html": true }),
    );
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.0, StatusCode::OK, "第一笔设置更新失败: {:?}", first.1);
    assert_eq!(second.0, StatusCode::OK, "第二笔设置更新失败: {:?}", second.1);

    let (status, current) = send_json(app, "GET", "/api/settings", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(current["preset_tail_role"], json!("assistant"));
    assert_eq!(current["render_html"], json!(true));

    // 避免共享测试应用污染后续用例。
    let _ = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "preset_tail_role": "user", "render_html": false }),
    )
    .await;
}

#[tokio::test]
async fn generation_limits_return_bad_request() {
    let app = test_app();
    let (_, character) = upload_character(app, "参数上限.json").await;
    let cid = character["id"].as_str().unwrap();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap();
    let (status, _) = send_json(
        app,
        "POST",
        "/api/chat/send",
        json!({ "session_id": sid, "message": "hi", "max_tokens": 65537 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "max_context_tokens": 1048577 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn token_calculator_sse() {
    let app = test_app();
    let (_, char) = upload_character(app, "计算测试.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/send")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "session_id": sid, "character_id": cid, "message": "帮我算一下 12*34" })
                .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let events: Vec<Value> = text
        .split("\n\n")
        .filter_map(|block| {
            let block = block.trim();
            if block.is_empty() {
                return None;
            }
            let data_line = block.lines().find(|l| l.starts_with("data: "))?;
            serde_json::from_str(&data_line[6..]).ok()
        })
        .collect();
    let types: Vec<&str> = events.iter().filter_map(|e| e["type"].as_str()).collect();
    assert!(types.contains(&"tool_call"), "缺 tool_call: {types:?}");
    assert!(types.contains(&"tool_result"), "缺 tool_result: {types:?}");
    let tool_result = events.iter().find(|e| e["type"] == "tool_result").unwrap();
    assert_eq!(tool_result["output"]["result"].as_f64(), Some(408.0));
}

/// 回归:反思持续失败必须有界结束,绝不无限循环。
/// 复现 2026-08-06 日志事故:同一会话每毫秒一条「输出为空或过短」reflect 洪流
/// (根因:反思失败回退逻辑在 plan 的 direct 步骤前原地打转,attempt 永不递增)。
/// mock [[empty]] 钩子让每轮生成都输出空内容 → 反思必失败 → 引擎应重试有限次后正常 Finish。
#[tokio::test]
async fn agent_reflect_failure_is_bounded() {
    let app = test_app();
    let (_, char) = upload_character(app, "空输出测试.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/send")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "session_id": sid, "character_id": cid,
                "message": "你好 [[empty]]", "agent_mode": "deep"
            })
            .to_string(),
        ))
        .unwrap();
    // 有界保护:旧代码在此会无限紧密循环,10 秒超时直接判失败
    let resp = tokio::time::timeout(std::time::Duration::from_secs(10), app.clone().oneshot(req))
        .await
        .expect("反思失败场景未在 10 秒内有界结束(回归:无限循环)")
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let events: Vec<Value> = text
        .split("\n\n")
        .filter_map(|block| {
            let block = block.trim();
            if block.is_empty() {
                return None;
            }
            let data_line = block.lines().find(|l| l.starts_with("data: "))?;
            serde_json::from_str(&data_line[6..]).ok()
        })
        .collect();
    let types: Vec<&str> = events.iter().filter_map(|e| e["type"].as_str()).collect();
    assert!(types.contains(&"finish"), "缺 finish: {types:?}");
    // 反思重试次数有界:max_attempts=3,前 2 次失败重试,第 3 次失败放弃
    let retries = events
        .iter()
        .filter(|e| e["type"] == "step" && e["step"] == "重新生成")
        .count();
    assert_eq!(
        retries, 2,
        "空输出应恰好重试 2 次后放弃,实际 {retries} 次: {text}"
    );
    let failed_reflects = events
        .iter()
        .filter(|e| e["type"] == "step" && e["step"] == "反思未通过")
        .count();
    assert_eq!(
        failed_reflects, 3,
        "应恰好反思失败 3 次,实际 {failed_reflects} 次"
    );
}

/// 消息截断端点(「编辑用户消息后重发」的支撑):保留 anchor 消息,删除其后所有消息
#[tokio::test]
async fn truncate_messages_after_anchor() {
    let app = test_app();
    let (_, char) = upload_character(app, "截断测试.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();

    // 导入 4 条消息(role 任意,id 自增有序)
    let (status, imp) = send_json(
        app,
        "POST",
        "/api/import/chat",
        json!({ "session_id": sid, "messages": [
            { "role": "user", "content": "m1" },
            { "role": "assistant", "content": "r1" },
            { "role": "user", "content": "m2" },
            { "role": "assistant", "content": "r2" }
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(imp["imported"], json!(4));

    let (_, history) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    let msgs = history["messages"].as_array().unwrap().clone();
    assert_eq!(msgs.len(), 4);
    let anchor = msgs[1]["id"].as_i64().unwrap(); // 第二条(assistant r1)为锚点

    // 截断:删除 id > anchor 的消息(m2 / r2)
    let (status, res) = send_json(
        app,
        "POST",
        &format!("/api/chat/sessions/{sid}/truncate"),
        json!({ "anchor_id": anchor }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["ok"], json!(true));
    assert_eq!(res["deleted"], json!(2));

    // 历史只剩前两条,锚点消息本身保留
    let (_, history) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    let msgs = history["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["content"], json!("m1"));
    assert_eq!(msgs[1]["content"], json!("r1"));

    // 会话不存在 → 404
    let (status, _) = send_json(
        app,
        "POST",
        "/api/chat/sessions/no-such/truncate",
        json!({ "anchor_id": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// 串行化修改全局配置(settings / agent-flows)的测试,避免同进程内相互污染
async fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    // 测试进程级:把运行时主提示词目录指向空目录,避免真实项目根的 AGENTS_RUNTIME.md
    // 被注入 mock 断言(与 build_test_app 临时数据目录的隔离语义一致)。Once 保证只设一次。
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let dir = std::env::temp_dir().join("kedai-test-runtime-prompt-empty");
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("KEDAI_RUNTIME_PROMPT_DIR", &dir);
    });
    LOCK.get_or_init(|| Mutex::new(())).lock().await
}

/// 以指定 agent_mode 发送消息,解析 SSE 事件列表
async fn sse_events(
    app: &axum::Router,
    sid: &str,
    cid: &str,
    message: &str,
    agent_mode: &str,
) -> Vec<Value> {
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/send")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "session_id": sid,
                "character_id": cid,
                "message": message,
                "agent_mode": agent_mode,
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "chat/send 应 200");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    text.split("\n\n")
        .filter_map(|block| {
            let block = block.trim();
            if block.is_empty() {
                return None;
            }
            let data_line = block.lines().find(|l| l.starts_with("data: "))?;
            serde_json::from_str(&data_line[6..]).ok()
        })
        .collect()
}

/// 恢复流程配置为未启用
async fn reset_flow(app: &axum::Router) {
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/agent-flows",
        json!({ "config": { "enabled": false, "steps": [] } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "重置流程配置应 200");
}

/// 回归:纯变量更新消息(正文为空)必须落库并携带 extra.mvu 快照。
/// 否则前端 loadHistory 回放会把变量树回滚到更新前(刷新丢变量/状态栏回退)。
#[tokio::test]
async fn pure_mvu_update_message_persists_with_snapshot() {
    let app = test_app();
    let (_, char) = upload_character(app, "纯变量更新.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    // 模型仅输出 <UpdateVariable> 块(无正文)
    let patch = r#"<UpdateVariable><JSONPatch>[{"op":"replace","path":"/hp","value":88}]</JSONPatch></UpdateVariable>"#;
    let events = sse_events(
        app,
        &sid,
        &cid,
        &format!("更新变量 [[reply:{patch}]]"),
        "deep",
    )
    .await;
    // 补丁已应用并推送 vars 事件
    let vars_evt = events
        .iter()
        .find(|e| e["type"] == "vars")
        .expect("应有 vars 事件");
    assert_eq!(vars_evt["stat_data"]["hp"], json!(88));
    assert!(
        events.iter().any(|e| e["type"] == "finish"),
        "应有 finish 事件"
    );
    // 核心:消息落库且 extra.mvu 快照正确(正文为空也落库)
    let (status, hist) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let msgs = hist["messages"].as_array().unwrap();
    let last = msgs.last().expect("应有 assistant 消息");
    assert_eq!(last["role"], "assistant");
    assert_eq!(
        last["extra"]["mvu"]["stat_data"]["hp"],
        json!(88),
        "纯变量更新消息必须落库快照(否则刷新回放会回滚变量树): {hist}"
    );
}

/// 配置反思提示词后,deep 模式反思步骤调用 LLM 判定:
/// mock [[reply:FAIL ...]] 让草稿与反思判定都输出 FAIL → 反思失败重试 3 次(共 4 次判定)后有界结束。
/// 未配置提示词时,机械规则对该草稿(非空、非截断、无提问)会直接通过——
/// 因此「反思未通过」恰好 4 次即证明 LLM 判定生效。
#[tokio::test]
async fn reflect_prompt_triggers_llm_reflection() {
    let _guard = test_lock().await;
    let app = test_app();
    // 配置反思提示词(结束前恢复,避免污染同进程其他测试)
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "reflect_prompt": "你是质检员:检查草稿是否回应了用户,输出 PASS 或 FAIL。" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "设置 reflect_prompt 应 200");
    let (_, char) = upload_character(app, "反思提示词.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    let events = sse_events(
        app,
        &sid,
        &cid,
        "这段回复如何 [[reply:FAIL 回复未回应提问]]",
        "deep",
    )
    .await;
    let failed_reflects = events
        .iter()
        .filter(|e| e["type"] == "step" && e["step"] == "反思未通过")
        .count();
    assert_eq!(
        failed_reflects, 4,
        "LLM 反思应判定失败 4 次(重试 3 次)后有界结束(机械规则不会判该草稿失败): {events:?}"
    );
    assert!(events.iter().any(|e| e["type"] == "finish"), "应有 finish");
    // 恢复默认(机械规则)
    let (status, _) = send_json(app, "PUT", "/api/settings", json!({ "reflect_prompt": "" })).await;
    assert_eq!(status, StatusCode::OK, "恢复 reflect_prompt 应 200");
}

/// 状态自动注入:角色卡仅有 [InitVar](无 {{format_message_variable}} 常驻条目)时,
/// 引擎仍会把「当前状态」+ 更新协议注入 system —— 模型看得到状态,才会输出 <UpdateVariable>。
#[tokio::test]
async fn state_block_auto_injected_into_system() {
    let _guard = test_lock().await;
    let app = test_app();
    let card = json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "name": "状态角色",
        "description": "测试状态自动注入",
        "first_mes": "你好,我是状态角色",
        "data": {
            "name": "状态角色",
            "description": "测试状态自动注入",
            "character_book": {
                "entries": [
                    {
                        "id": 0,
                        "comment": "[InitVar]",
                        "content": "hp: 100\n好感度: 50",
                        "constant": false,
                        "enabled": false,
                        "position": 0,
                        "order": 100
                    }
                ]
            }
        }
    });
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"状态角色.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
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
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();

    // mock [[floors]] 回显全部 LLM 消息 → 断言 system 含自动注入的状态块。
    // 注意:注入文本中的 <UpdateVariable> 示例会被引擎按「模型输出的补丁」正常剥离
    // (parse_update_variable),故只断言状态与协议标题。
    let events = sse_events(app, &sid, &cid, "你好 [[floors]]", "fast").await;
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("缺 finish 事件");
    let content = finish["content"].as_str().unwrap_or("");
    assert!(
        content.contains("[当前状态]"),
        "system 应含自动注入的状态块:\n{content}"
    );
    assert!(
        content.contains("好感度"),
        "状态 JSON 应含 InitVar 初始化的值:\n{content}"
    );
    assert!(
        content.contains("[状态更新协议]"),
        "应含更新协议说明:\n{content}"
    );
}

/// custom 多步流程:中间步骤的 <UpdateVariable> 补丁在步骤完成时立即应用。
/// 断言:第一个 vars 事件出现在步骤 2 执行之前(旧实现只在收尾解析,中间步骤变量会丢),
/// 且最终消息 extra.mvu 快照落库正确。
#[tokio::test]
async fn custom_flow_applies_mvu_updates_per_step() {
    let _guard = test_lock().await;
    let app = test_app();
    // 两步 direct 生成流程
    let flow = json!({
        "enabled": true,
        "steps": [
            {
                "id": "s1", "name": "第一步", "goal": "生成变量",
                "action": "direct", "generates": true, "enabled": true,
                "system_prompt": "", "temperature": 0.7, "max_tokens": 1024, "tools": null
            },
            {
                "id": "s2", "name": "第二步", "goal": "生成正文",
                "action": "direct", "generates": true, "enabled": true,
                "system_prompt": "", "temperature": 0.7, "max_tokens": 1024, "tools": null
            }
        ]
    });
    let (status, _) = send_json(app, "PUT", "/api/agent-flows", json!({ "config": flow })).await;
    assert_eq!(status, StatusCode::OK, "配置流程应 200");
    let (_, char) = upload_character(app, "自定义变量.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    let patch = r#"<UpdateVariable><JSONPatch>[{"op":"replace","path":"/hp","value":77}]</JSONPatch></UpdateVariable>"#;
    let events = sse_events(
        app,
        &sid,
        &cid,
        &format!("第一步 [[reply:{patch}]]"),
        "custom",
    )
    .await;
    // 最终消息落库快照正确
    let (_, hist) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    let msgs = hist["messages"].as_array().unwrap();
    let last = msgs.last().expect("应有 assistant 消息");
    assert_eq!(
        last["extra"]["mvu"]["stat_data"]["hp"],
        json!(77),
        "快照应落库: {hist}"
    );
    // 时序:第一个 vars 事件在步骤 2「执行中…」(index=2)之前推送
    let first_vars = events
        .iter()
        .position(|e| e["type"] == "vars")
        .expect("应有 vars 事件");
    let step2_exec = events
        .iter()
        .position(|e| e["type"] == "step" && e["step"] == "执行中…" && e["index"] == json!(2))
        .expect("应有步骤 2 执行中事件");
    assert!(
        first_vars < step2_exec,
        "中间步骤补丁应在步骤 2 执行前应用(旧实现只在收尾解析,中间步骤变量会丢): {events:?}"
    );
    // 恢复流程配置
    reset_flow(app).await;
}

/// custom 流程反思回退时,即时应用的变量补丁必须回滚到步骤基线:
/// 否则 delta 等非幂等补丁被重生成重复应用,变量值翻倍(10 → 30 而非 10)。
#[tokio::test]
async fn custom_flow_reflect_retry_rolls_back_mvu_delta() {
    let _guard = test_lock().await;
    let app = test_app();
    // 两步流程:生成(delta 补丁 + 逗号结尾文本 → 截断规则必失败)→ 反思
    let flow = json!({
        "enabled": true,
        "steps": [
            {
                "id": "s1", "name": "生成", "goal": "生成变量",
                "action": "direct", "generates": true, "enabled": true,
                "system_prompt": "", "temperature": 0.7, "max_tokens": 1024, "tools": null
            },
            {
                "id": "s2", "name": "反思", "goal": "检查质量",
                "action": "reflect", "enabled": true
            }
        ]
    });
    let (status, _) = send_json(app, "PUT", "/api/agent-flows", json!({ "config": flow })).await;
    assert_eq!(status, StatusCode::OK, "配置流程应 200");
    let (_, char) = upload_character(app, "回滚变量.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    // 草稿以逗号结尾 → 反思截断规则失败 → 回退重试;delta 补丁随每次生成即时应用
    let patch = r#"文本,<UpdateVariable><JSONPatch>[{"op":"delta","path":"/hp","value":10}]</JSONPatch></UpdateVariable>"#;
    let events = sse_events(
        app,
        &sid,
        &cid,
        &format!("开始 [[reply:{patch}]]"),
        "custom",
    )
    .await;
    // 确实发生了反思回退(截断规则失败)
    let retries = events
        .iter()
        .filter(|e| e["type"] == "step" && e["step"] == "重新生成")
        .count();
    assert!(
        retries >= 2,
        "截断草稿应触发反思重试,实际 {retries} 次: {events:?}"
    );
    // 核心:变量值只加一次(delta 不因回退重生成而翻倍;delta 补丁值为 f64)
    let (_, hist) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    let msgs = hist["messages"].as_array().unwrap();
    let last = msgs.last().expect("应有 assistant 消息");
    assert_eq!(
        last["extra"]["mvu"]["stat_data"]["hp"].as_f64(),
        Some(10.0),
        "delta 补丁经反思回退后应只应用一次(基线回滚),实际: {hist}"
    );
    // 恢复流程配置
    reset_flow(app).await;
}

/// M1 回归:AGENT 模式工具循环轮次上限由设置驱动(默认 32),不再硬编码 8 轮。
/// 9 轮工具调用 + 设置上限 10 → 全部执行,卡片均有 tool_result 终态(无 running 悬挂),
/// 且模型请求轮数与工具执行轮数一致(无 off-by-one 丢弃)。
#[tokio::test]
async fn agent_tool_loop_exceeds_eight_rounds() {
    let _guard = test_lock().await;
    let app = test_app();
    // 上限提到 10(结束时恢复默认,避免污染同进程其他测试)
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "max_tool_rounds": 10 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "设置 max_tool_rounds 应 200");
    let (_, char) = upload_character(app, "多轮工具.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    // mock 钩子:连续 9 轮返回 read 工具调用,第 10 轮返回正文
    let args = r#"{"queries":[{"type":"character_prompt"}]}"#;
    let events = sse_events(
        app,
        &sid,
        &cid,
        &format!("[[tool_loop:read|9 {args}]]"),
        "agent",
    )
    .await;
    let tool_calls = events.iter().filter(|e| e["type"] == "tool_call").count();
    let tool_results = events.iter().filter(|e| e["type"] == "tool_result").count();
    assert_eq!(
        tool_calls, 9,
        "9 轮工具调用应全部执行(旧实现最多 8 轮): {events:?}"
    );
    assert_eq!(
        tool_results, 9,
        "每轮工具调用都应有 tool_result 终态(无 running 悬挂): {events:?}"
    );
    assert!(
        events.iter().any(|e| e["type"] == "finish"),
        "应有 finish 事件"
    );
    // 无工具循环上限提示(9 < 10)
    assert!(
        !events
            .iter()
            .any(|e| e["type"] == "step" && e["step"] == "达到工具调用轮次上限"),
        "未达上限不应提示: {events:?}"
    );
    // 恢复默认
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "max_tool_rounds": 32 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "恢复 max_tool_rounds 应 200");
}

/// M1 回归:轮次上限配置生效——设置上限 3,模型持续请求工具时,每次生成尝试内恰好
/// 执行 3 轮后停止(不再发起新的模型请求)。agent 模式对空结果会反思重试(既有语义),
/// 故断言每次尝试内工具调用数 = min(mock 请求轮数 5, 上限 3) = 3,共 3 次尝试 = 9。
/// 若无上限,每次尝试将执行满 5 轮;若 off-by-one,上限轮会被丢弃(无 tool_result)。
#[tokio::test]
async fn agent_tool_loop_respects_configured_limit() {
    let _guard = test_lock().await;
    let app = test_app();
    let (status, _) = send_json(app, "PUT", "/api/settings", json!({ "max_tool_rounds": 3 })).await;
    assert_eq!(status, StatusCode::OK, "设置 max_tool_rounds 应 200");
    let (_, char) = upload_character(app, "轮次上限.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    // mock 钩子:模型会持续请求 5 轮,但引擎应在上限 3 轮后停止
    let args = r#"{"queries":[{"type":"character_prompt"}]}"#;
    let events = sse_events(
        app,
        &sid,
        &cid,
        &format!("[[tool_loop:read|5 {args}]]"),
        "agent",
    )
    .await;
    let tool_calls = events.iter().filter(|e| e["type"] == "tool_call").count();
    let tool_results = events.iter().filter(|e| e["type"] == "tool_result").count();
    // 每次尝试内恰 3 轮:上限提示出现 3 次(每次尝试一次),即每次尝试被限制在 3 轮
    let limit_hits = events
        .iter()
        .filter(|e| e["type"] == "step" && e["step"] == "达到工具调用轮次上限")
        .count();
    assert_eq!(limit_hits, 3, "3 次尝试各应触发一次上限提示: {events:?}");
    // 每次尝试 3 轮 × 3 次尝试 = 9;无上限时每次尝试会执行 5 轮
    assert_eq!(
        tool_calls, 9,
        "每次尝试工具轮数应受配置上限 3 限制(否则为 5 轮/次): {events:?}"
    );
    assert_eq!(
        tool_results, 9,
        "已执行轮次应全部有 tool_result 终态(无 running 悬挂): {events:?}"
    );
    assert!(
        events.iter().any(|e| e["type"] == "finish"),
        "应有 finish 事件"
    );
    // 恢复默认
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "max_tool_rounds": 32 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "恢复 max_tool_rounds 应 200");
}

/// M1:max_tool_rounds 设置读写与校验——默认 32、PUT 后回读一致、非法值(0/201)被拒绝。
#[tokio::test]
async fn settings_max_tool_rounds_roundtrip_and_validation() {
    let _guard = test_lock().await;
    let app = test_app();
    // 默认 32
    let (_, s) = send_json(app, "GET", "/api/settings", json!({})).await;
    assert_eq!(s["max_tool_rounds"], json!(32), "默认应为 32: {s}");
    // 合法值 5
    let (status, s) = send_json(app, "PUT", "/api/settings", json!({ "max_tool_rounds": 5 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s["settings"]["max_tool_rounds"], json!(5));
    // 非法值 0 → 拒绝并保持 5
    let (status, s) = send_json(app, "PUT", "/api/settings", json!({ "max_tool_rounds": 0 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "0 应返回 400: {s}");
    let (_, current) = send_json(app, "GET", "/api/settings", json!({})).await;
    assert_eq!(current["max_tool_rounds"], json!(5), "0 不得半提交");
    // 非法值 201(超上限 200)→ 拒绝并保持 5
    let (status, s) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "max_tool_rounds": 201 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "201 应返回 400: {s}");
    let (_, current) = send_json(app, "GET", "/api/settings", json!({})).await;
    assert_eq!(current["max_tool_rounds"], json!(5), "201 不得半提交");
    // 恢复默认
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "max_tool_rounds": 32 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "恢复应 200");
}

/// M2 回归:并行工具调用回填为 OpenAI 标准结构——一轮 2 个 tool_calls 时,
/// 下一轮消息只含 1 条 assistant(携带完整 tool_calls[2])+ 2 条 tool 结果。
/// 旧实现为每个 call 单独追加 assistant,违反标准,严格后端会 400。
#[tokio::test]
async fn parallel_tool_calls_echoed_as_single_assistant_message() {
    let app = test_app();
    let (_, char) = upload_character(app, "并行工具.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    // mock 钩子:首轮返回 2 个 role 工具调用;第 2 轮回显 LLM 消息结构
    let events = sse_events(
        app,
        &sid,
        &cid,
        r#"[[tool_echo:role {"sides":6}]]"#,
        "agent",
    )
    .await;
    // 两个调用都有结果终态
    let results = events.iter().filter(|e| e["type"] == "tool_result").count();
    assert_eq!(results, 2, "两个并行调用都应有 tool_result: {events:?}");
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("缺 finish");
    let content = finish["content"].as_str().unwrap_or("");
    // 回显中应恰好一条 assistant 消息携带 2 个 tool_calls(而非两条各 1 个)
    assert!(
        content.contains("[assistant [calls:2]]"),
        "应恰好一条 assistant 消息携带完整 tool_calls[2](旧实现为两条 [calls:1]):\n{content}"
    );
    assert!(
        !content.contains("[calls:1]"),
        "不应出现单调用 assistant 消息: {content}"
    );
    // 两条 tool 结果消息
    let tool_lines = content
        .lines()
        .filter(|l| l.starts_with("[tool]") || l.starts_with("[tool "))
        .count();
    assert_eq!(tool_lines, 2, "应回填 2 条 tool 结果消息: {content}");
}

/// M2 回归:Custom 白名单危险工具真实执行成功(白名单即授权语义)。
/// 旧实现外层 allowed=true 但 execute 内部二次权限裁决拒绝,白名单敏感/危险工具实际被拒。
#[tokio::test]
async fn custom_whitelist_dangerous_tool_executes_successfully() {
    let _guard = test_lock().await;
    let app = test_app();
    // 单步流程,步骤工具白名单 = ["write"](dangerous 工具,未在会话/角色授权)
    let flow = json!({
        "enabled": true,
        "steps": [
            {
                "id": "s1", "name": "白名单写入", "goal": "写入气泡",
                "action": "direct", "generates": true, "enabled": true,
                "system_prompt": "", "temperature": 0.7, "max_tokens": 1024,
                "tools": ["write"]
            }
        ]
    });
    let (status, _) = send_json(app, "PUT", "/api/agent-flows", json!({ "config": flow })).await;
    assert_eq!(status, StatusCode::OK, "配置流程应 200");
    let (_, char) = upload_character(app, "白名单写入.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    // mock [[tool:...]] 单轮工具调用:write 写气泡(target=bubble,无授权)
    let events = sse_events(
        app,
        &sid,
        &cid,
        r#"[[tool:write {"target":"bubble","content":"白名单放行"}]]#"#,
        "custom",
    )
    .await;
    // 白名单放行:无授权事件,直接执行成功
    assert!(
        !events
            .iter()
            .any(|e| e["type"] == "tool_authorization_required"),
        "白名单工具不应弹授权: {events:?}"
    );
    let result = events
        .iter()
        .find(|e| e["type"] == "tool_result")
        .expect("应有 tool_result");
    assert!(
        result["output"].get("error").is_none(),
        "白名单 write 应执行成功,而非二次权限裁决拒绝: {events:?}"
    );
    assert!(
        events.iter().any(|e| e["type"] == "finish"),
        "应有 finish: {events:?}"
    );
    // 恢复流程配置
    reset_flow(app).await;
}

/// M3:顶层模型失败产生 Error 终态(带 code/retryable),而非「空内容 finish 伪装正常结束」。
#[tokio::test]
async fn upstream_model_error_emits_error_terminal_event() {
    let app = test_app();
    let (_, char) = upload_character(app, "错误终态.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    // mock [[fail:...]]:模型请求返回错误(非中断)
    let events = sse_events(
        app,
        &sid,
        &cid,
        "触发错误 [[fail:上游连接超时 504]]",
        "deep",
    )
    .await;
    let err = events
        .iter()
        .find(|e| e["type"] == "error")
        .expect("模型失败应发 error 终态事件: {events:?}");
    assert_eq!(err["code"], "request_timeout", "错误码分类: {err}");
    assert_eq!(err["retryable"], json!(true), "超时应可重试: {err}");
    assert!(
        err["message"].as_str().unwrap().contains("超时"),
        "应携带错误消息: {err}"
    );
    // 不应再有空 finish 伪装成功
    let finish = events.iter().find(|e| e["type"] == "finish");
    assert!(
        finish.is_none(),
        "模型失败不应发空 finish 伪装正常结束: {events:?}"
    );
}

#[tokio::test]
async fn resource_proxy_rejects_invalid_urls() {
    let app = test_app();
    // 非 https 拒绝
    let (status, body) = send_json(
        app,
        "GET",
        "/api/resource/proxy?url=http%3A%2F%2Fexample.com%2Fpage.html",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "http 应拒绝: {body}");
    assert!(body["ok"] == json!(false));
    // javascript: 拒绝
    let (status, body) = send_json(
        app,
        "GET",
        "/api/resource/proxy?url=javascript%3Aalert(1)",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "javascript: 应拒绝: {body}");
    // 私网地址拒绝(SSRF):127.0.0.1
    let (status, body) = send_json(
        app,
        "GET",
        "/api/resource/proxy?url=https%3A%2F%2F127.0.0.1%2Fpage",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "私网应拒绝: {body}");
}

/// 多开场:角色卡 alternate_greetings 提取、建会话按 greeting_index 播种、
/// 越界回退、regreet 会话内切换、PUT 更新备用开场列表。
#[tokio::test]
async fn multi_greeting_seed_and_switch() {
    let app = test_app();
    // 上传带备用开场的角色卡(含一个空串条目,应被过滤)
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"多开场.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        json!({
            "spec": "chara_card_v2",
            "spec_version": "1.0",
            "name": "多开场角色",
            "description": "多开场测试",
            "first_mes": "主开场文本",
            "alternate_greetings": ["备用开场A", "备用开场B", "", "备用开场C"]
        })
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
    assert_eq!(char["chara_name"], json!("多开场角色"));
    let cid = char["id"].as_str().unwrap().to_string();
    assert_eq!(
        char["alternate_greetings"],
        json!(["备用开场A", "备用开场B", "备用开场C"]),
        "空串备用开场应被过滤"
    );

    // greeting_index=0 → 主开场
    let (_, s0) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid, "greeting_index": 0 }),
    )
    .await;
    let (_, h0) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={}", s0["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let m0 = h0["messages"].as_array().unwrap();
    assert_eq!(m0.len(), 1);
    assert_eq!(m0[0]["content"], json!("主开场文本"));

    // greeting_index=2 → 第三条备用开场(下标对齐 alternate_greetings,不含空串)
    let (_, s2) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid, "greeting_index": 2 }),
    )
    .await;
    let (_, h2) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={}", s2["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let m2 = h2["messages"].as_array().unwrap();
    assert_eq!(m2.len(), 1);
    assert_eq!(m2[0]["content"], json!("备用开场B"));

    // 越界 index 回退到最后一个开场
    let (_, s99) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid, "greeting_index": 99 }),
    )
    .await;
    let (_, h99) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={}", s99["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let m99 = h99["messages"].as_array().unwrap();
    assert_eq!(m99[0]["content"], json!("备用开场C"));

    // 会话内切换(regreet):先发一条用户消息,再切换为备用开场A
    let sid = s0["id"].as_str().unwrap().to_string();
    let (status, _) = send_json(
        app,
        "POST",
        "/api/chat/send",
        json!({ "session_id": sid, "character_id": cid, "message": "你好" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, regreet) = send_json(
        app,
        "POST",
        &format!("/api/chat/sessions/{sid}/regreet"),
        json!({ "greeting_index": 1 }),
    )
    .await;
    assert_eq!(regreet["ok"], json!(true));
    // 清空后重新播种:历史仅剩新开场一条
    let (_, hr) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    let mr = hr["messages"].as_array().unwrap();
    assert_eq!(mr.len(), 1, "regreet 应清空会话并按新开场重新播种: {hr}");
    assert_eq!(mr[0]["content"], json!("备用开场A"));
    assert_eq!(mr[0]["extra"]["first_mes"], json!(true));

    // PUT 更新备用开场列表(清空 = 删除字段)
    let (status, updated) = send_json(
        app,
        "PUT",
        &format!("/api/characters/{cid}"),
        json!({ "alternate_greetings": ["新备用X"] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["alternate_greetings"], json!(["新备用X"]));
    let (_, cleared) = send_json(
        app,
        "PUT",
        &format!("/api/characters/{cid}"),
        json!({ "alternate_greetings": [] }),
    )
    .await;
    assert!(cleared.get("alternate_greetings").is_none(), "空数组应清空备用开场: {cleared}");
}

#[tokio::test]
async fn slash_commands_list() {
    // 阶段四 4a:GET /api/slash/commands 返回内置命令清单(前端输入框联想)
    let app = test_app();
    let (status, json) = send_json(app, "GET", "/api/slash/commands", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let commands = json["commands"].as_array().expect("commands 应为数组");
    let names: Vec<&str> = commands
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    for expected in ["echo", "var", "setvar", "getvar", "addvar", "help"] {
        assert!(names.contains(&expected), "命令清单应包含 {expected}: {names:?}");
    }
    // 每项含 name/description/params 元信息
    for c in commands {
        assert!(c["name"].is_string());
        assert!(c["description"].is_string());
        assert!(c["params"].is_string());
    }
}

#[tokio::test]
async fn audio_crud() {
    // 阶段五 5a:GET /api/audio 返回默认双通道;PUT settings 改 mode/volume;
    // PUT playlist 校验 URL 协议白名单(非法协议 400,校验失败不落盘不改内存)
    let app = test_app();
    let _guard = test_lock().await;

    // 1) 默认状态:bgm repeat_all / ambient play_one_and_stop / volume 50
    let (status, json) = send_json(app, "GET", "/api/audio", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["audio"]["bgm"]["mode"], "repeat_all");
    assert_eq!(json["audio"]["ambient"]["mode"], "play_one_and_stop");
    assert_eq!(json["audio"]["bgm"]["volume"], 50);
    assert!(json["audio"]["bgm"]["enabled"].as_bool().unwrap());

    // 2) PUT settings:改 ambient mode + volume(部分字段合并)
    let (status, json) = send_json(
        app,
        "PUT",
        "/api/audio/settings",
        json!({ "type": "ambient", "settings": { "mode": "repeat_all", "volume": 77 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["audio"]["ambient"]["mode"], "repeat_all");
    assert_eq!(json["audio"]["ambient"]["volume"], 77);
    // bgm 不受影响
    assert_eq!(json["audio"]["bgm"]["mode"], "repeat_all");

    // 3) 非法通道 → 400
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/audio/settings",
        json!({ "type": "music", "settings": { "muted": true } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 4) PUT playlist:合法 URL 替换成功
    let (status, json) = send_json(
        app,
        "PUT",
        "/api/audio/playlist",
        json!({
            "type": "bgm",
            "tracks": [
                { "title": "开场曲", "url": "https://example.com/a.mp3" },
                { "title": "主城", "url": "http://example.com/b.ogg" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["audio"]["bgm"]["playlist"].as_array().unwrap().len(), 2);

    // 5) 非法协议 → 400,且不落盘不改内存
    let (status, body) = send_json(
        app,
        "PUT",
        "/api/audio/playlist",
        json!({
            "type": "bgm",
            "tracks": [{ "title": "本地", "url": "file:///C:/x.mp3" }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("协议"));
    let (_, after) = send_json(app, "GET", "/api/audio", json!({})).await;
    assert_eq!(after["audio"]["bgm"]["playlist"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn render_frame_document_is_served() {
    // 阶段五 5b:GET /render-frame.html 返回宿主文档(CSP 断掉网络出口,
    // frame-ancestors 'self' 允许本机页面框入)
    let app = test_app();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/render-frame.html")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "text/html; charset=utf-8");
    let csp = resp.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("default-src 'none'"), "CSP 应为 default-src 'none': {csp}");
    assert!(csp.contains("script-src 'unsafe-inline'"));
    assert!(csp.contains("connect-src 'none'"));
    assert!(csp.contains("frame-ancestors 'self'"));
    assert_eq!(resp.headers()["x-frame-options"], "SAMEORIGIN");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&bytes);
    assert!(html.contains("kedai-render-panel-v1"), "宿主文档应含面板 channel");
    assert!(html.contains("booted"), "宿主文档应含 boot 闩锁");
}

