// 酒馆助手(SillyTavern-Assistant)插件兼容:端到端集成测试
//
// 场景:上传带酒馆助手插件的角色卡(character_book 内嵌:
//   [InitVar] 初始变量(好感度 0,enabled=false)
//   分阶段人设(EJS 模板 if 链,constant 常驻)
//   变量更新规则({{format_message_variable::stat_data}} 状态注入)
// )→ 对话:
//   1. 首轮注入:世界书条目经 EJS 渲染 → 好感度 0 命中「羞怯壁花」分支,
//      {{format_message_variable}} 展开为状态文本,EJS 源码不漏给模型
//   2. 模型回复带 <UpdateVariable><JSONPatch> → 补丁应用、树持久化、
//      SSE vars 事件推送、存储/显示内容剥离该块、消息 extra.mvu 快照
//   3. 好感度 150 → 下一轮注入阶段切换为「悄然萌芽」
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
    // 测试进程级:把运行时主提示词目录指向空目录,避免真实项目根的 AGENTS_RUNTIME.md
    // 被注入 mock 断言(与 build_test_app 临时数据目录的隔离语义一致)。APP 只初始化一次。
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

/// 上传带酒馆助手插件的角色卡(JSON,V2 格式),first_mes 可自定义
async fn upload_assistant_character_with(
    app: &axum::Router,
    first_mes: &str,
) -> (StatusCode, Value) {
    let card = json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "name": "雪白芽衣",
        "description": "兔族少女,害羞内向。",
        "first_mes": first_mes,
        "data": {
            "name": "雪白芽衣",
            "description": "兔族少女,害羞内向。",
            "character_book": {
                "entries": [
                    {
                        "id": 0,
                        "comment": "[InitVar]",
                        "content": "世界:\n  年分: 2024\n  月份: 2\n芽衣:\n  当前行动: \"在图书馆看书\"\n心之所向:\n  好感度: 0",
                        "constant": false,
                        "enabled": false,
                        "position": 0,
                        "order": 100
                    },
                    {
                        "id": 1,
                        "comment": "分阶段人设",
                        "content": "<%_ if (getvar('stat_data.心之所向.好感度') <= 100) { _%>【羞怯壁花】会结巴、躲闪,不敢直视。\n<%_ } else if (getvar('stat_data.心之所向.好感度') <= 200) { _%>【悄然萌芽】会偷偷观察,小声回应。\n<%_ } else { _%>【半露欣颜】会主动分享,脸红但不躲闪。\n<%_ } _%>",
                        "constant": true,
                        "enabled": true,
                        "position": 0,
                        "order": 100
                    },
                    {
                        "id": 2,
                        "comment": "变量更新规则",
                        "content": "当前状态:\n{{format_message_variable::stat_data}}\n规则:回复末尾用 <UpdateVariable><Analysis>…</Analysis><JSONPatch>[…]</JSONPatch></UpdateVariable> 更新变量。",
                        "constant": true,
                        "enabled": true,
                        "position": 4,
                        "order": 100
                    }
                ]
            }
        }
    });
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"芽衣.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        card
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

/// 上传带酒馆助手插件的角色卡(JSON,V2 格式,默认首楼)
async fn upload_assistant_character(app: &axum::Router) -> (StatusCode, Value) {
    upload_assistant_character_with(app, "……你、你好……").await
}

/// 轮询历史直到谓词满足(落库在后台 spawn),返回最新历史
async fn poll_history_until(app: &axum::Router, sid: &str, pred: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..60 {
        let (_, h) = send_json(
            app,
            "GET",
            &format!("/api/chat/history?session_id={sid}"),
            json!({}),
        )
        .await;
        if pred(&h) {
            return h;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("等待落库超时")
}
async fn send_chat(
    app: &axum::Router,
    session_id: &str,
    character_id: &str,
    message: &str,
) -> Vec<Value> {
    let mut payload = json!({ "character_id": character_id, "message": message });
    if !session_id.is_empty() {
        payload["session_id"] = json!(session_id);
    }
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/send")
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
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

/// 从 [[floors]] 回显文本中提取 system 消息全文(回显每行 `[role] content`,
/// content 可含换行 → 按角色行首切分再拼接)
fn extract_system(echoed: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in echoed.lines() {
        if let Some(rest) = line.strip_prefix("[system] ") {
            if let Some(c) = current.take() {
                parts.push(c);
            }
            current = Some(rest.to_string());
        } else if line.starts_with("[user] ") || line.starts_with("[assistant] ") {
            if let Some(c) = current.take() {
                parts.push(c);
            }
        } else if let Some(c) = current.as_mut() {
            c.push('\n');
            c.push_str(line);
        }
    }
    if let Some(c) = current.take() {
        parts.push(c);
    }
    parts.join("\n")
}

/// 取角色首个会话 id
async fn session_id_for(app: &axum::Router, cid: &str) -> String {
    let (_, list) = send_json(
        app,
        "GET",
        &format!("/api/chat/sessions?character_id={cid}"),
        json!({}),
    )
    .await;
    let sessions = list["sessions"].as_array().cloned().unwrap_or_default();
    assert!(!sessions.is_empty(), "应有会话: {list}");
    sessions[0]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn assistant_card_injects_rendered_stage_and_state() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, char) = upload_assistant_character(app).await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();

    // 首轮:[[floors]] 回显完整 LLM 消息,用于断言注入内容(同时创建会话)
    let events = send_chat(app, "", &cid, "你好 [[floors]]").await;
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    let echoed = finish["content"].as_str().unwrap();
    let system = extract_system(echoed);
    // EJS 渲染:初始好感度 0 → 羞怯壁花分支;EJS 源码不漏
    assert!(
        system.contains("【羞怯壁花】"),
        "注入应含渲染后的羞怯壁花: {system}"
    );
    assert!(!system.contains("【悄然萌芽】"), "不该渲染未命中分支");
    assert!(!system.contains("getvar"), "EJS 源码不应泄漏给模型");
    assert!(!system.contains("<%"), "EJS 源码不应泄漏给模型");
    // {{format_message_variable::stat_data}} 展开为状态文本
    assert!(system.contains("好感度: 0"), "状态注入应含好感度: {system}");
    assert!(system.contains("年分: 2024"), "状态注入应含世界信息");
}

#[tokio::test]
async fn update_variable_protocol_applied_stripped_and_switches_stage() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, char) = upload_assistant_character(app).await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();

    // 第一轮:建会话
    send_chat(app, "", &cid, "你好").await;
    let sid = session_id_for(app, &cid).await;

    // 第二轮:模型回复带 <UpdateVariable> JSONPatch(好感度 0 → 150)
    let patch_reply = "芽衣轻轻点了点头。\n<UpdateVariable>\n<Analysis>好感度上升至 150,进入悄然萌芽阶段</Analysis>\n<JSONPatch>\n[\n  { \"op\": \"replace\", \"path\": \"/心之所向/好感度\", \"value\": 150 }\n]\n</JSONPatch>\n</UpdateVariable>";
    let events = send_chat(
        app,
        &sid,
        &cid,
        &format!("我们聊聊吧 [[reply:{patch_reply}]]"),
    )
    .await;

    // 1) SSE 流中出现 vars 事件,携带最新 stat_data
    let vars_evt = events
        .iter()
        .find(|e| e["type"] == "vars")
        .unwrap_or_else(|| panic!("应有 vars 事件: {events:?}"));
    assert_eq!(vars_evt["stat_data"]["心之所向"]["好感度"], json!(150));

    // 2) finish 内容剥离 <UpdateVariable> 块
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    let content = finish["content"].as_str().unwrap();
    assert!(
        !content.contains("<UpdateVariable"),
        "finish 内容应剥离协议块: {content}"
    );
    assert!(content.contains("芽衣轻轻点了点头"));

    // 3) 历史消息:存储剥离 + extra.mvu 快照(落库在后台 spawn,轮询等待)
    let mut history = json!({});
    for _ in 0..60 {
        let (_, h) = send_json(
            app,
            "GET",
            &format!("/api/chat/history?session_id={sid}"),
            json!({}),
        )
        .await;
        let msgs = h["messages"].as_array().cloned().unwrap_or_default();
        if msgs.iter().any(|m| {
            m["role"] == "assistant"
                && m["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("芽衣轻轻点了点头")
        }) {
            history = h;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let msgs = history["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("等待落库超时: 事件={events:?}, 历史={history}"));
    let last = msgs.last().unwrap();
    assert_eq!(last["role"], json!("assistant"));
    assert!(
        !last["content"]
            .as_str()
            .unwrap()
            .contains("<UpdateVariable"),
        "存储应剥离协议块"
    );
    assert_eq!(
        last["extra"]["mvu"]["stat_data"]["心之所向"]["好感度"],
        json!(150),
        "消息应带变量快照"
    );

    // 4) 第三轮:好感度 150 → 注入阶段切换为悄然萌芽,状态文本反映新值
    let events2 = send_chat(app, &sid, &cid, "再来一轮 [[floors]]").await;
    let finish2 = events2
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    let echoed2 = finish2["content"].as_str().unwrap();
    let system2 = extract_system(echoed2);
    assert!(
        system2.contains("【悄然萌芽】"),
        "应切换到悄然萌芽: {system2}"
    );
    assert!(!system2.contains("【羞怯壁花】"), "不应再显示羞怯壁花");
    assert!(system2.contains("好感度: 150"), "状态注入应反映新好感度");
}

#[tokio::test]
async fn card_plugins_detected_in_character_detail() {
    let _guard = test_lock().await;
    let app = test_app();

    // 助手卡:详情应返回 card_plugins(酒馆助手,5 项特性全命中)
    let (status, char) = upload_assistant_character(app).await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, detail) = send_json(app, "GET", &format!("/api/characters/{cid}"), json!({})).await;
    let plugins = detail["card_plugins"]
        .as_array()
        .expect("应有 card_plugins: {detail}");
    assert_eq!(plugins.len(), 1);
    let p = &plugins[0];
    assert_eq!(p["id"], json!("sillytavern-assistant"));
    assert_eq!(p["enabled"], json!(true));
    assert_eq!(p["source"], json!("character_card"));
    let detected_ids: Vec<&str> = p["features"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["detected"] == json!(true))
        .filter_map(|f| f["id"].as_str())
        .collect();
    assert!(detected_ids.contains(&"initvar"));
    assert!(detected_ids.contains(&"ejs"));
    assert!(detected_ids.contains(&"variable"));
    assert!(detected_ids.contains(&"status"));
    assert!(detected_ids.contains(&"protocol"));

    // 普通卡:无 card_plugins
    let plain = json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "name": "普通角色",
        "description": "普通",
        "data": { "name": "普通角色", "description": "普通", "character_book": { "entries": [] } }
    });
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"普通2.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        plain
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/characters/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let pc: Value = serde_json::from_slice(&bytes).unwrap();
    let (_, pdetail) = send_json(
        app,
        "GET",
        &format!("/api/characters/{}", pc["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    assert!(
        pdetail["card_plugins"].is_null() || pdetail["card_plugins"].as_array().unwrap().is_empty(),
        "普通卡不应有插件: {pdetail}"
    );
}

#[tokio::test]
async fn non_assistant_card_unaffected() {
    let _guard = test_lock().await;
    let app = test_app();

    // 普通卡(无 [InitVar]/EJS):注入行为不变
    let card = json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "name": "普通角色",
        "description": "普通描述",
        "data": {
            "name": "普通角色",
            "description": "普通描述",
            "character_book": { "entries": [] }
        }
    });
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"普通.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        card
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/characters/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let char: Value = serde_json::from_slice(&bytes).unwrap();
    let cid = char["id"].as_str().unwrap().to_string();

    let events = send_chat(app, "", &cid, "你好 [[floors]]").await;
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    let echoed = finish["content"].as_str().unwrap();
    assert!(echoed.contains("[system] "), "普通卡应正常注入 system");
    // 无 vars 事件(没有变量更新)
    assert!(
        !events.iter().any(|e| e["type"] == "vars"),
        "普通卡不应有 vars 事件"
    );
}

#[tokio::test]
async fn assistant_vars_endpoint_overwrites_session_tree() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, char) = upload_assistant_character(app).await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();

    // 首轮建会话
    send_chat(app, "", &cid, "你好").await;
    let sid = session_id_for(app, &cid).await;

    // 未知会话 → 404
    let (s404, _) = send_json(
        app,
        "PUT",
        "/api/chat/sessions/no-such/assistant-vars",
        json!({ "stat_data": {} }),
    )
    .await;
    assert_eq!(s404, StatusCode::NOT_FOUND, "未知会话应 404");

    // PUT 覆盖会话树:好感度 99
    let (s, r) = send_json(
        app,
        "PUT",
        &format!("/api/chat/sessions/{sid}/assistant-vars"),
        json!({ "stat_data": { "心之所向": { "好感度": 99 }, "世界": { "年分": 2042 } } }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "覆盖失败: {r}");
    assert_eq!(r["ok"], json!(true));

    // 下一轮注入应反映新树(99 而非 150/0)
    let events = send_chat(app, &sid, &cid, "看看状态 [[floors]]").await;
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    let system = extract_system(finish["content"].as_str().unwrap());
    assert!(
        system.contains("好感度: 99"),
        "注入应反映覆盖后的树: {system}"
    );
    assert!(
        system.contains("年分: 2042"),
        "注入应反映覆盖后的树: {system}"
    );
}

#[tokio::test]
async fn truncate_rolls_back_variable_tree_to_anchor_snapshot() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, char) = upload_assistant_character(app).await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();

    // 第一轮:建会话,记录首条 user 消息 id(后续作为截断 anchor)
    send_chat(app, "", &cid, "你好").await;
    let sid = session_id_for(app, &cid).await;
    let (_, h0) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    let user0 = h0["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .expect("应有首轮 user 消息")
        .clone();
    let anchor_id = user0["id"].as_i64().unwrap();

    // 第二轮:好感度 0 → 150
    let patch_reply = "芽衣轻轻点了点头。\n<UpdateVariable>\n<Analysis>好感度上升至 150</Analysis>\n<JSONPatch>\n[\n  { \"op\": \"replace\", \"path\": \"/心之所向/好感度\", \"value\": 150 }\n]\n</JSONPatch>\n</UpdateVariable>";
    send_chat(
        app,
        &sid,
        &cid,
        &format!("我们聊聊吧 [[reply:{patch_reply}]]"),
    )
    .await;
    let events = send_chat(app, &sid, &cid, "再来一轮 [[floors]]").await;
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    let system = extract_system(finish["content"].as_str().unwrap());
    assert!(
        system.contains("好感度: 150"),
        "回滚前注入应为 150: {system}"
    );

    // 模拟前端「编辑首轮 user 消息并重发」:truncate(anchor=首轮 user)+ 回滚变量树
    // (anchor 之前无 assistant 消息带 mvu 快照 → 前端推空树 → 引擎从 [InitVar] 重新初始化)
    let (s, r) = send_json(
        app,
        "POST",
        &format!("/api/chat/sessions/{sid}/truncate"),
        json!({ "anchor_id": anchor_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "截断失败: {r}");
    let (s2, r2) = send_json(
        app,
        "PUT",
        &format!("/api/chat/sessions/{sid}/assistant-vars"),
        json!({ "stat_data": {} }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "回滚失败: {r2}");

    // 重发后:变量回滚到 [InitVar] 初始值(好感度 0、年分 2024)
    let events2 = send_chat(app, &sid, &cid, "重发后的新分支 [[floors]]").await;
    let finish2 = events2
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    let system2 = extract_system(finish2["content"].as_str().unwrap());
    assert!(
        system2.contains("好感度: 0"),
        "回滚后应回到初始好感度: {system2}"
    );
    assert!(
        system2.contains("年分: 2024"),
        "回滚后应回到初始世界: {system2}"
    );
    assert!(!system2.contains("好感度: 150"), "回滚后不应残留旧分支变量");
}

// ===== 两步生成:首楼状态栏槽位 + 工具调用(update_status_bar / update_variables)=====

/// 首楼含 <StatusPlaceHolderImpl/>:占位符**保留**(由前端脚本渲染为动态 HTML 状态栏,
/// 服务端替换会毁掉渲染挂载点导致 HTML 渲染失效);update_status_bar 的状态文本
/// 仅作为本轮气泡 extra.status_bar 显示,不写入首楼。
#[tokio::test]
async fn status_slot_tool_preserves_placeholder_and_bubble() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, char) =
        upload_assistant_character_with(app, "……你、你好……\n<StatusPlaceHolderImpl/>").await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();

    // 第一轮:建会话(首楼写入占位符)
    send_chat(app, "", &cid, "你好").await;
    let sid = session_id_for(app, &cid).await;

    // 第二轮:正文回复 + 第二步工具调用(update_status_bar)
    let events = send_chat(
        app,
        &sid,
        &cid,
        "[[reply:芽衣微微一笑。[[mvu_tool:update_status_bar|{\"text\":\"好感度 0→3 · 情绪:稍安\"}]]]]",
    )
    .await;
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    assert!(
        finish["content"].as_str().unwrap().contains("芽衣微微一笑"),
        "正文应保留"
    );

    // 首楼落库:占位符保留、不写入状态文本、不设置 last 锚点;气泡带状态栏
    let h = poll_history_until(app, &sid, |h| {
        h["messages"]
            .as_array()
            .map(|m| {
                m.last()
                    .map(|x| x["extra"]["status_bar"] == json!("好感度 0→3 · 情绪:稍安"))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    })
    .await;
    let msgs = h["messages"].as_array().unwrap();
    let first = msgs.first().unwrap();
    assert_eq!(first["role"], json!("assistant"));
    assert_eq!(
        first["extra"]["first_mes"],
        json!(true),
        "首楼应带 first_mes 标记"
    );
    assert!(
        first["content"]
            .as_str()
            .unwrap()
            .contains("StatusPlaceHolderImpl"),
        "占位符应保留供前端脚本渲染: {}",
        first["content"]
    );
    assert!(
        !first["content"].as_str().unwrap().contains("好感度 0→3"),
        "首楼不应写入状态文本: {}",
        first["content"]
    );
    assert!(
        first["extra"].get("status_slot").is_none(),
        "不应落库 last 锚点: {}",
        first["extra"]
    );
    // 当前气泡状态栏
    let last = msgs.last().unwrap();
    assert_eq!(
        last["extra"]["status_bar"],
        json!("好感度 0→3 · 情绪:稍安"),
        "气泡应带状态栏: {}",
        last["extra"]
    );

    // 第三轮:占位符仍在 → 继续保留,不写首楼;气泡状态栏更新为新文本
    send_chat(
        app,
        &sid,
        &cid,
        "[[reply:芽衣点了点头。[[mvu_tool:update_status_bar|{\"text\":\"好感度 3→6 · 情绪:平静\"}]]]]",
    )
    .await;
    let h2 = poll_history_until(app, &sid, |h| {
        h["messages"]
            .as_array()
            .map(|m| {
                m.last()
                    .map(|x| x["extra"]["status_bar"] == json!("好感度 3→6 · 情绪:平静"))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    })
    .await;
    let first2 = h2["messages"].as_array().unwrap().first().unwrap();
    assert!(
        first2["content"]
            .as_str()
            .unwrap()
            .contains("StatusPlaceHolderImpl"),
        "占位符应始终保留: {}",
        first2["content"]
    );
    assert!(
        !first2["content"].as_str().unwrap().contains("好感度 0→3"),
        "首楼不应写入任何状态文本: {}",
        first2["content"]
    );
    let last2 = h2["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(
        last2["extra"]["status_bar"],
        json!("好感度 3→6 · 情绪:平静"),
        "气泡状态栏应更新"
    );
}

/// 第二步经 [[mvu_tool:update_variables]] 更新变量树:vars 事件 + extra.mvu 快照 + diff 兜底状态栏
#[tokio::test]
async fn mvu_variables_tool_updates_tree_and_snapshot() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, char) = upload_assistant_character(app).await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();

    // 第一轮:建会话
    send_chat(app, "", &cid, "你好").await;
    let sid = session_id_for(app, &cid).await;

    // 第二轮:第二步工具调用更新变量树(replace 0→150 + delta 年分 2024→2025)
    let events = send_chat(
        app,
        &sid,
        &cid,
        "[[reply:芽衣的耳朵抖了抖。[[mvu_tool:update_variables|{\"patches\":[{\"op\":\"replace\",\"path\":\"/心之所向/好感度\",\"value\":150},{\"op\":\"delta\",\"path\":\"/世界/年分\",\"value\":1}]}]]]]",
    )
    .await;

    // 1) vars 事件携带最新树
    let vars_evt = events
        .iter()
        .find(|e| e["type"] == "vars")
        .unwrap_or_else(|| panic!("应有 vars 事件: {events:?}"));
    assert_eq!(vars_evt["stat_data"]["心之所向"]["好感度"], json!(150));
    assert_eq!(
        vars_evt["stat_data"]["世界"]["年分"],
        json!(2025.0),
        "delta 结果为浮点"
    );

    // 2) 消息 extra.mvu 快照 + diff 兜底状态栏(未调用 update_status_bar → diff 生成)
    let h = poll_history_until(app, &sid, |h| {
        h["messages"]
            .as_array()
            .map(|m| {
                m.last()
                    .map(|x| x["extra"]["mvu"]["stat_data"]["心之所向"]["好感度"] == json!(150))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    })
    .await;
    let last = h["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(
        last["extra"]["mvu"]["stat_data"]["世界"]["年分"],
        json!(2025.0),
        "快照应含 delta 结果"
    );
    let bar = last["extra"]["status_bar"]
        .as_str()
        .expect("应有 diff 兜底状态栏");
    assert!(
        bar.contains("好感度: 0→150"),
        "diff 状态栏应含好感度变化: {bar}"
    );
}

/// 回退:第二步不调用工具时,旧文本协议(<StatusBar>)仍生效,正文剥离协议标签
#[tokio::test]
async fn status_bar_text_protocol_fallback() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, char) =
        upload_assistant_character_with(app, "……你、你好……\n<StatusPlaceHolderImpl/>").await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();

    // 第一轮:建会话
    send_chat(app, "", &cid, "你好").await;
    let sid = session_id_for(app, &cid).await;

    // 第二轮:文本协议 <StatusBar>(reply 内嵌 [[mvu_text:SB{…}]] 钩子:首层 reply 消费第一轮,
    // 残余 marker 保留到第二轮 → 第二轮输出 <StatusBar> 协议文本 → 回退路径提取)
    let events = send_chat(
        app,
        &sid,
        &cid,
        "[[reply:芽衣轻声应了一声。[[mvu_text:SB{好感度 0→3 · 情绪:稍安}]]]]",
    )
    .await;
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    // 正文剥离 <StatusBar> 协议标签
    assert!(
        !finish["content"].as_str().unwrap().contains("<StatusBar"),
        "正文应剥离 StatusBar 标签: {}",
        finish["content"]
    );
    assert!(finish["content"]
        .as_str()
        .unwrap()
        .contains("芽衣轻声应了一声"));

    // 回退路径状态栏落库
    let h = poll_history_until(app, &sid, |h| {
        h["messages"]
            .as_array()
            .map(|m| {
                m.last()
                    .map(|x| x["extra"]["status_bar"] == json!("好感度 0→3 · 情绪:稍安"))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    })
    .await;
    let last = h["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(
        last["extra"]["status_bar"],
        json!("好感度 0→3 · 情绪:稍安"),
        "回退协议状态栏应落库"
    );
    assert!(
        !last["content"].as_str().unwrap().contains("<StatusBar"),
        "存储正文应剥离 StatusBar 标签"
    );
}

/// 端到端:世界书条目中的 getwi / getqr / getChatMessage / injectPrompt 经渲染上下文真实读取。
/// 场景:先经 API 创建快速回复(战斗/开始),再上传角色卡——角色卡内嵌世界书条目:
///   [GENERATE:BEFORE] 条目内容用 injectPrompt 登记注入 + getChatMessage(-1) 读取历史;
///   常驻条目用 getqr('战斗','开始') 读取快速回复、getwi('引子') 读取另一世界书条目。
/// 发送 [[floors]] 回显完整消息,断言注入含渲染结果且 EJS 源码不漏。
#[tokio::test]
async fn ejs_context_reads_world_quickreply_history_and_injects() {
    let _guard = test_lock().await;
    let app = test_app();

    // 1) 创建快速回复(getqr 数据源)
    let (status, qr) = send_json(
        app,
        "POST",
        "/api/quick-replies",
        json!({ "name": "战斗", "label": "开始", "content": "拔剑战斗,士气 <%= getvar('士气') %>" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建快速回复失败: {qr}");
    let qr_id = qr["quick_reply"]["id"].as_i64().expect("应有快速回复 id");

    // 2) 上传角色卡:内嵌世界书含 [GENERATE:BEFORE] 注入条目与常驻条目(用 getqr/getwi/getChatMessage)
    let card = json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "name": "EJS 上下文测试",
        "description": "测试角色",
        "first_mes": "你来了。",
        "data": {
            "name": "EJS 上下文测试",
            "description": "测试角色",
            "character_book": {
                "entries": [
                    {
                        "id": 0,
                        "comment": "[InitVar]",
                        "content": "士气: 88",
                        "constant": false,
                        "enabled": false
                    },
                    {
                        "id": 1,
                        "comment": "[GENERATE:BEFORE]",
                        "content": "<% injectPrompt('战斗提示', getqr('战斗', '开始')); %><% if (getChatMessage(-1, 'assistant') !== '') { %>历史首条: <%= getChatMessage(0) %><% } %>",
                        "constant": false,
                        "enabled": true
                    },
                    {
                        "id": 2,
                        "comment": "引子",
                        "content": "故事开始于图书馆。",
                        "constant": true,
                        "enabled": true
                    },
                    {
                        "id": 3,
                        "comment": "联动人设",
                        "content": "开场设定:<%= getwi('引子') %> <%= getqr('战斗', '开始') %>",
                        "constant": true,
                        "enabled": true
                    }
                ]
            }
        }
    });
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"ctx.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        card
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/characters/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "上传失败");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let char: Value = serde_json::from_slice(&bytes).unwrap();
    let cid = char["id"].as_str().unwrap().to_string();

    // 3) 首轮 [[floors]]:断言 system 含渲染后的注入与条目,EJS 源码不漏
    let events = send_chat(app, "", &cid, "你好 [[floors]]").await;
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    let echoed = finish["content"].as_str().unwrap();
    let system = extract_system(echoed);
    // injectPrompt 登记的注入并入 system 尾部(渲染后的快速回复内容)
    assert!(
        system.contains("拔剑战斗,士气 88"),
        "getqr 渲染结果应经 injectPrompt 注入 system: {system}"
    );
    // getChatMessage(-1, 'assistant') 命中首楼 → 输出历史首条(即开场白)
    assert!(
        system.contains("历史首条: 你来了。"),
        "getChatMessage 应读取历史消息: {system}"
    );
    // getwi 读取另一条世界书条目内容
    assert!(
        system.contains("开场设定:故事开始于图书馆。 拔剑战斗,士气 88"),
        "getwi/getqr 渲染结果应进入常驻条目: {system}"
    );
    assert!(!system.contains("getqr"), "EJS 源码不应泄漏: {system}");
    assert!(!system.contains("<%"), "EJS 源码不应泄漏: {system}");

    // 4) 快速回复 CRUD 其余路径:更新 + 删除
    let (s2, upd) = send_json(
        app,
        "PUT",
        &format!("/api/quick-replies/{qr_id}"),
        json!({ "name": "战斗", "label": "开始", "content": "更新内容", "enabled": false }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "更新失败: {upd}");
    assert_eq!(upd["quick_reply"]["enabled"], json!(false));
    let (s3, _) = send_json(app, "DELETE", &format!("/api/quick-replies/{qr_id}"), json!({})).await;
    assert_eq!(s3, StatusCode::NO_CONTENT, "删除应 204");
    // 删除后列表为空
    let (s4, list) = send_json(app, "GET", "/api/quick-replies?all=true", json!({})).await;
    assert_eq!(s4, StatusCode::OK);
    assert!(
        list["quick_replies"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false),
        "删除后列表应为空: {list}"
    );
}

/// 端到端:RENDER 注入标签并入生成注入(system 首/尾)。
/// 场景:角色卡内嵌两条 enabled 常驻条目,comment 分别为 [RENDER:BEFORE]/[RENDER:AFTER],
/// content 为纯文本标记;发送 [[floors]] 回显完整消息,断言 BEFORE 内容出现在 system 开头、
/// AFTER 内容出现在 system 末尾,且标签/EJS 源码不泄漏。
/// 语义注记:酒馆原版 RENDER 仅影响显示渲染、不影响生成;kedai 无独立显示渲染管道,
/// 故并入生成注入(system 首/尾),差异见 plan6-ecosystem-devtools.md 6a 实施记录。
#[tokio::test]
async fn render_tag_injects_into_system_edges() {
    let _guard = test_lock().await;
    let app = test_app();

    let card = json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "name": "RENDER 注入测试",
        "description": "测试角色",
        "first_mes": "你好。",
        "data": {
            "name": "RENDER 注入测试",
            "description": "测试角色",
            "character_book": {
                "entries": [
                    {
                        "id": 0,
                        "comment": "[RENDER:BEFORE]",
                        "content": "开头注入标记",
                        "constant": true,
                        "enabled": true
                    },
                    {
                        "id": 1,
                        "comment": "[RENDER:AFTER]",
                        "content": "结尾注入标记",
                        "constant": true,
                        "enabled": true
                    }
                ]
            }
        }
    });
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"render.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        card
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/characters/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "上传失败");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let char: Value = serde_json::from_slice(&bytes).unwrap();
    let cid = char["id"].as_str().unwrap().to_string();

    let events = send_chat(app, "", &cid, "你好 [[floors]]").await;
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    let echoed = finish["content"].as_str().unwrap();
    let system = extract_system(echoed);
    // RENDER:BEFORE 内容应出现在 system 开头(生成注入 system 首)
    assert!(
        system.starts_with("开头注入标记"),
        "RENDER:BEFORE 内容应在 system 开头: {system}"
    );
    // RENDER:AFTER 内容应注入 system 尾部(生成注入 system 尾,计入 protected_tail);
    // extract_system 会把回显分隔符(---FLOOR-SEP---)拼进尾部,故用 contains + 顺序断言
    assert!(
        system.contains("结尾注入标记"),
        "RENDER:AFTER 内容应注入 system: {system}"
    );
    // 标签源码不泄漏(渲染后注入的是内容,非 comment 标签)
    assert!(!system.contains("[RENDER"), "标签源码不应泄漏: {system}");
    // 边界顺序:BEFORE 在 AFTER 之前
    let before_pos = system.find("开头注入标记").expect("应有 BEFORE 标记");
    let after_pos = system.find("结尾注入标记").expect("应有 AFTER 标记");
    assert!(before_pos < after_pos, "BEFORE 应位于 AFTER 之前");
    // AFTER 是 system 尾部注入:其后仅剩回显分隔符/空白,无其它注入内容
    let tail_after = &system[after_pos + "结尾注入标记".len()..];
    assert!(
        tail_after.trim().is_empty() || tail_after.trim() == "---FLOOR-SEP---",
        "AFTER 内容应在 system 末尾,其后仅剩回显分隔符: {system}"
    );
}

/// 端到端:@@ 装饰器(if/unless/var)在世界书注入前执行。
/// 场景:常驻条目带 @@if getvar('开关') → 变量为真时注入、为假时跳过;
/// @@var 在渲染前写入变量树(条目内容可读取该变量)。
#[tokio::test]
async fn worldbook_decorators_gate_injection_and_write_vars() {
    let _guard = test_lock().await;
    let app = test_app();

    let card = json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "name": "装饰器测试",
        "description": "测试角色",
        "first_mes": "你好。",
        "data": {
            "name": "装饰器测试",
            "description": "测试角色",
            "character_book": {
                "entries": [
                    {
                        "id": 0,
                        "comment": "[InitVar]",
                        "content": "开关: true\n暗号: \"夜莺\"",
                        "constant": false,
                        "enabled": false
                    },
                    {
                        "id": 1,
                        "comment": "条件人设",
                        "content": "@@if getvar('开关')\n开关开启,暗号=<%= getvar('暗号') %>",
                        "constant": true,
                        "enabled": true
                    },
                    {
                        "id": 2,
                        "comment": "除非人设",
                        "content": "@@unless getvar('隐藏')\n除非未命中时注入",
                        "constant": true,
                        "enabled": true
                    },
                    {
                        "id": 3,
                        "comment": "变量写入",
                        "content": "@@var 注入计数 = 7\n写入后计数=<%= getvar('注入计数') %>",
                        "constant": true,
                        "enabled": true
                    }
                ]
            }
        }
    });
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"deco.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        card
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/characters/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "上传失败");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let char: Value = serde_json::from_slice(&bytes).unwrap();
    let cid = char["id"].as_str().unwrap().to_string();

    let events = send_chat(app, "", &cid, "你好 [[floors]]").await;
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    let system = extract_system(finish["content"].as_str().unwrap());
    // @@if 命中:开关为真 → 注入
    assert!(
        system.contains("开关开启,暗号=夜莺"),
        "@@if 命中应注入: {system}"
    );
    // @@unless 未命中:隐藏变量不存在 → 注入
    assert!(
        system.contains("除非未命中时注入"),
        "@@unless 未命中应注入: {system}"
    );
    // @@var 渲染前写入变量树 → 条目内容可读取
    assert!(
        system.contains("写入后计数=7"),
        "@@var 应写入变量树供模板读取: {system}"
    );
    assert!(!system.contains("@@if"), "装饰器行不应泄漏: {system}");
    assert!(!system.contains("<%"), "EJS 源码不应泄漏: {system}");
}
