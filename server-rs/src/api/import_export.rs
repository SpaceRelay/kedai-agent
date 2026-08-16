// 导入导出路由:/api/export/chat、/api/import/chat(SillyTavern 兼容)
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use crate::models::types::StMessage;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct SessionQuery {
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Deserialize)]
pub struct ImportBody {
    #[serde(default)]
    pub session_id: Option<String>,
    pub messages: Vec<StMessage>,
}

/// GET /api/export/chat:导出会话消息
pub async fn export_chat(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SessionQuery>,
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
    if state.sessions.get(&sid).is_none() {
        return Json(json!({ "error": "会话不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND);
    }
    let messages = state.sessions.export_chat(&sid);
    Json(json!({ "session_id": sid, "messages": messages })).into_response()
}

/// POST /api/import/chat:导入消息(先清空再逐条追加)
pub async fn import_chat(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ImportBody>,
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
    if state.sessions.get(&sid).is_none() {
        return Json(json!({ "error": "会话不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND);
    }
    match state.sessions.import_chat(&sid, body.messages) {
        Ok(imported) => Json(json!({ "ok": true, "imported": imported })).into_response(),
        Err(e) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
    }
}
