// Agent 强化工具集 · search 域(中层 L3 青层工具域):
//   search 联网搜索(DuckDuckGo 默认,端点可配置)
// 安全核心:resolve_public_http_url 拒绝 localhost/私网/元数据地址(SSRF 防护),
// 供搜索端点与 api/resource.rs 资源代理共用(经 agent_tools.rs 重导出,保持原路径)。
// 附带 HTML 解析与百分号编解码辅助函数。
use crate::models::types::{ToolContext, ToolDefinition};
use crate::services::settings_service::DEFAULT_SEARCH_ENDPOINT;
use crate::tools::registry::ToolRegistry;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use super::agent_tools::ToolDeps;

// ==================== search:联网搜索(DDG 默认,端点可配置) ====================
pub(super) fn register_search(registry: &ToolRegistry, deps: Arc<ToolDeps>) {
    registry.register(
        ToolDefinition {
            name: "search".into(),
            description: "联网搜索:返回标题/链接/摘要列表。默认使用 DuckDuckGo 免费接口(可在设置中更换搜索端点,如自建 SearXNG)。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "max_results": { "type": "integer", "description": "结果条数(默认 5,上限 10)" }
                },
                "required": ["query"]
            }),
        },
        Arc::new(move |args: Value, _ctx: ToolContext| {
            let deps = deps.clone();
            Box::pin(async move {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                if query.is_empty() {
                    return Err("缺少 query 参数".into());
                }
                let max_results = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(5).clamp(1, 10) as usize;
                let endpoint = {
                    // 设置快照:不留锁跨 await(锁在 settings_snapshot 内即释放)
                    let s = deps.settings_snapshot();
                    let e = s.search_endpoint.trim().to_string();
                    if e.is_empty() { DEFAULT_SEARCH_ENDPOINT.to_string() } else { e }
                };
                let sep = if endpoint.contains('?') { "&" } else { "?" };
                let url = format!("{endpoint}{sep}q={}", percent_encode(&query));
                let target = resolve_public_http_url(&url).await?;
                let mut client_builder = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(15))
                    .redirect(reqwest::redirect::Policy::none());
                for address in &target.addresses {
                    client_builder = client_builder.resolve(&target.host, *address);
                }
                let client = client_builder
                    .build()
                    .map_err(|e| format!("构建搜索客户端失败: {e}"))?;
                let resp = client
                    .get(&url)
                    .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Kedai/0.2")
                    .send()
                    .await
                    // 连接类失败(2026-09-15 实测:同一端点两次独立调用均失败,中文长查询
                    // 与单词查询结果一致,证明失败在端点这一侧而非查询串)给出可执行出路,
                    // 否则模型只会反复重试同一个连不通的地址。
                    .map_err(|e| {
                        format!(
                            "搜索端点不可达({endpoint}):{e}。\
                             本次失败与查询词无关,重试同一端点大概率仍失败;\
                             请在设置中改用可达的搜索端点(如自建 SearXNG),或改用 read(type=file) 等离线资料"
                        )
                    })?;
                if resp.status().is_redirection() {
                    return Err(format!(
                        "搜索接口重定向已禁用(端点 {endpoint} 返回重定向;该端点可能需要登录或已失效)"
                    ));
                }
                if !resp.status().is_success() {
                    return Err(format!(
                        "搜索接口返回 {}(端点 {endpoint};请确认端点是可用搜索服务或在设置中更换)",
                        resp.status()
                    ));
                }
                let html = resp.text().await.map_err(|e| format!("读取搜索响应失败: {e}"))?;
                let results = parse_search_html(&html, max_results);
                Ok(json!({ "query": query, "total": results.len(), "results": results }).to_string())
            })
        }),
    );
}

#[derive(Debug)]
pub(crate) struct ResolvedHttpTarget {
    pub(crate) host: String,
    pub(crate) addresses: Vec<std::net::SocketAddr>,
}

/// 解析并校验公开 HTTP(S) 目标:拒绝 localhost/私网/元数据地址(SSRF 防护)。
/// 供搜索端点与角色卡资源代理共用。
pub(crate) async fn resolve_public_http_url(raw: &str) -> Result<ResolvedHttpTarget, String> {
    // 补回 url crate 原错(如 invalid port / relative URL 等具体成因);其 Display
    // 不回显完整 URL,无泄露风险
    let url = reqwest::Url::parse(raw).map_err(|e| format!("搜索端点 URL 无效({e})"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("搜索端点仅允许 http/https".into());
    }
    let host = url.host_str().ok_or("搜索端点缺少主机")?;
    if host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case("metadata.google.internal")
    {
        return Err("搜索端点指向受保护地址".into());
    }
    let port = url.port_or_known_default().ok_or("搜索端点端口无效")?;
    let resolved: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("解析搜索端点失败: {e}"))?
        .collect();
    if resolved.is_empty() {
        return Err("搜索端点未解析到地址".into());
    }
    if resolved.iter().any(|address| !is_public_ip(address.ip())) {
        return Err("搜索端点解析到受保护地址".into());
    }
    Ok(ResolvedHttpTarget {
        host: host.to_string(),
        addresses: resolved,
    })
}

pub(crate) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.octets()[0] == 0
                || ip.octets()[0] >= 224
                || ip == Ipv4Addr::new(169, 254, 169, 254))
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(mapped));
            }
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || is_ipv6_unique_local(ip)
                || is_ipv6_link_local(ip))
        }
    }
}

fn is_ipv6_unique_local(ip: Ipv6Addr) -> bool {
    ip.octets()[0] & 0xfe == 0xfc
}
fn is_ipv6_link_local(ip: Ipv6Addr) -> bool {
    ip.segments()[0] & 0xffc0 == 0xfe80
}

/// 解析 DuckDuckGo HTML 搜索结果(result__a 标题 + result__snippet 摘要)
pub(super) fn parse_search_html(html: &str, max: usize) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    // 两个正则为写死字面量,编译必然成功
    let title_re = regex::Regex::new(
        r#"(?is)<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]*)"[^>]*>(.*?)</a>"#,
    )
    .expect("搜索结果标题正则为常量,编译必然成功");
    let snip_re =
        regex::Regex::new(r#"(?is)<a[^>]*class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</a>"#)
            .expect("搜索结果摘要正则为常量,编译必然成功");
    let snips: Vec<String> = snip_re
        .captures_iter(html)
        .map(|c| strip_html(&c[1]))
        .collect();
    let mut si = 0usize;
    for cap in title_re.captures_iter(html) {
        if out.len() >= max {
            break;
        }
        let raw_url = decode_html(&cap[1]);
        let url = normalize_search_url(&raw_url);
        let title = strip_html(&cap[2]);
        if title.is_empty() || url.is_empty() {
            continue;
        }
        let snippet = snips.get(si).cloned().unwrap_or_default();
        si += 1;
        out.push(json!({ "title": title, "url": url, "snippet": snippet }));
    }
    out
}

/// DDG 结果链接是 //duckduckgo.com/l/?uddg=<编码后的真实地址>&rut=… → 提取 uddg
pub(super) fn normalize_search_url(raw: &str) -> String {
    if let Some(pos) = raw.find("uddg=") {
        let rest = &raw[pos + 5..];
        let encoded = rest.split('&').next().unwrap_or("");
        let decoded = percent_decode(encoded);
        if decoded.starts_with("http://") || decoded.starts_with("https://") {
            return decoded;
        }
    }
    if raw.starts_with("//") {
        return format!("https:{raw}");
    }
    raw.to_string()
}

/// 简单 HTML 标签剥除
pub(super) fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_html(&out).trim().to_string()
}

/// 解码常见 HTML 实体
pub(super) fn decode_html(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// 百分号编码(仅编码需要转义的字符,保留安全字符)
pub(super) fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 百分号解码
pub(super) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
