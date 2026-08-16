// 插件 API:/api/plugins/tools(列出已加载工具插件 + 热重载 + 导入)
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use crate::plugins::ToolPluginLoader;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::sync::Arc;

/// GET /api/plugins/tools:列出已注册工具 + 磁盘插件文件 + 加载错误
pub async fn list_tools(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let tools = state.tool_registry.list_definitions();
    let loader = ToolPluginLoader::new(state.config.data_dir.join("plugins").join("tools"));
    let files = loader.list_files();
    Json(json!({
        "tools": tools,
        "files": files,
    }))
}

/// POST /api/plugins/tools/reload:重新加载磁盘上的工具插件(注册覆盖)
pub async fn reload_tools(State(state): State<Arc<AppState>>) -> Response {
    let loader = ToolPluginLoader::new(state.config.data_dir.join("plugins").join("tools"));
    let (count, errors) = loader.load_all(&state.tool_registry);
    if errors.is_empty() {
        Json(json!({ "ok": true, "loaded": count }))
            .into_response()
            .with_status(StatusCode::OK)
    } else {
        Json(json!({ "ok": false, "loaded": count, "errors": errors }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST)
    }
}

/// POST /api/plugins/tools/upload:导入工具插件 JSON 文件(写入 data/plugins/tools 并注册)。
/// multipart 字段名 file,复用 characters 的通用 multipart 解析。
pub async fn upload_tool(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(boundary) = crate::api::characters::parse_boundary(content_type) else {
        return err_json("缺少文件字段(file)", StatusCode::BAD_REQUEST);
    };
    let parts = crate::api::characters::parse_multipart(&body, &boundary);
    let Some(file) = parts.into_iter().find(|p| p.filename.is_some()) else {
        return err_json("缺少文件字段(file)", StatusCode::BAD_REQUEST);
    };
    let file_name = file.filename.unwrap_or_else(|| "plugin.json".to_string());
    if !file_name.to_lowercase().ends_with(".json") {
        return err_json("插件文件须为 .json 格式", StatusCode::BAD_REQUEST);
    }
    let file_bytes = file.content;
    // 校验 JSON 结构(至少含 name / script)
    let cfg: Result<crate::plugins::ToolPluginConfig, _> = serde_json::from_slice(&file_bytes);
    match cfg {
        Ok(c) => {
            if c.name.trim().is_empty() || c.script.trim().is_empty() {
                return err_json("插件须包含 name 与 script 字段", StatusCode::BAD_REQUEST);
            }
            // 文件名规范化:仅保留安全字符
            let safe: String = file_name
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
                .collect();
            let safe = if safe.is_empty() {
                "plugin.json".to_string()
            } else {
                safe
            };
            let dir = state.config.data_dir.join("plugins").join("tools");
            std::fs::create_dir_all(&dir).ok();
            let path = dir.join(&safe);
            if let Err(e) = std::fs::write(&path, &file_bytes) {
                return err_json(
                    &format!("写文件失败: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
            // 注册该插件
            let loader = ToolPluginLoader::new(dir);
            if let Err(e) = loader.load_file(&path, &state.tool_registry) {
                return err_json(&format!("插件注册失败: {e}"), StatusCode::BAD_REQUEST);
            }
            Json(json!({ "ok": true, "name": c.name, "file": safe }))
                .into_response()
                .with_status(StatusCode::CREATED)
        }
        Err(e) => err_json(&format!("插件 JSON 解析失败: {e}"), StatusCode::BAD_REQUEST),
    }
}

/// DELETE /api/plugins/tools/{name}:删除已导入的工具插件(文件 + 注册)
pub async fn delete_tool(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    // 文件名校验(仅允许安全字符)
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe != name || !name.to_lowercase().ends_with(".json") {
        return err_json("非法文件名", StatusCode::BAD_REQUEST);
    }
    let dir = state.config.data_dir.join("plugins").join("tools");
    let path = dir.join(&name);
    // 注销对应工具:删除文件前解析其 name(删除后无法反查);文件无效时跳过注册但保留删除
    if path.exists() {
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(cfg) = serde_json::from_slice::<crate::plugins::ToolPluginConfig>(&bytes) {
                state.tool_registry.unregister(&cfg.name);
            }
        }
        let _ = std::fs::remove_file(&path);
    }
    // 重载剩余插件文件(与 reload 语义一致)
    let loader = ToolPluginLoader::new(dir);
    let (count, _) = loader.load_all(&state.tool_registry);
    Json(json!({ "ok": true, "removed": name, "reloaded": count })).into_response()
}

/// 运行时错误响应(复用 WithStatus)
fn err_json(msg: &str, code: StatusCode) -> Response {
    Json(json!({ "error": msg }))
        .into_response()
        .with_status(code)
}
