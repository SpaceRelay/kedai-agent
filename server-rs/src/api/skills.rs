// Skill 库路由:/api/skills(列表/导入/更新/删除)
// 导入格式:单对象 {name,description,content} 或数组 [{...}] 或 {skills: [...]} 包壳
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use crate::services::skill_service::SkillImport;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// 导入请求体(untagged 兼容三种形态)
#[derive(Deserialize)]
#[serde(untagged)]
pub enum ImportBody {
    One(SkillImport),
    Many(Vec<SkillImport>),
    Wrapped { skills: Vec<SkillImport> },
}

#[derive(Deserialize, Default)]
pub struct UpdateSkillBody {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

/// GET /api/skills:全部技能(含已停用,前端按钮可切换)
pub async fn list(State(state): State<Arc<AppState>>) -> Json<Value> {
    let skills = state.skills.list(false);
    Json(json!({ "skills": skills }))
}

/// POST /api/skills:导入技能(同名覆盖)
pub async fn import_skills(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ImportBody>,
) -> Response {
    let items = match body {
        ImportBody::One(x) => vec![x],
        ImportBody::Many(v) => v,
        ImportBody::Wrapped { skills } => skills,
    };
    match state.skills.import(items) {
        Ok(n) => Json(json!({ "ok": true, "imported": n })).into_response(),
        Err(e) => err_json(&e),
    }
}

/// PUT /api/skills/{id}:更新(启用/停用、改名、改内容,仅提供字段生效)
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateSkillBody>,
) -> Response {
    match state.skills.update(
        &id,
        body.enabled,
        body.name.as_deref(),
        body.description.as_deref(),
        body.content.as_deref(),
    ) {
        Some(s) => Json(json!({ "ok": true, "skill": s })).into_response(),
        None => err_json("技能不存在"),
    }
}

/// DELETE /api/skills/{id}:删除技能
pub async fn delete(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    if state.skills.delete(&id) {
        Json(json!({ "ok": true })).into_response()
    } else {
        err_json("技能不存在")
    }
}

fn err_json(msg: &str) -> Response {
    Json(json!({ "error": msg }))
        .into_response()
        .with_status(StatusCode::NOT_FOUND)
}
