// 会话与消息路由:/api/chat/sessions、/history、/messages/:id、/clear
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use crate::parsing::macros::{expand_macros, MacroCtx};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct SessionsQuery {
    #[serde(default)]
    pub character_id: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateSessionBody {
    #[serde(default)]
    pub character_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// 开场序号:0=主开场(first_mes),1..=备用开场(alternate_greetings);缺省 0
    #[serde(default)]
    pub greeting_index: Option<usize>,
}

/// 会话内切换开场请求体:{ greeting_index }
#[derive(Deserialize)]
pub struct RegreetBody {
    pub greeting_index: usize,
}

#[derive(Deserialize)]
pub struct MessageQuery {
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateMessageBody {
    #[serde(default)]
    pub content: Option<String>,
}

/// mvu 变量快照保存体:{ stat_data, display_data }
#[derive(Deserialize)]
pub struct SaveVariablesBody {
    #[serde(default)]
    pub stat_data: Option<serde_json::Value>,
    #[serde(default)]
    pub display_data: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub struct ClearBody {
    #[serde(default)]
    pub session_id: Option<String>,
}

/// 截断消息体:{ anchor_id } — 保留该消息,删除其后所有消息(供「编辑用户消息后重发」)
#[derive(Deserialize)]
pub struct TruncateBody {
    pub anchor_id: i64,
}

/// 切换 swipe 版本体:{ swipe_id } — 目标版本索引(extra.swipes 数组下标)
#[derive(Deserialize)]
pub struct SwipeBody {
    pub swipe_id: usize,
}

/// 会话列表:
///   - 带 character_id → 仅该角色的会话
///   - 不带 → 全部会话(联表角色名 + 消息数),供聊天记录面板
pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SessionsQuery>,
) -> Response {
    let Some(cid) = q.character_id else {
        let all = state
            .sessions
            .list_all()
            .into_iter()
            .map(|s| {
                let count = state.sessions.message_count(&s.id);
                let mut v = serde_json::to_value(&s).unwrap_or(serde_json::Value::Null);
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("message_count".into(), json!(count));
                }
                v
            })
            .collect::<Vec<_>>();
        return Json(json!({ "sessions": all })).into_response();
    };
    if cid.trim().is_empty() {
        return Json(json!({ "error": "缺少 character_id 查询参数" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let sessions = state.sessions.list_by_character(&cid);
    Json(json!({ "sessions": sessions })).into_response()
}

pub async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateSessionBody>,
) -> Response {
    let Some(cid) = body.character_id else {
        return Json(json!({ "error": "缺少 character_id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    if cid.trim().is_empty() {
        return Json(json!({ "error": "缺少 character_id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    match state.sessions.create(&cid, body.title.as_deref()) {
        Ok(s) => {
            // 角色带开场白时,作为会话第一条 assistant 消息写入,
            // 使聊天窗口自动显示开场白,且引擎上下文天然包含它
            seed_first_message(&state, &s.id, &cid, body.greeting_index.unwrap_or(0));
            Json(s).into_response().with_status(StatusCode::CREATED)
        }
        Err(e) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// 收集角色全部开场:主开场(first_mes)+ 备用开场(alternate_greetings),均去空。
/// 返回空 Vec = 无任何开场。
fn collect_greetings(rec: &crate::models::types::CharacterRecord) -> Vec<String> {
    let mut list: Vec<String> = Vec::new();
    if let Some(fm) = rec.first_mes.as_deref() {
        if !fm.trim().is_empty() {
            list.push(fm.to_string());
        }
    }
    if let Some(alts) = &rec.alternate_greetings {
        list.extend(
            alts.iter()
                .filter(|s| !s.trim().is_empty())
                .cloned(),
        );
    }
    list
}

/// 若角色有开场且会话尚无任何消息,把指定序号的开场(越界回退 0)作为首条
/// assistant 消息写入。供 create_session、chat/send(ensure_session)与 regreet 复用。
pub(crate) fn seed_first_message(
    state: &AppState,
    session_id: &str,
    character_id: &str,
    greeting_index: usize,
) {
    let Some(rec) = state.characters.get(character_id) else {
        return;
    };
    let greetings = collect_greetings(&rec);
    if greetings.is_empty() {
        return;
    }
    if !state.sessions.get_messages(session_id).is_empty() {
        return;
    }
    let idx = greeting_index.min(greetings.len() - 1);
    let fm = &greetings[idx];
    let _ = state.sessions.add_message(
        session_id,
        "assistant",
        fm,
        json!({ "ts": chrono::Utc::now().timestamp_millis(), "first_mes": true }),
    );
}

/// POST /api/chat/sessions/{id}/regreet:会话内切换开场 —— 清空会话全部消息后,
/// 按 greeting_index 重新播种开场(越界回退 0)。
pub async fn regreet(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<RegreetBody>,
) -> Response {
    let Some(session) = state.sessions.get(&session_id) else {
        return Json(json!({ "error": "会话不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND);
    };
    state.sessions.clear_messages(&session_id);
    seed_first_message(&state, &session_id, &session.character_id, body.greeting_index);
    Json(json!({ "ok": true, "greeting_index": body.greeting_index })).into_response()
}

pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    if state.sessions.delete(&id) {
        // P6:kaleido 两表无 FK 级联,显式清理契约运行态;失败仅记日志不阻断删除
        // (残留行仅占用存储,不影响正确性——读写均按 session_id 过滤)。
        if let Err(e) = state.kaleido_state.delete_for_session(&id) {
            crate::utils::logger::warn(
                "会话契约运行态清理失败",
                &[("session_id", json!(id)), ("error", json!(e))],
            );
        }
        StatusCode::NO_CONTENT.into_response()
    } else {
        Json(json!({ "error": "会话不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND)
    }
}

pub async fn history(
    State(state): State<Arc<AppState>>,
    Query(q): Query<MessageQuery>,
) -> Response {
    let Some(sid) = q.session_id else {
        return Json(json!({ "error": "缺少 session_id 查询参数" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    if sid.trim().is_empty() {
        return Json(json!({ "error": "缺少 session_id 查询参数" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let Some(session) = state.sessions.get(&sid) else {
        return Json(json!({ "error": "会话不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND);
    };
    let messages = state.sessions.get_messages(&sid);
    // 显示层宏展开(只读):让宏在聊天气泡渲染时持续工作。
    // - 每条消息的宏上下文 history = 该条之前的消息({{lastMessage}} 显示「上一条」)
    // - vars 用持久化副本:{{setvar}}/{{addvar}} 仅影响本轮后续消息的显示,不写库
    //   (避免显示层重复回放 addvar 造成变量重复追加,污染生成状态)
    // - {{time}}/{{random}} 等随每次请求新鲜展开
    // 返回 content_display(展开后文本,渲染用)+ 保持 content 原文(编辑消息用,不污染存储)
    let mut values = Vec::with_capacity(messages.len());
    if !messages.is_empty() {
        let (name, description, personality, scenario) =
            match state.characters.get(&session.character_id) {
                Some(c) => (
                    c.chara_name.clone(),
                    c.description.clone(),
                    c.data_raw
                        .as_ref()
                        .and_then(|raw| raw.get("personality"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    c.data_raw
                        .as_ref()
                        .and_then(|raw| raw.get("scenario"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                ),
                None => (
                    "角色".to_string(),
                    String::new(),
                    String::new(),
                    String::new(),
                ),
            };
        let mut vars = state.sessions.load_session_vars(&sid);
        // 酒馆助手变量树副本:{{format_message_variable}}/{{getvar::stat_data.…}} 显示用
        let mut assistant_vars = state.sessions.load_assistant_vars(&sid);
        // 7 作用域变量(计划二):chat 树/扁平层镜像同步,作用域宏族显示用
        let mut scopes = crate::parsing::scopes::ScopeVars::new();
        scopes.sync_chat_tree(&assistant_vars);
        scopes.sync_chat_flat(&vars);
        for (i, m) in messages.iter().enumerate() {
            let before: Vec<(String, String)> = messages[..i]
                .iter()
                .map(|x| (x.role.clone(), x.content.clone()))
                .collect();
            let mut mctx = MacroCtx {
                character_name: &name,
                character_description: &description,
                user_name: "用户",
                user_input: "",
                personality: &personality,
                scenario: &scenario,
                history: &before,
                vars: &mut vars,
                assistant_vars: Some(&mut assistant_vars),
                scopes: Some(&mut scopes),
            };
            let display = expand_macros(&m.content, &mut mctx);
            let mut v = serde_json::to_value(m).unwrap_or_else(|_| json!({ "content": m.content }));
            if let Some(obj) = v.as_object_mut() {
                obj.insert("content_display".into(), json!(display));
            }
            values.push(v);
        }
    }
    Json(json!({ "messages": values })).into_response()
}

pub async fn update_message(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<MessageQuery>,
    Json(body): Json<UpdateMessageBody>,
) -> Response {
    let Some(sid) = q.session_id else {
        return Json(json!({ "error": "缺少 session_id 或 id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    let Some(content) = body.content else {
        return Json(json!({ "error": "缺少 content" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    if sid.trim().is_empty() {
        return Json(json!({ "error": "缺少 session_id 或 id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    match state.sessions.update_message(&sid, id, &content) {
        Some(m) => Json(m).into_response(),
        None => Json(json!({ "error": "消息不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND),
    }
}

/// POST /api/chat/messages/{id}/swipe — 切换消息的 swipe 版本。
/// 从 extra.swipes 取目标版本,更新 content 列(激活版本冗余存储,GET /history
/// 零改动)并写入 extra.swipe_id;旧消息无 swipes(单版本)或索引越界返回 400。
pub async fn swipe_message(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<MessageQuery>,
    Json(body): Json<SwipeBody>,
) -> Response {
    let Some(sid) = q.session_id else {
        return Json(json!({ "error": "缺少 session_id 或 id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    if sid.trim().is_empty() {
        return Json(json!({ "error": "缺少 session_id 或 id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let Some(msg) = state.sessions.get_message(&sid, id) else {
        return Json(json!({ "error": "消息不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND);
    };
    let swipes = match msg.extra.get("swipes").and_then(|s| s.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => {
            return Json(json!({ "error": "该消息没有可切换的版本" }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST)
        }
    };
    let swipe = match swipes.get(body.swipe_id) {
        Some(v) => v,
        None => {
            return Json(json!({ "error": "swipe_id 越界" }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST)
        }
    };
    let content = swipe
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    if state.sessions.update_message_content(&sid, id, &content).is_none() {
        return Json(json!({ "error": "消息不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND);
    }
    state
        .sessions
        .merge_message_extra(&sid, id, json!({ "swipe_id": body.swipe_id }));
    Json(json!({ "content": content, "swipe_id": body.swipe_id, "swipes_count": swipes.len() }))
        .into_response()
}

pub async fn delete_message(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<MessageQuery>,
) -> Response {
    let Some(sid) = q.session_id else {
        return Json(json!({ "error": "缺少 session_id 或 id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    if sid.trim().is_empty() {
        return Json(json!({ "error": "缺少 session_id 或 id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    if state.sessions.delete_message(&sid, id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        Json(json!({ "error": "消息不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND)
    }
}

pub async fn clear(State(state): State<Arc<AppState>>, Json(body): Json<ClearBody>) -> Response {
    let Some(sid) = body.session_id else {
        return Json(json!({ "error": "缺少 session_id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    if sid.trim().is_empty() {
        return Json(json!({ "error": "缺少 session_id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    state.sessions.clear_messages(&sid);
    Json(json!({ "ok": true })).into_response()
}

/// POST /api/chat/sessions/{id}/truncate:保留 anchor_id 消息,删除其后所有消息。
/// 供「编辑用户消息后重发」:先更新消息内容,再截断其后上下文,最后重新触发生成。
pub async fn truncate_messages(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<TruncateBody>,
) -> Response {
    if state.sessions.get(&session_id).is_none() {
        return Json(json!({ "error": "会话不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND);
    }
    if body.anchor_id <= 0 {
        // 负/零 id 是流式临时消息标记或非法值;按 id>anchor 语义会把整段历史删光。
        // 防御任何调用方误传(重发锚点必须是落库后的真实 id)。
        return Json(json!({ "error": "截断锚点无效:必须是落库消息的正 id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let deleted = state
        .sessions
        .truncate_messages_after(&session_id, body.anchor_id);
    Json(json!({ "ok": true, "deleted": deleted })).into_response()
}

/// PATCH /api/chat/messages/:id/variables:保存 mvu 变量快照到消息 extra.mvu
pub async fn save_variables(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<MessageQuery>,
    Json(body): Json<SaveVariablesBody>,
) -> Response {
    let Some(sid) = q.session_id else {
        return Json(json!({ "error": "缺少 session_id 或 id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    let mut patch = serde_json::Map::new();
    if let Some(sd) = body.stat_data {
        patch.insert("stat_data".into(), sd);
    }
    if let Some(dd) = body.display_data {
        patch.insert("display_data".into(), dd);
    }
    if patch.is_empty() {
        return Json(json!({ "error": "缺少变量数据" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let patch = serde_json::Value::Object(patch);
    match state
        .sessions
        .merge_message_extra(&sid, id, serde_json::json!({ "mvu": patch }))
    {
        Some(m) => Json(m).into_response(),
        None => Json(json!({ "error": "消息不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND),
    }
}

/// PUT /api/chat/sessions/{id}/assistant-vars:覆盖会话级 mvu 变量树(整树覆写)。
/// 用于「编辑/重发消息(truncate)后回滚变量状态」:前端把截断点前最近一条
/// assistant 消息的 extra.mvu 快照推回,避免新分支基于旧变量状态生成。
pub async fn save_assistant_vars(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<SaveVariablesBody>,
) -> Response {
    if state.sessions.get(&id).is_none() {
        return Json(json!({ "error": "会话不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND);
    }
    // 会话树只存 stat_data(与引擎一致);无 stat_data 时视为空树覆写
    let tree = body
        .stat_data
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    let vars = crate::parsing::assistant::AssistantVars::from_value(tree);
    if let Err(e) = state.sessions.save_assistant_vars(&id, &vars) {
        return Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Json(json!({ "ok": true })).into_response()
}

/// GET /api/chat/init-vars?character_id=xx:角色可用 [InitVar] 初始变量(供前端首屏注入)
pub async fn init_vars(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SessionsQuery>,
) -> Response {
    let Some(cid) = q.character_id else {
        return Json(json!({ "error": "缺少 character_id 查询参数" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    let mut entries: Vec<crate::parsing::world_book::WorldEntry> = Vec::new();
    // 角色卡内嵌 character_book
    if let Some(rec) = state.characters.get(&cid) {
        if let Some(raw) = rec.data_raw.as_ref() {
            entries.extend(crate::parsing::world_book::character_book_entries(raw));
        }
    }
    // 独立世界书(绑定该角色或全局)
    entries.extend(state.world_books.collect_entries_for_character(&cid));
    // 过滤出 [InitVar] 条目(含禁用的——初始化约定允许禁用条目提供初始值)
    let mut vars = serde_json::json!({});
    for e in entries {
        if !e.comment.contains("[InitVar]") {
            continue;
        }
        if let Some(obj) = vars.as_object_mut() {
            obj.insert(e.comment.clone(), serde_json::Value::String(e.content));
        }
    }
    Json(json!({ "character_id": cid, "entries": vars })).into_response()
}
