// 快速回复路由:/api/quick-replies(列表/新建/更新/删除)
use crate::api::app_state::AppState;
use crate::api::{db_err, not_found, validation, WithStatus};
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
    let svc = state.quick_replies.clone();
    let list = state
        .db_call(move || svc.list(only_enabled))
        .await
        .expect("读取快速回复任务失败");
    Json(json!({ "quick_replies": list }))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(body): Json<QuickReplyRecord>,
) -> Response {
    let svc = state.quick_replies.clone();
    match state.db_call(move || svc.create(body)).await {
        Err(e) => db_err(&e),
        Ok(Ok(record)) => Json(json!({ "ok": true, "quick_reply": record }))
            .into_response()
            .with_status(StatusCode::CREATED),
        Ok(Err(e)) => validation(e),
    }
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<QuickReplyRecord>,
) -> Response {
    let svc = state.quick_replies.clone();
    match state.db_call(move || svc.update(id, body)).await {
        Err(e) => db_err(&e),
        Ok(Some(record)) => Json(json!({ "ok": true, "quick_reply": record })).into_response(),
        Ok(None) => not_found("快速回复不存在"),
    }
}

pub async fn delete(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    let svc = state.quick_replies.clone();
    match state.db_call(move || svc.delete(id)).await {
        Err(e) => db_err(&e),
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found("快速回复不存在"),
    }
}
