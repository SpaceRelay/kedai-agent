// 连接设置:OpenAI 兼容 Base URL 规范化、API Key 脱敏展示、默认搜索端点。
use super::RuntimeSettings;

/// 默认搜索端点(DuckDuckGo HTML 免费接口,无需 API Key)
pub const DEFAULT_SEARCH_ENDPOINT: &str = "https://html.duckduckgo.com/html/";

impl RuntimeSettings {
    /// API Key 脱敏展示(仅保留后 4 位)
    pub fn masked_api_key(&self) -> String {
        mask_key(&self.openai_api_key)
    }

    /// embedding API Key 脱敏展示(仅保留后 4 位;与聊天 Key 同策略)
    pub fn masked_embedding_api_key(&self) -> String {
        mask_key(&self.embedding_api_key)
    }
}

/// 脱敏:非空时返回 `****xxxx`(保留后 4 位)
pub fn mask_key(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    let last = key.chars().rev().take(4).collect::<Vec<_>>();
    let last: String = last.into_iter().rev().collect();
    format!("****{last}")
}

/// 规范化 OpenAI 兼容 API 地址(自动补全格式):
/// - 去空白与尾部斜杠
/// - 无协议时补协议:本机地址(localhost/127.x/0.0.0.0/[::1])补 http://,其余补 https://
/// - 无路径时补 `/v1`(OpenAI 兼容服务普遍要求 /v1,缺它请求 /models 会 404)
pub fn normalize_base_url(input: &str) -> String {
    let mut s = input.trim().to_string();
    if s.is_empty() {
        return s;
    }
    // 去尾部斜杠
    while s.ends_with('/') {
        s.pop();
    }
    // 补协议
    if !s.starts_with("http://") && !s.starts_with("https://") {
        let lower = s.to_lowercase();
        let is_local = lower.starts_with("localhost")
            || lower.starts_with("127.")
            || lower.starts_with("0.0.0.0")
            || lower.starts_with("[::1]");
        s = if is_local {
            format!("http://{s}")
        } else {
            format!("https://{s}")
        };
    }
    // 补 /v1(仅当 host 后无任何路径时)
    let (_, rest) = s.split_once("://").unwrap_or(("", s.as_str()));
    let has_path = match rest.find('/') {
        None => false,
        Some(i) => !rest[i..].trim_matches('/').is_empty(),
    };
    if !has_path {
        s.push_str("/v1");
    }
    s
}
