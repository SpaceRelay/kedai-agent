// 角色卡远程资源界面代理:服务端 fetch 作者提供的 HTML 页面(下载资源界面),
// 供前端 srcdoc 沙箱 iframe 渲染。
//
// 背景:原版酒馆助手用 `$('body').load('https://…')` 把作者页面内容注入 body 显示;
// kedai 不直接注入远程脚本,改为「服务端 fetch → srcdoc 沙箱」:
//   - 绕开浏览器跨域限制(CORS)与 X-Frame-Options(作者服务器禁止被 iframe 嵌入时,
//     iframe src 直嵌会显示「已阻止此内容」,服务端 fetch 不受此限制)
//   - 安全:仅 https、SSRF 防护(复用 resolve_public_http_url:拒绝 localhost/私网/
//     元数据地址)、绑定已解析 IP 防 DNS rebinding、禁用重定向、响应大小上限
use crate::api::{err_with_code, ErrorCode};
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

/// 按声明编码解码资源页 HTML。
/// from_utf8_lossy 对 GBK/GB2312 等中文编码的作者页面会整页乱码(替换字符铺满),
/// 这里按 content-type charset → <meta charset> 嗅探 → UTF-8 兜底的顺序选择编码。
fn decode_resource_html(bytes: &[u8], content_type: Option<&str>) -> String {
    let label = content_type
        .and_then(charset_from_content_type)
        .or_else(|| sniff_meta_charset(bytes));
    let encoding = label
        .as_deref()
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    encoding.decode(bytes).0.into_owned()
}

/// 解析 content-type 中的 charset 参数(text/html; charset=gbk)
fn charset_from_content_type(content_type: &str) -> Option<String> {
    for part in content_type.split(';').skip(1) {
        let (k, v) = part.split_once('=')?;
        if k.trim().eq_ignore_ascii_case("charset") {
            let label = v.trim().trim_matches('"').trim_matches('\'');
            if !label.is_empty() {
                return Some(label.to_string());
            }
        }
    }
    None
}

/// 嗅探前 4KB 里的 <meta charset="..."> / <meta http-equiv content="...charset=...">
fn sniff_meta_charset(bytes: &[u8]) -> Option<String> {
    let head_len = bytes.len().min(4096);
    let head = String::from_utf8_lossy(&bytes[..head_len]).to_lowercase();
    let idx = head.find("charset")?;
    let after = &head[idx + "charset".len()..];
    let after = after.trim_start();
    let after = after.strip_prefix('=')?;
    let after = after.trim_start();
    let after = after.trim_start_matches(['"', '\'']);
    let end = after
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != ':')
        .unwrap_or(after.len());
    let label = after[..end].trim();
    if label.is_empty() {
        None
    } else {
        Some(label.to_string())
    }
}
/// GET /api/resource/proxy?url=<https URL>
/// 成功 `{ ok: true, html, base_url }`;失败 `{ ok: false, error, code }` + 真实状态码
/// (批次 1 起统一带 code,不再出现「无 code 的裸 error」)。
pub async fn proxy(Query(q): Query<ProxyQuery>) -> Response {
    let raw = q.url.trim();
    // 仅 https(作者资源页强制加密传输;http/javascript:/data: 一律拒绝)
    let parsed = match reqwest::Url::parse(raw) {
        Ok(u) if u.scheme() == "https" && u.host_str().is_some() => u,
        _ => {
            return err_with_code(
                ErrorCode::Validation,
                "仅支持 https 资源地址",
                StatusCode::BAD_REQUEST,
            );
        }
    };
    // SSRF 防护:拒绝 localhost / 私网 / 元数据地址
    let target = match resolve_public_http_url(raw).await {
        Ok(t) => t,
        Err(e) => {
            // 泄露封堵(批次 1):该解析器为**搜索工具**编写,错误串里既有「搜索端点」
            // 这类错域措辞,DNS 失败分支还会拼 OS 原文(os error 11001),两者都不该
            // 出现在资源卡片的用户提示里。原文只进日志,回给用户稳定文案。
            tracing::warn!(error = %e, url = raw, "资源地址解析被拒");
            return err_with_code(
                ErrorCode::Validation,
                "资源地址不可用,请确认其为公开可访问的 https 地址",
                StatusCode::BAD_REQUEST,
            );
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
            // 泄露封堵:reqwest 构建错误原文只进日志
            tracing::error!(error = %e, "构建资源代理请求客户端失败");
            return err_with_code(
                ErrorCode::Internal,
                "构建请求客户端失败,详情见服务端日志",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let resp = match client.get(raw).send().await {
        Ok(r) => r,
        Err(e) => {
            // 泄露封堵:上游连接错误原文(含 IP/端口/证书细节)只进日志
            tracing::error!(error = %e, url = raw, "请求资源页失败");
            return err_with_code(
                ErrorCode::Upstream,
                "请求资源页失败,请稍后重试",
                StatusCode::BAD_GATEWAY,
            );
        }
    };
    if resp.status().is_redirection() {
        return err_with_code(
            ErrorCode::Upstream,
            "资源页重定向已禁用",
            StatusCode::BAD_GATEWAY,
        );
    }
    if !resp.status().is_success() {
        // 上游状态码属用户可理解信息(404/403 等),保留但不带响应正文
        return err_with_code(
            ErrorCode::Upstream,
            format!("资源页返回 {}", resp.status()),
            StatusCode::BAD_GATEWAY,
        );
    }
    // content-type 在 bytes() 消费 resp 前取出(charset 解码用)
    let resp_content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            // 泄露封堵:读取中断原文只进日志
            tracing::error!(error = %e, url = raw, "读取资源页失败");
            return err_with_code(
                ErrorCode::Upstream,
                "读取资源页失败,请稍后重试",
                StatusCode::BAD_GATEWAY,
            );
        }
    };
    if bytes.len() > MAX_RESOURCE_BYTES {
        return err_with_code(
            ErrorCode::Upstream,
            "资源页过大(超过 5MB)",
            StatusCode::BAD_GATEWAY,
        );
    }
    let html = decode_resource_html(&bytes, resp_content_type.as_deref());
    // 作者域名作为 base(srcdoc 内相对 CSS/JS/图片按作者域名解析)
    let base_url = format!(
        "{}://{}",
        parsed.scheme(),
        parsed.host_str().unwrap_or_default()
    );
    Json(json!({ "ok": true, "html": html, "base_url": base_url })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_charset_from_content_type() {
        assert_eq!(
            charset_from_content_type("text/html; charset=gbk").as_deref(),
            Some("gbk")
        );
        assert_eq!(
            charset_from_content_type("text/html; charset=\"GB2312\"").as_deref(),
            Some("GB2312")
        );
        assert_eq!(charset_from_content_type("text/html"), None);
    }

    #[test]
    fn sniffs_meta_charset() {
        let head = br#"<html><head><meta charset="gbk"></head>"#;
        assert_eq!(sniff_meta_charset(head).as_deref(), Some("gbk"));
        let equiv = br#"<meta http-equiv="Content-Type" content="text/html; charset=GB18030">"#;
        assert_eq!(sniff_meta_charset(equiv).as_deref(), Some("gb18030"));
        assert_eq!(sniff_meta_charset(b"<html><head></head>"), None);
    }

    #[test]
    fn decodes_gbk_instead_of_lossy_mojibake() {
        // GBK 编码的「中文」二字;from_utf8_lossy 会产生替换字符乱码
        let (gbk, _, _) = encoding_rs::GBK.encode("中文页面");
        assert_eq!(
            decode_resource_html(&gbk, Some("text/html; charset=gbk")),
            "中文页面"
        );
        // 无 content-type 时走 meta 嗅探
        let mut html = b"<meta charset=gbk>".to_vec();
        html.extend_from_slice(&gbk);
        assert_eq!(
            decode_resource_html(&html, None),
            "<meta charset=gbk>中文页面"
        );
        // UTF-8 默认兜底
        assert_eq!(
            decode_resource_html("中文页面".as_bytes(), None),
            "中文页面"
        );
    }
}
