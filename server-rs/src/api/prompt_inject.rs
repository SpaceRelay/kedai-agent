// 提示词注入 API:GET/PUT /api/prompt-inject(简单模式 + 楼层系统配置)
// POST /api/prompt-inject/import:导入酒馆(SillyTavern)预设 JSON → 楼层
use crate::api::app_state::AppState;
use crate::api::characters::{parse_boundary, parse_multipart};
use crate::api::WithStatus;
use crate::parsing::preset::parse_st_preset;
use crate::services::prompt_inject_service::PromptInjectConfig;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// GET /api/prompt-inject:返回当前注入配置(简单模式 + 楼层列表)
pub async fn get_prompt_inject(State(state): State<Arc<AppState>>) -> Response {
    let cfg = state
        .prompt_inject
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get()
        .clone();
    Json(json!({ "ok": true, "config": cfg })).into_response()
}

/// PUT body:全量配置
#[derive(Debug, Deserialize)]
pub struct UpdatePromptInjectBody {
    pub config: PromptInjectConfig,
}

/// PUT /api/prompt-inject:全量保存注入配置(校验枚举,失败 400;成功写回 prompt_floors.json)
pub async fn update_prompt_inject(
    State(state): State<Arc<AppState>>,
    Json(body): Json<UpdatePromptInjectBody>,
) -> Response {
    // B-1:set 内含同步 JSON 落盘,持锁 + 写文件整体挪阻塞线程池
    let svc = state.prompt_inject.clone();
    let result = state
        .db_call(move || {
            let mut svc = svc.lock().unwrap_or_else(|e| e.into_inner());
            svc.set(body.config).map(|()| svc.get().clone())
        })
        .await;
    match result {
        Ok(Ok(cfg)) => Json(json!({ "ok": true, "config": cfg })).into_response(),
        Ok(Err(e)) | Err(e) => Json(json!({ "error": format!("保存失败: {e}") }))
            .into_response()
            .with_status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

const MAX_PRESET: usize = 30 * 1024 * 1024;

/// POST /api/prompt-inject/import:导入酒馆预设 JSON。
/// multipart 字段:file(预设 JSON,必填,≤30MB)、mode(可选:"replace" 替换现有楼层[默认] / "append" 追加)。
/// 导入后自动切换为复杂模式(预设即楼层集)。返回 { ok, imported, config }。
pub async fn import(
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
    let Some(file_part) = parts.iter().find(|p| p.field == "file") else {
        return Json(json!({ "error": "缺少文件字段(file)" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    if file_part.content.len() > MAX_PRESET {
        return Json(json!({ "error": "预设文件超过 30MB 上限" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let text = String::from_utf8_lossy(&file_part.content);
    let floors = match parse_st_preset(&text) {
        Ok(f) => f,
        Err(e) => {
            return Json(json!({ "error": e }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST);
        }
    };
    if floors.is_empty() {
        return Json(json!({ "error": "预设中没有可导入的提示词条目" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let mode = parts
        .iter()
        .find(|p| p.field == "mode")
        .map(|p| String::from_utf8_lossy(&p.content).trim().to_lowercase())
        .unwrap_or_else(|| "replace".to_string());

    // B-1:set 内含同步 JSON 落盘;楼层合并、持锁、写文件整体挪阻塞线程池
    let svc = state.prompt_inject.clone();
    let result = state
        .db_call(move || {
            let mut svc = svc.lock().unwrap_or_else(|e| e.into_inner());
            let mut cfg = svc.get().clone();
            if mode == "append" {
                // 追加:order 接续现有楼层;id 冲突时自动改名
                let base = cfg.floors.len();
                let mut used: std::collections::HashSet<String> =
                    cfg.floors.iter().map(|f| f.id.clone()).collect();
                for (i, mut f) in floors.into_iter().enumerate() {
                    if used.contains(&f.id) {
                        f.id = format!("{}-import{}", f.id, i);
                    }
                    used.insert(f.id.clone());
                    f.order = base + i;
                    cfg.floors.push(f);
                }
            } else {
                // 替换(默认):清空现有楼层,写入预设条目
                cfg.floors = floors;
            }
            cfg.mode = crate::services::prompt_inject_service::InjectMode::Complex;
            let imported = cfg.floors.len();
            svc.set(cfg).map(|()| (imported, svc.get().clone()))
        })
        .await;
    match result {
        Ok(Ok((imported, config))) => {
            Json(json!({ "ok": true, "imported": imported, "config": config })).into_response()
        }
        Ok(Err(e)) | Err(e) => Json(json!({ "error": format!("导入失败: {e}") }))
            .into_response()
            .with_status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}
