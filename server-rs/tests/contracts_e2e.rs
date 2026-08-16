// P5 契约应用路径与 KaleidoState 运行态接线:端到端集成测试
//
// 覆盖三条主路径:
//   1. mvu_tool(两步生成 update_variables):契约 updateRules 只声明部分字段,
//      补丁含「声明 + 未声明」字段 → 门控只放行声明字段;kaleido_changelog 逐 op 落账
//      (source=agent、path、old/new);kaleido_state 有行且 meta.lastTurnId > 0。
//   2. 正文 <UpdateVariable> 文本协议:契约卡同样产生 changelog 记录。
//   3. 无契约角色(存量卡):kaleido_state 无行(回归,零行为变化)。
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
    // 与 tests/assistant.rs 同款隔离:运行时主提示词目录指向空目录
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

/// 上传角色卡:酒馆助手插件变量树 + 可选契约(extensions.nlkaleido)。
/// 契约 updateRules 只声明「心之所向.好感度」,「世界.年分」未声明(供门控拒绝断言)。
/// 注意:契约提取读 data_raw 顶层 extensions(flatten_v3_data 仅对 v3 卡提顶层,
/// v2 卡 data.extensions 不上提),故顶层直接内嵌契约。
async fn upload_character_with_contract(app: &axum::Router) -> (StatusCode, Value) {
    let contract = json!({
        "version": 1,
        "id": "mai-card-contract",
        "schema": { "properties": {} },
        "updateRules": {
            "心之所向.好感度": {
                "path": "心之所向.好感度",
                "type": "number",
                "updateMode": "every_turn",
                "display": true
            }
        }
    });
    let card = json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "name": "雪白芽衣",
        "description": "兔族少女,害羞内向。",
        "first_mes": "……你、你好……",
        "extensions": { "nlkaleido": contract },
        "data": {
            "name": "雪白芽衣",
            "description": "兔族少女,害羞内向。",
            "first_mes": "……你、你好……",
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
    upload_card(app, &card).await
}

/// 上传无契约的酒馆助手角色卡(存量卡,回归用)
async fn upload_character_without_contract(app: &axum::Router) -> (StatusCode, Value) {
    let card = json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "name": "无契约芽衣",
        "description": "兔族少女。",
        "first_mes": "……你好。",
        "data": {
            "name": "无契约芽衣",
            "description": "兔族少女。",
            "first_mes": "……你好。",
            "character_book": {
                "entries": [
                    {
                        "id": 0,
                        "comment": "[InitVar]",
                        "content": "心之所向:\n  好感度: 0",
                        "constant": false,
                        "enabled": false,
                        "position": 0,
                        "order": 100
                    },
                    {
                        "id": 1,
                        "comment": "变量更新规则",
                        "content": "当前状态:\n{{format_message_variable::stat_data}}",
                        "constant": true,
                        "enabled": true,
                        "position": 4,
                        "order": 100
                    }
                ]
            }
        }
    });
    upload_card(app, &card).await
}

async fn upload_card(app: &axum::Router, card: &Value) -> (StatusCode, Value) {
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"card.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
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

/// 轮询历史直到谓词满足(落库在引擎收尾后),返回最新历史
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

/// 读取该会话的 kaleido_changelog(解析 entry_json;按 seq 升序返回)
async fn kaleido_entries(app: &axum::Router, sid: &str) -> Vec<Value> {
    let _ = app;
    let data_dir = std::env::temp_dir().join(format!("kedai-test-{}", std::process::id()));
    let db = data_dir.join("kedai.db");
    // 直查测试库(app 与被测服务共享同一 data_dir;只读打开避免锁冲突)
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("打开测试库失败");
    let mut stmt = conn
        .prepare("SELECT entry_json FROM kaleido_changelog WHERE session_id = ?1 ORDER BY seq ASC")
        .unwrap();
    let rows = stmt
        .query_map(rusqlite::params![sid], |row| row.get::<_, String>(0))
        .unwrap();
    rows.filter_map(|r| r.ok())
        .map(|raw| serde_json::from_str(&raw).expect("entry_json 应为合法 JSON"))
        .collect()
}

/// 读取该会话的 kaleido_state 行(无行返回 None)
async fn kaleido_state_row(app: &axum::Router, sid: &str) -> Option<Value> {
    let _ = app;
    let data_dir = std::env::temp_dir().join(format!("kedai-test-{}", std::process::id()));
    let db = data_dir.join("kedai.db");
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("打开测试库失败");
    conn.query_row(
        "SELECT contract_version, stat_data, meta_json, revision_seq FROM kaleido_state WHERE session_id = ?1",
        rusqlite::params![sid],
        |row| {
            Ok(json!({
                "contractVersion": row.get::<_, i64>(0)?,
                "statData": serde_json::from_str::<Value>(&row.get::<_, String>(1)?).unwrap_or(Value::Null),
                "meta": serde_json::from_str::<Value>(&row.get::<_, String>(2)?).unwrap_or(Value::Null),
                "revisionSeq": row.get::<_, i64>(3)?,
            }))
        },
    )
    .ok()
}

/// 契约卡 + mvu_tool 驱动:门控只放行声明字段;kaleido_changelog 逐 op 落账;kaleido_state 有行
#[tokio::test]
async fn contract_gates_mvu_tool_and_commits_kaleido_state() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, char) = upload_character_with_contract(app).await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();

    // 第一轮:建会话(生成开始消息,变量树就绪)
    send_chat(app, "", &cid, "你好").await;
    let sid = session_id_for(app, &cid).await;

    // 第二轮:mvu_tool 补丁含「声明字段(好感度) + 未声明字段(年分)」。
    // 注意:[[reply: 在首个 "]]" 截断,JSON 数组收尾的 "]" 会与之拼接成 "]]" 被吃掉,
    // 故参数后多补一个 "]" 还原(mock 钩子约定:marker 取到首个 "]]" 截止)。
    let events = send_chat(
        app,
        &sid,
        &cid,
        "[[reply:芽衣的耳朵抖了抖。[[mvu_tool:update_variables|{\"patches\":[{\"op\":\"replace\",\"path\":\"/心之所向/好感度\",\"value\":150},{\"op\":\"replace\",\"path\":\"/世界/年分\",\"value\":2030}]}]]]]]",
    )
    .await;
    eprintln!(">>> events: {events:?}");
    let vars_evt = events.iter().find(|e| e["type"] == "vars");
    eprintln!(">>> vars_evt: {vars_evt:?}");

    // 落库就绪后断言(引擎收尾同步提交,轮询兜底异步竞态)
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

    // 1) 门控:vars 快照只含声明字段的新值,未声明字段(年分)被拒 → 保持 2024
    assert_eq!(
        last["extra"]["mvu"]["stat_data"]["心之所向"]["好感度"],
        json!(150),
        "声明字段应放行"
    );
    assert_eq!(
        last["extra"]["mvu"]["stat_data"]["世界"]["年分"],
        json!(2024),
        "未声明字段应被契约拒绝(保持原值)"
    );

    // 2) kaleido_changelog:source=agent、path/old/new 正确(仅好感度一条)
    let entries = kaleido_entries(app, &sid).await;
    assert!(!entries.is_empty(), "契约生效路径应产生 changelog 记录");
    let applied: Vec<&Value> = entries
        .iter()
        .filter(|e| e["source"] == json!("agent"))
        .collect();
    assert_eq!(applied.len(), 1, "未声明字段不落账: {entries:?}");
    let e = applied[0];
    assert_eq!(e["path"], json!("心之所向.好感度"));
    assert_eq!(e["old"], json!(0));
    assert_eq!(e["new"], json!(150));
    // turn_id 是引擎的「历史消息数」近似轮次(首轮建会话 2 条 + 本轮 2 条 = 4),
    // 与引擎既有约定一致(mod.rs:629 同款:ctx_data.history.len() as u64)。
    assert_eq!(e["turnId"], json!(4), "第二轮 turn_id=历史消息数(4)");
    assert!(e["seq"].as_u64().unwrap() > 0, "seq 应为表内自增值");
    assert_eq!(e["confidence"], json!("high"), "缺省置信度视为 high");

    // 3) kaleido_state:有行、contract_version=1、meta.lastTurnId>0、stat_data 同步
    let state = kaleido_state_row(app, &sid)
        .await
        .expect("契约卡应有运行态行");
    assert_eq!(state["contractVersion"], json!(1));
    let last_turn: u64 = state["meta"]["lastTurnId"].as_u64().unwrap_or(0);
    assert!(last_turn > 0, "meta.lastTurnId 应推进: {state}");
    assert_eq!(
        state["meta"]["confidence"]["心之所向.好感度"],
        json!("high"),
        "applied 路径置信度记入 meta"
    );
    assert_eq!(
        state["statData"]["心之所向"]["好感度"],
        json!(150),
        "运行态 stat_data 应与最新树一致"
    );
    // revision_seq 首轮 commit 即应为 changelog 最大 seq(修复:INSERT 分支写死 0)
    let max_seq = entries
        .iter()
        .map(|e| e["seq"].as_u64().unwrap_or(0))
        .max()
        .unwrap_or(0);
    assert_eq!(
        state["revisionSeq"].as_u64(),
        Some(max_seq),
        "revision_seq 应等于 changelog 最大 seq: {state}"
    );
}

/// 契约卡 + 正文 <UpdateVariable> 文本协议:同样产生 changelog 记录(正文路径接线)
#[tokio::test]
async fn contract_changelog_for_update_variable_text_protocol() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, char) = upload_character_with_contract(app).await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();

    send_chat(app, "", &cid, "你好").await;
    let sid = session_id_for(app, &cid).await;

    // 第二轮:正文携带 <UpdateVariable> JSONPatch(好感度 0 → 33)
    let patch_reply = "芽衣轻轻点了点头。\n<UpdateVariable>\n<Analysis>好感度上升</Analysis>\n<JSONPatch>\n[\n  { \"op\": \"replace\", \"path\": \"/心之所向/好感度\", \"value\": 33 }\n]\n</JSONPatch>\n</UpdateVariable>";
    let events = send_chat(
        app,
        &sid,
        &cid,
        &format!("我们聊聊吧 [[reply:{patch_reply}]]"),
    )
    .await;
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("应有 finish 事件");
    assert!(
        !finish["content"].as_str().unwrap().contains("<UpdateVariable"),
        "正文应剥离协议块"
    );

    let _ = poll_history_until(app, &sid, |h| {
        h["messages"]
            .as_array()
            .map(|m| {
                m.last()
                    .map(|x| x["extra"]["mvu"]["stat_data"]["心之所向"]["好感度"] == json!(33))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    })
    .await;

    // 正文路径同样落账(source=agent、old/new 正确)
    let entries = kaleido_entries(app, &sid).await;
    let hit = entries
        .iter()
        .find(|e| e["path"] == json!("心之所向.好感度"))
        .unwrap_or_else(|| panic!("正文路径应产生 changelog: {entries:?}"));
    assert_eq!(hit["source"], json!("agent"));
    assert_eq!(hit["old"], json!(0));
    assert_eq!(hit["new"], json!(33));
}

/// 无契约角色(存量卡):kaleido_state 无行(回归,零行为变化)
#[tokio::test]
async fn no_contract_character_has_no_kaleido_state() {
    let _guard = test_lock().await;
    let app = test_app();

    let (status, char) = upload_character_without_contract(app).await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();

    // 两轮:mvu_tool 更新变量(应原样放行,不走门控)
    send_chat(app, "", &cid, "你好").await;
    let sid = session_id_for(app, &cid).await;
    send_chat(
        app,
        &sid,
        &cid,
        "[[reply:芽衣的耳朵抖了抖。[[mvu_tool:update_variables|{\"patches\":[{\"op\":\"replace\",\"path\":\"/心之所向/好感度\",\"value\":88}]}]]]]",
    )
    .await;
    let h = poll_history_until(app, &sid, |h| {
        h["messages"]
            .as_array()
            .map(|m| {
                m.last()
                    .map(|x| x["extra"]["mvu"]["stat_data"]["心之所向"]["好感度"] == json!(88))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    })
    .await;
    let last = h["messages"].as_array().unwrap().last().unwrap();
    // 无契约:补丁原样放行(变量照常更新)
    assert_eq!(
        last["extra"]["mvu"]["stat_data"]["心之所向"]["好感度"],
        json!(88),
        "无契约应原样放行"
    );

    // kaleido_state 无行;kaleido_changelog 无该会话记录
    assert!(
        kaleido_state_row(app, &sid).await.is_none(),
        "无契约角色不应产生运行态行"
    );
    assert!(
        kaleido_entries(app, &sid).await.is_empty(),
        "无契约角色不应产生 changelog"
    );
}

/// P8 老卡兼容层:契约 default 填充——InitVar 只写了部分字段,契约声明的
/// 其余字段(含 InitVar 未建层的嵌套)在首轮会话初始化时按 default 补齐;
/// InitVar 已写的值不被覆盖。
#[tokio::test]
async fn contract_defaults_fill_missing_initvar_fields() {
    let _guard = test_lock().await;
    let app = test_app();

    // 契约声明三个字段:心之所向.好感度(InitVar 已写)、心之所向.信任度
    // (InitVar 缺失,但所在层已存在)、世界.天气(InitVar 未建该键,层已存在)
    let contract = json!({
        "version": 1,
        "id": "defaults-contract",
        "schema": { "properties": {} },
        "updateRules": {
            "心之所向.好感度": {
                "path": "心之所向.好感度", "type": "number",
                "default": 99, "updateMode": "every_turn", "display": true
            },
            "心之所向.信任度": {
                "path": "心之所向.信任度", "type": "number",
                "default": 10, "updateMode": "every_turn", "display": true
            },
            "世界.天气": {
                "path": "世界.天气", "type": "string",
                "default": "晴", "updateMode": "every_turn", "display": true
            }
        }
    });
    // InitVar 只写好感度(已有值不得被 default=99 覆盖)与世界层(无天气键)
    let card = json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "name": "默认值测试芽衣",
        "description": "兔族少女。",
        "first_mes": "……你好。",
        "extensions": { "nlkaleido": contract },
        "data": {
            "name": "默认值测试芽衣",
            "description": "兔族少女。",
            "first_mes": "……你好。",
            "character_book": {
                "entries": [
                    {
                        "id": 0,
                        "comment": "[InitVar]",
                        "content": "心之所向:\n  好感度: 5\n世界:\n  年分: 2024",
                        "constant": false,
                        "enabled": false,
                        "position": 0,
                        "order": 100
                    }
                ]
            }
        }
    });
    let (status, char) = upload_card(app, &card).await;
    assert_eq!(status, StatusCode::CREATED, "上传失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();

    // 两轮:首轮建会话(InitVar 收集 + 契约 default 填充 + 落库);次轮
    // mvu_tool 更新好感度(任意值)——只有产生变量变更的轮次才写 extra.mvu
    // 快照,快照树即「InitVar ∪ 契约 default」的完整初始树
    send_chat(app, "", &cid, "你好").await;
    let sid = session_id_for(app, &cid).await;
    send_chat(
        app,
        &sid,
        &cid,
        "[[reply:芽衣点头。[[mvu_tool:update_variables|{\"patches\":[{\"op\":\"replace\",\"path\":\"/心之所向/好感度\",\"value\":6}]}]]]]",
    )
    .await;
    let h = poll_history_until(app, &sid, |h| {
        h["messages"]
            .as_array()
            .map(|m| {
                m.last()
                    .map(|x| x["extra"]["mvu"]["stat_data"].is_object())
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    })
    .await;
    let messages = h["messages"].as_array().unwrap();
    let tree = &messages.last().unwrap()["extra"]["mvu"]["stat_data"];
    assert_eq!(
        tree["心之所向"]["好感度"], json!(6),
        "mvu_tool 更新生效: {tree}"
    );
    // changelog 的 old 值暴露初始基线:若 default(99)覆盖了 InitVar 的 5,
    // old 会是 99——由此证明填充不覆盖已写值
    let entries = kaleido_entries(app, &sid).await;
    let hit = entries
        .iter()
        .find(|e| e["path"] == json!("心之所向.好感度"))
        .expect("好感度应有 changelog 记录");
    assert_eq!(hit["old"], json!(5), "default 不得覆盖 InitVar 已写值");
    assert_eq!(hit["new"], json!(6));
    assert_eq!(
        tree["心之所向"]["信任度"], json!(10),
        "缺失字段应补契约 default: {tree}"
    );
    assert_eq!(
        tree["世界"]["天气"], json!("晴"),
        "InitVar 未建的键应补 default(已有层保留): {tree}"
    );
    assert_eq!(
        tree["世界"]["年分"], json!(2024),
        "非契约声明的 InitVar 字段保持不动: {tree}"
    );
}
