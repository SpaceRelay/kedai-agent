// API 路由组装 + health + 头像静态服务 + SPA 回退
pub mod agent;
pub mod agent_flows;
pub mod app_state;
pub mod audio;
pub mod characters;
pub mod chat;
pub mod contracts;
pub mod contract_history;
pub mod import_export;
pub mod kaleido;
pub mod macros;
pub mod plugins;
pub mod prompt_inject;
pub mod quick_replies;
pub mod resource;
pub mod security;
pub mod sessions;
pub mod settings;
pub mod skills;
pub mod slash_commands;
pub mod tokens;
pub mod tool_permissions;
pub mod user_scripts;
pub mod variables;
pub mod world_books;

use crate::api::app_state::AppState;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::json;
use std::sync::Arc;

/// 组装全部路由(API + 头像 + 前端静态)
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        // 健康检查
        .route(
            "/api/health",
            get(|| async {
                Json(json!({
                    "ok": true,
                    "ts": chrono::Utc::now().timestamp_millis(),
                }))
            }),
        )
        // 同源页面仅在 loopback Host/Origin 下引导内存 token;响应禁止缓存。
        .route("/api/bootstrap", get(bootstrap))
        // 角色卡
        .route("/api/characters", get(characters::list))
        .route("/api/characters/upload", post(characters::upload))
        .route(
            "/api/characters/{id}",
            get(characters::get)
                .put(characters::update)
                .delete(characters::delete),
        )
        // 会话 / 消息
        .route(
            "/api/chat/sessions",
            get(sessions::list_sessions).post(sessions::create_session),
        )
        .route(
            "/api/chat/sessions/{id}",
            axum::routing::delete(sessions::delete_session),
        )
        .route(
            "/api/chat/sessions/{id}/truncate",
            post(sessions::truncate_messages),
        )
        .route(
            "/api/chat/sessions/{id}/regreet",
            post(sessions::regreet),
        )
        .route(
            "/api/chat/sessions/{id}/assistant-vars",
            axum::routing::put(sessions::save_assistant_vars),
        )
        .route("/api/chat/history", get(sessions::history))
        .route(
            "/api/chat/messages/{id}",
            put(sessions::update_message).delete(sessions::delete_message),
        )
        .route(
            "/api/chat/messages/{id}/variables",
            axum::routing::patch(sessions::save_variables),
        )
        // 阶段六 6f:切换消息 swipe 版本(extra.swipes 数组)
        .route(
            "/api/chat/messages/{id}/swipe",
            post(sessions::swipe_message),
        )
        // 7 作用域变量(计划二):GET 读整树 / PUT 整树覆写 / PATCH JSON Patch 子集
        .route(
            "/api/variables",
            get(variables::get_variables)
                .put(variables::put_variables)
                .patch(variables::patch_variables),
        )
        .route("/api/chat/init-vars", get(sessions::init_vars))
        .route("/api/chat/clear", post(sessions::clear))
        // 聊天(SSE)
        .route("/api/chat/send", post(chat::send))
        .route("/api/chat/stop", post(chat::stop))
        .route("/api/chat/compact", post(chat::compact))
        .route("/api/chat/compact/clear", post(chat::clear_compact))
        // 设置
        .route(
            "/api/settings",
            get(settings::get_settings).put(settings::update_settings),
        )
        .route("/api/settings/connect", post(settings::connect))
        .route(
            "/api/settings/refresh-models",
            post(settings::refresh_models),
        )
        .route("/api/settings/models", get(settings::models))
        .route("/api/settings/info", get(settings::info))
        .route(
            "/api/settings/model",
            put(settings::switch_model).get(settings::get_model),
        )
        .route(
            "/api/settings/agent-prompt",
            get(settings::get_agent_prompt).put(settings::save_agent_prompt),
        )
        .route(
            "/api/settings/prompt-preview",
            get(settings::prompt_preview),
        )
        // Agent
        .route("/api/agent/plan", post(agent::plan))
        .route("/api/agent/execute", post(agent::execute))
        .route("/api/agent/interrupt", post(agent::interrupt))
        .route(
            "/api/agent/tool-permissions",
            get(tool_permissions::list)
                .post(tool_permissions::authorize)
                .delete(tool_permissions::revoke),
        )
        .route(
            "/api/agent/tool-permissions/resolve",
            post(tool_permissions::resolve),
        )
        // Token / 导入导出
        .route("/api/token/count", post(tokens::count))
        .route("/api/token/session-total", get(tokens::session_total))
        .route("/api/token/global-total", get(tokens::global_total))
        .route("/api/export/chat", get(import_export::export_chat))
        .route("/api/import/chat", post(import_export::import_chat))
        // 世界书
        .route("/api/world-books", get(world_books::list))
        .route("/api/world-books/upload", post(world_books::upload))
        .route(
            "/api/world-books/auto-assign-check",
            get(world_books::auto_assign_check),
        )
        .route(
            "/api/world-books/{id}",
            put(world_books::update).delete(world_books::delete),
        )
        .route(
            "/api/world-books/{id}/entries",
            get(world_books::entries)
                .put(world_books::save_entries)
                .post(world_books::add_entry),
        )
        // 角色卡内嵌世界书(character_book)条目编辑
        .route(
            "/api/characters/{id}/world-entries",
            get(world_books::character_entries).put(world_books::save_character_entries),
        )
        // 契约编辑(P6 面板):读/写/移除角色卡内嵌契约(extensions.nlkaleido)
        .route(
            "/api/characters/{id}/contract",
            get(contracts::get)
                .put(contracts::put)
                .delete(contracts::delete),
        )
        // 契约变更历史与回滚(阶段 C):历史列表(最新在前)+ 按 seq 回滚
        .route(
            "/api/characters/{id}/contract/history",
            get(contract_history::list),
        )
        .route(
            "/api/characters/{id}/contract/rollback",
            post(contract_history::rollback),
        )
        // 契约引擎 HTTP 出口(P7 方案 A):外部工具与 ST 前端调用同一引擎,
        // 与多步工具 apply_patch 共用 VariableApplyService(单库无分叉)
        .route("/api/variable/update", post(kaleido::update))
        .route("/api/variable/state", get(kaleido::get_state))
        .route("/api/variable/changelog", get(kaleido::changelog))
        // 插件(自定义工具)
        .route("/api/plugins/tools", get(plugins::list_tools))
        .route("/api/plugins/tools/reload", post(plugins::reload_tools))
        .route("/api/plugins/tools/upload", post(plugins::upload_tool))
        .route(
            "/api/plugins/tools/{name}",
            axum::routing::delete(plugins::delete_tool),
        )
        // 技能库(提示词技能)
        .route("/api/skills", get(skills::list).post(skills::import_skills))
        .route(
            "/api/skills/{id}",
            put(skills::update).delete(skills::delete),
        )
        // 角色卡远程资源界面代理(https + SSRF 防护)
        .route("/api/resource/proxy", get(resource::proxy))
        // 提示词注入(简单模式 + 楼层系统 + 酒馆预设导入)
        .route(
            "/api/prompt-inject",
            get(prompt_inject::get_prompt_inject).put(prompt_inject::update_prompt_inject),
        )
        .route("/api/prompt-inject/import", post(prompt_inject::import))
        // 快速回复(Quick Replies):getqr 的渲染数据源 + 管理 CRUD
        .route(
            "/api/quick-replies",
            get(quick_replies::list).post(quick_replies::create),
        )
        .route(
            "/api/quick-replies/{id}",
            put(quick_replies::update).delete(quick_replies::delete),
        )
        // 音频播放器(bgm/ambient 双通道):读取 / 设置 / 播放列表
        .route("/api/audio", get(audio::get))
        .route("/api/audio/settings", put(audio::update_settings))
        .route("/api/audio/playlist", put(audio::update_playlist))
        // 用户脚本(ScriptTree,阶段三):global / character 两级脚本树全量读写
        .route(
            "/api/scripts/tree",
            get(user_scripts::get_tree).put(user_scripts::save_tree),
        )
        // slash 命令清单(阶段四 4a):前端输入框联想
        .route("/api/slash/commands", get(slash_commands::list_commands))
        // 宏调试(阶段六 6b):纯扁平 vars 展开,供前端「宏调试」面板验证模板结果
        .route("/api/macros/expand", post(macros::expand))
        // 自定义 Agent 执行流程(custom 模式):流程库 + 当前选择 + 增删
        .route(
            "/api/agent-flows",
            get(agent_flows::get_agent_flows).put(agent_flows::update_agent_flows),
        )
        .route(
            "/api/agent-flows/select",
            post(agent_flows::select_agent_flow),
        )
        .route(
            "/api/agent-flows/{id}",
            axum::routing::delete(agent_flows::delete_agent_flow),
        )
        // 头像静态服务
        .route("/api/avatars/{file}", get(avatar_file))
        // 角色卡脚本沙箱文档(独立 CSP,绕过全站 script-src 'self' 对内联脚本的限制)
        .route("/sandbox.html", get(sandbox_document))
        // 角色卡远程资源界面宿主文档(独立 CSP:允许作者页面脚本/网络,断掉与宿主的一切共享)
        .route("/resource-frame.html", get(resource_frame_document))
        // 消息渲染面板宿主文档(TH-render 等价物:独立 CSP,断掉一切网络出口)
        .route("/render-frame.html", get(render_frame_document))
        // 前端静态(生产:磁盘 dist 或内嵌);SPA 回退
        .fallback(spa_fallback)
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
}

async fn bootstrap(State(state): State<Arc<AppState>>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(axum::body::Body::from(
            json!({ "token": state.config.api_token }).to_string(),
        ))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// GET /api/avatars/{file}:从 DATA_DIR/avatars 读取
async fn avatar_file(State(state): State<Arc<AppState>>, Path(file): Path<String>) -> Response {
    // 防目录穿越
    let safe: String = file
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
        .collect();
    if safe != file {
        return Json(json!({ "error": "Not Found" })).into_response();
    }
    let path = state.config.data_dir.join("avatars").join(&file);
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mime = guess_mime(&file);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .body(axum::body::Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::NOT_FOUND.into_response())
        }
        Err(_) => Json(json!({ "error": "Not Found" })).into_response(),
    }
}

/// GET /sandbox.html — 角色卡脚本沙箱 bootstrap 文档。
///
/// 本身不含业务逻辑:只监听 postMessage({type:'boot', script}),把脚本体作为内联
/// `<script>` 注入自身执行。之所以需要独立文档而不用 srcdoc:srcdoc iframe 会继承
/// 父页面的 CSP,而全站 CSP 是 `script-src 'self'`(无 'unsafe-inline'),内联脚本
/// 会被直接拒绝;用 src 加载的文档则按自身响应头的 CSP 计算(见 security::sandbox_headers)。
/// 动态注入 `<script>` 走的是 script-src,不需要 'unsafe-eval'。
async fn sandbox_document() -> Response {
    const HTML: &str = include_str!("sandbox_template.html");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(
            header::CACHE_CONTROL,
            "no-store, no-cache, must-revalidate, max-age=0",
        )
        .body(axum::body::Body::from(HTML))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// GET /resource-frame.html — 角色卡远程资源界面宿主文档。
///
/// srcdoc/blob iframe 会继承父页面 CSP(script-src 'self'),作者页面(Vite 打包的
/// 内联 module script)无法执行;本独立文档用 src 加载,按自身宽松 CSP
/// (见 security::resource_frame_headers)计算,允许作者页面的脚本/样式/图片/网络,
/// 但不与宿主共享 origin(iframe 无 allow-same-origin),拿不到宿主
/// DOM/localStorage/token。宿主经后端代理抓取作者页面 HTML 后 postMessage 投递,
/// 本页用 DOM 重建(appendChild)触发脚本执行(document.write 会丢失 module script)。
async fn resource_frame_document() -> Response {
    const HTML: &str = include_str!("resource_frame_template.html");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(
            header::CACHE_CONTROL,
            "no-store, no-cache, must-revalidate, max-age=0",
        )
        .body(axum::body::Body::from(HTML))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// GET /render-frame.html — 消息渲染面板宿主文档(TH-render 等价物)。
///
/// 消息正文中约定式识别的 HTML 代码块(含 `<html`/`<head`/`<body` 标记)渲染为
/// 面板 iframe,本文档用 src 加载(不继承父页面 CSP,见 security::render_frame_headers),
/// 宿主经 postMessage 投递面板 HTML,本页 appendChild 注入执行。面板脚本运行在
/// 无 same-origin 的沙箱 iframe 里,断掉一切网络出口,与宿主完全隔离;面板内
/// parent.postMessage 上报高度(kd-panel-resize)与事件(kd-panel-event)。
async fn render_frame_document() -> Response {
    const HTML: &str = include_str!("render_frame_template.html");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(
            header::CACHE_CONTROL,
            "no-store, no-cache, must-revalidate, max-age=0",
        )
        .body(axum::body::Body::from(HTML))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// SPA 回退:先匹配实际静态文件,未命中时 GET 非 /api/ 返回 index.html;否则 404 {"error":"Not Found"}
async fn spa_fallback(State(state): State<Arc<AppState>>, req: axum::extract::Request) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    if method == axum::http::Method::GET && !path.starts_with("/api/") {
        // 归一化静态路径:丢弃 ".." / 空段 / 点段,防止目录穿越读到 web/dist 之外的文件
        let rel: String = path
            .split('/')
            .filter(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
            .collect::<Vec<_>>()
            .join("/");
        let rel = rel.trim_start_matches('/');
        // 根路径或空路径 → 直接 index.html
        let target = if rel.is_empty() { "index.html" } else { rel };
        // 1) 实际静态文件(磁盘优先,回退内嵌)
        if let Some(bytes) = read_file_or_embedded(&state, target) {
            let mime = guess_mime(target);
            // index.html 禁止缓存(no-cache):WebView2/浏览器会缓存旧的 index.html,
            // 导致加载旧 hash 的 JS/CSS,表现为「改了代码重启后还是旧界面」。
            // 其余静态资源(带 hash 的 assets/*)同样禁止缓存,保证部署后立即生效。
            let cache_control = if target == "index.html" {
                "no-store, no-cache, must-revalidate, max-age=0"
            } else {
                "no-cache, must-revalidate"
            };
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, cache_control)
                .body(axum::body::Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
        // 2) SPA 回退:未命中一律返回 index.html(与 Node 版 setNotFoundHandler 一致)
        if let Some(index) = read_file_or_embedded(&state, "index.html") {
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .header(
                    header::CACHE_CONTROL,
                    "no-store, no-cache, must-revalidate, max-age=0",
                )
                .body(axum::body::Body::from(index))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    }
    // 其余一律 404 {"error":"Not Found"}
    Json(json!({ "error": "Not Found" }))
        .into_response()
        .with_status(StatusCode::NOT_FOUND)
}

/// 读取前端资源。发布版默认只使用内嵌 dist；仅显式设置 KEDAI_WEB_DIST 时启用磁盘覆盖。
fn read_file_or_embedded(state: &AppState, rel: &str) -> Option<Vec<u8>> {
    if let Some(web_dist) = &state.config.web_dist {
        let disk = web_dist.join(rel);
        if let Ok(bytes) = std::fs::read(&disk) {
            return Some(bytes);
        }
    }
    embedded::get(rel)
}

/// 内嵌 web/dist(编译时嵌入;要求目录存在)
mod embedded {
    use include_dir::{include_dir, Dir};
    pub static DIST: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../web/dist");

    pub fn get(rel: &str) -> Option<Vec<u8>> {
        let norm = rel.trim_start_matches('/');
        if norm.is_empty() || norm == "index.html" {
            return DIST.get_file("index.html").map(|f| f.contents().to_vec());
        }
        DIST.get_file(norm).map(|f| f.contents().to_vec())
    }
}

fn guess_mime(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if lower.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if lower.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".woff2") {
        "font/woff2"
    } else if lower.ends_with(".ico") {
        "image/x-icon"
    } else {
        "application/octet-stream"
    }
}

/// 便捷:带状态码的 JSON 响应(各路由子模块 use super::WithStatus)
pub trait WithStatus {
    fn with_status(self, code: StatusCode) -> Response;
}

impl WithStatus for Response {
    fn with_status(mut self, code: StatusCode) -> Response {
        *self.status_mut() = code;
        self
    }
}

#[cfg(test)]
mod resource_frame_tests {
    /// 资源界面宿主文档须为沙箱 iframe(无 allow-same-origin)提供 Cache API 兼容层。
    ///
    /// 背景:角色卡作者页面(Vite 打包 SPA,如「干物吸血鬼少女与夜间工作」v2.1)初始化时
    /// 读取 `globalThis.caches` 做资源缓存与更新检查;沙箱 iframe 是 opaque origin,
    /// Cache Storage 被禁用,读取该属性直接抛 SecurityError,作者页面落入「重试」错误态,
    /// 资源界面(下载/协议/角色选择)无法显示。修复:宿主文档注入内存 `__kdCaches` shim,
    /// 并把注入作者脚本中的 `globalThis.caches` 引用替换为 shim(与 localStorage 同款策略)。
    const TEMPLATE: &str = include_str!("resource_frame_template.html");

    #[test]
    fn template_defines_kd_caches_shim() {
        assert!(
            TEMPLATE.contains("__kdCaches"),
            "宿主文档缺少 __kdCaches 内存 shim(Cache API 兼容),沙箱内作者脚本读取 globalThis.caches 会抛 SecurityError"
        );
        // shim 至少支持作者页面用到的 open(),否则界面仍走错误分支
        assert!(
            TEMPLATE.contains("open:") || TEMPLATE.contains("function open"),
            "__kdCaches shim 应暴露 open() 供作者页面缓存/更新检查使用"
        );
    }

    #[test]
    fn template_rewrites_author_caches_access() {
        assert!(
            TEMPLATE.contains(r"globalThis\.caches"),
            "宿主文档应包含把 globalThis.caches 引用重写为 __kdCaches 的脚本重写规则(防沙箱 Cache API SecurityError)"
        );
    }

    #[test]
    fn template_caches_shim_replayable_body() {
        // 作者页面 put 后再次 match 需能重复 arrayBuffer() 读 body;
        // 直接存/返回原 Response 会因 body 已消费走「Cache Missed」分支反复下载(无限循环)。
        assert!(
            TEMPLATE.contains("arrayBuffer()") && TEMPLATE.contains("bodyPromise"),
            "__kdCaches 应把响应体快照为 ArrayBuffer 并在 match 时重建 Response,保证缓存可重复命中,否则作者页面反复重新下载"
        );
    }

    #[test]
    fn template_keeps_existing_storage_shims() {
        assert!(TEMPLATE.contains("__kdMemoryStorage"), "localStorage shim 不应被移除");
        assert!(TEMPLATE.contains("__kdSessionStorage"), "sessionStorage shim 不应被移除");
    }
}
