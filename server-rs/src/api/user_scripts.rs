// 用户脚本路由:/api/scripts/tree(读取/全量保存 ScriptTree)
// scope=global(owner_id 恒为空)或 character(需 character_id 指定角色卡)。
// 角色级脚本存于角色卡 data_raw.extensions.tavern_helper(随卡导出),兼容 ST 卡格式。
use crate::api::app_state::AppState;
use crate::api::{db_err, WithStatus};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct TreeQuery {
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub character_id: Option<String>,
}

/// GET /api/scripts/tree?scope=global|character&character_id=
pub async fn get_tree(State(state): State<Arc<AppState>>, Query(q): Query<TreeQuery>) -> Response {
    let scope = q.scope.as_deref().unwrap_or("global");
    let owner = match scope {
        "global" => "",
        "character" => match q.character_id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) => id,
            None => {
                return Json(json!({ "error": "角色级脚本需指定 character_id" }))
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST)
            }
        },
        other => {
            return Json(
                json!({ "error": format!("未知脚本作用域: {other}(仅支持 global/character)") }),
            )
            .into_response()
            .with_status(StatusCode::BAD_REQUEST)
        }
    };
    let svc = state.user_scripts.clone();
    let scope_owned = scope.to_string();
    let owner_owned = owner.to_string();
    match state
        .db_call(move || svc.get_tree(&scope_owned, &owner_owned))
        .await
    {
        Err(e) => db_err(&e),
        Ok(Ok(trees)) => Json(json!({ "scope": scope, "trees": trees })).into_response(),
        Ok(Err(e)) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND),
    }
}

#[derive(Deserialize)]
pub struct SaveTreeBody {
    pub trees: Value,
}

/// PUT /api/scripts/tree?scope=global|character&character_id= — 全量覆盖保存
pub async fn save_tree(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TreeQuery>,
    Json(body): Json<SaveTreeBody>,
) -> Response {
    let scope = q.scope.as_deref().unwrap_or("global");
    let owner = match scope {
        "global" => "",
        "character" => match q.character_id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) => id,
            None => {
                return Json(json!({ "error": "角色级脚本需指定 character_id" }))
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST)
            }
        },
        other => {
            return Json(
                json!({ "error": format!("未知脚本作用域: {other}(仅支持 global/character)") }),
            )
            .into_response()
            .with_status(StatusCode::BAD_REQUEST)
        }
    };
    let svc = state.user_scripts.clone();
    let scope_owned = scope.to_string();
    let owner_owned = owner.to_string();
    let trees = body.trees.clone();
    match state
        .db_call(move || svc.save_tree(&scope_owned, &owner_owned, &trees))
        .await
    {
        Err(e) => db_err(&e),
        Ok(Ok(())) => Json(json!({ "ok": true, "scope": scope })).into_response(),
        Ok(Err(e)) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
    }
}
