// 阶段六 6f 集成测试:重生成(regenerate)原地更新原 assistant 消息(swipes 追加)+
// swipe 版本切换端点。链路:POST /api/chat/send(regenerate_assistant_id)→ GET /history
// 断言消息行不新增、id 不变、extra.swipes 追加;POST /api/chat/messages/{id}/swipe
// 断言切换/越界/单版本拒绝。mock 输出确定性:先编辑原消息制造差异,保证 swipes 追加。
// 注意:新会话含 first_mes 开场白(assistant),首轮生成后为 开场+user+assistant 共 3 条。
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

/// 上传空角色卡(mock 连接器不依赖角色内容)
async fn upload_character(app: &axum::Router) -> String {
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"角色.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        json!({ "spec": "chara_card_v2", "spec_version": "1.0", "name": "swipe 测试", "description": "测试", "first_mes": "你好" })
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

/// 发送 chat 请求并消费完整 SSE;extra_fields 可附加 regenerate_assistant_id 等。
async fn send_chat(
    app: &axum::Router,
    sid: &str,
    cid: &str,
    extra_fields: Value,
) -> (StatusCode, String) {
    let mut body = json!({
        "session_id": sid, "character_id": cid,
        "message": "你好", "agent_mode": "deep",
    });
    if let (Some(obj), Some(fields)) = (body.as_object_mut(), extra_fields.as_object()) {
        for (k, v) in fields {
            obj.insert(k.clone(), v.clone());
        }
    }
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/send")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// 发送 chat 并断言成功完成
async fn send_chat_ok(app: &axum::Router, sid: &str, cid: &str, extra_fields: Value) {
    let (status, text) = send_chat(app, sid, cid, extra_fields).await;
    assert_eq!(status, StatusCode::OK, "chat/send 应 200: {text}");
    assert!(
        text.contains("\"type\":\"finish\"") || text.contains("\"type\": \"finish\""),
        "应收到 finish 事件: {text}"
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

/// 最后一条 assistant 消息(regenerate 锚点语义:必须是最后一条且 role=assistant)
fn last_assistant(msgs: &[Value]) -> Value {
    msgs.last().unwrap().clone()
}

async fn swipe(app: &axum::Router, sid: &str, id: i64, swipe_id: usize) -> (StatusCode, Value) {
    send_json(
        app,
        "POST",
        &format!("/api/chat/messages/{id}/swipe?session_id={sid}"),
        json!({ "swipe_id": swipe_id }),
    )
    .await
}

#[tokio::test]
async fn regenerate_updates_in_place_and_swipe_roundtrip() {
    let _guard = test_lock().await;
    let app = test_app();
    let cid = upload_character(app).await;
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();

    // 首轮生成:开场 + user + assistant;assistant 无 swipes
    send_chat_ok(app, &sid, &cid, json!({})).await;
    let msgs = history(app, &sid).await;
    assert_eq!(msgs.len(), 3, "应为 开场+user+assistant 共 3 条: {msgs:?}");
    let last = last_assistant(&msgs);
    let aid = last["id"].as_i64().unwrap();
    assert_eq!(last["role"], json!("assistant"));
    assert!(
        last["extra"].get("swipes").is_none(),
        "首轮消息不应有 swipes"
    );

    // 单版本消息不可切换
    let (status, res) = swipe(app, &sid, aid, 0).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "单版本应拒绝切换: {res}");

    // 编辑原消息制造与 mock 生成不同的旧内容,保证 regenerate 后 swipes 追加
    let (status, _) = send_json(
        app,
        "PUT",
        &format!("/api/chat/messages/{aid}?session_id={sid}"),
        json!({ "content": "旧内容一" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // regenerate:原地更新原行,不新增消息
    send_chat_ok(app, &sid, &cid, json!({ "regenerate_assistant_id": aid })).await;
    let msgs = history(app, &sid).await;
    assert_eq!(msgs.len(), 3, "regenerate 不应新增消息行: {msgs:?}");
    let m = &msgs[2];
    assert_eq!(m["id"], json!(aid), "原消息 id 应保持不变");
    let new_content = m["content"].as_str().unwrap().to_string();
    assert_ne!(new_content, "旧内容一");
    assert!(
        new_content.contains("（模拟回复）"),
        "新内容应为 mock 回复: {new_content}"
    );
    let swipes = m["extra"]["swipes"]
        .as_array()
        .expect("regenerate 后应有 swipes");
    assert_eq!(swipes.len(), 2, "swipes 应追加为 2 条: {swipes:?}");
    assert_eq!(swipes[0]["content"], json!("旧内容一"));
    assert_eq!(swipes[1]["content"], json!(new_content));
    assert_eq!(m["extra"]["swipe_id"], json!(1), "激活版本应为新版本");

    // 切换到版本 0(旧内容)
    let (status, res) = swipe(app, &sid, aid, 0).await;
    assert_eq!(status, StatusCode::OK, "切换应成功: {res}");
    assert_eq!(res["content"], json!("旧内容一"));
    assert_eq!(res["swipe_id"], json!(0));
    assert_eq!(res["swipes_count"], json!(2));
    let msgs = history(app, &sid).await;
    assert_eq!(
        msgs[2]["content"],
        json!("旧内容一"),
        "history 应反映切换后的激活版本"
    );

    // 切回版本 1
    let (status, res) = swipe(app, &sid, aid, 1).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["content"], json!(new_content));

    // 越界
    let (status, res) = swipe(app, &sid, aid, 9).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "越界应 400: {res}");

    // 再次 regenerate:mock 输出与最后版本相同 → 不重复追加(去重生效)
    send_chat_ok(app, &sid, &cid, json!({ "regenerate_assistant_id": aid })).await;
    let msgs = history(app, &sid).await;
    let swipes = msgs[2]["extra"]["swipes"].as_array().unwrap();
    assert_eq!(swipes.len(), 2, "与最后版本相同不应重复追加: {swipes:?}");
    assert_eq!(msgs.len(), 3, "重复 regenerate 仍不新增消息行");
}

#[tokio::test]
async fn regenerate_anchor_validation() {
    let _guard = test_lock().await;
    let app = test_app();
    let cid = upload_character(app).await;
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    send_chat_ok(app, &sid, &cid, json!({})).await;
    let msgs = history(app, &sid).await;
    let uid = msgs[1]["id"].as_i64().unwrap(); // user 消息
    let aid = msgs[2]["id"].as_i64().unwrap(); // 最后一条 assistant

    // 不存在的锚点 → 409
    let (status, _) = send_chat(
        app,
        &sid,
        &cid,
        json!({ "regenerate_assistant_id": 999999 }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "不存在的锚点应 409");

    // user 消息作锚点(非最后一条 assistant)→ 409
    let (status, _) = send_chat(app, &sid, &cid, json!({ "regenerate_assistant_id": uid })).await;
    assert_eq!(status, StatusCode::CONFLICT, "user 消息锚点应 409");

    // 与 resend 锚点互斥 → 400
    let (status, _) = send_chat(
        app,
        &sid,
        &cid,
        json!({ "regenerate_assistant_id": aid, "resend_message_id": uid }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "双锚点应 400");
}
