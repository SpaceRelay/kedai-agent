// 命令执行审计与执行器状态 API(阶段 B/E)。
//
// 端点:
//   GET    /api/exec/tier    当前执行器等级(前端「等级可见」展示)
//   GET    /api/exec/audit   审计列表(支持 source/risk 过滤与 limit)
//   DELETE /api/exec/audit   清空审计
//
// 安全说明:审计含命令原文与输出摘要,已随应用整体受 bootstrap token 鉴权保护
// (同其它 /api 端点);不额外放宽。
use crate::api::app_state::AppState;
use crate::api::errors::{err_with_code, ErrorCode};
use crate::services::exec::{audit, detect_tier};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// 审计查询参数(全部可选)。
#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    pub limit: Option<usize>,
    /// chat | task | android
    #[serde(default)]
    pub source: Option<String>,
    /// safe | sensitive | destructive | admin
    #[serde(default)]
    pub risk: Option<String>,
}

/// GET /api/exec/tier:当前可用执行器等级 + 是否支持 Shizuku 通道。
/// 前端授权面板据此展示「等级可见」并可提示用户开启通道。
pub async fn tier(State(_state): State<Arc<AppState>>) -> Response {
    let t = detect_tier();
    Json(json!({
        "tier": t.as_str(),
        "label": t.label(),
    }))
    .into_response()
}

/// GET /api/exec/audit:审计列表(时间倒序)。
pub async fn list_audit(
    State(state): State<Arc<AppState>>,
    Query(q): Query<AuditQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(100);
    let rows = audit::list(&state.db, limit, q.source.as_deref(), q.risk.as_deref());
    Json(json!({ "entries": rows })).into_response()
}

/// DELETE /api/exec/audit:清空审计(设置面板「清空」按钮)。
pub async fn clear_audit(State(state): State<Arc<AppState>>) -> Response {
    match audit::clear(&state.db) {
        Ok(n) => Json(json!({ "ok": true, "deleted": n })).into_response(),
        Err(e) => err_with_code(ErrorCode::Db, e, StatusCode::INTERNAL_SERVER_ERROR),
    }
}
