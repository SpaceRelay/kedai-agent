// 导入导出路由:/api/export/chat、/api/import/chat(SillyTavern 兼容)
use crate::api::app_state::AppState;
use crate::api::{db_err, WithStatus};
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
    let svc = state.sessions.clone();
    let sid_q = sid.clone();
    let messages = match state
        .db_call(move || {
            // 存在性校验 + 导出合并进同一阻塞任务(两次读取一次调度)
            svc.get(&sid_q)?;
            Some(svc.export_chat(&sid_q))
        })
        .await
    {
        Err(e) => return db_err(&e),
        Ok(None) => {
            return Json(json!({ "error": "会话不存在" }))
                .into_response()
                .with_status(StatusCode::NOT_FOUND)
        }
        Ok(Some(m)) => m,
    };
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
    let svc = state.sessions.clone();
    let sid_q = sid.clone();
    let imported = state
        .db_call(move || {
            // 存在性校验 + 导入合并进同一阻塞任务
            svc.get(&sid_q)?;
            Some(svc.import_chat(&sid_q, body.messages))
        })
        .await;
    match imported {
        Err(e) => db_err(&e),
        Ok(None) => Json(json!({ "error": "会话不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND),
        Ok(Some(Ok(n))) => Json(json!({ "ok": true, "imported": n })).into_response(),
        Ok(Some(Err(e))) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
    }
}
