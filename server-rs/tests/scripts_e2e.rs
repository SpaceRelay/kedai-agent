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

    // **授权门(2026-09-14)**:角色卡脚本默认不执行,须先授权。
    // 哈希由后端唯一计算(GET 返回 current_hash),前端原样回传(PUT)。
    grant_card_scripts(app, &cid).await;

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

/// 走「GET 取 current_hash → PUT 回传」的完整授权流程(哈希唯一来源是后端)。
async fn grant_card_scripts(app: &axum::Router, cid: &str) {
    let (status, got) = send_json(
        app,
        "GET",
        &format!("/api/script-authorizations?character_id={cid}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "查询授权态应 200: {got}");
    assert_eq!(
        got["authorized"],
        json!(false),
        "初始应为未授权(fail-closed)"
    );
    let hash = got["current_hash"].as_str().expect("应返回 current_hash");
    assert_eq!(hash.len(), 64, "SHA-256 hex 应为 64 字符");

    let (status, granted) = send_json(
        app,
        "PUT",
        "/api/script-authorizations",
        json!({ "character_id": cid, "script_hash": hash }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "授权应 200: {granted}");

    // 授权后状态应为已授权
    let (_, after) = send_json(
        app,
        "GET",
        &format!("/api/script-authorizations?character_id={cid}"),
        json!({}),
    )
    .await;
    assert_eq!(after["authorized"], json!(true), "授权后应为已授权");
}

/// 保存一份「写 global 变量」的角色脚本(供门禁测试复用)
async fn put_marker_script(app: &axum::Router, cid: &str, marker: &str) {
    let content =
        format!("TavernHelper.setVariables({{ marker: '{marker}' }}, {{ type: 'global' }});");
    let (status, _) = send_json(
        app,
        "PUT",
        &format!("/api/scripts/tree?scope=character&character_id={cid}"),
        json!({ "trees": [
            { "type": "script", "enabled": true, "id": "gate-1", "name": "标记", "content": content }
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// **fail-closed 核心断言**:未授权时角色卡脚本**不得执行**(写不进去)。
#[tokio::test]
async fn unauthorized_card_script_does_not_run() {
    let _guard = test_lock().await;
    let app = test_app();
    let cid = upload_character(app).await;
    put_marker_script(app, &cid, "不该出现").await;

    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    send_chat(app, &sid, &cid).await;

    let (_, got) = send_json(
        app,
        "GET",
        &format!("/api/variables?session_id={sid}&scope=global"),
        json!({}),
    )
    .await;
    assert_ne!(
        got["data"]["marker"],
        json!("不该出现"),
        "未授权的角色卡脚本**不得**执行,实际写出了变量: {got}"
    );
}

/// 授权 → 执行;撤销 → 再次不执行(门禁双向可验证)。
#[tokio::test]
async fn authorization_gate_grants_then_revokes() {
    let _guard = test_lock().await;
    let app = test_app();
    let cid = upload_character(app).await;
    put_marker_script(app, &cid, "已授权").await;
    grant_card_scripts(app, &cid).await;

    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    send_chat(app, &sid, &cid).await;

    let (_, got) = send_json(
        app,
        "GET",
        &format!("/api/variables?session_id={sid}&scope=global"),
        json!({}),
    )
    .await;
    assert_eq!(
        got["data"]["marker"],
        json!("已授权"),
        "授权后脚本应执行: {got}"
    );

    // 撤销授权 → 换一份脚本(旧授权本就失效),再次断言不执行
    let (status, _) = send_json(
        app,
        "DELETE",
        &format!("/api/script-authorizations?character_id={cid}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "撤销应 200");
    put_marker_script(app, &cid, "撤销后不该出现").await;

    let (_, session2) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid2 = session2["id"].as_str().unwrap().to_string();
    send_chat(app, &sid2, &cid).await;
    let (_, got2) = send_json(
        app,
        "GET",
        &format!("/api/variables?session_id={sid2}&scope=global"),
        json!({}),
    )
    .await;
    assert_ne!(
        got2["data"]["marker"],
        json!("撤销后不该出现"),
        "撤销后脚本不得执行: {got2}"
    );
}

/// 哈希不匹配(卡在读取与授权之间被改)→ 409,防陈旧哈希锁错版本。
#[tokio::test]
async fn grant_with_stale_hash_is_rejected() {
    let _guard = test_lock().await;
    let app = test_app();
    let cid = upload_character(app).await;
    put_marker_script(app, &cid, "当前").await;

    // 拿真实哈希,然后改成另一份脚本,再拿旧哈希去授权 → 应冲突
    let (_, got) = send_json(
        app,
        "GET",
        &format!("/api/script-authorizations?character_id={cid}"),
        json!({}),
    )
    .await;
    let stale = got["current_hash"].as_str().unwrap().to_string();
    put_marker_script(app, &cid, "已变更").await;

    let (status, resp) = send_json(
        app,
        "PUT",
        "/api/script-authorizations",
        json!({ "character_id": cid, "script_hash": stale }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "陈旧哈希应 409,实际: {resp}");
    assert!(
        resp["current_hash"].is_string(),
        "应回带新的 current_hash 供前端重试"
    );
}
