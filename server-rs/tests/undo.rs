// 批次 6.1「回退快照/undo」集成测试:写工具(write/replace/create/update_variables/
// memory_write)执行前落逆操作快照,POST /api/undo/{id}/restore 逆序恢复。
// 链路:chat/send(mock [[tool:...]] 钩子)→ 工具执行(run_tool 两段式快照)→
// GET /api/chat/sessions/{id}/undo 列表 → restore 断言状态复原。
// mock 连接器不依赖角色内容;文件类断言直读 data_dir 下角色文件区。
// 全文件共享 app(临时目录按进程隔离)+ 全局锁串行(快照/settings 为进程级共享态)。
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::PathBuf;
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

/// 测试数据目录(与 lib.rs build_test_app 同规则:temp/kedai-test-{pid})。
/// build_test_app 的数据目录是**进程级共享**单例,生命周期等于测试进程,
/// 不能套 RAII 守卫(会删掉后续用例要用的库)。
fn data_dir() -> PathBuf {
    std::env::temp_dir().join(format!("kedai-test-{}", std::process::id()))
}

fn char_file(character_id: &str, rel: &str) -> PathBuf {
    data_dir()
        .join("character_files")
        .join(character_id)
        .join(rel)
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

/// 上传空角色卡(mock 连接器不依赖角色内容;name 仅作标识)
async fn upload_character(app: &axum::Router, name: &str) -> String {
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        json!({ "spec": "chara_card_v2", "spec_version": "1.0", "name": name, "description": "测试", "first_mes": "你好" })
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

async fn create_session(app: &axum::Router, cid: &str) -> String {
    let (status, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "建会话应 201: {session}");
    session["id"].as_str().unwrap().to_string()
}

/// 会话级授权危险工具(write/replace/create/update_variables/memory_write 均为 Dangerous)
async fn authorize(app: &axum::Router, sid: &str, tool: &str) {
    let (status, res) = send_json(
        app,
        "POST",
        "/api/agent/tool-permissions",
        json!({ "session_id": sid, "tool": tool, "scope": "session" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "授权 {tool} 应 200: {res}");
}

/// 发送携带 [[tool:...]] 钩子的 chat 消息并消费完整 SSE;断言正常完成
async fn send_tool_chat(app: &axum::Router, sid: &str, cid: &str, marker: &str) {
    // agent 模式:工具循环所在(fast/deep 不执行工具,deep 会把 mock ToolCall 当空输出重试)
    let body = json!({
        "session_id": sid, "character_id": cid,
        "message": marker, "agent_mode": "agent",
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/send")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    assert_eq!(status, StatusCode::OK, "chat/send 应 200: {text}");
    assert!(
        text.contains("\"type\":\"finish\"") || text.contains("\"type\": \"finish\""),
        "应收到 finish 事件: {text}"
    );
}

/// 会话快照列表(新→旧)
async fn list_undo(app: &axum::Router, sid: &str) -> Vec<Value> {
    let (status, res) = send_json(
        app,
        "GET",
        &format!("/api/chat/sessions/{sid}/undo"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "undo 列表应 200: {res}");
    res["snapshots"].as_array().cloned().unwrap_or_default()
}

async fn restore(app: &axum::Router, id: &str) -> (StatusCode, Value) {
    send_json(app, "POST", &format!("/api/undo/{id}/restore"), json!({})).await
}

/// 造会话:上传角色 + 建会话 + 授权指定工具
async fn setup(app: &axum::Router, name: &str, tools: &[&str]) -> (String, String) {
    let cid = upload_character(app, name).await;
    let sid = create_session(app, &cid).await;
    for t in tools {
        authorize(app, &sid, t).await;
    }
    (cid, sid)
}

/// ① write(file) 后 restore → 文件内容复原;旧快照(写入前文件不存在)restore → 文件删除
#[tokio::test]
async fn write_file_restore_recovers_content() {
    let _guard = test_lock().await;
    let app = test_app();
    let (cid, sid) = setup(app, "undo 写入", &["write"]).await;
    let file = char_file(&cid, "undo_notes.md");

    send_tool_chat(
        app,
        &sid,
        &cid,
        r#"[[tool:write {"target":"file","path":"undo_notes.md","content":"v1 内容"}]]"#,
    )
    .await;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "v1 内容");
    send_tool_chat(
        app,
        &sid,
        &cid,
        r#"[[tool:write {"target":"file","path":"undo_notes.md","content":"v2 内容"}]]"#,
    )
    .await;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "v2 内容");

    let snaps = list_undo(app, &sid).await;
    assert_eq!(snaps.len(), 2, "两次 write 应产生 2 条快照: {snaps:?}");
    assert_eq!(snaps[0]["tool_name"], json!("write"));
    assert!(
        snaps[0]["label"]
            .as_str()
            .unwrap()
            .contains("undo_notes.md"),
        "label 应为中文可读简述: {}",
        snaps[0]["label"]
    );
    // anchor = 快照时该会话 max(messages.id)(已有多条消息,应为正 id)
    assert!(snaps[0]["anchor_message_id"].as_i64().unwrap() > 0);

    // 回退 v2 写入 → 内容复原为 v1
    let (status, res) = restore(app, snaps[0]["id"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK, "restore 应 200: {res}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "v1 内容",
        "restore 后应复原为 v1"
    );

    // 再回退 v1 写入(写入前文件不存在,逆操作 = 删除)
    let (status, res) = restore(app, snaps[1]["id"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK, "restore 应 200: {res}");
    assert!(!file.exists(), "写入前文件不存在,restore 后应删除");
}

/// ② create 新文件后 restore → 文件被删
#[tokio::test]
async fn create_file_restore_deletes_file() {
    let _guard = test_lock().await;
    let app = test_app();
    let (cid, sid) = setup(app, "undo 新建", &["create"]).await;
    let file = char_file(&cid, "undo_new.txt");

    send_tool_chat(
        app,
        &sid,
        &cid,
        r#"[[tool:create {"items":[{"type":"file","path":"undo_new.txt","content":"新建内容"}]}]]"#,
    )
    .await;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "新建内容");

    let snaps = list_undo(app, &sid).await;
    assert_eq!(snaps.len(), 1, "create 应产生 1 条快照: {snaps:?}");
    assert_eq!(snaps[0]["tool_name"], json!("create"));

    let (status, res) = restore(app, snaps[0]["id"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK, "restore 应 200: {res}");
    assert!(!file.exists(), "create 的逆操作应删除新建文件");
}

/// ③ update_variables 后 restore → 变量树复原
#[tokio::test]
async fn update_variables_restore_recovers_tree() {
    let _guard = test_lock().await;
    let app = test_app();
    let (cid, sid) = setup(app, "undo 变量", &["update_variables"]).await;

    // 预置变量树(走 PUT 整树覆写)
    let (status, res) = send_json(
        app,
        "PUT",
        "/api/variables",
        json!({ "session_id": sid, "scope": "chat", "data": { "score": 5 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "PUT variables 应 200: {res}");

    send_tool_chat(
        app,
        &sid,
        &cid,
        r#"[[tool:update_variables {"patches":[{"op":"replace","path":"/score","value":99}]}]]"#,
    )
    .await;
    let (_, vars) = send_json(
        app,
        "GET",
        &format!("/api/variables?session_id={sid}"),
        json!({}),
    )
    .await;
    assert_eq!(vars["data"]["score"], json!(99), "工具应已把 score 改为 99");

    let snaps = list_undo(app, &sid).await;
    assert_eq!(
        snaps.len(),
        1,
        "update_variables 应产生 1 条快照: {snaps:?}"
    );
    assert_eq!(snaps[0]["tool_name"], json!("update_variables"));
    assert_eq!(snaps[0]["label"], json!("更新变量树"));

    let (status, res) = restore(app, snaps[0]["id"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK, "restore 应 200: {res}");
    let (_, vars) = send_json(
        app,
        "GET",
        &format!("/api/variables?session_id={sid}"),
        json!({}),
    )
    .await;
    assert_eq!(vars["data"]["score"], json!(5), "restore 后变量树应复原");
}

/// ④ undo_enabled=false(PUT settings)→ 不再产生新快照
#[tokio::test]
async fn undo_disabled_produces_no_snapshot() {
    let _guard = test_lock().await;
    let app = test_app();
    let (cid, sid) = setup(app, "undo 开关", &["write"]).await;

    let (status, res) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "undo_enabled": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "PUT settings 应 200: {res}");

    send_tool_chat(
        app,
        &sid,
        &cid,
        r#"[[tool:write {"target":"file","path":"undo_off.md","content":"x"}]]"#,
    )
    .await;
    assert!(
        char_file(&cid, "undo_off.md").exists(),
        "开关只影响快照,不影响工具正常执行"
    );
    let snaps = list_undo(app, &sid).await;
    assert!(
        snaps.is_empty(),
        "undo_enabled=false 时不应产生快照: {snaps:?}"
    );

    // 恢复开关(共享 app,避免污染本 target 其它用例)
    let (status, res) =
        send_json(app, "PUT", "/api/settings", json!({ "undo_enabled": true })).await;
    assert_eq!(status, StatusCode::OK, "恢复 undo_enabled 应 200: {res}");
}

/// ⑤ restore 旧快照 → 该快照及同会话更新的快照一并清除;已清除的快照再 restore → 404
#[tokio::test]
async fn restore_clears_newer_snapshots() {
    let _guard = test_lock().await;
    let app = test_app();
    let (cid, sid) = setup(app, "undo 清理", &["write"]).await;

    send_tool_chat(
        app,
        &sid,
        &cid,
        r#"[[tool:write {"target":"file","path":"undo_p1.md","content":"一"}]]"#,
    )
    .await;
    send_tool_chat(
        app,
        &sid,
        &cid,
        r#"[[tool:write {"target":"file","path":"undo_p2.md","content":"二"}]]"#,
    )
    .await;
    let snaps = list_undo(app, &sid).await;
    assert_eq!(snaps.len(), 2, "两次 write 应产生 2 条快照: {snaps:?}");
    let newer = snaps[0]["id"].as_str().unwrap().to_string();
    let older = snaps[1]["id"].as_str().unwrap().to_string();

    // 回退较旧的快照:它自身与更新的快照都失效清除
    let (status, res) = restore(app, &older).await;
    assert_eq!(status, StatusCode::OK, "restore 应 200: {res}");
    let snaps = list_undo(app, &sid).await;
    assert!(
        snaps.is_empty(),
        "回退旧快照后同会话更新快照应一并清除: {snaps:?}"
    );

    let (status, _) = restore(app, &newer).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "已清除的快照再 restore 应 404"
    );
    let (status, _) = restore(app, &older).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "已回退的快照再 restore 应 404"
    );
}

/// ⑥ 非写工具(calculator,Safe 无需授权)不产生快照
#[tokio::test]
async fn readonly_tool_produces_no_snapshot() {
    let _guard = test_lock().await;
    let app = test_app();
    let (cid, sid) = setup(app, "undo 只读", &[]).await;

    send_tool_chat(
        app,
        &sid,
        &cid,
        r#"[[tool:calculator {"expression":"1+2"}]]"#,
    )
    .await;
    let snaps = list_undo(app, &sid).await;
    assert!(snaps.is_empty(), "calculator 不应产生快照: {snaps:?}");
}
