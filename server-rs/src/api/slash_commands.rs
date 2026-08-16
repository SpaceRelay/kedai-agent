// slash 命令清单路由:/api/slash/commands(供前端输入框联想)
use crate::api::app_state::AppState;
use axum::extract::State;
use axum::Json;
use serde_json::json;
use std::sync::Arc;

pub async fn list_commands(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(json!({ "commands": state.slash.list() }))
}
