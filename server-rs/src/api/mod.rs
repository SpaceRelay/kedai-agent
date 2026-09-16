// API 路由组装 + health + 头像静态服务 + SPA 回退
//
// 代际: L2(中层·干 / Orchestration)——HTTP 面编排层。
// 判据: 路由 handler 薄、委托 services;承担鉴权与 SSE 流装配。
// 纪律: 本层的 `app_state.rs` 是**组合根装配点**(把各层拼装成可运行系统),
//       故允许依赖各层(含 L3 的 mcp/plugins)——这是分层架构对组合根的通用豁免,
//       已在 arch-layers.json 登记为 class=wiring。
// 详见 docs/契约-架构与数据.md §2.2、§3(entry 与组合根说明)。
// 路由表按域拆至 routes/ 子模块;静态文档与 SPA 回退在 static_files.rs;
// 响应工具(WithStatus/db_err/sse_response)在 util.rs;
// 结构化错误码(ErrorCode/err_with_code)在 errors.rs,经下方再导出保持 crate::api::* 路径不变。
pub mod agent;
pub mod agent_flows;
pub mod app_state;
pub mod audio;
pub mod characters;
pub mod chat;
pub mod contract_history;
pub mod contracts;
pub mod diagnostics;
pub mod exec;
pub mod import_export;
pub mod kaleido;
pub mod macros;
pub mod memory;
pub mod plugins;
pub mod prompt_inject;
pub mod quick_replies;
pub mod repo_index;
pub mod resource;
pub mod security;
pub mod sessions;
pub mod settings;
pub mod skills;
pub mod slash_commands;
pub mod tasks;
pub mod tokens;
pub mod tool_permissions;
pub mod undo;
pub mod user_scripts;
pub mod variables;
pub mod world_books;

mod errors;
pub(crate) mod json_body;
pub(crate) mod request_id;
pub(crate) mod routes;
pub(crate) mod static_files;
mod util;

// 统一错误出口(api/errors.rs):handler 一律用这些构造器,勿再就地拼 Json。
// code_for_status 仅供 errors.rs 内部由 err_status 使用,不再对外导出。
pub(crate) use errors::{
    conflict, err_status, err_with_code, internal, not_found, upstream, validation, ErrorCode,
};
pub use util::WithStatus;
pub(crate) use util::{db_err, sse_response};

use crate::api::app_state::AppState;
use axum::extract::{DefaultBodyLimit, Query};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// /api/health 查询参数:仅 `deep=1`(或 `deep=true`)触发整库 `PRAGMA quick_check`
#[derive(Deserialize)]
struct HealthQuery {
    #[serde(default)]
    deep: Option<String>,
}

/// health 的 DB 依赖探活:返回 "ok" / "error"(不 panic,不改变 liveness 语义)。
///
/// - 默认只跑 `SELECT 1`(廉价,每次请求可跑,验证连接池可取连接且库可读);
/// - `deep` 为真时改跑 `PRAGMA quick_check`(整库一致性校验,大库代价高,仅显式 ?deep=1 触发)。
///
/// 取连接失败或 SQL 出错一律记 "error"——依赖降级要可见,但不影响 `ok` 字段(见 handler 注释)。
fn probe_db(db: &crate::models::db::Db, deep: bool) -> &'static str {
    let Ok(conn) = db.read() else {
        return "error";
    };
    let healthy = if deep {
        conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
            .map(|value| value == "ok")
            .unwrap_or(false)
    } else {
        conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .map(|value| value == 1)
            .unwrap_or(false)
    };
    if healthy {
        "ok"
    } else {
        "error"
    }
}

/// 方法不允许的统一响应(批次 1):405 + `{error, code: VALIDATION}`。
/// 供 `Router::method_not_allowed_fallback` 使用——此前是 405 空 body,前端只能
/// 显示「请求失败 405」;补 code 后与 errors.rs 的错误契约一致,可程序化分支。
async fn method_not_allowed_json() -> Response {
    err_with_code(
        ErrorCode::Validation,
        "请求方法不被该接口支持",
        StatusCode::METHOD_NOT_ALLOWED,
    )
}

/// 组装全部路由(API + 头像 + 前端静态)
pub fn build_router(state: Arc<AppState>) -> Router {
    // 健康检查暴露数据目录:桌面壳/启动器复用已运行实例前据此校验指向同一数据目录,
    // 防止静默挂到另一套库(双数据目录分叉期间「聊天记录丢失」的隐形放大器)
    let health_data_dir = state.config.data_dir.to_string_lossy().to_string();
    // health 的依赖探测句柄(Arc 克隆,使 handler 闭包不持有整个 AppState)
    let health_db = state.db.clone();
    Router::new()
        // 健康检查:顺带暴露构建指纹(version/build_id/build_time,均由 build.rs 在
        // 编译期注入),测试版与便携版是否同步可经此端点直接比对,见 MAINTENANCE.md。
        .route(
            "/api/health",
            get(move |Query(query): Query<HealthQuery>| {
                let data_dir = health_data_dir.clone();
                let db = health_db.clone();
                async move {
                    // deep=1/true 才跑整库 quick_check(见 probe_db);放 spawn_blocking,
                    // 避免大库全量校验阻塞 tokio worker。
                    let deep = query
                        .deep
                        .as_deref()
                        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
                    let db_status = if deep {
                        let db = db.clone();
                        tokio::task::spawn_blocking(move || probe_db(&db, true))
                            .await
                            .unwrap_or("error")
                    } else {
                        probe_db(&db, false)
                    };
                    Json(json!({
                        // 冻结点:恒 true(liveness)。src-tauri 的 health_ok() 以此字段
                        // 判定「已有实例在运行」,用于单实例复用/端口竞态,不得改为依赖态。
                        "ok": true,
                        "ts": chrono::Utc::now().timestamp_millis(),
                        "version": env!("CARGO_PKG_VERSION"),
                        "build_id": env!("KEDAI_DIST_HASH"),
                        "build_time": env!("KEDAI_BUILD_TIME"),
                        "data_dir": data_dir,
                        // 依赖态:db 不可用时置 "error",但 ok 仍为 true(依赖降级 ≠ 进程不活)
                        "dependencies": { "db": db_status },
                    }))
                }
            }),
        )
        // 同源页面仅在 loopback Host/Origin 下引导内存 token;响应禁止缓存。
        .route("/api/bootstrap", get(static_files::bootstrap))
        // 头像静态服务
        .route("/api/avatars/{file}", get(static_files::avatar_file))
        // 角色卡脚本沙箱文档(独立 CSP,绕过全站 script-src 'self' 对内联脚本的限制)
        .route("/sandbox.html", get(static_files::sandbox_document))
        // 角色卡远程资源界面宿主文档(独立 CSP:允许作者页面脚本/网络,断掉与宿主的一切共享)
        .route(
            "/resource-frame.html",
            get(static_files::resource_frame_document),
        )
        // 消息渲染面板宿主文档(TH-render 等价物:独立 CSP,断掉一切网络出口)
        .route(
            "/render-frame.html",
            get(static_files::render_frame_document),
        )
        // 分域 API 路由(实现见 routes/ 子模块;merge 后统一 with_state,与原单链语义一致)
        .merge(routes::chat::chat_routes())
        .merge(routes::settings::settings_routes())
        .merge(routes::agent::agent_routes())
        .merge(routes::content::content_routes())
        .merge(routes::misc::misc_routes())
        // 前端静态(生产:磁盘 dist 或内嵌);SPA 回退
        .fallback(static_files::spa_fallback)
        // 方法不允许(如 DELETE /api/health / GET /api/chat/send):axum 默认回 405
        // **空 body**,前端 request() 解析不出 error/code,只能显示「请求失败 405」。
        // 批次 1 统一为 JSON + code(与 errors.rs 的错误契约一致)。
        .method_not_allowed_fallback(method_not_allowed_json)
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(state, security::guard))
        // 上传 body 限制:axum 默认 2MB,角色卡/世界书/预设 30MB 上限(MAX_UPLOAD/MAX_PRESET)
        // 会被先拦下,这里放行到 35MB(multipart 封装有少量 overhead),再由 handler 内精确校验;
        // agent 执行流程保存(JSON body)同样受此上限覆盖。
        .layer(DefaultBodyLimit::max(35 * 1024 * 1024))
        // CORS:仅放行本机来源(浏览器同源页面本不需要 CORS;此限制用于挡住恶意网页
        // 跨域读取/触发本地服务,同时兼容开发模式 Vite(5173)与桌面窗口)。
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(tower_http::cors::AllowOrigin::predicate(|origin, _| {
                    let host = origin.to_str().ok().and_then(|o| {
                        o.trim_start_matches("http://")
                            .trim_start_matches("https://")
                            .split('/')
                            .next()
                            .and_then(|h| h.split(':').next())
                    });
                    matches!(host, Some("127.0.0.1") | Some("localhost"))
                }))
                .allow_methods(tower_http::cors::Any)
                .allow_headers(tower_http::cors::Any),
        )
        // 请求关联 ID + 访问日志:必须放在**链尾**,成为最外层。
        // axum 的 `.layer()` 是「后加者在外层」,故从外到内为
        // request_id → CORS → DefaultBodyLimit → security::guard → handler。
        // security::guard 的 401/403 早退响应因此也在本层之内,响应头同样带上
        // X-Request-Id(早退路径的可定位性是本批次的关键验收点)。
        .layer(axum::middleware::from_fn(request_id::attach))
}
