// 阶段三 3b-3 端到端:角色卡后端脚本在「消息生成完成后」执行,
// 脚本经 TavernHelper 兼容桥写回 global 作用域 → 收尾落库 scope_variables 表 → API 可读。
// 链路:PUT /api/scripts/tree(character)→ POST /api/chat/send → GET /api/variables?scope=global
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::OnceLock;
use tokio::sync::{Mutex, MutexGuard};
use tower::ServiceExt;

fn test_app() -> &'static axum::Router {
    static APP: OnceLock<axum::Router> = OnceLock::new();
    APP.get_or_init(|| {
        std::env::set_var("CONNECTOR", "mock");
        kedai_server::build_test_app().expect("构建测试应用失败")
    })
}

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

/// 上传角色卡(空卡,mock 连接器不依赖角色内容)
async fn upload_character(app: &axum::Router) -> String {
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"角色.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        json!({ "spec": "chara_card_v2", "spec_version": "1.0", "name": "脚本娘", "description": "测试", "first_mes": "你好" })
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
    char["id"].as_str().unwrap().to_string()
}

/// 发送 chat 请求,消费 SSE 直至完成
async fn send_chat(app: &axum::Router, sid: &str, cid: &str) {
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/send")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "session_id": sid, "character_id": cid,
                "message": "你好", "agent_mode": "deep",
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "chat/send 应 200");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    assert!(
        text.contains("\"type\":\"finish\"") || text.contains("\"type\": \"finish\""),
        "应收到 finish 事件(脚本执行与变量落库发生在 finish 之前)"
    );
}

/// 角色卡后端脚本:消息生成完成后执行,写 global 作用域 → 引擎收尾落库 → API 可读
#[tokio::test]
async fn character_backend_script_runs_and_persists_scope() {
    let _guard = test_lock().await;
    let app = test_app();
    let cid = upload_character(app).await;

    // 保存角色脚本:脚本仅写 global 变量(纯逻辑,无 DOM/网络依赖)
    let (status, _) = send_json(
        app,
        "PUT",
        &format!("/api/scripts/tree?scope=character&character_id={cid}"),
        json!({ "trees": [
            {
                "type": "script", "enabled": true, "id": "e2e-1", "name": "统计回合",
                "content": "var before = TavernHelper.getVariables({ type: 'global' });\nvar n = (before.rounds || 0) + 1;\nTavernHelper.setVariables({ rounds: n, last: '执行于消息生成后' }, { type: 'global' });"
            }
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();

    // 发送两轮:脚本每轮把 global.rounds +1
    send_chat(app, &sid, &cid).await;
    send_chat(app, &sid, &cid).await;

    // 脚本写回已落库 scope_variables(global 作用域,scope_id 为空)
    let (status, got) = send_json(
        app,
        "GET",
        "/api/variables?session_id={sid}&scope=global",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let data = &got["data"];
    assert_eq!(
        data["rounds"],
        json!(2),
        "两轮 chat 后脚本应已执行两次,实际 {got}"
    );
    assert_eq!(data["last"], json!("执行于消息生成后"));
}
