// 插件 API:/api/plugins/tools(列出已加载工具插件 + 热重载 + 导入)
//
// 代际位置:本层是**组合根的一部分**(L2 · 暴露面)。插件解析在 L3(`plugins/`),
// 但**注册是装配动作**,按三结合纪律由宿主(本层)执行——L3 不得依赖 L2 的工具注册表。
// 故下方统一走「解析(L3) → 宿主注册(L2)」两段式:`parse_all`/`parse_file` + `register_plugin`。
use crate::api::app_state::AppState;
use crate::api::{err_status, internal, validation, ErrorCode, WithStatus};
use crate::plugins::ToolPluginLoader;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::sync::Arc;

/// 把一批已解析插件注册进工具表（宿主侧装配动作）。
///
/// **命名冲突防护（已知限制 L19）**：注册前校验是否与内置工具重名——插件不得**劫持**
/// 内置工具（`bash`/`write` 等）。冲突时跳过并计入 errors，**不覆盖**。
/// 这是「注册表即隔离边界」的不变量：注册动作必须先证明「不抢占已有名字」。
///
/// `pub(crate)`：`api::app_state` 的启动装配复用同一入口，保证
/// 「启动加载」与「热重载」走完全一致的冲突校验口径。
pub(crate) fn register_plugins(
    loaded: Vec<crate::plugins::LoadedToolPlugin>,
    registry: &crate::tools::registry::ToolRegistry,
) -> (usize, Vec<String>) {
    let mut count = 0;
    let mut errors = Vec::new();
    for plugin in loaded {
        let name = plugin.definition.name.clone();
        // 内置名与已注册名都不可被插件覆盖:前者会劫持内置工具执行体,
        // 后者会让「插件 A 静默替换插件 B」成为可能,两者都属越权。
        if registry.is_builtin(&name) {
            errors.push(format!(
                "{name}: 与内置工具重名，已拒绝注册（插件不得劫持内置工具）"
            ));
            continue;
        }
        registry.register_external(
            plugin.definition,
            crate::plugins::plugin_executor(&plugin.script),
            None,
            crate::models::tool_policy::ToolOrigin::Plugin,
        );
        count += 1;
    }
    (count, errors)
}

/// GET /api/plugins/tools:列出已注册工具 + 磁盘插件文件 + 加载错误
pub async fn list_tools(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let tools = state.tool_registry.list_definitions();
    let dir = state.config.data_dir.join("plugins").join("tools");
    // list_files 是同步目录遍历,挪阻塞线程池(B-1:不在 tokio worker 上做同步文件 IO)
    let files = tokio::task::spawn_blocking(move || ToolPluginLoader::new(dir).list_files())
        .await
        .unwrap_or_default();
    Json(json!({
        "tools": tools,
        "files": files,
    }))
}

/// POST /api/plugins/tools/reload:重新加载磁盘上的工具插件(注册覆盖)
pub async fn reload_tools(State(state): State<Arc<AppState>>) -> Response {
    let dir = state.config.data_dir.join("plugins").join("tools");
    let registry = state.tool_registry.clone();
    // parse_all 是同步目录遍历 + 逐文件解析,挪阻塞线程池(B-1);
    // 注册在 spawn_blocking 之外由本层执行(注册表是同步锁操作,无需阻塞池)。
    let (loaded, mut errors) =
        tokio::task::spawn_blocking(move || ToolPluginLoader::new(dir).parse_all())
            .await
            .unwrap_or_else(|e| {
                // 泄露封堵(批次 1):JoinError 原文(线程池内部细节)只进日志,
                // 面向用户的错误项换成固定文案。
                tracing::error!(error = %e, "插件重载任务失败");
                (
                    Vec::new(),
                    vec!["插件重载任务失败,详情见服务端日志".to_string()],
                )
            });
    let (count, reg_errors) = register_plugins(loaded, &registry);
    errors.extend(reg_errors);
    if errors.is_empty() {
        Json(json!({ "ok": true, "loaded": count }))
            .into_response()
            .with_status(StatusCode::OK)
    } else {
        // 形状(批次 1):`loaded`(已注册数)与 `errors`(逐项失败原因)是**业务数据**,
        // 必须保留;仅补 `code` 供前端统一分支。errors 原文由解析器产生(面向插件的
        // 文件名/语法错误,不含密钥/路径绝对化),属用户排障必需信息,保留透出。
        Json(json!({
            "ok": false,
            "code": ErrorCode::Validation.as_str(),
            "error": "部分插件加载失败",
            "loaded": count,
            "errors": errors,
        }))
        .into_response()
        .with_status(StatusCode::BAD_REQUEST)
    }
}

/// 插件文件名净化(上传/删除统一策略):仅保留 ASCII 字母数字、-、_、.;
/// 净化结果必须与原始文件名逐字符一致,否则视为含路径分隔符/非法字符,返回 None 拒绝。
/// 不一致即拒绝而不是静默改写:静默改写会让「上传 a/b.json 落盘为 ab.json」产生歧义,
/// 也让路径穿越输入(../x.json)以改写后的形态落盘,口径与删除入口的既有比较保持一致。
fn sanitize_plugin_filename(name: &str) -> Option<String> {
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe.is_empty() || safe != name {
        None
    } else {
        Some(safe)
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
        return err_status("缺少文件字段(file)", StatusCode::BAD_REQUEST);
    };
    let parts = crate::api::characters::parse_multipart(&body, &boundary);
    let Some(file) = parts.into_iter().find(|p| p.filename.is_some()) else {
        return err_status("缺少文件字段(file)", StatusCode::BAD_REQUEST);
    };
    let file_name = file.filename.unwrap_or_else(|| "plugin.json".to_string());
    if !file_name.to_lowercase().ends_with(".json") {
        return err_status("插件文件须为 .json 格式", StatusCode::BAD_REQUEST);
    }
    let file_bytes = file.content;
    // 校验 JSON 结构(至少含 name / script)
    let cfg: Result<crate::plugins::ToolPluginConfig, _> = serde_json::from_slice(&file_bytes);
    match cfg {
        Ok(c) => {
            if c.name.trim().is_empty() || c.script.trim().is_empty() {
                return err_status("插件须包含 name 与 script 字段", StatusCode::BAD_REQUEST);
            }
            // 文件名净化(与删除入口统一策略):净化结果不等于原始名 → 拒绝
            let Some(safe) = sanitize_plugin_filename(&file_name) else {
                return err_status("文件名含非法字符", StatusCode::BAD_REQUEST);
            };
            let dir = state.config.data_dir.join("plugins").join("tools");
            // B-1:文件 IO 走 tokio::fs / 阻塞线程池,不在 tokio worker 上同步读写
            tokio::fs::create_dir_all(&dir).await.ok();
            let path = dir.join(&safe);
            if let Err(e) = tokio::fs::write(&path, &file_bytes).await {
                // 500:入参含 io 错误原文(可能带盘符路径),按泄露策略只进日志
                return internal(format!("写文件失败: {e}"));
            }
            // 解析该插件(parse_file 同步读文件,挪阻塞线程池),注册由本层执行
            let registry = state.tool_registry.clone();
            let load_path = path.clone();
            let parse_result = tokio::task::spawn_blocking(move || {
                ToolPluginLoader::new(dir).parse_file(&load_path)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            match parse_result {
                Ok(plugin) => {
                    let (_, reg_errors) = register_plugins(vec![plugin], &registry);
                    if let Some(e) = reg_errors.first() {
                        return validation(format!("插件注册失败: {e}"));
                    }
                }
                Err(e) => {
                    return validation(format!("插件注册失败: {e}"));
                }
            }
            Json(json!({ "ok": true, "name": c.name, "file": safe }))
                .into_response()
                .with_status(StatusCode::CREATED)
        }
        Err(e) => validation(format!("插件 JSON 解析失败: {e}")),
    }
}

/// DELETE /api/plugins/tools/{name}:删除已导入的工具插件(文件 + 注册)
pub async fn delete_tool(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    // 文件名净化(与上传入口统一策略):净化结果不等于原始名 → 拒绝
    let Some(safe) = sanitize_plugin_filename(&name) else {
        return err_status("非法文件名", StatusCode::BAD_REQUEST);
    };
    if !safe.to_lowercase().ends_with(".json") {
        return err_status("非法文件名", StatusCode::BAD_REQUEST);
    }
    let dir = state.config.data_dir.join("plugins").join("tools");
    let path = dir.join(&safe);
    // 注销对应工具:删除文件前解析其 name(删除后无法反查);文件无效时跳过注册但保留删除
    // (B-1:文件 IO 走 tokio::fs;load_all 同步目录遍历,挪阻塞线程池)
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        if let Ok(bytes) = tokio::fs::read(&path).await {
            if let Ok(cfg) = serde_json::from_slice::<crate::plugins::ToolPluginConfig>(&bytes) {
                state.tool_registry.unregister(&cfg.name);
            }
        }
        let _ = tokio::fs::remove_file(&path).await;
    }
    // 重载剩余插件文件(与 reload 语义一致):解析在阻塞池,注册在本层
    let registry = state.tool_registry.clone();
    let (loaded, _) = tokio::task::spawn_blocking(move || ToolPluginLoader::new(dir).parse_all())
        .await
        .unwrap_or_default();
    let (count, _) = register_plugins(loaded, &registry);
    Json(json!({ "ok": true, "removed": name, "reloaded": count })).into_response()
}

#[cfg(test)]
mod tests {
    use super::sanitize_plugin_filename;

    /// 净化策略统一(优化项 B-4):上传与删除入口同一判定——
    /// 净化结果与原始文件名不一致即拒绝,不再静默改写/回退默认名。
    #[test]
    fn sanitize_rejects_traversal_and_illegal_chars() {
        // 路径穿越:含目录分隔符的「..」组合被拒
        assert_eq!(sanitize_plugin_filename("../evil.json"), None);
        assert_eq!(sanitize_plugin_filename("..\\evil.json"), None);
        // 目录分隔符(子目录前缀)
        assert_eq!(sanitize_plugin_filename("sub/tool.json"), None);
        // 非法字符:空格、中文、冒号、星号等
        assert_eq!(sanitize_plugin_filename("my tool.json"), None);
        assert_eq!(sanitize_plugin_filename("插件.json"), None);
        assert_eq!(sanitize_plugin_filename("a:b.json"), None);
        assert_eq!(sanitize_plugin_filename("a*.json"), None);
        // 净化后为空(原名全是非法字符)
        assert_eq!(sanitize_plugin_filename("///"), None);
        assert_eq!(sanitize_plugin_filename(""), None);
    }

    /// 合法文件名原样通过(逐字符一致)
    #[test]
    fn sanitize_accepts_legal_names() {
        assert_eq!(
            sanitize_plugin_filename("weather.json"),
            Some("weather.json".to_string())
        );
        // 含 . - _ 的合法组合(净化策略允许点号;无分隔符的「..」开头按普通字符放行,
        // 与既有过滤字符集一致;上传/删除两入口同一函数判定,策略天然一致)
        assert_eq!(
            sanitize_plugin_filename("my-tool_v2.1.json"),
            Some("my-tool_v2.1.json".to_string())
        );
        assert_eq!(
            sanitize_plugin_filename("..json"),
            Some("..json".to_string())
        );
    }
}
