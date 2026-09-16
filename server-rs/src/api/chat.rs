// 聊天路由(核心):/api/chat/send(SSE)、/api/chat/stop
use crate::agents::engine::AgentRunRequest;
use crate::api::app_state::AppState;
use crate::api::json_body::JsonBody;
use crate::api::{db_err, err_status, upstream};
use crate::models::types::{GenerationParams, PlanStep, SseEvent};
use crate::services::prompt_inject_service::{output_budget_for_word_count, InjectMode};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use tracing::Instrument;

/// `pending_runs` 占位守卫:插入即武装,离开作用域(含早退 return、panic、任务被丢弃)
/// 自动移除。背景(2026-09-13 批次 2):此前清理逻辑散落在 5 处早退 + 1 处 spawn 收尾,
/// 若引擎在 `engine.run` 内 panic,spawn 收尾不执行 → 该会话条目永久残留,此后所有
/// 发送都 409(只能重启)。RAII 把「必然清理」变成类型保证,顺带删掉重复代码。
struct PendingRunGuard {
    pending:
        Arc<std::sync::Mutex<std::collections::HashMap<String, tokio::sync::watch::Sender<bool>>>>,
    session_id: String,
}

impl Drop for PendingRunGuard {
    fn drop(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.session_id);
    }
}

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

pub async fn send(
    State(state): State<Arc<AppState>>,
    JsonBody(body): JsonBody<SendBody>,
) -> Response {
    let message = body.message.unwrap_or_default().trim().to_string();
    if message.is_empty() {
        return err_status("消息不能为空", StatusCode::BAD_REQUEST);
    }

    // 定位会话(指定 session_id 或按角色取首个)
    let session_id = match &body.session_id {
        Some(sid) => sid.clone(),
        None => {
            let Some(cid) = &body.character_id else {
                return err_status("缺少 session_id 或 character_id", StatusCode::BAD_REQUEST);
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
                Ok(Err(e)) => return err_status(&e, StatusCode::INTERNAL_SERVER_ERROR),
            }
        }
    };
    let session = match state.sessions.get(&session_id) {
        Some(s) => s,
        None => return err_status("会话不存在", StatusCode::NOT_FOUND),
    };
    if state.engine.is_active(&session_id) {
        return err_status("该会话正在生成中", StatusCode::CONFLICT);
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
        params.tools =
            crate::tools::tool_sets::exclude_meta(state.tool_registry.list_definitions());
    }
    // 自定义流程(custom 模式):校验启用与合法性,步骤快照随请求传入引擎
    let mut flow_steps: Vec<PlanStep> = Vec::new();
    if mode == "custom" {
        let flow = match state.flow.lock().unwrap_or_else(|e| e.into_inner()).get() {
            Some(f) => f.clone(),
            // 流程库为空或未选中(仅删除全部流程后出现)
            None => {
                return err_status(
                    "未选择执行流程:请先在设置中新建或选择一个 Agent 执行流程",
                    StatusCode::BAD_REQUEST,
                );
            }
        };
        if !flow.enabled || flow.steps.is_empty() {
            return err_status(
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
            return err_status(format!("执行流程配置无效:{e}"), StatusCode::BAD_REQUEST);
        }
        flow_steps = flow.steps.into_iter().filter(|s| s.enabled).collect();
    }

    const MAX_GENERATION_TOKENS: u32 = 65_536;
    const MAX_CONTEXT_TOKENS: u32 = 1_048_576;
    if params.max_tokens == 0
        || params.max_tokens > MAX_GENERATION_TOKENS
        || max_context_tokens.is_some_and(|v| v == 0 || v > MAX_CONTEXT_TOKENS)
    {
        return err_status("生成参数超出安全上限", StatusCode::BAD_REQUEST);
    }

    // 原子占位必须发生在任何消息写入之前;所有前置校验已完成。
    // 守卫随作用域自动清理:早退 / panic / 任务丢弃都必然移除条目(见 PendingRunGuard)。
    let pending_guard = {
        let mut pending = state
            .guards
            .pending_runs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if pending.contains_key(&session_id) || state.engine.is_active(&session_id) {
            return err_status("该会话正在生成中", StatusCode::CONFLICT);
        }
        let (cancel, _receiver) = tokio::sync::watch::channel(false);
        pending.insert(session_id.clone(), cancel);
        drop(pending);
        PendingRunGuard {
            pending: state.guards.pending_runs.clone(),
            session_id: session_id.clone(),
        }
    };

    // 消息写入三路分支(互斥):
    //   1) 重生成锚点(regenerate_assistant_id):生成新版本,不落新 user 消息,
    //      引擎收尾原地更新该 assistant 消息行(swipes 追加)。
    //   2) 重发锚点(resend_message_id):复用最后一条 user 消息,不落新行。
    //   3) 常规:先落一条 user 消息再生成。
    if let Some(message_id) = body.regenerate_assistant_id {
        // 与重发锚点互斥:两个锚点同时出现属于请求错误
        if body.resend_message_id.is_some() {
            return err_status("重生成与重发锚点互斥,不能同时指定", StatusCode::BAD_REQUEST);
        }
        let valid = state
            .sessions
            .get_messages(&session_id)
            .last()
            .is_some_and(|m| m.id == message_id && m.role == "assistant");
        if !valid {
            return err_status("重生成锚点无效或已过期", StatusCode::CONFLICT);
        }
    } else if let Some(message_id) = body.resend_message_id {
        let valid = state
            .sessions
            .get_messages(&session_id)
            .last()
            .is_some_and(|m| m.id == message_id && m.role == "user" && m.content == message);
        if !valid {
            return err_status("重发消息锚点无效或已过期", StatusCode::CONFLICT);
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
            return err_status(&e, StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    let pending_cancelled = state
        .guards
        .pending_runs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&session_id)
        .is_none_or(|flag| *flag.borrow());
    if pending_cancelled {
        return err_status("生成已中断", StatusCode::CONFLICT);
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
    //
    // span 不跨 tokio::spawn 自动继承:此处显式建 span + `.instrument(...)` 闭合。
    // span 在此(仍在 http_request span 作用域内)创建,故父链含 http_request,
    // 引擎日志因此同时带上 sessionId 与 requestId(后者经 span extension 穿透)。
    // 既有手工传参(req.session_id)保持不变,那是既有契约。
    let run_span = tracing::info_span!("agent_run", sessionId = session_id.as_str());
    tokio::spawn(
        async move {
            // 占位守卫随任务存续:引擎正常结束、panic 或任务被丢弃都会自动清理(RAII),
            // 不再依赖 spawn 收尾语句执行(引擎 panic 时它不会执行)。
            let _pending_guard = pending_guard;
            let _ = engine.run(req, tx).await;
        }
        .instrument(run_span),
    );

    let stream = async_stream::stream! {
        while let Some(event) = rx.recv().await {
            let json_str = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
            let sse_event = Event::default().data(json_str);
            yield Ok::<Event, Infallible>(sse_event);
        }
    };

    // 装配单点在 api::util::sse_response(KeepAlive 30s + no-transform),
    // 与 /api/tasks/events 共用——两处必须一致,见该函数注释。
    super::sse_response(stream)
}

pub async fn stop(
    State(state): State<Arc<AppState>>,
    JsonBody(body): JsonBody<StopBody>,
) -> Response {
    let Some(sid) = body.session_id else {
        return err_status("缺少 session_id", StatusCode::BAD_REQUEST);
    };
    if sid.trim().is_empty() {
        return err_status("缺少 session_id", StatusCode::BAD_REQUEST);
    }
    if let Some(cancel) = state
        .guards
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
    JsonBody(body): JsonBody<CompactBody>,
) -> Response {
    let Some(sid) = body.session_id else {
        return err_status("缺少 session_id", StatusCode::BAD_REQUEST);
    };
    if sid.trim().is_empty() {
        return err_status("缺少 session_id", StatusCode::BAD_REQUEST);
    }
    // 会话正在生成时拒绝(避免与生成流程的 auto 压缩/历史读取并发)
    if state.engine.is_active(&sid) {
        return err_status("该会话正在生成中,暂不能压缩", StatusCode::CONFLICT);
    }
    match state.engine.compact_session(&sid).await {
        Ok(compacted) => Json(json!({ "ok": true, "compacted": compacted })).into_response(),
        Err(e) => err_status(&e, StatusCode::BAD_REQUEST),
    }
}

/// POST /api/chat/compact/clear:撤销压缩,恢复完整原文历史(阶段借鉴 harness)。
/// 无摘要时返回 cleared=false;原文消息从未删除,清除摘要行即恢复。
pub async fn clear_compact(
    State(state): State<Arc<AppState>>,
    JsonBody(body): JsonBody<CompactBody>,
) -> Response {
    let Some(sid) = body.session_id else {
        return err_status("缺少 session_id", StatusCode::BAD_REQUEST);
    };
    if sid.trim().is_empty() {
        return err_status("缺少 session_id", StatusCode::BAD_REQUEST);
    }
    if state.engine.is_active(&sid) {
        return err_status("该会话正在生成中,暂不能恢复", StatusCode::CONFLICT);
    }
    match state.engine.clear_compaction(&sid) {
        Ok(cleared) => Json(json!({ "ok": true, "cleared": cleared })).into_response(),
        Err(e) => err_status(&e, StatusCode::INTERNAL_SERVER_ERROR),
    }
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

#[derive(Deserialize, Clone)]
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
    // 概率 roll 与判定顺序已收敛到 prompt_kit 单点(收敛前本处自算 subsec_nanos%100,
    // 与引擎侧 simple_roll 不同源;见 services::prompt_kit 的原语注释)。
    let roll = crate::services::prompt_kit::world_entry_roll();
    let all_texts: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();
    for e in entries {
        if !e.enabled || injected.len() >= MAX_INJECT {
            continue;
        }
        let hit = if e.constant {
            true
        } else {
            // 本场景的判定参数(与引擎侧的差异**显式**表达,不再靠各自隐式实现):
            // - 扫描**全部消息**(不分角色):作者页自组的是扁平完整上下文
            //   (chat_history 整段压成一条消息、触发词常缀在该消息开头的
            //   `/* system log … */` 注释行里),不存在「最近聊天」概念;
            // - 未声明 scan_depth 时兜底 0(= 扫全部):条目 `depth`(ST 语义是插入深度)
            //   不参与窗口——用它收窄会让触发条目永不命中(2026-09-14 吸血鬼卡丢格式根因)。
            crate::services::prompt_kit::triggered_entry_hits(e, &all_texts, 0, roll)
        };
        if !hit {
            continue;
        }
        let content = e.content.trim();
        if content.is_empty() || content.len() > budget {
            continue;
        }
        budget -= content.len();
        // keep_empty_bracket=false:本场景 comment 为空时不产生空 `[]` 行
        // (与引擎侧的约定不同,差异经参数显式表达;单点实现见 prompt_kit::world_entry_text)。
        let text = crate::services::prompt_kit::world_entry_text(&e.comment, content, false);
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
    JsonBody(body): JsonBody<GenerateRawBody>,
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
        return err_status("messages 不能为空", StatusCode::BAD_REQUEST);
    }
    if body.messages.len() > MAX_MESSAGES {
        return err_status("messages 过多(上限 200)", StatusCode::BAD_REQUEST);
    }
    let mut total = 0usize;
    for m in &body.messages {
        if !matches!(m.role.as_str(), "system" | "user" | "assistant") {
            return err_status("role 仅支持 system/user/assistant", StatusCode::BAD_REQUEST);
        }
        if m.content.len() > MAX_MSG_LEN {
            return err_status("单条消息过长(上限 64KB)", StatusCode::BAD_REQUEST);
        }
        total += m.content.len();
    }
    if total > MAX_TOTAL_LEN {
        return err_status("消息总长超限(256KB)", StatusCode::BAD_REQUEST);
    }
    // 显式 max_tokens=0 属非法(上游 OpenAI 兼容端同样要求 ≥1):直接 400,不静默
    // 当默认值——否则调用方以为「0 表示不限制」,实际拿到的是下限之外的意外小预算。
    if body.max_tokens == Some(0) {
        return err_status(
            "max_tokens 非法:必须 ≥ 1,或省略以使用默认预算",
            StatusCode::BAD_REQUEST,
        );
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
        let entry_count = entries.len();
        injected = inject_worldbook_for_raw(&entries, &mut messages);
        // 无论是否命中都记录:注入 0 条是「丢格式」的头号嫌疑(命中数=0 与
        // character_id 缺失是两种不同故障,必须有日志可区分)。
        if injected.is_empty() {
            tracing::warn!(
                character_id = cid,
                entries = entry_count,
                injected = 0,
                "generate_raw_worldbook_inject_empty"
            );
        } else {
            tracing::info!(
                character_id = cid,
                entries = entry_count,
                injected = injected.len(),
                "generate_raw_worldbook_inject"
            );
        }
    } else {
        // 缺 character_id 时注入整段跳过,模型自由发挥 → 卡片 JSON 解析失败。
        // 作者页依赖宿主 bridge 传当前角色 id,缺失属配置/时序问题,必须留痕。
        tracing::warn!(
            messages = messages.len(),
            "generate_raw_without_character_id"
        );
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
    // 结构化输出预算:作者页(吸血鬼卡等)要求整个回复有且仅有一个 JSON,截断即等于失败。
    let mut budget = generate_raw_budget(body.max_tokens, params.max_tokens);
    let connector = state.engine.connector.read().await.clone();
    let (_cancel_tx, abort_rx) = tokio::sync::watch::channel(false);
    let mut heal_rounds = 0u32;
    // 总调用轮次(首轮 + 自愈重发):诊断日志用它统计「白烧」成本(本端点不落
    // usage/审计,重发消耗只能从日志看)。
    let mut attempts = 0u32;
    // 上一次(截断但至少可读的)产出:自愈重发失败时回退它,而不是把
    // 「本来能拿到半截文本」变成整体报错(卡片的报错提示比半截文本更无用)。
    let mut last_text: Option<String> = None;
    loop {
        attempts += 1;
        let mut attempt = params.clone();
        attempt.max_tokens = budget;
        let chunks = match connector
            .generate(&messages, attempt, abort_rx.clone())
            .await
        {
            Ok(chunks) => chunks,
            Err(e) => {
                return match last_text {
                    Some(prev) => {
                        tracing::warn!(
                            error = %e,
                            attempts,
                            budget,
                            chars = prev.len(),
                            "generate_raw_retry_failed_fallback_partial"
                        );
                        Json(json!({ "ok": true, "text": prev, "injected": injected }))
                            .into_response()
                    }
                    // 上游失败:原文只进日志(可能含连接串/上游报错细节),响应体给稳定文案
                    None => upstream(format!("生成失败:{e}")),
                };
            }
        };
        let text: String = chunks
            .iter()
            .filter_map(|c| match c {
                crate::models::types::LlmStreamChunk::Token(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        let finish = chunks.iter().rev().find_map(|c| match c {
            crate::models::types::LlmStreamChunk::Finish { reason } => Some(reason.clone()),
            _ => None,
        });
        match generate_raw_heal_budget(finish.as_deref(), budget, heal_rounds) {
            Some(next) => {
                tracing::warn!(
                    used = budget,
                    next,
                    chars = text.len(),
                    "generate_raw_truncated_retry"
                );
                last_text = Some(text);
                budget = next;
                heal_rounds += 1;
            }
            None => {
                if finish.as_deref() == Some("length") {
                    // 自愈用尽仍被截断:卡片会把这当作「丢格式」,留可诊断痕迹
                    tracing::warn!(
                        budget,
                        chars = text.len(),
                        attempts,
                        "generate_raw_still_truncated"
                    );
                } else if attempts > 1 {
                    // 自愈重发后成功:记录总轮次与最终长度,重发消耗可从日志统计
                    tracing::warn!(
                        budget,
                        chars = text.len(),
                        attempts,
                        "generate_raw_healed_after_retry"
                    );
                }
                return Json(json!({ "ok": true, "text": text, "injected": injected }))
                    .into_response();
            }
        }
    }
}

/// generate-raw 的最小输出预算。
///
/// 角色卡资源页(吸血鬼卡等)要求模型「整个回复有且仅有一个 JSON」,完整响应含
/// thinking / self_check / context / character_state 等字段,常达数千 token;
/// 而全局默认 `default_max_tokens = 1024` 会让 JSON 在字段中途腰斩,卡片
/// `JSON.parse` 失败 → 弹出「丢格式」并把叙事原文当纯文本显示(2026-09-13
/// Android 真机复现)。本路径输出是一个必须完整的结构化对象,截断即等于失败,
/// 故给它专属下限(不改用户的全局设置语义,普通聊天仍尊重用户设置)。
const GENERATE_RAW_MIN_TOKENS: u32 = 8192;
/// 截断自愈的预算上限:一次翻倍即可覆盖绝大多数长卡,又不会顶爆上游模型输出上限。
const GENERATE_RAW_RETRY_MAX_TOKENS: u32 = 32768;
/// 请求显式 max_tokens 的合法上限:与 settings 侧 `1..=131072` 校验区间保持一致,
/// 超限钳制而非报错(调用方多是资源页脚本,尽量可用;0 已在入口 400 拒绝)。
const GENERATE_RAW_MAX_TOKENS_LIMIT: u32 = 131_072;

/// 卡片生成的输出预算:调用方显式指定时尊重之(钳在 1..=131072),未指定时取
/// 「用户设置 vs 结构化下限」的较大者(用户把全局 max_tokens 调大到下限之上时用用户的)。
fn generate_raw_budget(requested: Option<u32>, setting: u32) -> u32 {
    match requested {
        Some(v) => v.clamp(1, GENERATE_RAW_MAX_TOKENS_LIMIT),
        None => setting.clamp(GENERATE_RAW_MIN_TOKENS, GENERATE_RAW_MAX_TOKENS_LIMIT),
    }
}

/// `finish_reason=length` 时的重发预算(截断自愈):翻倍并封顶,最多重发 2 次。
/// 本路径专属常量 GENERATE_RAW_RETRY_MAX_TOKENS;算法与触发条件收敛在
/// `utils::retry::truncation_heal_budget`(2026-09-13 批次 4.1 四路合一)。
/// 旧条件 `used < CAP` 由公共函数的「未增长即 None」等价覆盖(used≥CAP 两者都 None);
/// 唯一差异在 used==0(旧实现 Some(0)/现 None)——本路径预算经 generate_raw_budget
/// 恒 ≥1(入口已 400 拒绝显式 0,函数内再兜底钳到 1),故此差异不可达,不造特例。
fn generate_raw_heal_budget(finish_reason: Option<&str>, used: u32, rounds: u32) -> Option<u32> {
    crate::utils::retry::truncation_heal_budget(
        finish_reason,
        used,
        rounds,
        GENERATE_RAW_RETRY_MAX_TOKENS,
        2,
    )
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
            scan_depth: None,
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

    /// 扫描窗口只认 scan_depth(ST scanDepth);条目 depth 是插入深度,不参与窗口。
    /// **回归护栏**:2026-09-14 吸血鬼卡「丢格式」根因——格式条目 extensions.depth=1
    /// 被误当扫描窗口,窗口收窄到 1 条,带 `system log` 触发词的 chat_history 永不命中。
    #[test]
    fn raw_inject_scan_depth_window_not_entry_depth() {
        let mut e = entry("格式", vec!["system log"], "格式规范", false, false);
        // 条目 depth=1(插入深度)但未声明 scan_depth:作者页上下文很短,
        // 全量扫描必须命中(旧实现在此不命中 → 格式规范不注入)
        e.depth = 1;
        let messages = vec![
            msg("user", "/* system log(IGNORE the line): */ 开场"),
            msg("user", "Read the xml tag `<user_input>`."),
        ];
        let mut first = messages.clone();
        assert_eq!(
            inject_worldbook_for_raw(&[e.clone()], &mut first).len(),
            1,
            "条目 depth 不得收窄 generate-raw 的扫描窗口"
        );

        // 显式声明 scan_depth=1 时才收窄:触发词在窗口外 → 不命中
        let narrow = entry("格式", vec!["system log"], "格式规范", false, false);
        let mut narrow = narrow;
        narrow.scan_depth = Some(1);
        let mut only_tail = messages.clone();
        assert!(inject_worldbook_for_raw(&[narrow.clone()], &mut only_tail).is_empty());
        // scan_depth=0 = 全部历史 → 命中
        let mut all = narrow;
        all.scan_depth = Some(0);
        let mut all_msgs = messages;
        assert_eq!(inject_worldbook_for_raw(&[all], &mut all_msgs).len(), 1);
    }

    /// 真实吸血鬼卡形状:作者页把整段 chat_history 压成一条 user 消息,触发词
    /// `system log` 缀在该消息开头的注释行里;格式条目 depth 声明为 1。
    /// 修复前该条目永不注入 → 卡片 JSON 解析失败报「丢格式」。
    #[test]
    fn raw_inject_vampire_card_format_entry_hits() {
        let format = entry(
            "[INS]🔗响应格式🔗[FORMAT]",
            vec!["system log"],
            "<response_format_guidance>有且仅有一个 JSON</response_format_guidance>",
            false,
            true,
        );
        // 该卡权威数据:格式条目 extensions.depth=1, extensions.scan_depth 缺省
        let mut format = format;
        format.depth = 1;
        let world = entry("🐧核心叙事🐧", vec![], "常驻世界观", true, false);

        let mut messages = vec![
            msg(
                "user",
                "/* system log(IGNORE the line): */\n<chat_history>\n## bootstrap_00\n</chat_history>",
            ),
            msg("system", "<current_status>当前暂无历史记录</current_status>"),
            msg("user", "\nRead the xml tag `<user_input>`.\n"),
        ];
        let injected = inject_worldbook_for_raw(&[world, format], &mut messages);
        assert!(
            injected.iter().any(|c| c.contains("响应格式")),
            "格式规范条目必须注入(否则卡片报丢格式):{injected:?}"
        );
    }

    /// 卡片生成预算:未显式指定时不低于结构化下限,避免默认 1024 截断 JSON
    /// (2026-09-13 Android 真机「丢格式」根因)
    #[test]
    fn raw_budget_applies_structured_floor() {
        // 默认 1024(Android 首装)→ 抬到下限
        assert_eq!(generate_raw_budget(None, 1024), GENERATE_RAW_MIN_TOKENS);
        // 用户已调到下限之上 → 尊重用户
        assert_eq!(generate_raw_budget(None, 10000), 10000);
        // 作者页显式指定 → 原样尊重(不擅自抬高)
        assert_eq!(generate_raw_budget(Some(512), 1024), 512);
        assert_eq!(generate_raw_budget(Some(2048), 1024), 2048);
        // 显式值钳在上限内(与 settings 1..=65536 同区间;0 由入口 400 拒绝,
        // 此处兜底钳到 1,防直接调用绕过校验)
        assert_eq!(generate_raw_budget(Some(0), 1024), 1);
        assert_eq!(
            generate_raw_budget(Some(u32::MAX), 1024),
            GENERATE_RAW_MAX_TOKENS_LIMIT
        );
        assert_eq!(
            generate_raw_budget(None, u32::MAX),
            GENERATE_RAW_MAX_TOKENS_LIMIT,
            "用户设置异常大时同样钳上限"
        );
    }

    /// 截断自愈:仅 finish=length 触发翻倍、封顶、次数上限;stop/None 不重发
    #[test]
    fn raw_heal_budget_only_on_length_with_cap_and_round_limit() {
        assert_eq!(
            generate_raw_heal_budget(Some("length"), GENERATE_RAW_MIN_TOKENS, 0),
            Some(GENERATE_RAW_MIN_TOKENS * 2)
        );
        assert_eq!(
            generate_raw_heal_budget(Some("length"), 20000, 1),
            Some(GENERATE_RAW_RETRY_MAX_TOKENS)
        );
        // 已到封顶值 → 不再重发
        assert_eq!(
            generate_raw_heal_budget(Some("length"), GENERATE_RAW_RETRY_MAX_TOKENS, 0),
            None
        );
        // 重发次数用尽 → 不再重发
        assert_eq!(
            generate_raw_heal_budget(Some("length"), GENERATE_RAW_MIN_TOKENS, 2),
            None
        );
        assert_eq!(generate_raw_heal_budget(Some("stop"), 1024, 0), None);
        assert_eq!(generate_raw_heal_budget(None, 1024, 0), None);
    }

    /// 占位守卫:离开作用域必然移除条目——含 panic 路径
    /// (2026-09-13 批次 2:修复「引擎 panic → 该会话此后永久 409」)
    #[test]
    fn pending_run_guard_removes_entry_on_drop_and_panic() {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        let map = Arc::new(Mutex::new(HashMap::new()));
        {
            let (tx, _rx) = tokio::sync::watch::channel(false);
            map.lock().unwrap().insert("s1".to_string(), tx);
            let guard = PendingRunGuard {
                pending: map.clone(),
                session_id: "s1".into(),
            };
            assert!(map.lock().unwrap().contains_key("s1"), "持有期间占位应在位");
            drop(guard);
        }
        assert!(!map.lock().unwrap().contains_key("s1"), "drop 后应移除");

        // panic 路径:catch_unwind 捕获后条目也必须已移除(修复的核心场景)
        let map_for_panic = map.clone();
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // 静音预期的 panic 输出
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (tx, _rx) = tokio::sync::watch::channel(false);
            map_for_panic.lock().unwrap().insert("s2".to_string(), tx);
            let _guard = PendingRunGuard {
                pending: map_for_panic.clone(),
                session_id: "s2".into(),
            };
            panic!("模拟引擎 panic");
        }));
        std::panic::set_hook(hook);
        assert!(result.is_err(), "应捕获到 panic");
        assert!(!map.lock().unwrap().contains_key("s2"), "panic 后应移除");
    }
}
