// 聊天路由(核心):/api/chat/send(SSE)、/api/chat/stop
use crate::agents::engine::AgentRunRequest;
use crate::api::app_state::AppState;
use crate::api::db_err;
use crate::models::types::{GenerationParams, PlanStep, SseEvent};
use crate::services::prompt_inject_service::{output_budget_for_word_count, InjectMode};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::{IntoResponse, Response, Sse};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct SendBody {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub character_id: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub agent_mode: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 重发锚点:必须是当前会话现存的最后一条 user 消息,且正文与请求一致。
    #[serde(default)]
    pub resend_message_id: Option<i64>,
    /// 重生成锚点(阶段六 6f):必须是当前会话现存的最后一条 assistant 消息。
    /// 生成新版本时原地更新该消息(swipes 追加),不新增消息行;与 resend_message_id 互斥。
    #[serde(default)]
    pub regenerate_assistant_id: Option<i64>,
}

#[derive(Deserialize)]
pub struct StopBody {
    #[serde(default)]
    pub session_id: Option<String>,
}

/// 手动压缩请求体(阶段借鉴 harness):对指定会话较早历史做摘要压缩。
#[derive(Deserialize)]
pub struct CompactBody {
    #[serde(default)]
    pub session_id: Option<String>,
}

pub async fn send(State(state): State<Arc<AppState>>, Json(body): Json<SendBody>) -> Response {
    let message = body.message.unwrap_or_default().trim().to_string();
    if message.is_empty() {
        return err_json("消息不能为空", StatusCode::BAD_REQUEST);
    }

    // 定位会话(指定 session_id 或按角色取首个)
    let session_id = match &body.session_id {
        Some(sid) => sid.clone(),
        None => {
            let Some(cid) = &body.character_id else {
                return err_json("缺少 session_id 或 character_id", StatusCode::BAD_REQUEST);
            };
            // 无会话直接发消息的路径:ensure_session + 补开场白一并挪进阻塞线程(DB 并发改造)
            let svc = state.sessions.clone();
            let characters = state.characters.clone();
            let cid = cid.clone();
            match state
                .db_call(move || {
                    let s = svc.ensure_session(&cid)?;
                    crate::api::sessions::seed_first_message(&characters, &svc, &s.id, &cid, 0);
                    Ok::<_, String>(s)
                })
                .await
            {
                Err(e) => return db_err(&e),
                Ok(Ok(s)) => s.id,
                Ok(Err(e)) => return err_json(&e, StatusCode::INTERNAL_SERVER_ERROR),
            }
        }
    };
    let session = match state.sessions.get(&session_id) {
        Some(s) => s,
        None => return err_json("会话不存在", StatusCode::NOT_FOUND),
    };
    if state.engine.is_active(&session_id) {
        return err_json("该会话正在生成中", StatusCode::CONFLICT);
    }

    let mode = match body.agent_mode.as_deref() {
        Some("agent") => "agent",
        Some("deep") => "deep",
        Some("custom") => "custom",
        _ => "fast",
    };
    // 生成参数:请求体优先,否则回退运行期设置(前端可编辑)
    // (设置快照:不留锁跨 await)
    let (mut params, max_context_tokens) = {
        let s = state.settings_snapshot();
        (
            GenerationParams {
                temperature: body.temperature.unwrap_or(s.default_temperature),
                top_p: body.top_p.unwrap_or(s.default_top_p),
                max_tokens: body.max_tokens.unwrap_or(s.default_max_tokens),
                stop: None,
                tools: Vec::new(),
                // 工具循环轮次上限:运行期设置(默认 32),AGENT/CUSTOM 工具循环使用
                max_tool_rounds: Some(s.max_tool_rounds),
                // 工具选择策略:默认 auto(模型自行决定是否调用工具)
                tool_choice: crate::models::types::ToolChoice::Auto,
                parallel_tool_calls: None,
            },
            Some(s.max_context_tokens),
        )
    };
    // 简单模式注入的输出预算协调(R1):字数要求 > 当前输出上限时自动上调,
    // 否则思考模型推理占用预算后回复被截断,表现为「注入不生效」。
    {
        let inj = state
            .prompt_inject
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get()
            .clone();
        if inj.mode == InjectMode::Simple
            && inj.simple.word_count_enabled
            && inj.simple.word_count > 0
        {
            let before = params.max_tokens;
            params.max_tokens =
                output_budget_for_word_count(inj.simple.word_count, params.max_tokens);
            if params.max_tokens != before {
                tracing::info!(
                    word_count = inj.simple.word_count,
                    max_tokens_before = before,
                    max_tokens_after = params.max_tokens,
                    "简单模式字数注入:输出预算自动上调"
                );
            }
        }
    }
    // AGENT 模式:把工具定义下发给模型(function calling);其余模式不启用
    // (custom 模式由引擎按步骤的工具配置注入)。多步变量工具(get_state/apply_patch)
    // 不进正文默认列表——它们只服务于多步变量驱动器与显式白名单,泄漏进正文会
    // 诱导模型在正文轮里误用变量补丁通道。
    if mode == "agent" {
        params.tools = crate::tools::tool_sets::exclude_meta(
            state.tool_registry.list_definitions(),
        );
    }
    // 自定义流程(custom 模式):校验启用与合法性,步骤快照随请求传入引擎
    let mut flow_steps: Vec<PlanStep> = Vec::new();
    if mode == "custom" {
        let flow = match state.flow.lock().unwrap_or_else(|e| e.into_inner()).get() {
            Some(f) => f.clone(),
            // 流程库为空或未选中(仅删除全部流程后出现)
            None => {
                return err_json(
                    "未选择执行流程:请先在设置中新建或选择一个 Agent 执行流程",
                    StatusCode::BAD_REQUEST,
                );
            }
        };
        if !flow.enabled || flow.steps.is_empty() {
            return err_json(
                "自定义模式需要先在设置中启用并保存执行流程",
                StatusCode::BAD_REQUEST,
            );
        }
        if let Err(e) = state
            .flow
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .validate(&flow)
        {
            return err_json(format!("执行流程配置无效:{e}"), StatusCode::BAD_REQUEST);
        }
        flow_steps = flow.steps.into_iter().filter(|s| s.enabled).collect();
    }

    const MAX_GENERATION_TOKENS: u32 = 65_536;
    const MAX_CONTEXT_TOKENS: u32 = 1_048_576;
    if params.max_tokens == 0
        || params.max_tokens > MAX_GENERATION_TOKENS
        || max_context_tokens.is_some_and(|v| v == 0 || v > MAX_CONTEXT_TOKENS)
    {
        return err_json("生成参数超出安全上限", StatusCode::BAD_REQUEST);
    }

    // 原子占位必须发生在任何消息写入之前;所有前置校验已完成。
    {
        let mut pending = state.pending_runs.lock().unwrap_or_else(|e| e.into_inner());
        if pending.contains_key(&session_id) || state.engine.is_active(&session_id) {
            return err_json("该会话正在生成中", StatusCode::CONFLICT);
        }
        let (cancel, _receiver) = tokio::sync::watch::channel(false);
        pending.insert(session_id.clone(), cancel);
    }

    // 消息写入三路分支(互斥):
    //   1) 重生成锚点(regenerate_assistant_id):生成新版本,不落新 user 消息,
    //      引擎收尾原地更新该 assistant 消息行(swipes 追加)。
    //   2) 重发锚点(resend_message_id):复用最后一条 user 消息,不落新行。
    //   3) 常规:先落一条 user 消息再生成。
    if let Some(message_id) = body.regenerate_assistant_id {
        // 与重发锚点互斥:两个锚点同时出现属于请求错误
        if body.resend_message_id.is_some() {
            state
                .pending_runs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&session_id);
            return err_json("重生成与重发锚点互斥,不能同时指定", StatusCode::BAD_REQUEST);
        }
        let valid = state
            .sessions
            .get_messages(&session_id)
            .last()
            .is_some_and(|m| m.id == message_id && m.role == "assistant");
        if !valid {
            state
                .pending_runs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&session_id);
            return err_json("重生成锚点无效或已过期", StatusCode::CONFLICT);
        }
    } else if let Some(message_id) = body.resend_message_id {
        let valid = state
            .sessions
            .get_messages(&session_id)
            .last()
            .is_some_and(|m| m.id == message_id && m.role == "user" && m.content == message);
        if !valid {
            state
                .pending_runs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&session_id);
            return err_json("重发消息锚点无效或已过期", StatusCode::CONFLICT);
        }
    } else {
        let model = state.engine.model();
        let prompt_tokens = {
            let mut ts = state
                .token_service
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            ts.count_tokens(&message, &model)
        };
        let sessions = state.sessions.clone();
        let write_session_id = session_id.clone();
        let write_message = message.clone();
        let write_result = tokio::task::spawn_blocking(move || {
            sessions.add_message(
                &write_session_id,
                "user",
                &write_message,
                json!({ "ts": chrono::Utc::now().timestamp_millis(), "prompt_tokens": prompt_tokens }),
            )
        }).await;
        if let Err(e) = write_result.unwrap_or_else(|e| Err(format!("消息写入任务失败: {e}")))
        {
            state
                .pending_runs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&session_id);
            return err_json(&e, StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    let pending_cancelled = state
        .pending_runs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&session_id)
        .is_none_or(|flag| *flag.borrow());
    if pending_cancelled {
        state
            .pending_runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id);
        return err_json("生成已中断", StatusCode::CONFLICT);
    }

    let character_id = session.character_id.clone();
    let engine = state.engine.clone();

    // 创建 mpsc 事件通道,流式发送给客户端
    let (tx, mut rx) = tokio::sync::mpsc::channel::<SseEvent>(1024);
    let req = AgentRunRequest {
        session_id: session_id.clone(),
        character_id,
        user_input: message,
        mode: mode.to_string(),
        params,
        max_context_tokens,
        flow: if mode == "custom" {
            Some(flow_steps)
        } else {
            None
        },
        regenerate_assistant_id: body.regenerate_assistant_id,
    };

    // 后台运行 Agent。assistant 消息落库已移至引擎收尾(Finish 事件发出前完成),
    // 保证前端收到 finish 后 loadHistory 能取到完整数据,消除落库竞态。
    let pending_runs = state.pending_runs.clone();
    let run_session_id = session_id.clone();
    tokio::spawn(async move {
        let _ = engine.run(req, tx).await;
        pending_runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&run_session_id);
    });

    let stream = async_stream::stream! {
        while let Some(event) = rx.recv().await {
            let json_str = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
            let sse_event = Event::default().data(json_str);
            yield Ok::<Event, Infallible>(sse_event);
        }
    };

    let mut response = Sse::new(stream)
        .keep_alive(
            axum::response::sse::KeepAlive::new().interval(std::time::Duration::from_secs(30)),
        )
        .into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache, no-transform"),
    );
    response
}

pub async fn stop(State(state): State<Arc<AppState>>, Json(body): Json<StopBody>) -> Response {
    let Some(sid) = body.session_id else {
        return err_json("缺少 session_id", StatusCode::BAD_REQUEST);
    };
    if sid.trim().is_empty() {
        return err_json("缺少 session_id", StatusCode::BAD_REQUEST);
    }
    if let Some(cancel) = state
        .pending_runs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&sid)
    {
        let _ = cancel.send(true);
    }
    state.engine.stop(&sid);
    Json(json!({ "ok": true })).into_response()
}

/// POST /api/chat/compact:手动压缩会话历史(阶段借鉴 harness)。
/// 模式为 off 时拒绝;历史不足时返回 compacted=false。压缩结果落库,原文消息保留(可逆)。
pub async fn compact(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CompactBody>,
) -> Response {
    let Some(sid) = body.session_id else {
        return err_json("缺少 session_id", StatusCode::BAD_REQUEST);
    };
    if sid.trim().is_empty() {
        return err_json("缺少 session_id", StatusCode::BAD_REQUEST);
    }
    // 会话正在生成时拒绝(避免与生成流程的 auto 压缩/历史读取并发)
    if state.engine.is_active(&sid) {
        return err_json("该会话正在生成中,暂不能压缩", StatusCode::CONFLICT);
    }
    match state.engine.compact_session(&sid).await {
        Ok(compacted) => Json(json!({ "ok": true, "compacted": compacted })).into_response(),
        Err(e) => err_json(&e, StatusCode::BAD_REQUEST),
    }
}

/// POST /api/chat/compact/clear:撤销压缩,恢复完整原文历史(阶段借鉴 harness)。
/// 无摘要时返回 cleared=false;原文消息从未删除,清除摘要行即恢复。
pub async fn clear_compact(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CompactBody>,
) -> Response {
    let Some(sid) = body.session_id else {
        return err_json("缺少 session_id", StatusCode::BAD_REQUEST);
    };
    if sid.trim().is_empty() {
        return err_json("缺少 session_id", StatusCode::BAD_REQUEST);
    }
    if state.engine.is_active(&sid) {
        return err_json("该会话正在生成中,暂不能恢复", StatusCode::CONFLICT);
    }
    match state.engine.clear_compaction(&sid) {
        Ok(cleared) => Json(json!({ "ok": true, "cleared": cleared })).into_response(),
        Err(e) => err_json(&e, StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// 本模块统一错误响应:按状态码自动附带结构化错误码(见 api/errors.rs)。
fn err_json(msg: impl AsRef<str>, status: StatusCode) -> Response {
    crate::api::err_with_code(crate::api::code_for_status(status), msg, status)
}

/// 自由生成请求体:角色卡资源页(吸血鬼卡等)内的作者脚本经宿主桥接调用
/// (TavernHelper.generate 等价物)。与 /api/chat/send 不同:不写会话、不流式,
/// 一次性把调用方提供的完整消息列表发给当前模型并返回纯文本。
#[derive(Deserialize)]
pub struct GenerateRawBody {
    pub messages: Vec<GenerateRawMessage>,
    /// 可选角色 id:提供时按该角色的世界书(内嵌 character_book + 绑定/全局书)
    /// 做关键字匹配注入——对齐酒馆 TavernHelper.generate 语义(作者页自组历史,
    /// 世界书由宿主核心注入;吸血鬼卡 "system log" 触发的输出格式规范即依赖此)。
    #[serde(default)]
    pub character_id: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

#[derive(Deserialize)]
pub struct GenerateRawMessage {
    pub role: String,
    pub content: String,
}

/// generate-raw 世界书匹配注入(对齐酒馆 generate 语义):
/// - constant 条目逐条作 system 消息置于最前(保持条目顺序)
/// - 关键字/正则命中条目作 system 消息插到最后一条 user 消息之前(紧贴尾部,
///   模型最近读到);扫描窗口为最近 depth 条消息(depth<=0 扫全部,不分角色——
///   作者页自组历史里触发字样常由 assistant 消息携带,如吸血鬼卡 system log 注释行)
/// - 概率闸(use_probability)与主引擎一致;注入条目数/总长设上限防失控
///
/// 返回注入条目的 comment 清单(响应元信息,便于调试与前端诊断)。
fn inject_worldbook_for_raw(
    entries: &[crate::parsing::world_book::WorldEntry],
    messages: &mut Vec<GenerateRawMessage>,
) -> Vec<String> {
    const MAX_INJECT: usize = 40;
    const MAX_INJECT_CHARS: usize = 128 * 1024;
    let mut injected: Vec<String> = Vec::new();
    let mut constant_msgs: Vec<GenerateRawMessage> = Vec::new();
    let mut triggered_msgs: Vec<GenerateRawMessage> = Vec::new();
    let mut budget = MAX_INJECT_CHARS;
    let roll = {
        use std::time::{SystemTime, UNIX_EPOCH};
        (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
            % 100) as i64
    };
    let all_texts: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();
    for e in entries {
        if !e.enabled || injected.len() >= MAX_INJECT {
            continue;
        }
        let hit = if e.constant {
            true
        } else {
            // 扫描窗口:depth<=0 全部;否则最近 depth 条消息
            let depth = if e.depth <= 0 {
                all_texts.len()
            } else {
                (e.depth as usize).min(all_texts.len())
            };
            let window = &all_texts[all_texts.len().saturating_sub(depth)..];
            if window.is_empty() {
                false
            } else {
                crate::parsing::world_book::entry_matches_texts(e, window)
                    && crate::parsing::world_book::entry_probability_pass(e, roll)
            }
        };
        if !hit {
            continue;
        }
        let content = e.content.trim();
        if content.is_empty() || content.len() > budget {
            continue;
        }
        budget -= content.len();
        let text = if e.comment.is_empty() {
            content.to_string()
        } else {
            format!("[{}]\n{}", e.comment, content)
        };
        let msg = GenerateRawMessage {
            role: "system".to_string(),
            content: text,
        };
        if e.constant {
            constant_msgs.push(msg);
        } else {
            triggered_msgs.push(msg);
        }
        injected.push(e.comment.clone());
    }
    if constant_msgs.is_empty() && triggered_msgs.is_empty() {
        return injected;
    }
    // 触发条目插到最后一条 user 消息之前;没有 user 则追加尾部
    if !triggered_msgs.is_empty() {
        let pos = messages
            .iter()
            .rposition(|m| m.role == "user")
            .unwrap_or(messages.len());
        let mut tail = messages.split_off(pos);
        messages.append(&mut triggered_msgs);
        messages.append(&mut tail);
    }
    // 常驻条目置顶
    if !constant_msgs.is_empty() {
        constant_msgs.append(messages);
        *messages = constant_msgs;
    }
    injected
}

/// POST /api/chat/generate-raw — 自由一次性生成(角色卡资源页作者脚本用)。
///
/// 信任模型:资源 iframe 是沙箱无 token,只能经宿主页面(postMessage 桥)调用本端点;
/// 宿主即已登录的聊天前端,生成能力与用户手动发消息等价。防御性限制:
/// 消息条数/单条长度/总长度设上限(作者脚本失控也不会打出超大请求);
/// role 白名单;始终非流式(作者页自行组装完整历史,无 SSE 需求)。
pub async fn generate_raw(
    State(state): State<Arc<AppState>>,
    Json(body): Json<GenerateRawBody>,
) -> Response {
    tracing::info!(
        messages = body.messages.len(),
        total_chars = body
            .messages
            .iter()
            .map(|m| m.content.len() as u64)
            .sum::<u64>(),
        "generate_raw_call"
    );
    const MAX_MESSAGES: usize = 200;
    const MAX_MSG_LEN: usize = 64 * 1024;
    const MAX_TOTAL_LEN: usize = 256 * 1024;
    if body.messages.is_empty() {
        return err_json("messages 不能为空", StatusCode::BAD_REQUEST);
    }
    if body.messages.len() > MAX_MESSAGES {
        return err_json("messages 过多(上限 200)", StatusCode::BAD_REQUEST);
    }
    let mut total = 0usize;
    for m in &body.messages {
        if !matches!(m.role.as_str(), "system" | "user" | "assistant") {
            return err_json("role 仅支持 system/user/assistant", StatusCode::BAD_REQUEST);
        }
        if m.content.len() > MAX_MSG_LEN {
            return err_json("单条消息过长(上限 64KB)", StatusCode::BAD_REQUEST);
        }
        total += m.content.len();
    }
    if total > MAX_TOTAL_LEN {
        return err_json("消息总长超限(256KB)", StatusCode::BAD_REQUEST);
    }

    // 世界书匹配注入(带 character_id 时):角色内嵌 character_book + 绑定/全局世界书,
    // 对齐酒馆 TavernHelper.generate 语义——作者页只自组历史与 user_input,
    // 世界书关键字命中由宿主核心注入(吸血鬼卡 "system log" → 输出格式规范)。
    let mut messages = body.messages;
    let mut injected: Vec<String> = Vec::new();
    if let Some(cid) = body
        .character_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let entries = {
            let characters = state.characters.clone();
            let world_books = state.world_books.clone();
            let cid = cid.to_string();
            tokio::task::spawn_blocking(move || {
                let mut entries: Vec<crate::parsing::world_book::WorldEntry> = Vec::new();
                if let Some(c) = characters.get(&cid) {
                    if let Some(raw) = c.data_raw.as_ref() {
                        entries.extend(crate::parsing::world_book::character_book_entries(raw));
                    }
                }
                entries.extend(world_books.collect_entries_for_character(&cid));
                entries
            })
            .await
            .unwrap_or_default()
        };
        injected = inject_worldbook_for_raw(&entries, &mut messages);
        if !injected.is_empty() {
            tracing::info!(
                character_id = cid,
                injected = injected.len(),
                "generate_raw_worldbook_inject"
            );
        }
    }

    let messages: Vec<crate::models::types::LlmMessage> = messages
        .iter()
        .map(|m| crate::models::types::LlmMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        })
        .collect();
    let params = {
        // 设置快照:不留锁跨 await
        let s = state.settings_snapshot();
        GenerationParams {
            temperature: body.temperature.unwrap_or(s.default_temperature),
            top_p: body.top_p.unwrap_or(s.default_top_p),
            max_tokens: body.max_tokens.unwrap_or(s.default_max_tokens),
            stop: None,
            tools: Vec::new(),
            max_tool_rounds: Some(1),
            tool_choice: crate::models::types::ToolChoice::None,
            parallel_tool_calls: None,
        }
    };
    let connector = state.engine.connector.read().await;
    let (_cancel_tx, abort_rx) = tokio::sync::watch::channel(false);
    match connector.generate(&messages, params, abort_rx).await {
        Ok(chunks) => {
            let text: String = chunks
                .iter()
                .filter_map(|c| match c {
                    crate::models::types::LlmStreamChunk::Token(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect();
            Json(json!({ "ok": true, "text": text, "injected": injected })).into_response()
        }
        Err(e) => err_json(format!("生成失败:{e}"), StatusCode::BAD_GATEWAY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        comment: &str,
        keys: Vec<&str>,
        content: &str,
        constant: bool,
        use_regex: bool,
    ) -> crate::parsing::world_book::WorldEntry {
        crate::parsing::world_book::WorldEntry {
            id: 0,
            comment: comment.to_string(),
            keys: keys.into_iter().map(|s| s.to_string()).collect(),
            keys_secondary: Vec::new(),
            regex: None,
            use_regex,
            content: content.to_string(),
            constant,
            enabled: true,
            position: 0,
            depth: 4,
            order: 100,
            case_sensitive: false,
            sticky: 0,
            cooldown: 0,
            probability: 100,
            use_probability: false,
            role: None,
            decorators: Vec::new(),
        }
    }

    fn msg(role: &str, content: &str) -> GenerateRawMessage {
        GenerateRawMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    /// 吸血鬼卡场景:作者页自组历史(assistant 消息携带 system log 注释行)+ user_input;
    /// constant 世界观条目置顶,system log 触发的格式规范插到最后一条 user 之前
    #[test]
    fn raw_inject_constant_head_triggered_before_last_user() {
        let entries = vec![
            entry("世界观", vec![], "吸血鬼世界观设定", true, false),
            entry(
                "🔗故事编写🔗",
                vec!["system log"],
                "<response_format_guidance>仅输出一个 JSON</response_format_guidance>",
                false,
                true,
            ),
            entry("无关条目", vec!["火车站"], "不应出现", false, false),
        ];
        let mut messages = vec![
            msg("system", "你是角色扮演模型"),
            msg("assistant", "{\n\t\"thinking\": \"...\",\n\t\"context\": {}\n}\n/* system log(IGNORE the line): continue */"),
            msg("user", "Read the xml tag `<user_input>`."),
        ];
        let injected = inject_worldbook_for_raw(&entries, &mut messages);
        assert_eq!(injected, vec!["世界观", "🔗故事编写🔗"]);
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[0].role, "system");
        assert!(
            messages[0].content.contains("吸血鬼世界观设定"),
            "常驻条目置顶"
        );
        // 触发条目在最后一条 user 之前
        assert_eq!(messages[3].role, "system");
        assert!(messages[3].content.contains("response_format_guidance"));
        assert_eq!(messages[4].role, "user");
    }

    /// 无命中时消息原样;disabled 条目跳过
    #[test]
    fn raw_inject_no_hit_keeps_messages() {
        let mut e = entry("格式", vec!["system log"], "x", false, false);
        let entries = vec![e.clone()];
        let mut messages = vec![msg("user", "普通对话")];
        let injected = inject_worldbook_for_raw(&entries, &mut messages);
        assert!(injected.is_empty());
        assert_eq!(messages.len(), 1);
        // disabled 跳过
        e.enabled = false;
        let mut messages2 = vec![msg("user", "含 system log 字样")];
        assert!(inject_worldbook_for_raw(&[e], &mut messages2).is_empty());
        assert_eq!(messages2.len(), 1);
    }

    /// depth 窗口:只扫最近 N 条消息,窗口外不命中
    #[test]
    fn raw_inject_depth_window() {
        let mut e = entry("格式", vec!["system log"], "格式规范", false, false);
        e.depth = 1;
        let entries = vec![e.clone()];
        // system log 在倒数第二条(depth=1 扫不到)
        let mut messages = vec![msg("assistant", "… system log …"), msg("user", "继续")];
        assert!(inject_worldbook_for_raw(&entries, &mut messages).is_empty());
        // depth=0 扫全部 → 命中
        e.depth = 0;
        let entries2 = vec![e];
        let mut messages2 = vec![msg("assistant", "… system log …"), msg("user", "继续")];
        assert_eq!(inject_worldbook_for_raw(&entries2, &mut messages2).len(), 1);
    }
}
