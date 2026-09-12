// 回退快照 API(批次 6.1「undo」):
//   GET  /api/chat/sessions/{id}/undo  列出会话回退快照(新→旧,不含 payload)
//   POST /api/undo/{id}/restore        按快照逆序执行逆操作恢复
// 快照产生点在 tools/registry.rs run_tool(两段式);本模块只做列表与恢复入口。
// 同步 DB/文件操作经 state.db_call 挪进阻塞线程池(DB 并发改造惯例)。
use crate::api::app_state::AppState;
use crate::api::{db_err, WithStatus};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::sync::Arc;

/// GET /api/chat/sessions/{id}/undo:快照列表(新→旧)
pub async fn list_snapshots(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Response {
    if session_id.trim().is_empty() {
        return err_json("缺少 session_id", StatusCode::BAD_REQUEST);
    }
    let undo = state.undo.clone();
    match state.db_call(move || undo.list(&session_id)).await {
        Err(e) => db_err(&e),
        Ok(snapshots) => Json(json!({ "snapshots": snapshots })).into_response(),
    }
}

/// POST /api/undo/{id}/restore:按 payload 逆序执行逆操作。
/// 成功后删除该快照及同会话更新的所有快照(回退到更早状态后,更新快照语义失效);
/// 失败返回 500 中文错误——已执行的部分不回滚(逆操作各自独立幂等,按提示手动处理)。
pub async fn restore_snapshot(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    if id.trim().is_empty() {
        return err_json("缺少快照 id", StatusCode::BAD_REQUEST);
    }
    let undo = state.undo.clone();
    match state.db_call(move || undo.restore(&id)).await {
        Err(e) => db_err(&e),
        Ok(Err(e)) => err_json(&e, StatusCode::INTERNAL_SERVER_ERROR),
        Ok(Ok(None)) => err_json("快照不存在或已被回退", StatusCode::NOT_FOUND),
        Ok(Ok(Some(label))) => Json(json!({ "ok": true, "restored": label })).into_response(),
    }
}

fn err_json(msg: &str, status: StatusCode) -> Response {
    Json(json!({ "error": msg }))
        .into_response()
        .with_status(status)
}
