// Skill 库路由:/api/skills(列表/导入/更新/删除)
// 导入格式:单对象 {name,description,content} 或数组 [{...}] 或 {skills: [...]} 包壳
use crate::api::app_state::AppState;
use crate::api::{db_err, WithStatus};
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
    /// 工具白名单(JSON 数组字符串,空数组 = 不限制)
    #[serde(default)]
    pub allowed_tools: Option<String>,
    /// 是否可作为子智能体技能派发
    #[serde(default)]
    pub run_as_subagent: Option<bool>,
    /// 可选模型名覆盖(空 = 用当前模型)
    #[serde(default)]
    pub model: Option<String>,
}

/// GET /api/skills:全部技能(含已停用,前端按钮可切换)
pub async fn list(State(state): State<Arc<AppState>>) -> Json<Value> {
    let svc = state.skills.clone();
    let skills = state
        .db_call(move || svc.list(false))
        .await
        .expect("读取技能列表任务失败");
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
    let svc = state.skills.clone();
    match state.db_call(move || svc.import(items)).await {
        Err(e) => db_err(&e),
        Ok(Ok(n)) => Json(json!({ "ok": true, "imported": n })).into_response(),
        Ok(Err(e)) => err_json(&e),
    }
}

/// PUT /api/skills/{id}:更新(启用/停用、改名、改内容,仅提供字段生效)
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateSkillBody>,
) -> Response {
    let svc = state.skills.clone();
    let updated = state
        .db_call(move || {
            svc.update(
                &id,
                body.enabled,
                body.name.as_deref(),
                body.description.as_deref(),
                body.content.as_deref(),
                body.allowed_tools.as_deref(),
                body.run_as_subagent,
                body.model.as_deref(),
            )
        })
        .await;
    match updated {
        Err(e) => db_err(&e),
        Ok(Some(s)) => Json(json!({ "ok": true, "skill": s })).into_response(),
        Ok(None) => err_json("技能不存在"),
    }
}

/// DELETE /api/skills/{id}:删除技能
pub async fn delete(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let svc = state.skills.clone();
    match state.db_call(move || svc.delete(&id)).await {
        Err(e) => db_err(&e),
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => err_json("技能不存在"),
    }
}

fn err_json(msg: &str) -> Response {
    Json(json!({ "error": msg }))
        .into_response()
        .with_status(StatusCode::NOT_FOUND)
}
