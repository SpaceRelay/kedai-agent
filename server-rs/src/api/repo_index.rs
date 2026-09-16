// 仓库索引面板数据源(开发期工具):
//   GET /api/repo-index[?q=]  读取 .kedai-index/index.json,供前端「仓库索引」面板浏览。
//
// 索引由 `node .kedai-index/build.mjs` 生成,属开发期派生数据(已 gitignore)。
// 生产打包后该文件通常不存在,此时返回 available=false 并给出指引,前端显示空态——
// 这是诚实行为:仓库索引本就不是运行时功能。
//
// 安全:只读固定相对路径(仓库根 .kedai-index/index.json),不接受任意路径参数,
// 因此不存在目录穿越面。
use crate::api::app_state::AppState;
use crate::api::ErrorCode;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// 「索引不可用」的稳定文案(批次 1):读取/解析失败的**原始错误**(含盘符路径、
/// serde 行列号)只进日志,响应里给固定文案 + 重新生成指引,保持可操作性。
const INDEX_REBUILD_HINT: &str = "请重新运行 node .kedai-index/build.mjs --full 生成索引";

#[derive(Deserialize, Default)]
pub struct RepoIndexQuery {
    /// 可选关键词:后端只做粗筛(路径/摘要/符号名子串),精细过滤由前端做
    pub q: Option<String>,
    /// 可选条数上限,默认 300、上限 2000(防超大响应)
    pub limit: Option<usize>,
}

/// 定位 index.json:优先环境变量 KEDAI_REPO_INDEX(便于非标准部署),
/// 其次当前工作目录与可执行文件所在目录的祖先链(开发期 cargo run / 便携版）。
fn locate_index_file() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("KEDAI_REPO_INDEX") {
        let pb = std::path::PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.to_path_buf());
        }
    }
    for root in roots {
        // 沿祖先链最多向上 4 层查找 .kedai-index/index.json
        let mut cur: Option<&std::path::Path> = Some(root.as_path());
        for _ in 0..5 {
            let Some(dir) = cur else { break };
            let candidate = dir.join(".kedai-index").join("index.json");
            if candidate.is_file() {
                return Some(candidate);
            }
            cur = dir.parent();
        }
    }
    None
}

/// GET /api/repo-index:返回仓库索引快照(可选 q 粗筛)
pub async fn get_repo_index(
    State(_state): State<Arc<AppState>>,
    Query(query): Query<RepoIndexQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(300).clamp(1, 2000);
    let Some(file) = locate_index_file() else {
        return Json(json!({
            "available": false,
            "code": ErrorCode::NotFound.as_str(),
            "reason": "未找到 .kedai-index/index.json;请先在仓库根运行 node .kedai-index/build.mjs --full"
        }))
        .into_response();
    };

    let raw = match tokio::fs::read_to_string(&file).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "索引文件读取失败");
            return Json(json!({
                "available": false,
                "code": ErrorCode::Internal.as_str(),
                "reason": format!("索引文件读取失败,{INDEX_REBUILD_HINT}")
            }))
            .into_response();
        }
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "索引文件解析失败");
            return Json(json!({
                "available": false,
                "code": ErrorCode::Internal.as_str(),
                "reason": format!("索引文件解析失败(可能构建中断),{INDEX_REBUILD_HINT}")
            }))
            .into_response();
        }
    };

    let mut items: Vec<Value> = parsed
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // 关键词粗筛:路径 / 摘要 / 深摘要 / 符号名
    if let Some(q) = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let needle = q.to_lowercase();
        items.retain(|it| {
            let hay = format!(
                "{} {} {} {}",
                it.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("summary").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("deepSummary").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("symbols")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr
                        .iter()
                        .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
                        .collect::<Vec<_>>()
                        .join(" "))
                    .unwrap_or_default(),
            )
            .to_lowercase();
            hay.contains(&needle)
        });
    }

    let total = items.len();
    items.truncate(limit);

    Json(json!({
        "available": true,
        "generatedAt": parsed.get("generatedAt").cloned().unwrap_or(Value::Null),
        "gitHead": parsed.get("gitHead").cloned().unwrap_or(Value::Null),
        "fileCount": parsed.get("fileCount").cloned().unwrap_or(json!(total)),
        "vectorEnabled": parsed.get("vectorEnabled").cloned().unwrap_or(json!(false)),
        "matched": total,
        "items": items,
    }))
    .into_response()
}
