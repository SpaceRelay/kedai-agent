// 提示词注入集成测试:PUT/GET /api/prompt-inject 往返 + mock [[floors]] 钩子回显断言。
// 提示词注入配置为全局共享(所有会话生效),测试间用 test_lock 串行化(仿 world_books.rs)。
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
    // 测试进程级:运行时主提示词目录指向空目录,避免真实项目根 AGENTS_RUNTIME.md 混入断言。
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let dir = std::env::temp_dir().join("kedai-test-runtime-prompt-empty");
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("KEDAI_RUNTIME_PROMPT_DIR", &dir);
    });
    APP.get_or_init(|| build_test_app().expect("构建测试应用失败"))
}

/// 串行化全局共享状态(注入配置)的测试
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

async fn upload_character(app: &axum::Router, name: &str) -> (StatusCode, Value) {
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

/// 上传角色 + 建会话,返回 (sid, cid)
async fn new_session(app: &axum::Router) -> (String, String) {
    let (_, char) = upload_character(app, "注入测试.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    (sid, cid)
}

/// 发送消息,解析 SSE 事件列表
async fn sse_events(app: &axum::Router, sid: &str, cid: &str, message: &str) -> Vec<Value> {
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/send")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "session_id": sid, "character_id": cid, "message": message }).to_string(),
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

/// 取 finish 事件的 content(mock [[floors]] 回显的完整消息序列)
async fn floors_reply(app: &axum::Router, sid: &str, cid: &str) -> String {
    let events = sse_events(app, sid, cid, "查看注入 [[floors]]").await;
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("缺 finish 事件");
    finish["content"].as_str().unwrap_or("").to_string()
}

fn simple_cfg(word_count: u32, perspective: &str) -> Value {
    json!({
        "mode": "simple",
        "simple": {
            "word_count_enabled": true,
            "word_count": word_count,
            "paraphrase_enabled": true,
            "dialogue_enabled": true,
            "perspective_enabled": true,
            "perspective": perspective
        },
        "floors": []
    })
}

#[tokio::test]
async fn prompt_inject_crud_roundtrip() {
    let _guard = test_lock().await;
    let app = test_app();

    // 注入配置为全局共享,先恢复默认(前序测试可能已改)→ 断言默认 simple 可读
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({
            "config": {
                "mode": "simple",
                "simple": { "word_count_enabled": false, "word_count": 200, "paraphrase_enabled": false, "dialogue_enabled": false, "perspective_enabled": false, "perspective": "第三人称" },
                "floors": []
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, def) = send_json(app, "GET", "/api/prompt-inject", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(def["config"]["mode"], "simple");

    // PUT simple 配置 → GET 回读一致
    let cfg = simple_cfg(150, "第一人称");
    let (status, _saved) =
        send_json(app, "PUT", "/api/prompt-inject", json!({ "config": cfg })).await;
    assert_eq!(status, StatusCode::OK);
    let (_, got) = send_json(app, "GET", "/api/prompt-inject", json!({})).await;
    assert_eq!(got["config"]["mode"], "simple");
    assert_eq!(got["config"]["simple"]["word_count"], 150);
    assert_eq!(got["config"]["simple"]["perspective"], "第一人称");

    // PUT complex + 楼层 → GET 回读楼层字段完整
    let (status, saved) = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({
            "config": {
                "mode": "complex",
                "simple": { "word_count_enabled": false },
                "floors": [
                    {
                        "id": "f1", "name": "开场", "content": "内容A",
                        "role": "user", "position": "before",
                        "depth": 0, "enabled": true, "order": 0
                    },
                    {
                        "id": "f2", "name": "深度", "content": "内容B",
                        "role": "assistant", "position": "depth",
                        "depth": 2, "enabled": true, "order": 1
                    }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, got) = send_json(app, "GET", "/api/prompt-inject", json!({})).await;
    assert_eq!(got["config"]["mode"], "complex");
    assert_eq!(got["config"]["floors"].as_array().unwrap().len(), 2);
    assert_eq!(got["config"]["floors"][0]["id"], "f1");
    assert_eq!(got["config"]["floors"][0]["position"], "before");
    assert_eq!(got["config"]["floors"][1]["depth"], 2);
    assert_eq!(got["config"]["floors"][1]["role"], "assistant");
    let _ = saved;
}

#[tokio::test]
async fn simple_mode_injects_all_four_items_into_system() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, _) = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({ "config": simple_cfg(120, "第二人称") }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (sid, cid) = new_session(app).await;
    let reply = floors_reply(app, &sid, &cid).await;
    // mock 回显 `[system] ...`;四项注入文本按启用项全部拼入系统提示词
    assert!(
        reply.contains("请输出篇幅约为 120 字"),
        "缺字数注入:\n{reply}"
    );
    assert!(
        reply.contains("请以转述的方式复述内容"),
        "缺转述注入:\n{reply}"
    );
    assert!(reply.contains("请只以对话形式回复"), "缺对话注入:\n{reply}");
    assert!(
        reply.contains("请以第二人称视角进行叙述"),
        "缺视角注入:\n{reply}"
    );
}

#[tokio::test]
async fn complex_floors_inject_positions_and_macros() {
    let _guard = test_lock().await;
    let app = test_app();

    // 楼层1:user 角色 + before(对话开头);楼层2:system 角色 + 变量宏;
    // 楼层3:assistant 角色 + after(最新消息后);宏 {{char}} 应展开为角色名「测试角色」
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({
            "config": {
                "mode": "complex",
                "simple": { "word_count_enabled": false },
                "floors": [
                    {
                        "id": "f1", "name": "开场引导", "content": "{{char}}的开场引导",
                        "role": "user", "position": "before", "depth": 0,
                        "enabled": true, "order": 0
                    },
                    {
                        "id": "f2", "name": "世界变量", "content": "{{setvar::地点::酒馆}}\n当前地点={{getvar::地点}}",
                        "role": "system", "position": "system", "depth": 0,
                        "enabled": true, "order": 1
                    },
                    {
                        "id": "f3", "name": "收尾提醒", "content": "记住:{{char}}在{{getvar::地点}}",
                        "role": "assistant", "position": "after", "depth": 0,
                        "enabled": true, "order": 2
                    }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (sid, cid) = new_session(app).await;
    let reply = floors_reply(app, &sid, &cid).await;

    // system 楼层:setvar 输出为空、getvar 读到刚 set 的值(宏按楼层 order 展开)
    assert!(
        reply.contains("当前地点=酒馆"),
        "system 楼层宏展开失败:\n{reply}"
    );
    // 位置4 归位:所有楼层统一紧随 system,按 order 排(user 楼层 f1 → assistant 楼层 f3),
    // 均位于开场白 assistant 消息与真实用户消息之前(旧 before/after/depth 历史内散插已废弃)
    let idx_open = reply
        .find("[user] 测试角色的开场引导")
        .unwrap_or(usize::MAX);
    let idx_tail = reply
        .find("[assistant] 记住:测试角色在酒馆")
        .unwrap_or(usize::MAX);
    let idx_first_mes = reply
        .find("[assistant] 你好,我是测试角色")
        .unwrap_or(usize::MAX);
    let idx_user_msg = reply
        .find("[user] 查看注入 [[floors]]")
        .unwrap_or(usize::MAX);
    assert!(
        idx_open < idx_first_mes,
        "user 楼层应位于开场白之前:\n{reply}"
    );
    assert!(
        idx_open < idx_user_msg,
        "user 楼层应位于真实用户消息之前:\n{reply}"
    );
    assert!(
        idx_open < idx_tail,
        "楼层按 order 排序(user f1 应在前):\n{reply}"
    );
    assert!(
        idx_tail < idx_first_mes,
        "assistant 楼层应紧随 user 楼层、位于开场白之前(位置4 归位):\n{reply}"
    );
    // 楼层宏展开:{{char}}/{{getvar}} 均展开
    assert!(
        reply.contains("记住:测试角色在酒馆"),
        "assistant 楼层宏展开失败:\n{reply}"
    );
}

#[tokio::test]
async fn session_vars_persist_across_rounds() {
    let _guard = test_lock().await;
    let app = test_app();

    // 第一轮:setvar 楼层写入 计数=7
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({
            "config": {
                "mode": "complex",
                "simple": { "word_count_enabled": false },
                "floors": [
                    {
                        "id": "s1", "name": "写", "content": "{{setvar::计数::7}}",
                        "role": "system", "position": "system", "depth": 0,
                        "enabled": true, "order": 0
                    },
                    {
                        "id": "s2", "name": "读", "content": "计数={{getvar::计数}}",
                        "role": "system", "position": "system", "depth": 0,
                        "enabled": true, "order": 1
                    }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (sid, cid) = new_session(app).await;
    let r1 = floors_reply(app, &sid, &cid).await;
    assert!(r1.contains("计数=7"), "第一轮 setvar/getvar 未生效:\n{r1}");

    // 第二轮:移除 setvar 楼层,仅保留 getvar —— 应从 session_vars 持久化读取
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({
            "config": {
                "mode": "complex",
                "simple": { "word_count_enabled": false },
                "floors": [
                    {
                        "id": "s2", "name": "读", "content": "计数={{getvar::计数}}",
                        "role": "system", "position": "system", "depth": 0,
                        "enabled": true, "order": 0
                    }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let r2 = floors_reply(app, &sid, &cid).await;
    assert!(r2.contains("计数=7"), "会话变量未跨轮次持久化:\n{r2}");
}

// ===== 酒馆预设导入 =====

const ST_PRESET: &str = r#"{
    "name": "测试预设",
    "prompts": [
        { "identifier": "b1", "name": "开头", "content": "开场内容", "role": "user", "injection_position": 1, "enabled": true },
        { "identifier": "sys", "name": "系统块", "content": "{{setvar::地点::酒馆}}", "role": "user", "system_prompt": true, "enabled": true },
        { "identifier": "a1", "name": "收尾", "content": "{{char}}收尾", "role": "assistant", "attach_index": 1, "attach_role": "user", "attach_side": "end", "enabled": false }
    ],
    "prompt_order": [
        { "character_id": 100001, "order": [
            { "enabled": true, "identifier": "b1" },
            { "enabled": true, "identifier": "sys" },
            { "enabled": true, "identifier": "a1" }
        ] }
    ]
}"#;

/// 手拼 multipart 上传预设(仿 upload_character)
async fn import_preset(app: &axum::Router, preset_json: &str) -> (StatusCode, Value) {
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"preset.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        preset_json
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/prompt-inject/import")
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
async fn import_preset_replaces_floors() {
    let _guard = test_lock().await;
    let app = test_app();

    // 先 PUT 旧配置(2 个楼层)
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({
            "config": {
                "mode": "complex",
                "simple": { "word_count_enabled": false },
                "floors": [
                    { "id": "old1", "name": "旧1", "content": "旧内容", "role": "system", "position": "system", "depth": 0, "enabled": true, "order": 0 },
                    { "id": "old2", "name": "旧2", "content": "旧内容2", "role": "user", "position": "after", "depth": 0, "enabled": true, "order": 1 }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 导入预设 → 替换现有楼层
    let (status, resp) = import_preset(app, ST_PRESET).await;
    assert_eq!(status, StatusCode::OK, "resp: {resp}");
    assert_eq!(resp["imported"], 3);
    assert_eq!(resp["config"]["mode"], "complex");
    let floors = resp["config"]["floors"].as_array().unwrap();
    assert_eq!(floors.len(), 3);
    // 旧楼层被替换
    assert!(
        floors.iter().all(|f| f["id"] != "old1"),
        "旧楼层应被替换: {floors:?}"
    );
    // 顺序按 prompt_order:b1 → sys → a1;position/role/enabled 映射正确
    assert_eq!(floors[0]["id"], "b1");
    assert_eq!(floors[0]["position"], "before");
    assert_eq!(floors[0]["role"], "user");
    assert_eq!(floors[1]["id"], "sys");
    assert_eq!(floors[1]["position"], "system");
    assert_eq!(
        floors[1]["content"], "{{setvar::地点::酒馆}}",
        "宏应原样保留"
    );
    assert_eq!(floors[2]["id"], "a1");
    assert_eq!(
        floors[2]["position"], "after",
        "attach_* 条目应映射为 after"
    );
    assert_eq!(floors[2]["role"], "assistant");
    assert_eq!(
        floors[2]["enabled"], false,
        "预设禁用条目保留 enabled=false"
    );

    // 持久化:GET 回读一致
    let (_, got) = send_json(app, "GET", "/api/prompt-inject", json!({})).await;
    let got_floors = got["config"]["floors"].as_array().unwrap();
    assert_eq!(got_floors.len(), 3);
    assert_eq!(got_floors[0]["id"], "b1");
}

#[tokio::test]
async fn import_preset_invalid_inputs_400() {
    let _guard = test_lock().await;
    let app = test_app();

    // 非 JSON → 400
    let (status, resp) = import_preset(app, "not json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "resp: {resp}");
    assert!(
        resp["error"].as_str().unwrap_or("").contains("JSON"),
        "resp: {resp}"
    );

    // 缺 prompts 数组 → 400
    let (status, resp) = import_preset(app, r#"{"name":"无提示词"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "resp: {resp}");
    assert!(
        resp["error"].as_str().unwrap_or("").contains("prompts"),
        "resp: {resp}"
    );

    // 缺 file 字段 → 400
    let body =
        "--BOUND\r\nContent-Disposition: form-data; name=\"other\"\r\n\r\nx\r\n--BOUND--\r\n"
            .to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/api/prompt-inject/import")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// 禁词库配置:PUT 保存含 banned_words 的简单模式 → GET 回读一致(全链路透传)
#[tokio::test]
async fn prompt_inject_banned_words_roundtrip() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, saved) = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({
            "config": {
                "mode": "simple",
                "simple": {
                    "word_count_enabled": false,
                    "word_count": 200,
                    "paraphrase_enabled": false,
                    "dialogue_enabled": false,
                    "perspective_enabled": false,
                    "perspective": "第三人称",
                    "banned_words_enabled": true,
                    "banned_words": [
                        { "word": "笨蛋", "replacement": "傻瓜" },
                        { "word": "脏话", "replacement": "" }
                    ]
                },
                "floors": []
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "saved: {saved}");
    assert_eq!(saved["config"]["simple"]["banned_words_enabled"], true);

    let (_, got) = send_json(app, "GET", "/api/prompt-inject", json!({})).await;
    let simple = &got["config"]["simple"];
    assert_eq!(simple["banned_words_enabled"], true);
    let words = simple["banned_words"].as_array().unwrap();
    assert_eq!(words.len(), 2);
    assert_eq!(words[0]["word"], "笨蛋");
    assert_eq!(words[0]["replacement"], "傻瓜");
    assert_eq!(words[1]["word"], "脏话");
    assert_eq!(words[1]["replacement"], "");
}

/// 禁词库:旧配置缺 banned_words 字段 → 默认关闭 + 空表,不回滚、不报错(向后兼容)
#[tokio::test]
async fn prompt_inject_banned_words_absent_defaults() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, _) = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({
            "config": {
                "mode": "simple",
                "simple": { "word_count_enabled": false },
                "floors": []
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, got) = send_json(app, "GET", "/api/prompt-inject", json!({})).await;
    assert_eq!(got["config"]["simple"]["banned_words_enabled"], false);
    assert_eq!(
        got["config"]["simple"]["banned_words"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

// ===== 聊天气泡宏展开(history 显示层) =====

/// 发送普通消息(不触发 [[floors]] 回显),返回 SSE 事件
async fn send_plain(app: &axum::Router, sid: &str, cid: &str, message: &str) {
    let _ = sse_events(app, sid, cid, message).await;
}

/// GET /api/chat/history → 消息数组
async fn fetch_history(app: &axum::Router, sid: &str) -> Value {
    let (_, resp) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    resp["messages"].clone()
}

#[tokio::test]
async fn history_expands_macros_in_content_display() {
    let _guard = test_lock().await;
    let app = test_app();
    // 恢复默认注入配置,避免楼层干扰
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({ "config": simple_cfg(0, "第三人称") }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (sid, cid) = new_session(app).await;
    // 发送含宏的用户消息(存储原文,不展开)
    send_plain(app, &sid, &cid, "你好 {{char}},我的数字是{{random:42,42}}").await;

    let messages = fetch_history(app, &sid).await;
    let arr = messages.as_array().unwrap();
    assert!(arr.len() >= 2, "应含开场白 + 用户消息: {messages}");
    let user = arr.iter().find(|m| m["role"] == "user").unwrap();
    // content 保持原文(编辑用),content_display 展开(渲染用)
    assert_eq!(
        user["content"], "你好 {{char}},我的数字是{{random:42,42}}",
        "content 应为原文"
    );
    assert_eq!(
        user["content_display"], "你好 测试角色,我的数字是42",
        "content_display 应展开宏: {user}"
    );
    // 开场白(无宏)display 与原文一致
    let first = arr.iter().find(|m| m["role"] == "assistant").unwrap();
    assert_eq!(first["content_display"], first["content"]);
    // 每条消息都带 content_display
    assert!(arr.iter().all(|m| m.get("content_display").is_some()));
}

#[tokio::test]
async fn history_display_setvar_does_not_persist() {
    let _guard = test_lock().await;
    let app = test_app();

    // 探针楼层:生成侧会展开 {{getvar}}(从 DB 读)。
    // 用户消息含 {{addvar}} 时:生成侧不展开历史(避免副作用重复)→ 不落库;
    // 若显示层(GET history)把 addvar 写库,探针楼层会读到污染值。
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({
            "config": {
                "mode": "complex",
                "simple": { "word_count_enabled": false },
                "floors": [
                    { "id": "probe", "name": "探针", "content": "计数=[{{getvar::计数}}]",
                      "role": "system", "position": "system", "depth": 0, "enabled": true, "order": 0 }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (sid, cid) = new_session(app).await;
    // 用户消息含 addvar(生成侧不展开历史,DB 计数保持空)
    send_plain(app, &sid, &cid, "{{addvar::计数::1}}加一").await;

    // 显示层展开两次;若显示层 addvar 写库,DB 计数会被写成 "1"
    let m1 = fetch_history(app, &sid).await;
    let _ = fetch_history(app, &sid).await;
    let user1 = m1
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .unwrap();
    assert_eq!(
        user1["content_display"], "加一",
        "setvar/addvar 输出为空,仅保留正文: {user1}"
    );

    // 探针楼层在生成侧展开 {{getvar::计数}}:应从 DB 读(空 → 显示 []),证明显示层未落库
    let reply = floors_reply(app, &sid, &cid).await;
    assert!(
        reply.contains("计数=[]"),
        "显示层 addvar 不应持久化,DB 计数应保持空:\n{reply}"
    );
    assert!(
        !reply.contains("计数=[1]"),
        "显示层 addvar 不应持久化:\n{reply}"
    );
}
