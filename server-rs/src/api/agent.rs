// Agent 路由:/api/agent/plan、execute、interrupt
use crate::agents::planner::{make_custom_plan, make_plan};
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use axum::extract::State;
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
        return Json(json!({ "error": "缺少 message" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    let message = raw.trim().to_string();
    if message.is_empty() {
        return Json(json!({ "error": "缺少 message" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
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
                return Json(
                    json!({ "error": "未选择执行流程:请先在设置中新建或选择一个 Agent 执行流程" }),
                )
                .into_response()
                .with_status(StatusCode::BAD_REQUEST);
            }
        };
        if !flow.enabled {
            return Json(json!({ "error": "自定义模式需要先在设置中启用并保存执行流程" }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST);
        }
        if let Err(e) = state.flow.lock().unwrap_or_else(|e| e.into_inner()).validate(&flow) {
            return Json(json!({ "error": e }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST);
        }
        match make_custom_plan(&flow.steps) {
            Ok(p) => p,
            Err(e) => {
                return Json(json!({ "error": e }))
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST);
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

    // 若有 session_id 且存在 Agent 会话记录,附上历史状态
    let history = body.session_id.as_ref().and_then(|sid| {
        state.agent_sessions.find_by_session(sid).map(|a| {
            json!({
                "state": a.state,
                "plan": a.plan,
                "step_index": a.step_index,
            })
        })
    });

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
    Json(json!({ "error": "该接口尚未实现,请使用 /api/chat/send" }))
        .into_response()
        .with_status(StatusCode::NOT_IMPLEMENTED)
}

/// POST /api/agent/interrupt:中断 Agent(等价 /api/chat/stop)
pub async fn interrupt(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SessionBody>,
) -> Response {
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
    state.engine.stop(&sid);
    Json(json!({ "ok": true })).into_response()
}
