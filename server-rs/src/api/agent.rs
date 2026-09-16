// Agent 路由:/api/agent/plan、execute、interrupt
use crate::agents::planner::{make_custom_plan, make_plan};
use crate::api::app_state::AppState;
use crate::api::{err_status, validation};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct PlanBody {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub agent_mode: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Deserialize)]
pub struct SessionBody {
    #[serde(default)]
    pub session_id: Option<String>,
}

/// POST /api/agent/plan:预览行动计划(不执行)
pub async fn plan(State(state): State<Arc<AppState>>, Json(body): Json<PlanBody>) -> Response {
    let Some(raw) = body.message else {
        return validation("缺少 message");
    };
    let message = raw.trim().to_string();
    if message.is_empty() {
        return validation("缺少 message");
    }
    let mode = if body.agent_mode.as_deref() == Some("deep") {
        "deep"
    } else if body.agent_mode.as_deref() == Some("agent") {
        "agent"
    } else if body.agent_mode.as_deref() == Some("custom") {
        "custom"
    } else {
        "fast"
    };
    let plan = if mode == "custom" {
        // 自定义流程:从设置读取当前选中流程生成计划(未选择/未启用/非法 → 400)
        let flow = match state.flow.lock().unwrap_or_else(|e| e.into_inner()).get() {
            Some(f) => f.clone(),
            None => {
                return validation("未选择执行流程:请先在设置中新建或选择一个 Agent 执行流程");
            }
        };
        if !flow.enabled {
            return validation("自定义模式需要先在设置中启用并保存执行流程");
        }
        if let Err(e) = state
            .flow
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .validate(&flow)
        {
            return validation(e);
        }
        match make_custom_plan(&flow.steps) {
            Ok(p) => p,
            Err(e) => {
                return validation(e);
            }
        }
    } else {
        make_plan(&message, mode)
    };
    let tools: Vec<String> = state
        .tool_registry
        .list_definitions()
        .iter()
        .map(|t| t.name.clone())
        .collect();

    // 若有 session_id 且存在 Agent 会话记录,附上历史状态(同步 DB 读走阻塞线程池)
    let history = match &body.session_id {
        Some(sid) => {
            let svc = state.agent_sessions.clone();
            let sid = sid.clone();
            state
                .db_call(move || {
                    svc.find_by_session(&sid).map(|a| {
                        json!({
                            "state": a.state,
                            "plan": a.plan,
                            "step_index": a.step_index,
                        })
                    })
                })
                .await
                .ok()
                .flatten()
        }
        None => None,
    };

    Json(json!({
        "plan": plan,
        "summary": plan.summary,
        "tools": tools,
        "history": history,
    }))
    .into_response()
}

/// POST /api/agent/execute:未实现的历史桩，明确返回 501，避免客户端误判为已执行。
pub async fn execute(Json(_body): Json<SessionBody>) -> Response {
    err_status(
        "该接口尚未实现,请使用 /api/chat/send",
        StatusCode::NOT_IMPLEMENTED,
    )
}

/// POST /api/agent/interrupt:中断 Agent(等价 /api/chat/stop)
pub async fn interrupt(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SessionBody>,
) -> Response {
    let Some(sid) = body.session_id else {
        return validation("缺少 session_id");
    };
    if sid.trim().is_empty() {
        return validation("缺少 session_id");
    }
    state.engine.stop(&sid);
    Json(json!({ "ok": true })).into_response()
}

/// GET /api/chat/sessions/{id}/agent/trace:读取角色扮演模式下该会话的
/// Agent 记录(agent_sessions + tool_calls),供前端切会话/重启后恢复右侧
/// Agent 面板的「工具调用记录」(实跑问题 4:此前只有内存态,刷新即丢)。
/// 只读:不建会话、不改状态;无记录时返回 `null`,前端保持空闲态。
/// 说明:推理链(chain)明细无持久层,不在本响应内,恢复后仅工具调用可见。
pub async fn session_trace(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Response {
    if session_id.trim().is_empty() {
        return validation("缺少 session_id");
    }
    let svc = state.agent_sessions.clone();
    let sid = session_id.clone();
    let loaded = state
        .db_call(move || {
            svc.find_by_session(&sid)
                .map(|a| (a.id.clone(), a.state.clone(), a.plan.clone(), a.step_index))
        })
        .await;
    let (agent_id, agent_state, plan, step_index) = match loaded {
        Err(e) => return crate::api::db_err(&e),
        Ok(None) => return Json(json!({ "trace": null })).into_response(),
        Ok(Some(v)) => v,
    };
    let svc2 = state.agent_sessions.clone();
    let calls = state
        .db_call(move || svc2.list_tool_calls(&agent_id))
        .await
        .unwrap_or_default();
    Json(json!({
        "trace": {
            "state": agent_state,
            "plan": plan,
            "step_index": step_index,
            "tool_calls": calls,
        }
    }))
    .into_response()
}
