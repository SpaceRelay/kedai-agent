use crate::api::app_state::AppState;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, uri::Authority, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

const JSON_TYPES: &[&str] = &["application/json", "multipart/form-data"];

pub async fn guard(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let is_api = path.starts_with("/api/");
    if is_api {
        if state.config.auth_required && !valid_host(&state, request.headers().get(header::HOST)) {
            return reject(StatusCode::BAD_REQUEST, "Host 不受信任");
        }
        // 浏览器跨站请求一律拒绝:堵「缺 Origin 直接放行」的跨站旁路——
        // 浏览器发起的跨站请求即便不带 Origin 也会带 Sec-Fetch-Site: cross-site。
        if let Some(fs) = request
            .headers()
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
        {
            if fs.eq_ignore_ascii_case("cross-site") {
                return reject(StatusCode::FORBIDDEN, "跨站请求拒绝");
            }
        }
        if !valid_origin(request.headers().get(header::ORIGIN)) {
            return reject(StatusCode::FORBIDDEN, "Origin 不受信任");
        }
        if mutates(request.method())
            && !valid_content_type(request.headers().get(header::CONTENT_TYPE))
        {
            return reject(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Content-Type 必须为 application/json 或 multipart/form-data",
            );
        }
        // 写请求强化校验(KEDAI_STRICT_CLIENT_HEADER=1 开启,默认关):
        // 要求同源页面附带 X-Kedai-Client 头 + Origin,堵原生客户端可仿造、浏览器跨站
        // 无 Origin 的写旁路。默认关闭以不破坏 curl 等原生调用。
        if mutates(request.method()) && state.config.strict_client_header {
            if request.headers().get("x-kedai-client").is_none() {
                return reject(StatusCode::FORBIDDEN, "缺少 X-Kedai-Client 头");
            }
            if request.headers().get(header::ORIGIN).is_none() {
                return reject(StatusCode::FORBIDDEN, "写请求缺少 Origin");
            }
        }
        if state.config.auth_required && path == "/api/bootstrap" {
            if !state.config.bootstrap_enabled {
                return reject(StatusCode::NOT_FOUND, "bootstrap 已禁用");
            }
            let loopback_peer = request
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .is_some_and(|ConnectInfo(peer)| peer.ip().is_loopback());
            if !loopback_peer {
                return reject(StatusCode::FORBIDDEN, "bootstrap 仅允许真实 loopback 连接");
            }
            // 令牌桶限频:每对端 IP 每窗口最多 N 次,防本地恶意网页/脚本高频枚举 token。
            let peer = request
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(addr)| addr.ip().to_string())
                .unwrap_or_default();
            if !bootstrap_rate_ok(&state.bootstrap_limiter, &peer) {
                return reject(StatusCode::TOO_MANY_REQUESTS, "bootstrap 请求过于频繁");
            }
        }
        // 头像接口豁免 token:<img src="/api/avatars/..." 无法携带 Authorization 头,
        // 且头像文件仅从 DATA_DIR/avatars 读取、无敏感信息;host/origin 校验仍生效。
        let public =
            matches!(path, "/api/health" | "/api/bootstrap") || path.starts_with("/api/avatars/");
        if state.config.auth_required
            && !public
            && !valid_token(&state, request.headers().get(header::AUTHORIZATION))
        {
            return reject(StatusCode::UNAUTHORIZED, "缺少或无效的 bearer token");
        }
    }
    let path = path.to_owned();
    let response = next.run(request).await;
    if path == "/sandbox.html" {
        sandbox_headers(response)
    } else if path == "/resource-frame.html" {
        resource_frame_headers(response)
    } else if path == "/render-frame.html" {
        render_frame_headers(response)
    } else {
        let response = secure_headers(response);
        if is_api {
            api_data_headers(response)
        } else {
            response
        }
    }
}

fn mutates(method: &Method) -> bool {
    matches!(*method, Method::POST | Method::PUT | Method::PATCH)
}

fn valid_content_type(value: Option<&HeaderValue>) -> bool {
    value.and_then(|v| v.to_str().ok()).is_some_and(|v| {
        let lower = v.to_ascii_lowercase();
        JSON_TYPES.iter().any(|allowed| lower.starts_with(allowed))
    })
}

fn valid_token(state: &AppState, value: Option<&HeaderValue>) -> bool {
    value
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|candidate| {
            constant_time_eq(candidate.as_bytes(), state.config.api_token.as_bytes())
        })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

fn valid_origin(value: Option<&HeaderValue>) -> bool {
    let Some(origin) = value.and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https" | "tauri")
        && url.host_str().is_some_and(is_loopback_name)
}

fn authority_host(value: Option<&HeaderValue>) -> Option<String> {
    let raw = value?.to_str().ok()?;
    raw.parse::<Authority>().ok().map(|authority| {
        authority
            .host()
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(authority.host())
            .to_string()
    })
}

fn valid_host(state: &AppState, value: Option<&HeaderValue>) -> bool {
    authority_host(value).is_some_and(|name| {
        is_loopback_name(&name) || (state.config.allow_remote && !name.trim().is_empty())
    })
}

fn is_loopback_name(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

fn reject(status: StatusCode, message: &str) -> Response {
    // 401 附带结构化错误码 UNAUTHORIZED(前端据此提示重新加载页面,见 api/errors.rs);
    // 其余拒绝(403 loopback / 429 限频)保持裸 { error } 契约不变。
    if status == StatusCode::UNAUTHORIZED {
        return secure_headers(crate::api::err_with_code(
            crate::api::ErrorCode::Unauthorized,
            message,
            status,
        ));
    }
    secure_headers((status, Json(json!({ "error": message }))).into_response())
}

/// bootstrap 固定窗口限频(每对端 IP 每窗口 N 次)。
///
/// 窗口 60s、上限 30 次:正常页面只 bootstrap 一次,余量充足;高频枚举被挡。
fn bootstrap_rate_ok(
    limiter: &Arc<Mutex<std::collections::HashMap<String, (std::time::Instant, u32)>>>,
    peer: &str,
) -> bool {
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
    const MAX_REQUESTS: u32 = 30;

    let mut map = limiter.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    let entry = map.entry(peer.to_string()).or_insert((now, 0));
    if now.duration_since(entry.0) >= WINDOW {
        entry.0 = now;
        entry.1 = 0;
    }
    entry.1 += 1;
    entry.1 <= MAX_REQUESTS
}

fn secure_headers(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    headers.insert("content-security-policy", HeaderValue::from_static("default-src 'self'; base-uri 'none'; object-src 'none'; frame-src 'self'; frame-ancestors 'none'; form-action 'self'; img-src 'self' data: blob: https:; media-src 'self' https: http:; font-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self' http://127.0.0.1:* http://localhost:*"));
    response
}

/// /api/ 响应追加数据保护头(堵 T2 缓存/嵌入泄露)。
///
/// - `Cache-Control: no-store`:API 响应含 token/会话/角色数据,禁止任何缓存。
///   用 `entry` 语义:端点已显式设缓存策略(如 SSE 的 `no-cache, no-transform`、
///   bootstrap 的 `no-store`)时尊重原值,否则补默认 no-store。
/// - `Cross-Origin-Resource-Policy: same-origin`:阻止跨源 iframe/预加载读走响应。
fn api_data_headers(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    headers
        .entry("cross-origin-resource-policy")
        .or_insert(HeaderValue::from_static("same-origin"));
    response
}

/// /sandbox.html 专用安全头。
///
/// 全站 CSP 的 `script-src 'self'` 会禁止内联脚本,而 srcdoc iframe 继承父页面 CSP,
/// 导致角色卡脚本无法执行。改用独立文档 `/sandbox.html`(src 加载的文档不继承父 CSP)
/// 后,这里给它一套更宽松但仍然封闭的 CSP:允许内联脚本执行,同时断掉一切网络出口
/// (connect/img/style/font/media 全部 'none'),并用 frame-ancestors 'self' 限定只能被
/// 本机页面框入。iframe 自身仍带 sandbox="allow-scripts"(无 allow-same-origin),
/// 因此脚本运行在不透明来源里,拿不到宿主 DOM、cookie 与 localStorage。
fn sandbox_headers(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    // 同源 iframe 需要 SAMEORIGIN;DENY 会连本机页面的框入一起拒掉
    headers.insert("x-frame-options", HeaderValue::from_static("SAMEORIGIN"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(
            // 'unsafe-eval':角色卡界面 HTML 的 inline 事件处理器(onclick 等,如 WuWa
            // 状态栏 72 处)经降级桥在沙箱内用 new Function 求值;求值对象仅限作者自己
            // 的代码(与用户已授权执行的卡脚本同信任级),沙箱不透明来源+零网络出口不变
            "default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval'; connect-src 'none'; \
             img-src 'none'; style-src 'none'; font-src 'none'; media-src 'none'; \
             object-src 'none'; frame-src 'none'; child-src 'none'; worker-src 'none'; \
             form-action 'none'; base-uri 'none'; frame-ancestors 'self'",
        ),
    );
    response
}

/// /render-frame.html 专用安全头。
///
/// TH-render 等价物宿主文档:消息内 HTML 代码块(含 <html>/<head>/<body 标记)
/// 被约定式识别后,经本文档注入执行渲染。与 sandbox_headers 同级安全:
/// 只放行内联脚本执行,断掉一切网络出口(connect/img/style/font/media 全 'none'),
/// frame-ancestors 'self' 限定只能被本机页面框入;iframe 自身带
/// sandbox="allow-scripts"(无 allow-same-origin),面板脚本运行在不透明来源,
/// 拿不到宿主 DOM、cookie、localStorage 与 API token。
fn render_frame_headers(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    // 同源 iframe 需要 SAMEORIGIN;DENY 会连本机页面的框入一起拒掉
    headers.insert("x-frame-options", HeaderValue::from_static("SAMEORIGIN"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'none'; script-src 'unsafe-inline'; connect-src 'none'; \
             img-src 'none'; style-src 'none'; font-src 'none'; media-src 'none'; \
             object-src 'none'; frame-src 'none'; child-src 'none'; worker-src 'none'; \
             form-action 'none'; base-uri 'none'; frame-ancestors 'self'",
        ),
    );
    response
}

/// /resource-frame.html 专用安全头。
///
/// 角色卡远程资源界面宿主文档:允许作者页面脚本执行,并放开其加载自身资源所需的
/// 网络出口(图片/样式/字体/连接)。iframe 自身带 sandbox="allow-scripts"
/// (无 allow-same-origin),作者脚本运行在不透明来源,拿不到宿主 DOM/localStorage/
/// API token;iframe 内脚本只能 fetch/请求任意 https(作者页面自己的资源服务器,
/// 与直接在新窗口打开该页面等价)。frame-ancestors 'self' 限定只能被本机页面框入。
fn resource_frame_headers(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("SAMEORIGIN"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(
            // img/font/media/connect 放行 blob::作者页(吸血鬼卡等)把解密后的资源包
            // (立绘/文本/音频)经 URL.createObjectURL 生成本地 blob: URL 加载,
            // CSP 不含 blob: 会全部拦截,表现为开场消息/立绘/数值丢失。
            // blob: 只能由该文档自身的 opaque origin 创建,不放宽跨源边界。
            "default-src 'self'; script-src 'unsafe-inline'; connect-src https: http: blob:; \
             img-src https: http: data: blob:; style-src 'self' 'unsafe-inline' https: http:; \
             font-src 'self' https: http: data: blob:; media-src https: http: blob:; \
             object-src 'none'; frame-src https: http:; child-src https: http:; \
             worker-src 'none'; form-action https: http:; base-uri 'none'; \
             frame-ancestors 'self'",
        ),
    );
    response
}
