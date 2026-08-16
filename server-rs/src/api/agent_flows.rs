// 自定义 Agent 执行流程配置:GET/PUT /api/agent-flows + POST /api/agent-flows/select
// + DELETE /api/agent-flows/{id}(全局流程库,持久化 data/agent_flows.json)。
// 响应统一携带 library{current_flow_id, flows} 与 config(当前选中流程,兼容旧调用方)。
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use crate::services::agent_flow_service::{AgentFlowConfig, AgentFlowLibrary};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// 统一返回体:library(流程库)+ config(当前选中流程,可能为 null)
fn flow_payload(lib: &AgentFlowLibrary) -> serde_json::Value {
    let config = lib
        .current_flow_id
        .as_ref()
        .and_then(|id| lib.flows.iter().find(|f| &f.id == id))
        .cloned();
    json!({ "ok": true, "library": lib, "config": config })
}

pub async fn get_agent_flows(State(state): State<Arc<AppState>>) -> Response {
    let lib = state.flow.lock().unwrap_or_else(|e| e.into_inner()).get_library().clone();
    Json(flow_payload(&lib)).into_response()
}

#[derive(Deserialize)]
pub struct UpdateFlowBody {
    pub config: AgentFlowConfig,
}

/// PUT /api/agent-flows:保存(创建或更新)流程并设为当前选中;
/// config.id 为空时视为新建(后端分配 id)。校验失败 400。
pub async fn update_agent_flows(
    State(state): State<Arc<AppState>>,
    Json(body): Json<UpdateFlowBody>,
) -> Response {
    // 显式 let 绑定 guard:match 内联临时 guard 会存活到 match 结束,
    // 分支内再次 lock() 会触发 std Mutex 重入死锁
    let set_result = state.flow.lock().unwrap_or_else(|e| e.into_inner()).set(body.config);
    match set_result {
        Ok(()) => {
            let lib = state.flow.lock().unwrap_or_else(|e| e.into_inner()).get_library().clone();
            Json(flow_payload(&lib)).into_response()
        }
        Err(e) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
    }
}

#[derive(Deserialize)]
pub struct SelectFlowBody {
    pub id: String,
}

/// POST /api/agent-flows/select:切换当前选中流程
pub async fn select_agent_flow(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SelectFlowBody>,
) -> Response {
    let select_result = state.flow.lock().unwrap_or_else(|e| e.into_inner()).select(&body.id);
    match select_result {
        Ok(()) => {
            let lib = state.flow.lock().unwrap_or_else(|e| e.into_inner()).get_library().clone();
            Json(flow_payload(&lib)).into_response()
        }
        Err(e) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
    }
}

/// DELETE /api/agent-flows/{id}:删除流程(删除当前流程时回退到第一个)
pub async fn delete_agent_flow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let remove_result = state.flow.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    match remove_result {
        Ok(()) => {
            let lib = state.flow.lock().unwrap_or_else(|e| e.into_inner()).get_library().clone();
            Json(flow_payload(&lib)).into_response()
        }
        Err(e) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
    }
}
