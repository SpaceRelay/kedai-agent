// 静态资源与自包含文档:bootstrap / 头像 / iframe 宿主文档 / SPA 回退 / 内嵌 dist
// (自 api/mod.rs 迁入,纯代码移动,行为不变)
use crate::api::app_state::AppState;
use crate::api::util::WithStatus;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::sync::Arc;

pub(crate) async fn bootstrap(State(state): State<Arc<AppState>>) -> Response {
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
pub(crate) async fn avatar_file(
    State(state): State<Arc<AppState>>,
    Path(file): Path<String>,
) -> Response {
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
            // 头像真实字节嗅探优先:上传的头像一律存为 .png(历史行为),但 JSON 卡可能
            // 携带 JPEG/WebP 头像,扩展名猜测会给出错误 Content-Type,nosniff 下无法显示
            let mime = sniff_image_mime(&bytes).unwrap_or_else(|| guess_mime(&file));
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .body(axum::body::Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::NOT_FOUND.into_response())
        }
        Err(_) => Json(json!({ "error": "Not Found" })).into_response(),
    }
}

/// 按魔数嗅探图片真实格式(头像字节与扩展名不一致时纠正 Content-Type)
fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"BM") {
        Some("image/bmp")
    } else {
        None
    }
}

/// GET /sandbox.html — 角色卡脚本沙箱 bootstrap 文档。
///
/// 本身不含业务逻辑:只监听 postMessage({type:'boot', script}),把脚本体作为内联
/// `<script>` 注入自身执行。之所以需要独立文档而不用 srcdoc:srcdoc iframe 会继承
/// 父页面的 CSP,而全站 CSP 是 `script-src 'self'`(无 'unsafe-inline'),内联脚本
/// 会被直接拒绝;用 src 加载的文档则按自身响应头的 CSP 计算(见 security::sandbox_headers)。
/// 动态注入 `<script>` 走的是 script-src,不需要 'unsafe-eval'。
pub(crate) async fn sandbox_document() -> Response {
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
pub(crate) async fn resource_frame_document() -> Response {
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
pub(crate) async fn render_frame_document() -> Response {
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
pub(crate) async fn spa_fallback(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
) -> Response {
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
        if let Some(bytes) = read_file_or_embedded(&state, target).await {
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
        // 2) SPA 回退:仅对「路由式路径」返回 index.html。
        //    静态资源(assets/*.js|css、*.ico 等)未命中时必须 404 —— 若也回退成
        //    index.html,浏览器会把 HTML 当 JS 解析,前端表现为「面板加载失败」这类
        //    误导性报错,且懒加载重试永远失败。典型触发:前端重建后 chunk hash 变化,
        //    旧页面/旧缓存仍请求已不存在的旧 chunk。
        //    判定:末段含 '.' 视为资源请求(本应用为单页 hash 路由,无带点的路径)。
        let last_seg = rel.rsplit('/').next().unwrap_or("");
        let looks_like_asset = rel.starts_with("assets/") || last_seg.contains('.');
        if !looks_like_asset {
            if let Some(index) = read_file_or_embedded(&state, "index.html").await {
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
    }
    // 其余一律 404 {"error":"Not Found"}
    Json(json!({ "error": "Not Found" }))
        .into_response()
        .with_status(StatusCode::NOT_FOUND)
}

/// 读取前端资源。发布版默认只使用内嵌 dist；仅显式设置 KEDAI_WEB_DIST 时启用磁盘覆盖。
/// 磁盘分支用 tokio::fs(B-1:不在 tokio worker 上做同步文件 IO);
/// 内嵌分支是编译期嵌入的内存数据,保持同步零开销。
async fn read_file_or_embedded(state: &AppState, rel: &str) -> Option<Vec<u8>> {
    if let Some(web_dist) = &state.config.web_dist {
        let disk = web_dist.join(rel);
        if let Ok(bytes) = tokio::fs::read(&disk).await {
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
        // \bcaches\b 一条规则同时覆盖裸 caches / window.caches / self.caches / globalThis.caches
        // (__kdCaches 经 var 提升为全局对象属性,四种写法都解析到 shim)
        assert!(
            TEMPLATE.contains(r"\bcaches\b"),
            "宿主文档应包含把 caches 引用(裸/window./self./globalThis.)重写为 __kdCaches 的脚本重写规则(防沙箱 Cache API SecurityError)"
        );
    }

    #[test]
    fn template_nonce_survives_reload() {
        // 作者页面下载完成后 location.reload() 自举:reload 回来的是空宿主文档,
        // nonce 必须跨 reload 存活(window.name 持有;hash 可能被 SPA 路由改写),
        // 且每次文档加载都要发 ready,父页面才能以缓存 HTML + 存储快照重新 boot。
        assert!(
            TEMPLATE.contains("window.name"),
            "宿主文档应把 nonce 存入 window.name 以在 reload 后恢复,否则作者页面自举后永远停在空白模板"
        );
    }

    #[test]
    fn template_syncs_shim_mutations_to_parent() {
        // shim 持久化桥:localStorage/sessionStorage/Cache 变更须经 postMessage(store-sync)
        // 同步到父页面持久化,否则 reload/重启后下载成果丢失,仍回到下载页。
        assert!(
            TEMPLATE.contains("store-sync"),
            "宿主文档 shim 变更应发送 store-sync 消息供父页面持久化(下载→reload→命中缓存→进游戏界面链路)"
        );
        assert!(
            TEMPLATE.contains("__prime"),
            "宿主文档应支持 boot 快照恢复(__prime),否则重新 boot 后作者脚本读不到已下载的缓存"
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
        assert!(
            TEMPLATE.contains("__kdMemoryStorage"),
            "localStorage shim 不应被移除"
        );
        assert!(
            TEMPLATE.contains("__kdSessionStorage"),
            "sessionStorage shim 不应被移除"
        );
    }

    #[test]
    fn template_installs_tavern_helper_shim() {
        // 酒馆助手式资源页(吸血鬼卡 v2.1)用 window.TavernHelper.getCharData("current").data
        // 的 creator/character_version/creator_notes 推导资源包 AES 口令;
        // kedai 环境没有 TavernHelper 全局,口令为空 → 解密必抛 OperationError,
        // 96MB 下载完成后必然失败回下载页。宿主文档须在注入作者脚本前装好 shim,
        // 数据源是 boot 消息携带的 charData(当前角色卡元数据)。
        assert!(
            TEMPLATE.contains("window.TavernHelper"),
            "宿主文档应安装 window.TavernHelper 兼容 shim,否则作者页推导资源包口令失败、解密必然 OperationError"
        );
        assert!(
            TEMPLATE.contains("getCharData"),
            "TavernHelper shim 应提供 getCharData(作者页推导口令的入口)"
        );
        assert!(
            TEMPLATE.contains("m.charData"),
            "boot 消息应携带 charData(当前角色卡 creator/character_version/creator_notes)"
        );
        assert!(
            TEMPLATE.contains("getVariables") && TEMPLATE.contains("insertOrAssignVariables"),
            "TavernHelper shim 应提供变量读写(作者页设置/剧情变量持久化,经 storage shim 落地)"
        );
        // 吸血鬼卡 v2.1 完整 API 面:世界书(开场消息/设定来源)+ generate(开场白生成)
        // + 预设/事件安全空值;缺任一项作者页落入「文本为空喵」等错误态
        assert!(
            TEMPLATE.contains("getCharWorldbookNames") && TEMPLATE.contains("getWorldbook"),
            "TavernHelper shim 应提供世界书读接口(作者页开局消息/设定从角色世界书条目读取)"
        );
        assert!(
            TEMPLATE.contains("normalizeEntry") && TEMPLATE.contains("e.comment"),
            "世界书条目应做 name/comment 字段归一化(角色卡原始数据条目名在 comment,作者页按 m.name 查找,缺归一化则开局消息缺失)"
        );
        assert!(
            TEMPLATE.contains("m.worldbook"),
            "boot 消息应携带 worldbook(getCharWorldbookNames 是同步接口,只能随 boot 预发数据)"
        );
        assert!(
            TEMPLATE.contains("tavern-call") && TEMPLATE.contains("tavern-result"),
            "TavernHelper.generate 应经 postMessage RPC 桥(沙箱 iframe 无 token,须宿主转发)"
        );
        assert!(
            TEMPLATE.contains("eventOn"),
            "TavernHelper shim 应提供 eventOn 安全空值(作者页订阅酒馆事件,缺失会崩)"
        );
    }

    #[test]
    fn template_shims_parent_for_wide_mode() {
        // 作者页(吸血鬼卡 v2.1)的宽屏机制读 window.parent.document 往父文档注入
        // 撑满样式(为同源嵌入的酒馆设计);沙箱 iframe 下必抛 SecurityError,作者页
        // alert「宿主不允许访问父页面样式,无法进入宽屏」并停在下载完成页,游戏界面
        // 永不出现。宿主文档须在作者脚本执行前用代理遮蔽 window.parent(document 指向
        // 自身文档,postMessage 转发真实父窗口);消息桥与来源校验改用 REAL_PARENT,
        // 不受遮蔽影响。
        assert!(
            TEMPLATE.contains("var REAL_PARENT = window.parent"),
            "宿主文档应在顶层抓住真实父窗口引用(REAL_PARENT),消息桥不依赖可被替换的 parent"
        );
        assert!(
            TEMPLATE.contains("installParentShim") && TEMPLATE.contains("window.parent = proxy"),
            "宿主文档应安装 window.parent 代理([Replaceable] 属性赋值遮蔽),否则作者页宽屏机制抛 SecurityError 卡在下载完成页"
        );
        assert!(
            TEMPLATE.contains("event.source !== REAL_PARENT"),
            "message 来源校验应使用 REAL_PARENT,否则代理遮蔽后宿主 boot 消息被拒收"
        );
    }

    #[test]
    fn template_shims_raf_for_occluded_oopif() {
        // 沙箱 iframe(无 allow-same-origin)在站点隔离下是独立进程;宿主窗口被
        // 遮挡/最小化或宿主 WebView 面板非激活时,rAF 会被无限期停发(实测窗口前台
        // 但面板非激活时 23 秒才触发一次)。作者页(吸血鬼卡 v2.1)的下载进度、解密
        // 分片、「进入游戏」初始化动画全挂在 rAF 上,表现为点击后撒花定格、游戏界面
        // 永不出现。模板须把 rAF 映射到 setTimeout(遮挡下仅降频不停摆)。
        assert!(
            TEMPLATE.contains("window.requestAnimationFrame = function (cb)"),
            "宿主文档应为作者页安装 rAF→setTimeout 兜底,否则宿主 WebView 不可见时初始化卡死"
        );
        assert!(
            TEMPLATE.contains("window.cancelAnimationFrame = function (id)"),
            "cancelAnimationFrame 应一并映射到 clearTimeout,避免作者页取消动画失效"
        );
        // 宿主宽屏切换改变 iframe 尺寸时,作者页不一定收到 resize(实测切换后舞台
        // 按旧尺寸渲染缩在角落,需额外尺寸扰动才重排)。模板须轮询补发 resize。
        assert!(
            TEMPLATE.contains("__kdLastW")
                && TEMPLATE.contains("dispatchEvent(new Event('resize'))"),
            "宿主文档应轮询 innerWidth/innerHeight 并补发 resize,否则宽屏切换后作者页不重排"
        );
    }

    #[test]
    fn template_provides_real_preset_store() {
        // 吸血鬼卡 v2.1 实测:作者页经 getPreset('in_use') 读流式开关(空值 → 误判流式
        // 未关闭)、经 replacePreset 注入「防掉格式」提示词(空桩 → 注入永远失败,
        // 模型回复不带 JSON 变量块 → 丢格式 + 变量不更新)。模板必须提供真实预设 store
        // (内存 storage 经 store-sync 持久化),并把启用提示词并入 generate 请求。
        assert!(
            TEMPLATE.contains("kedai.tavern-preset.v1"),
            "模板应提供预设持久化键(kedai.tavern-preset.v1)"
        );
        assert!(
            TEMPLATE.contains("window.getPreset = getPresetImpl")
                && TEMPLATE.contains("window.replacePreset = replacePresetImpl"),
            "模板应把 getPreset/replacePreset 装为全局函数(作者页先查全局再查 TavernHelper)"
        );
        assert!(
            TEMPLATE.contains("__preset_prompts"),
            "generate 应并入预设启用的提示词(__preset_prompts),否则注入的提示永不生效"
        );
        assert!(
            TEMPLATE.contains("should_stream: false"),
            "默认预设应声明非流式(should_stream:false),作者页流式检测才不误报"
        );
    }
}

#[cfg(test)]
mod avatar_mime_tests {
    use super::sniff_image_mime;

    #[test]
    fn sniffs_real_format_over_extension() {
        // 历史行为:头像一律存 .png,但字节可能是 JPEG/WebP —— 服务端按魔数纠正
        assert_eq!(
            sniff_image_mime(&[0x89, 0x50, 0x4E, 0x47, 0x0D]),
            Some("image/png")
        );
        assert_eq!(
            sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_image_mime(b"GIF89a.."), Some("image/gif"));
        assert_eq!(
            sniff_image_mime(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(sniff_image_mime(b"BMxxxx"), Some("image/bmp"));
        assert_eq!(sniff_image_mime(b"{\"json\":true}"), None);
        assert_eq!(sniff_image_mime(b""), None);
    }
}
