// Token 计数(与 Node 版 token.service.ts / js-tiktoken 行为对齐)
// 编码映射:o200k_base / cl100k_base / p50k_base,精确匹配 → 前缀匹配 → 回退 cl100k_base
use crate::models::types::LlmMessage;
use std::collections::HashMap;

pub struct TokenService {
    cache: HashMap<String, std::sync::Arc<tiktoken_rs::CoreBPE>>,
}

/// 模型 → 编码名(Node 版 MODEL_ENCODINGS)
fn encoding_for_model(model: &str) -> Option<&'static str> {
    let m = model.to_lowercase();
    let base = match m.as_str() {
        "gpt-4o" | "gpt-4o-mini" => "o200k_base",
        "gpt-4" | "gpt-4-turbo" | "gpt-3.5-turbo" | "text-embedding-ada-002" => "cl100k_base",
        "text-davinci-003" => "p50k_base",
        _ => return None,
    };
    Some(base)
}

fn resolve_encoding_name(model: &str) -> &'static str {
    let m = model.to_lowercase();
    // 精确匹配
    if let Some(e) = encoding_for_model(&m) {
        return e;
    }
    // 前缀匹配,如 gpt-4o-2024-11-20 → gpt-4o
    let prefixes = [
        "gpt-4o",
        "gpt-4-turbo",
        "gpt-3.5-turbo",
        "text-embedding-ada-002",
        "gpt-4",
        "text-davinci-003",
    ];
    for p in prefixes {
        if m.starts_with(p) {
            if let Some(e) = encoding_for_model(p) {
                return e;
            }
        }
    }
    // 回退 cl100k_base
    "cl100k_base"
}

impl TokenService {
    pub fn new() -> Self {
        TokenService {
            cache: HashMap::new(),
        }
    }

    fn encoding(&mut self, model: &str) -> Option<std::sync::Arc<tiktoken_rs::CoreBPE>> {
        let name = resolve_encoding_name(model);
        if let Some(bpe) = self.cache.get(name) {
            return Some(bpe.clone());
        }
        // 用该编码对应的代表性模型加载
        let model_for_enc = match name {
            "o200k_base" => "gpt-4o",
            "p50k_base" => "text-davinci-003",
            _ => "gpt-4",
        };
        // BPE 词表首次加载需联网(tiktoken-rs 下载);失败不 panic,降级为估算计数。
        // 失败结果不缓存(None 直接返回),下次调用仍会重试,避免永久降级。
        let bpe = tiktoken_rs::get_bpe_from_model(model_for_enc)
            .or_else(|_| tiktoken_rs::cl100k_base())
            .ok()?;
        let bpe = std::sync::Arc::new(bpe);
        self.cache.insert(name.to_string(), bpe.clone());
        Some(bpe)
    }

    /// 无 BPE 词表时的估算:英文约 4 字符/token、中文约 1 字符/token,
    /// 取 bytes/4 与 chars 的折中并向下取整,偏保守(宁多算不少算,对上下文裁剪更安全)。
    fn estimate_tokens(text: &str) -> i64 {
        let chars = text.chars().count() as i64;
        let bytes = text.len() as i64;
        ((bytes + 3) / 4 + chars / 2).max(1)
    }

    /// 单段文本 token 数(词表加载失败时用估算值)
    pub fn count_tokens(&mut self, text: &str, model: &str) -> i64 {
        match self.encoding(model) {
            Some(bpe) => bpe.encode_ordinary(text).len() as i64,
            None => Self::estimate_tokens(text),
        }
    }

    /// 消息数组计数:每条 content token 数 + 4,最后 +2 回复开场
    pub fn count_message_tokens(&mut self, messages: &[LlmMessage], model: &str) -> i64 {
        let mut total: i64 = 0;
        for m in messages {
            total += self.count_tokens(&m.content, model) + 4;
        }
        total + 2
    }
}

impl Default for TokenService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_encoding() {
        assert_eq!(resolve_encoding_name("gpt-4o-mini"), "o200k_base");
        assert_eq!(resolve_encoding_name("gpt-4o-2024-11-20"), "o200k_base");
        assert_eq!(resolve_encoding_name("gpt-4-turbo"), "cl100k_base");
        assert_eq!(resolve_encoding_name("gpt-3.5-turbo"), "cl100k_base");
        assert_eq!(resolve_encoding_name("text-davinci-003"), "p50k_base");
        assert_eq!(resolve_encoding_name("unknown-model"), "cl100k_base");
    }

    #[test]
    fn test_estimate_tokens() {
        // 非空文本至少 1 token;空文本也 >= 1(占位消息)
        assert!(TokenService::estimate_tokens("") >= 1);
        assert!(TokenService::estimate_tokens("hello world") >= 1);
        // 长中文文本估算 > 短文本
        let zh_long = "你好，世界。".repeat(100);
        let zh_short = "你好";
        assert!(TokenService::estimate_tokens(&zh_long) > TokenService::estimate_tokens(zh_short));
    }

    #[test]
    fn test_count_message_tokens() {
        let mut svc = TokenService::new();
        let msgs = vec![
            LlmMessage::plain("user", "你好"),
            LlmMessage::plain("assistant", "你好,我是助手"),
        ];
        let n = svc.count_message_tokens(&msgs, "gpt-4o-mini");
        // 每条 >= 4 + 内容,最后 + 2
        assert!(n >= 4 + 4 + 2);
    }
}
