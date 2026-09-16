// 自定义 Agent 执行流程配置:GET/PUT /api/agent-flows + POST /api/agent-flows/select
// + DELETE /api/agent-flows/{id}(全局流程库,持久化 data/agent_flows.json)。
// 响应统一携带 library{current_flow_id, flows} 与 config(当前选中流程,兼容旧调用方)。
use crate::api::app_state::AppState;
use crate::api::{internal, validation};
use crate::services::agent_flow_service::{AgentFlowConfig, AgentFlowLibrary};
use axum::extract::{Path, State};
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
    let lib = state
        .flow
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_library()
        .clone();
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
    // B-1:set 内含同步 JSON 落盘,整体(持锁 + 校验 + 写文件)挪阻塞线程池;
    // 显式持锁在闭包内一次完成(set 后直接读库),避免 std Mutex 重入死锁
    let flow = state.flow.clone();
    let result = state
        .db_call(move || {
            let mut svc = flow.lock().unwrap_or_else(|e| e.into_inner());
            svc.set(body.config).map(|()| svc.get_library().clone())
        })
        .await;
    match result {
        Ok(Ok(lib)) => Json(flow_payload(&lib)).into_response(),
        Ok(Err(e)) => validation(e),
        Err(e) => internal(e),
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
    // B-1:select 内含同步落盘,持锁 + 写文件整体挪阻塞线程池
    let flow = state.flow.clone();
    let result = state
        .db_call(move || {
            let mut svc = flow.lock().unwrap_or_else(|e| e.into_inner());
            svc.select(&body.id).map(|()| svc.get_library().clone())
        })
        .await;
    match result {
        Ok(Ok(lib)) => Json(flow_payload(&lib)).into_response(),
        Ok(Err(e)) => validation(e),
        Err(e) => internal(e),
    }
}

/// DELETE /api/agent-flows/{id}:删除流程(删除当前流程时回退到第一个)
pub async fn delete_agent_flow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    // B-1:remove 内含同步落盘,持锁 + 写文件整体挪阻塞线程池
    let flow = state.flow.clone();
    let result = state
        .db_call(move || {
            let mut svc = flow.lock().unwrap_or_else(|e| e.into_inner());
            svc.remove(&id).map(|()| svc.get_library().clone())
        })
        .await;
    match result {
        Ok(Ok(lib)) => Json(flow_payload(&lib)).into_response(),
        Ok(Err(e)) => validation(e),
        Err(e) => internal(e),
    }
}
