// 角色扮演 Agent 记录只读恢复(实跑问题 4)集成测试:
// GET /api/chat/sessions/{id}/agent/trace 返回该会话 agent_sessions + tool_calls。
// 数据播种走真实 HTTP(上传角色 + 建会话)后直连测试库写 agent_sessions/tool_calls
// (本次新增的只是只读路由,写路径由引擎既有逻辑负责,这里直接造行验证读取契约)。
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kedai_server::build_test_app;
use serde_json::{json, Value};
use std::sync::OnceLock;
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

/// 测试库路径:与 build_test_app 同款(%TEMP%/kedai-test-{pid}/kedai.db)
fn test_db_path() -> std::path::PathBuf {
    let mut data_dir = std::env::temp_dir();
    data_dir.push(format!("kedai-test-{}", std::process::id()));
    data_dir.push("kedai.db");
    data_dir
}

/// 上传角色 + 建会话,返回 session_id(播种 agent_sessions 所需的外键行)
async fn make_session(app: &axum::Router) -> String {
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"trace.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        json!({
            "spec": "chara_card_v2",
            "spec_version": "1.0",
            "name": "轨迹测试角色",
            "description": "描述",
            "first_mes": "你好",
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
    let up: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    // 上传接口直接返回角色记录(非 {character:...} 包裹)
    let cid = up["id"]
        .as_str()
        .unwrap_or_else(|| panic!("上传角色应返回 id: {up}"))
        .to_string();
    let (status, json) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "建会话应 201: {json}");
    json["id"].as_str().unwrap().to_string()
}

/// 无 Agent 记录时返回 trace:null(前端保持空闲态;路由存在且契约正确)
#[tokio::test]
async fn agent_trace_null_when_no_record() {
    let app = test_app();
    let sid = make_session(app).await;
    let (status, json) = send_json(
        app,
        "GET",
        &format!("/api/chat/sessions/{sid}/agent/trace"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["trace"].is_null(), "无记录应为 null: {json}");
}

/// 有 Agent 记录时返回 state/plan/step_index/tool_calls(恢复右侧面板数据源)
#[tokio::test]
async fn agent_trace_returns_tool_calls() {
    let app = test_app();
    let sid = make_session(app).await;

    // 直连测试库播种 agent_sessions + 两条 tool_calls(与引擎写路径同表同列)
    let agent_id = {
        let conn = rusqlite::Connection::open(test_db_path()).expect("打开测试库失败");
        let agent_id = "agent-trace-test";
        conn.execute(
            "INSERT INTO agent_sessions (id, session_id, state, plan, steps, step_index, agent_mode, started_at, updated_at) \
             VALUES (?1, ?2, 'executing', '[\"步骤A\"]', '[]', 0, 'tool', 't', 't')",
            rusqlite::params![agent_id, sid],
        )
        .expect("播种 agent_sessions 失败");
        for (i, name) in ["read", "calculator"].iter().enumerate() {
            conn.execute(
                "INSERT INTO tool_calls (id, agent_session_id, name, input, output, duration_ms, created_at) \
                 VALUES (?1, ?2, ?3, '{\"x\":1}', '\"ok\"', ?4, ?5)",
                rusqlite::params![
                    format!("tc-{i}"),
                    agent_id,
                    name,
                    (i as i64 + 1) * 10,
                    format!("2026-09-03T00:00:0{i}.000Z")
                ],
            )
            .expect("播种 tool_calls 失败");
        }
        agent_id.to_string()
    };

    let (status, json) = send_json(
        app,
        "GET",
        &format!("/api/chat/sessions/{sid}/agent/trace"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "trace 应 200: {json}");
    let trace = &json["trace"];
    assert_eq!(trace["state"], "executing");
    assert_eq!(trace["plan"][0], "步骤A");
    assert_eq!(trace["step_index"], 0);
    let calls = trace["tool_calls"].as_array().expect("应含 tool_calls");
    assert_eq!(calls.len(), 2, "应返回两条工具调用: {calls:?}");
    // created_at 升序:read 在前
    assert_eq!(calls[0]["name"], "read");
    assert_eq!(calls[1]["name"], "calculator");
    assert_eq!(calls[0]["output"], json!("ok"));
    assert_eq!(calls[0]["duration_ms"], 10);
    let _ = agent_id;
}
