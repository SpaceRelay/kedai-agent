// 阶段六 6g-2 集成测试:后端脚本经 TavernHelper.importRaw*(character/worldbook/preset/chat)
// 直接调用引擎各 service(绕过 HTTP)。脚本在「消息生成完成后」执行(mock 连接器);
// 合法导入落库可见;缺 session_id / 不支持类型抛异常,仅记日志不中断主流程。
//
// **授权门(2026-09-14,known-limitations L12)**:角色卡脚本默认 fail-closed,不授权则
// 后端根本不执行——importRaw* 正是被授权门保护的「写数据」能力。故此处每例都先走
// 「GET 取 current_hash → PUT 授权」的真实流程(见 grant_card_scripts)。
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

/// 上传空角色卡(脚本载体)
async fn upload_character(app: &axum::Router) -> String {
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"角色.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        json!({ "spec": "chara_card_v2", "spec_version": "1.0", "name": "导入宿主", "description": "测试", "first_mes": "你好" })
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

/// 走真实授权流程:GET 取后端计算的 current_hash → PUT 回传授权。
/// 角色卡脚本默认 fail-closed,不调用本函数则后端不会执行该卡任何脚本。
async fn grant_card_scripts(app: &axum::Router, cid: &str) {
    let (status, got) = send_json(
        app,
        "GET",
        &format!("/api/script-authorizations?character_id={cid}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "查询授权态应 200: {got}");
    let hash = got["current_hash"]
        .as_str()
        .expect("应返回 current_hash")
        .to_string();
    assert!(!hash.is_empty(), "有脚本时 current_hash 不应为空");
    let (status, granted) = send_json(
        app,
        "PUT",
        "/api/script-authorizations",
        json!({ "character_id": cid, "script_hash": hash }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "授权应 200: {granted}");
}

/// 保存角色脚本树(并授权),返回会话 id
async fn setup_session_with_scripts(app: &axum::Router, cid: &str, trees: Value) -> String {
    let (status, _) = send_json(
        app,
        "PUT",
        &format!("/api/scripts/tree?scope=character&character_id={cid}"),
        json!({ "trees": trees }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "保存脚本树应成功");
    // 保存后即授权:哈希绑定刚写入的脚本内容
    grant_card_scripts(app, cid).await;
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    session["id"].as_str().unwrap().to_string()
}

/// 发送 chat 并断言主流程完成(脚本异常仅记日志,不应中断)
async fn send_chat_ok(app: &axum::Router, sid: &str, cid: &str) {
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
        "应收到 finish 事件(脚本执行在 finish 之前): {text}"
    );
}

async fn history(app: &axum::Router, sid: &str) -> Vec<Value> {
    let (_, got) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    got["messages"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn script_imports_character_and_chat() {
    let _guard = test_lock().await;
    let app = test_app();
    let cid = upload_character(app).await;
    // 脚本树内嵌会话 id,须先建会话再保存脚本
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    let (status, _) = send_json(
        app,
        "PUT",
        &format!("/api/scripts/tree?scope=character&character_id={cid}"),
        json!({ "trees": [
            { "type": "script", "enabled": true, "id": "imp-1", "name": "导入角色",
              "content": "TavernHelper.importRawCharacter('导入卡.json', JSON.stringify({ spec: 'chara_card_v2', spec_version: '1.0', name: '脚本导入卡', description: '导入测试', first_mes: '嗨' }));" },
            { "type": "script", "enabled": true, "id": "imp-2", "name": "导入会话",
              "content": format!("TavernHelper.importRawChat(JSON.stringify([{{ role: 'user', content: '脚本导入消息' }}]), '{}');", sid) }
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "保存脚本树应成功");
    // 授权门:不授权则后端不执行这些 importRaw* 脚本(它们正是被保护的能力)
    grant_card_scripts(app, &cid).await;

    send_chat_ok(app, &sid, &cid).await;

    // 角色导入生效(出现在角色列表)
    let (_, chars) = send_json(app, "GET", "/api/characters", json!({})).await;
    let names: Vec<String> = chars["characters"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["chara_name"].as_str().map(|s| s.to_string()))
        .collect();
    assert!(
        names.iter().any(|n| n.contains("脚本导入卡")),
        "importRawCharacter 应导入角色,实际 {names:?}"
    );

    // 会话导入生效:import_chat 是「清空后替换」语义(与 /api/import/chat 一致),
    // 首轮生成的 user+assistant 被替换为导入的 1 条消息
    let msgs = history(app, &sid).await;
    assert_eq!(msgs.len(), 1, "importRawChat 应替换会话消息,实际 {msgs:?}");
    assert_eq!(msgs[0]["content"], json!("脚本导入消息"));
}

#[tokio::test]
async fn script_imports_preset_and_worldbook() {
    let _guard = test_lock().await;
    let app = test_app();
    let cid = upload_character(app).await;
    let sid = setup_session_with_scripts(
        app,
        &cid,
        json!([
            { "type": "script", "enabled": true, "id": "imp-3", "name": "导入预设",
              "content": "TavernHelper.importRawPreset(JSON.stringify({ prompts: [{ identifier: 'p1', content: '楼层一', system_prompt: true }], prompt_order: [{ order: [{ identifier: 'p1' }] }] }));" },
            { "type": "script", "enabled": true, "id": "imp-4", "name": "导入世界书",
              "content": "TavernHelper.importRawWorldbook('书.json', JSON.stringify({ entries: [{ uid: 0, comment: '条目', keys: ['k'], content: '内容一' }] }));" }
        ]),
    )
    .await;

    send_chat_ok(app, &sid, &cid).await;

    // 预设 → 楼层系统
    let (_, pi) = send_json(app, "GET", "/api/prompt-inject", json!({})).await;
    let floors = pi["config"]["floors"].as_array().expect("应存在 floors");
    assert!(
        floors.iter().any(|f| f["content"] == json!("楼层一")),
        "importRawPreset 应导入楼层,实际 {floors:?}"
    );

    // 世界书 → 列表
    let (_, wb) = send_json(app, "GET", "/api/world-books", json!({})).await;
    let books = wb["world_books"].as_array().unwrap();
    assert!(!books.is_empty(), "importRawWorldbook 应导入世界书: {wb}");
    assert!(
        books
            .iter()
            .any(|b| b["name"].as_str().is_some_and(|n| n.contains("书"))),
        "世界书名称应从文件名解析: {wb}"
    );
}

#[tokio::test]
async fn script_import_errors_do_not_break_flow() {
    let _guard = test_lock().await;
    let app = test_app();
    let cid = upload_character(app).await;
    let sid = setup_session_with_scripts(
        app,
        &cid,
        json!([
            { "type": "script", "enabled": true, "id": "imp-5", "name": "缺会话",
              "content": "TavernHelper.importRawChat(JSON.stringify([{ role: 'user', content: 'x' }]), '');" },
            { "type": "script", "enabled": true, "id": "imp-6", "name": "regex不支持",
              "content": "TavernHelper.importRawTavernRegex('x');" }
        ]),
    )
    .await;

    // 脚本抛异常仅记日志,主流程正常完成
    send_chat_ok(app, &sid, &cid).await;

    // 错误导入不写入消息;新会话含 first_mes 开场 + user + assistant 共 3 条
    let msgs = history(app, &sid).await;
    assert_eq!(msgs.len(), 3, "错误导入不应写入消息,实际 {msgs:?}");
    assert!(
        msgs.iter().all(|m| m["content"] != json!("x")),
        "导入失败不应留下任何消息,实际 {msgs:?}"
    );
}
