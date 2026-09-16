// 用户脚本路由:/api/scripts/tree(读取/全量保存 ScriptTree)
// scope=global(owner_id 恒为空)或 character(需 character_id 指定角色卡)。
// 角色级脚本存于角色卡 data_raw.extensions.tavern_helper(随卡导出),兼容 ST 卡格式。
use crate::api::app_state::AppState;
use crate::api::{db_err, err_with_code, not_found, validation, ErrorCode, WithStatus};
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
            None => return validation("角色级脚本需指定 character_id"),
        },
        other => return validation(format!("未知脚本作用域: {other}(仅支持 global/character)")),
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
        Ok(Err(e)) => not_found(e),
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
            None => return validation("角色级脚本需指定 character_id"),
        },
        other => return validation(format!("未知脚本作用域: {other}(仅支持 global/character)")),
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
        Ok(Err(e)) => validation(e),
    }
}

// ---------------- 角色卡脚本授权台账(2026-09-14,known-limitations L12) ----------------
//
// 端点:
//   GET    /api/script-authorizations?character_id=   查询授权态(含实时计算的 current_hash)
//   PUT    /api/script-authorizations                 授权(hash 由前端回传后端下发的值)
//   DELETE /api/script-authorizations?character_id=   撤销
//   GET    /api/script-authorizations/list            全部已授权角色
//
// **哈希唯一来源是后端**:GET 返回 `current_hash`,前端授权时原样回传。这样不存在
// 「前后端各算一遍哈希、算法漂移导致永久不匹配」的风险——只有一份实现
// (见 services/script_authorization_service.rs 的 compute_hash)。

#[derive(Deserialize)]
pub struct AuthorizationQuery {
    #[serde(default)]
    pub character_id: Option<String>,
}

#[derive(Deserialize)]
pub struct GrantBody {
    pub character_id: String,
    /// GET 接口下发的 current_hash(后端会校验其与实时计算值一致)
    pub script_hash: String,
}

/// 取角色当前启用脚本集(授权门与状态查询共用同一取数口径)。
/// 同步 DB 读,调用方负责放进 `db_call` 或阻塞池。
fn card_scripts_of(
    user_scripts: &crate::services::user_script_service::UserScriptService,
    character_id: &str,
) -> Result<Vec<crate::scripts::loader::LoadedScript>, String> {
    let tree = user_scripts.get_character(character_id)?;
    Ok(crate::scripts::loader::collect_enabled_scripts(&tree))
}

/// GET /api/script-authorizations?character_id=
pub async fn get_authorization(
    State(state): State<Arc<AppState>>,
    Query(q): Query<AuthorizationQuery>,
) -> Response {
    let Some(character_id) = q.character_id.as_deref().filter(|s| !s.is_empty()) else {
        return err_with_code(
            ErrorCode::Validation,
            "需指定 character_id",
            StatusCode::BAD_REQUEST,
        );
    };
    // 脚本集与台账读同源,故在同一阻塞任务内完成(避免两次读之间卡被改)。
    let user_scripts = state.user_scripts.clone();
    let svc = state.script_authorizations.clone();
    let id = character_id.to_string();
    match state
        .db_call(move || {
            let scripts = card_scripts_of(&user_scripts, &id)?;
            svc.status(&id, &scripts)
        })
        .await
    {
        Err(e) => db_err(&e),
        Ok(Ok(status)) => Json(json!(status)).into_response(),
        Ok(Err(e)) => err_with_code(ErrorCode::Validation, e, StatusCode::BAD_REQUEST),
    }
}

/// PUT /api/script-authorizations — 授权角色卡脚本。
pub async fn grant_authorization(
    State(state): State<Arc<AppState>>,
    Json(body): Json<GrantBody>,
) -> Response {
    use crate::services::script_authorization_service::ScriptAuthorizationService;
    // ① 取脚本集并校验前端回传哈希与**后端实时计算值**一致:
    //    防陈旧/伪造哈希把卡锁在错误版本上(前端在读取与授权之间若卡被改,会 409)。
    let user_scripts = state.user_scripts.clone();
    let id = body.character_id.clone();
    let scripts =
        match tokio::task::spawn_blocking(move || card_scripts_of(&user_scripts, &id)).await {
            Err(e) => return db_err(&format!("取脚本集任务失败: {e}")),
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return err_with_code(ErrorCode::Validation, e, StatusCode::BAD_REQUEST),
        };
    let current = ScriptAuthorizationService::compute_hash(&scripts);
    if current.is_empty() {
        return err_with_code(
            ErrorCode::Validation,
            "该角色没有可授权的启用脚本",
            StatusCode::BAD_REQUEST,
        );
    }
    if current != body.script_hash {
        // 409 需回带新的 current_hash(前端据此重试),故此处单独构造带额外字段的响应;
        // 形状仍带 code 字段,与 errors.rs 的契约一致(不新增裸 {error})。
        return Json(json!({
            "error": "脚本已变更,授权哈希过期;请重新读取当前哈希后再授权",
            "code": ErrorCode::Conflict.as_str(),
            "current_hash": current,
        }))
        .into_response()
        .with_status(StatusCode::CONFLICT);
    }
    // ② 写台账(此时 hash 与脚本内容已确认一致)
    let svc = state.script_authorizations.clone();
    let id = body.character_id.clone();
    let hash = current.clone();
    match state.db_call(move || svc.grant(&id, &hash)).await {
        Err(e) => db_err(&e),
        Ok(Ok(())) => {
            Json(json!({ "ok": true, "character_id": body.character_id, "script_hash": current }))
                .into_response()
        }
        Ok(Err(e)) => err_with_code(ErrorCode::Validation, e, StatusCode::BAD_REQUEST),
    }
}

/// DELETE /api/script-authorizations?character_id=
pub async fn revoke_authorization(
    State(state): State<Arc<AppState>>,
    Query(q): Query<AuthorizationQuery>,
) -> Response {
    let Some(character_id) = q.character_id.as_deref().filter(|s| !s.is_empty()) else {
        return err_with_code(
            ErrorCode::Validation,
            "需指定 character_id",
            StatusCode::BAD_REQUEST,
        );
    };
    let svc = state.script_authorizations.clone();
    let id = character_id.to_string();
    match state.db_call(move || svc.revoke(&id)).await {
        Err(e) => db_err(&e),
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => err_with_code(ErrorCode::Validation, e, StatusCode::BAD_REQUEST),
    }
}

/// GET /api/script-authorizations/list — 全部已授权角色(设置页展示)
pub async fn list_authorizations(State(state): State<Arc<AppState>>) -> Response {
    let svc = state.script_authorizations.clone();
    match state.db_call(move || svc.list()).await {
        Err(e) => db_err(&e),
        Ok(Ok(rows)) => Json(json!({ "grants": rows })).into_response(),
        Ok(Err(e)) => err_with_code(ErrorCode::Validation, e, StatusCode::BAD_REQUEST),
    }
}
