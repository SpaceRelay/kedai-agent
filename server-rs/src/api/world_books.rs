// 世界书路由:/api/world-books(列表/上传/详情/更新/删除/条目编辑/自检)+ 角色卡内嵌条目
use crate::api::app_state::AppState;
use crate::api::characters::{parse_boundary, parse_multipart};
use crate::api::WithStatus;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub async fn list(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let books = state.world_books.list();
    Json(json!({ "world_books": books }))
}

/// GET /api/world-books/auto-assign-check — 自动分配属性机制自检
/// (可选 ?character_id= 指定角色:额外校验该角色/全局世界书条目解析链路)
#[derive(Deserialize)]
pub struct AutoAssignQuery {
    #[serde(default)]
    pub character_id: Option<String>,
}

pub async fn auto_assign_check(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AutoAssignQuery>,
) -> Json<serde_json::Value> {
    let result = state
        .world_books
        .auto_assign_check(query.character_id.as_deref());
    Json(result)
}

const MAX_UPLOAD: usize = 30 * 1024 * 1024;

/// 上传独立世界书(multipart:file + 可选 character_id 字段)
pub async fn upload(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(boundary) = parse_boundary(content_type) else {
        return Json(json!({ "error": "缺少文件字段(file)" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    let parts = parse_multipart(&body, &boundary);
    let Some(file) = parts.iter().find(|p| p.filename.is_some()) else {
        return Json(json!({ "error": "缺少文件字段(file)" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    if file.content.len() > MAX_UPLOAD {
        return Json(json!({ "error": "文件超过 30MB 上限" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let character_id = parts
        .iter()
        .find(|p| p.field == "character_id")
        .map(|p| String::from_utf8_lossy(&p.content).trim().to_string())
        .filter(|s| !s.is_empty());
    let file_name = file
        .filename
        .clone()
        .unwrap_or_else(|| "worldbook.json".to_string());
    match state
        .world_books
        .upload(&file.content, &file_name, character_id.as_deref())
    {
        Ok(b) => {
            state.clear_all_contracts();
            Json(b).into_response().with_status(StatusCode::CREATED)
        }
        Err(e) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
    }
}

#[derive(Deserialize)]
pub struct UpdateBody {
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 绑定角色 id;传空串表示清除绑定(转全局);缺省表示不变
    #[serde(default)]
    pub character_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateBody>,
) -> Response {
    let cid = body.character_id.map(|s| {
        if s.trim().is_empty() {
            None
        } else {
            Some(s.trim().to_string())
        }
    });
    match state.world_books.update(
        &id,
        body.enabled,
        cid.as_ref().map(|c| c.as_deref()),
        body.name.as_deref(),
    ) {
        Some(b) => {
            // 绑定关系/启停变化影响哪些角色能收到该世界书(含其契约条目)
            state.clear_all_contracts();
            Json(b).into_response()
        }
        None => Json(json!({ "error": "世界书不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND),
    }
}

pub async fn delete(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    if state.world_books.delete(&id) {
        state.clear_all_contracts();
        StatusCode::NO_CONTENT.into_response()
    } else {
        Json(json!({ "error": "世界书不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND)
    }
}

pub async fn entries(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match state.world_books.entries(&id) {
        Some(entries) => Json(json!({ "id": id, "entries": entries })).into_response(),
        None => Json(json!({ "error": "世界书不存在" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND),
    }
}

/// PUT /api/world-books/{id}/entries — 全量回写条目(内容/关键词/正则/常驻/激发/位置)
#[derive(Deserialize)]
pub struct SaveEntriesBody {
    pub entries: Vec<crate::models::types::WorldBookEntryView>,
}

pub async fn save_entries(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<SaveEntriesBody>,
) -> Response {
    let rec = match state.world_books.get(&id) {
        Some(r) => r,
        None => {
            return Json(json!({ "error": "世界书不存在" }))
                .into_response()
                .with_status(StatusCode::NOT_FOUND)
        }
    };
    let mut raw = match rec.data_raw {
        Some(r) => r,
        None => {
            return Json(json!({ "error": "世界书缺少原始数据" }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST)
        }
    };
    if raw.get("entries").is_none() {
        raw.as_object_mut()
            .map(|o| o.insert("entries".into(), serde_json::Value::Array(Vec::new())));
    }
    if let Some(entries) = raw.get_mut("entries") {
        crate::parsing::world_book::merge_entries_into(entries, &body.entries);
    }
    match state.world_books.save_data_raw(&id, &raw) {
        Some(()) => {
            // 条目内容变化可能改动 [nlkaleido_contract];世界书可能被多角色引用,清全部缓存
            state.clear_all_contracts();
            Json(json!({ "ok": true, "id": id, "entries": body.entries })).into_response()
        }
        None => Json(json!({ "error": "保存失败" }))
            .into_response()
            .with_status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// POST /api/world-books/{id}/entries — 新增一条空条目
pub async fn add_entry(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match state.world_books.add_entry(&id) {
        Some(view) => {
            state.clear_all_contracts();
            Json(json!({ "ok": true, "entry": view })).into_response()
        }
        None => Json(json!({ "error": "世界书不存在或保存失败" }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND),
    }
}

/// GET /api/characters/{id}/world-entries — 角色卡内嵌世界书(character_book)条目编辑视图
pub async fn character_entries(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.world_books.character_book_views(&id) {
        Some(entries) => Json(json!({ "character_id": id, "entries": entries })).into_response(),
        None => Json(json!({ "character_id": id, "entries": [] })).into_response(),
    }
}

/// PUT /api/characters/{id}/world-entries — 回写角色卡内嵌世界书条目(写回 data_raw.character_book.entries)
pub async fn save_character_entries(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<SaveEntriesBody>,
) -> Response {
    match state.world_books.save_character_book(&id, &body.entries) {
        Some(()) => {
            // 角色卡内嵌世界书条目可能含 [nlkaleido_contract],契约来源变化
            state.invalidate_contracts_for_character(&id);
            Json(json!({ "ok": true, "character_id": id, "entries": body.entries })).into_response()
        }
        None => Json(json!({ "error": "角色卡无内嵌世界书,或保存失败" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
    }
}
