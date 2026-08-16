// 快速回复路由:/api/quick-replies(列表/新建/更新/删除)
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use crate::services::quick_reply_service::QuickReplyRecord;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub all: Option<bool>,
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Json<serde_json::Value> {
    let only_enabled = !q.all.unwrap_or(false);
    Json(json!({ "quick_replies": state.quick_replies.list(only_enabled) }))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(body): Json<QuickReplyRecord>,
) -> Response {
    match state.quick_replies.create(body) {
        Ok(record) => Json(json!({ "ok": true, "quick_reply": record }))
            .into_response()
            .with_status(StatusCode::CREATED),
        Err(e) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
    }
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<QuickReplyRecord>,
) -> Response {
    match state.quick_replies.update(id, body) {
        Some(record) => Json(json!({ "ok": true, "quick_reply": record })).into_response(),
        None => Json(json!({ "error": "快速回复不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND),
    }
}

pub async fn delete(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    if state.quick_replies.delete(id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        Json(json!({ "error": "快速回复不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND)
    }
}
