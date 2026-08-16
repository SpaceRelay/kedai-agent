// Token 计数路由:/api/token/count + /api/token/session-total + /api/token/global-total
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use crate::models::types::LlmMessage;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct TokenCountBody {
    pub messages: Vec<LlmMessage>,
    #[serde(default)]
    pub model: Option<String>,
}

pub async fn count(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TokenCountBody>,
) -> Response {
    if body.messages.is_empty() {
        return Json(json!({ "error": "缺少 messages 数组" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let model = body.model.unwrap_or_else(|| state.engine.model());
    let total = {
        let mut ts = state.token_service.lock().unwrap_or_else(|e| e.into_inner());
        ts.count_message_tokens(&body.messages, &model)
    };
    Json(json!({ "total": total, "model": model })).into_response()
}

#[derive(Deserialize)]
pub struct SessionTotalQuery {
    pub session_id: String,
}

/// GET /api/token/session-total?session_id=xxx:返回当前会话累计 token
pub async fn session_total(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SessionTotalQuery>,
) -> Response {
    let conn = state.db.conn();
    let row: Option<(i64, i64, i64)> = conn
        .query_row(
            "SELECT total_prompt, total_completion, total_tokens FROM session_usage WHERE session_id = ?1",
            [&q.session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    match row {
        Some((p, c, t)) => Json(json!({
            "session_id": q.session_id,
            "total_prompt": p,
            "total_completion": c,
            "total_tokens": t,
        }))
        .into_response(),
        None => Json(json!({
            "session_id": q.session_id,
            "total_prompt": 0,
            "total_completion": 0,
            "total_tokens": 0,
        }))
        .into_response(),
    }
}

/// GET /api/token/global-total:返回全局累计 token
pub async fn global_total(State(state): State<Arc<AppState>>) -> Response {
    let conn = state.db.conn();
    let row: Option<(i64, i64, i64)> = conn
        .query_row(
            "SELECT total_prompt, total_completion, total_tokens FROM global_usage WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    match row {
        Some((p, c, t)) => Json(json!({
            "total_prompt": p,
            "total_completion": c,
            "total_tokens": t,
        }))
        .into_response(),
        None => Json(json!({
            "total_prompt": 0,
            "total_completion": 0,
            "total_tokens": 0,
        }))
        .into_response(),
    }
}

/// 更新 token 累计(会话 + 全局);供 chat.rs SSE finish 事件后调用
pub fn accumulate_usage(state: &AppState, session_id: &str, prompt: i64, completion: i64) {
    let total = prompt + completion;
    let now = crate::models::db::now_iso();
    let conn = state.db.conn();
    // 会话累计
    let _ = conn.execute(
        "INSERT INTO session_usage (session_id, total_prompt, total_completion, total_tokens, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(session_id) DO UPDATE SET
           total_prompt = total_prompt + ?2,
           total_completion = total_completion + ?3,
           total_tokens = total_tokens + ?4,
           updated_at = ?5",
        rusqlite::params![session_id, prompt, completion, total, now],
    );
    // 全局累计
    let _ = conn.execute(
        "UPDATE global_usage SET
           total_prompt = total_prompt + ?1,
           total_completion = total_completion + ?2,
           total_tokens = total_tokens + ?3,
           updated_at = ?4
         WHERE id = 1",
        rusqlite::params![prompt, completion, total, now],
    );
}
