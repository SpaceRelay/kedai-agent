// 聊天路由(核心):/api/chat/send(SSE)、/api/chat/stop
use crate::agents::engine::AgentRunRequest;
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use crate::models::types::{GenerationParams, PlanStep, SseEvent};
use crate::services::prompt_inject_service::{output_budget_for_word_count, InjectMode};
use crate::utils::logger;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::{IntoResponse, Response, Sse};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
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
        return Json(json!({ "error": "消息不能为空" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }

    // 定位会话(指定 session_id 或按角色取首个)
    let session_id = match &body.session_id {
        Some(sid) => sid.clone(),
        None => {
            let Some(cid) = &body.character_id else {
                return Json(json!({ "error": "缺少 session_id 或 character_id" }))
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST);
            };
            match state.sessions.ensure_session(cid) {
                Ok(s) => {
                    // 无会话直接发消息的路径:同样补上角色开场白(若有且会话为空)
                    crate::api::sessions::seed_first_message(&state, &s.id, cid, 0);
                    s.id
                }
                Err(e) => return err_json(&e, StatusCode::INTERNAL_SERVER_ERROR),
            }
        }
    };
    let session = match state.sessions.get(&session_id) {
        Some(s) => s,
        None => {
            return Json(json!({ "error": "会话不存在" }))
                .into_response()
                .with_status(StatusCode::NOT_FOUND)
        }
    };
    if state.engine.is_active(&session_id) {
        return Json(json!({ "error": "该会话正在生成中" }))
            .into_response()
            .with_status(StatusCode::CONFLICT);
    }

    let mode = match body.agent_mode.as_deref() {
        Some("agent") => "agent",
        Some("deep") => "deep",
        Some("custom") => "custom",
        _ => "fast",
    };
    // 生成参数:请求体优先,否则回退运行期设置(前端可编辑)
    let (mut params, max_context_tokens) = {
        let s = state.settings.lock().unwrap_or_else(|e| e.into_inner());
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
        let inj = state.prompt_inject.lock().unwrap_or_else(|e| e.into_inner()).get().clone();
        if inj.mode == InjectMode::Simple
            && inj.simple.word_count_enabled
            && inj.simple.word_count > 0
        {
            let before = params.max_tokens;
            params.max_tokens =
                output_budget_for_word_count(inj.simple.word_count, params.max_tokens);
            if params.max_tokens != before {
                logger::info(
                    "简单模式字数注入:输出预算自动上调",
                    &[
                        ("word_count", Value::from(inj.simple.word_count)),
                        ("max_tokens_before", Value::from(before)),
                        ("max_tokens_after", Value::from(params.max_tokens)),
                    ],
                );
            }
        }
    }
    // AGENT 模式:把工具定义下发给模型(function calling);其余模式不启用
    // (custom 模式由引擎按步骤的工具配置注入)。多步变量工具(get_state/apply_patch)
    // 不进正文默认列表——它们只服务于多步变量驱动器与显式白名单,泄漏进正文会
    // 诱导模型在正文轮里误用变量补丁通道。
    if mode == "agent" {
        params.tools = state
            .tool_registry
            .list_definitions()
            .into_iter()
            .filter(|t| !matches!(t.name.as_str(), "get_state" | "apply_patch"))
            .collect();
    }
    // 自定义流程(custom 模式):校验启用与合法性,步骤快照随请求传入引擎
    let mut flow_steps: Vec<PlanStep> = Vec::new();
    if mode == "custom" {
        let flow = match state.flow.lock().unwrap_or_else(|e| e.into_inner()).get() {
            Some(f) => f.clone(),
            // 流程库为空或未选中(仅删除全部流程后出现)
            None => {
                return Json(
                    json!({ "error": "未选择执行流程:请先在设置中新建或选择一个 Agent 执行流程" }),
                )
                .into_response()
                .with_status(StatusCode::BAD_REQUEST);
            }
        };
        if !flow.enabled || flow.steps.is_empty() {
            return Json(json!({ "error": "自定义模式需要先在设置中启用并保存执行流程" }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST);
        }
        if let Err(e) = state.flow.lock().unwrap_or_else(|e| e.into_inner()).validate(&flow) {
            return Json(json!({ "error": format!("执行流程配置无效:{e}") }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST);
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
            state.pending_runs.lock().unwrap_or_else(|e| e.into_inner()).remove(&session_id);
            return err_json("重生成与重发锚点互斥,不能同时指定", StatusCode::BAD_REQUEST);
        }
        let valid = state
            .sessions
            .get_messages(&session_id)
            .last()
            .is_some_and(|m| m.id == message_id && m.role == "assistant");
        if !valid {
            state.pending_runs.lock().unwrap_or_else(|e| e.into_inner()).remove(&session_id);
            return err_json("重生成锚点无效或已过期", StatusCode::CONFLICT);
        }
    } else if let Some(message_id) = body.resend_message_id {
        let valid = state
            .sessions
            .get_messages(&session_id)
            .last()
            .is_some_and(|m| m.id == message_id && m.role == "user" && m.content == message);
        if !valid {
            state.pending_runs.lock().unwrap_or_else(|e| e.into_inner()).remove(&session_id);
            return err_json("重发消息锚点无效或已过期", StatusCode::CONFLICT);
        }
    } else {
        let model = state.engine.model();
        let prompt_tokens = {
            let mut ts = state.token_service.lock().unwrap_or_else(|e| e.into_inner());
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
            state.pending_runs.lock().unwrap_or_else(|e| e.into_inner()).remove(&session_id);
            return err_json(&e, StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    let pending_cancelled = state
        .pending_runs
        .lock().unwrap_or_else(|e| e.into_inner())
        .get(&session_id)
        .is_none_or(|flag| *flag.borrow());
    if pending_cancelled {
        state.pending_runs.lock().unwrap_or_else(|e| e.into_inner()).remove(&session_id);
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
        pending_runs.lock().unwrap_or_else(|e| e.into_inner()).remove(&run_session_id);
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
        return Json(json!({ "error": "缺少 session_id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    if sid.trim().is_empty() {
        return Json(json!({ "error": "缺少 session_id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    if let Some(cancel) = state.pending_runs.lock().unwrap_or_else(|e| e.into_inner()).get(&sid) {
        let _ = cancel.send(true);
    }
    state.engine.stop(&sid);
    Json(json!({ "ok": true })).into_response()
}

/// POST /api/chat/compact:手动压缩会话历史(阶段借鉴 harness)。
/// 模式为 off 时拒绝;历史不足时返回 compacted=false。压缩结果落库,原文消息保留(可逆)。
pub async fn compact(State(state): State<Arc<AppState>>, Json(body): Json<CompactBody>) -> Response {
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

fn err_json(msg: &str, code: StatusCode) -> Response {
    Json(json!({ "error": msg }))
        .into_response()
        .with_status(code)
}
