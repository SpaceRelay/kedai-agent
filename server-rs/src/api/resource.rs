// 角色卡远程资源界面代理:服务端 fetch 作者提供的 HTML 页面(下载资源界面),
// 供前端 srcdoc 沙箱 iframe 渲染。
//
// 背景:原版酒馆助手用 `$('body').load('https://…')` 把作者页面内容注入 body 显示;
// kedai 不直接注入远程脚本,改为「服务端 fetch → srcdoc 沙箱」:
//   - 绕开浏览器跨域限制(CORS)与 X-Frame-Options(作者服务器禁止被 iframe 嵌入时,
//     iframe src 直嵌会显示「已阻止此内容」,服务端 fetch 不受此限制)
//   - 安全:仅 https、SSRF 防护(复用 resolve_public_http_url:拒绝 localhost/私网/
//     元数据地址)、绑定已解析 IP 防 DNS rebinding、禁用重定向、响应大小上限
use crate::api::WithStatus;
use crate::tools::agent_tools::resolve_public_http_url;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

/// 资源页大小上限(HTML 文本;超过拒绝,防内存压力)
const MAX_RESOURCE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Deserialize)]
pub struct ProxyQuery {
    pub url: String,
}

/// GET /api/resource/proxy?url=<https URL>
/// 返回 { ok: true, html, base_url } 或 { ok: false, error }(带状态码)。
pub async fn proxy(Query(q): Query<ProxyQuery>) -> Response {
    let raw = q.url.trim();
    // 仅 https(作者资源页强制加密传输;http/javascript:/data: 一律拒绝)
    let parsed = match reqwest::Url::parse(raw) {
        Ok(u) if u.scheme() == "https" && u.host_str().is_some() => u,
        _ => {
            return Json(json!({ "ok": false, "error": "仅支持 https 资源地址" }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST);
        }
    };
    // SSRF 防护:拒绝 localhost / 私网 / 元数据地址
    let target = match resolve_public_http_url(raw).await {
        Ok(t) => t,
        Err(e) => {
            return Json(json!({ "ok": false, "error": e }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST);
        }
    };
    // 绑定已解析 IP(防 DNS rebinding)+ 禁用重定向(防跳到本地)+ 短超时
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Kedai/0.2");
    for address in &target.addresses {
        builder = builder.resolve(&target.host, *address);
    }
    let client = match builder.build() {
        Ok(c) => c,
        Err(e) => {
            return Json(json!({ "ok": false, "error": format!("构建请求客户端失败: {e}") }))
                .into_response()
                .with_status(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let resp = match client.get(raw).send().await {
        Ok(r) => r,
        Err(e) => {
            return Json(json!({ "ok": false, "error": format!("请求资源页失败: {e}") }))
                .into_response()
                .with_status(StatusCode::BAD_GATEWAY);
        }
    };
    if resp.status().is_redirection() {
        return Json(json!({ "ok": false, "error": "资源页重定向已禁用" }))
            .into_response()
            .with_status(StatusCode::BAD_GATEWAY);
    }
    if !resp.status().is_success() {
        return Json(json!({ "ok": false, "error": format!("资源页返回 {}", resp.status()) }))
            .into_response()
            .with_status(StatusCode::BAD_GATEWAY);
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return Json(json!({ "ok": false, "error": format!("读取资源页失败: {e}") }))
                .into_response()
                .with_status(StatusCode::BAD_GATEWAY);
        }
    };
    if bytes.len() > MAX_RESOURCE_BYTES {
        return Json(json!({ "ok": false, "error": "资源页过大(超过 5MB)" }))
            .into_response()
            .with_status(StatusCode::BAD_GATEWAY);
    }
    let html = String::from_utf8_lossy(&bytes).to_string();
    // 作者域名作为 base(srcdoc 内相对 CSS/JS/图片按作者域名解析)
    let base_url = format!(
        "{}://{}",
        parsed.scheme(),
        parsed.host_str().unwrap_or_default()
    );
    Json(json!({ "ok": true, "html": html, "base_url": base_url })).into_response()
}
