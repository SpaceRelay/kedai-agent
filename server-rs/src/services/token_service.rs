// Token 计数(与 Node 版 token.service.ts / js-tiktoken 行为对齐)
// 编码映射:o200k_base / cl100k_base / p50k_base,精确匹配 → 前缀匹配 → 回退 cl100k_base
use crate::models::types::LlmMessage;
use std::collections::HashMap;

/// 单段文本计数缓存容量上限(2026-09-14)。
/// 键 = (编码名, 文本哈希, 文本字节长度)。存哈希而非原文避免长文本双份内存;
/// 64 位哈希 + 长度双重校验后碰撞概率可忽略,且即便碰撞也只影响裁剪量的微小偏差,
/// 不会造成数据错误(裁剪只决定「丢掉哪些历史」)。
///
/// 为什么需要:工具循环的预算闸门每摘要一轮就重算**全部**消息的 token
/// (`messages/trim.rs` 的预算循环),是 O(N²) 编码。实测 30 条 × 2k token 历史单次
/// 计数 29.4 ms,真实失控任务(88 万 token)下单次可达数百毫秒,且在循环里反复调用。
/// 有缓存后只有被改动的消息需要重编码,其余命中缓存,循环开销降为近似 O(N)。
const COUNT_CACHE_CAP: usize = 8192;

pub struct TokenService {
    cache: HashMap<String, std::sync::Arc<tiktoken_rs::CoreBPE>>,
    /// 文本 token 计数缓存:(编码名, 文本哈希, 字节长度) → token 数
    count_cache: HashMap<(String, u64, usize), i64>,
}

/// 文本哈希(缓存键用):FNV-1a 64 位,快速且无需引入额外依赖。
/// 与文本字节长度共同作为键,碰撞概率可忽略(见 COUNT_CACHE_CAP 说明)。
fn hash_text(text: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
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
            count_cache: HashMap::new(),
        }
    }

    fn encoding(&mut self, model: &str) -> Option<std::sync::Arc<tiktoken_rs::CoreBPE>> {
        let name = resolve_encoding_name(model);
        if let Some(bpe) = self.cache.get(name) {
            return Some(bpe.clone());
        }
        // 用编码对应的直接构造器加载(tiktoken-rs 0.12:get_bpe_from_model 已废弃,
        // 替代 API bpe_for_model 走 &'static 单例、下载失败会 panic,不符合降级语义,故不用)。
        // BPE 词表首次加载需联网(tiktoken-rs 下载);失败不 panic,降级为估算计数。
        // 失败结果不缓存(None 直接返回),下次调用仍会重试,避免永久降级。
        let bpe = match name {
            "o200k_base" => tiktoken_rs::o200k_base(),
            "p50k_base" => tiktoken_rs::p50k_base(),
            _ => tiktoken_rs::cl100k_base(),
        }
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

    /// 手动保存的 BPE 词表加载保持有效。
    /// 仅用于性能诊断/对照测量(见 examples/perf_probe3),不影响生产逻辑。
    pub fn clear_count_cache(&mut self) {
        self.count_cache.clear();
    }

    /// 单段文本 token 数(词表加载失败时用估算值)。
    /// 带 (编码, 文本) 结果缓存:同文本重复计数直接命中(见 COUNT_CACHE_CAP 说明)。
    pub fn count_tokens(&mut self, text: &str, model: &str) -> i64 {
        let name = resolve_encoding_name(model);
        let key = (name.to_string(), hash_text(text), text.len());
        if let Some(hit) = self.count_cache.get(&key) {
            return *hit;
        }
        let n = match self.encoding(model) {
            Some(bpe) => bpe.encode_ordinary(text).len() as i64,
            None => Self::estimate_tokens(text),
        };
        if self.count_cache.len() >= COUNT_CACHE_CAP {
            // 简单清空而非 LRU:计数是纯函数,清空仅损失一次重算,无正确性影响
            self.count_cache.clear();
        }
        self.count_cache.insert(key, n);
        n
    }

    /// 消息数组计数:每条 content token 数 + 4,最后 +2 回复开场。
    ///
    /// **必须计入 `tool_calls` 的 id/name/arguments**(2026-09-14 修复):
    /// 连接器按 OpenAI 格式原样序列化 `function.arguments` 进请求体
    /// (`connectors/openai_compatible/mod.rs` 的 to_openai_messages),它们真实占用 prompt。
    /// 此前只累加 `content`,导致工具循环里「参数很大但正文为空」的 assistant 消息被计成
    /// 近乎零成本 → 预算闸门永远判定「未超预算」→ 工具历史裁剪失效。
    /// 实测后果:plan 模式任务 prompt 无界膨胀至 88 万,单任务累计 150 万 token 不收敛。
    /// 口径偏保守(宁可多算):每个 tool_call 额外计 8(token 结构开销 + id 近似)。
    pub fn count_message_tokens(&mut self, messages: &[LlmMessage], model: &str) -> i64 {
        let mut total: i64 = 0;
        for m in messages {
            total += self.count_tokens(&m.content, model) + 4;
            if let Some(calls) = &m.tool_calls {
                for c in calls {
                    total += self.count_tokens(&c.name, model)
                        + self.count_tokens(&c.arguments, model)
                        + 8;
                }
            }
        }
        total + 2
    }

    /// 单条消息成本(与 `count_message_tokens` 同口径,含 tool_calls)。
    /// 裁剪逻辑丢弃单条消息时用此方法,避免两处口径分叉导致裁剪量算错。
    pub fn count_single_message_tokens(&mut self, m: &LlmMessage, model: &str) -> i64 {
        let mut total = self.count_tokens(&m.content, model) + 4;
        if let Some(calls) = &m.tool_calls {
            for c in calls {
                total +=
                    self.count_tokens(&c.name, model) + self.count_tokens(&c.arguments, model) + 8;
            }
        }
        total
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

    /// 2026-09-14 修复回归:tool_calls 的 arguments 必须计入。
    /// 此前只数 content,导致「正文为空但参数巨大」的工具调用消息被计成近乎零成本,
    /// 预算闸门因此永不触发(实测 plan 任务 prompt 膨胀至 88 万 token)。
    #[test]
    fn count_message_tokens_includes_tool_call_arguments() {
        let mut svc = TokenService::new();
        let model = "gpt-4o-mini";

        // 正文为空的 assistant 消息,但带一个超大参数的 tool_call
        let mut with_call = LlmMessage::plain("assistant", "");
        with_call.tool_calls = Some(vec![crate::models::types::ToolCallArgs {
            id: "call_1".into(),
            name: "write".into(),
            arguments: format!("{{\"content\":\"{}\"}}", "很长的参数内容".repeat(500)),
        }]);

        let bare = svc.count_message_tokens(&[LlmMessage::plain("assistant", "")], model);
        let with = svc.count_message_tokens(&[with_call.clone()], model);

        assert!(
            with > bare + 500,
            "工具参数必须显著计入:bare={bare} with={with}"
        );
        // 单条口径与数组口径一致(同一消息,数组版多 +2 开场)
        let single = svc.count_single_message_tokens(&with_call, model);
        assert_eq!(with, single + 2, "单条与数组计数口径必须一致");
    }

    /// 无 tool_calls 的普通消息不受影响(保持既有语义)
    #[test]
    fn count_message_tokens_unchanged_for_plain_messages() {
        let mut svc = TokenService::new();
        let m = LlmMessage::plain("user", "hello");
        assert_eq!(
            svc.count_message_tokens(std::slice::from_ref(&m), "gpt-4o-mini"),
            svc.count_single_message_tokens(&m, "gpt-4o-mini") + 2
        );
    }

    /// 计数缓存(2026-09-14):同文本重复计数结果稳定,且缓存不改变数值。
    /// 不同模型走不同编码 → 必须是不同的缓存键(不得互相污染)。
    #[test]
    fn count_cache_returns_same_values_and_separates_models() {
        let mut svc = TokenService::new();
        let text = "雨声很密,她把手插进口袋。".repeat(50);
        let a = svc.count_tokens(&text, "gpt-4o-mini");
        let b = svc.count_tokens(&text, "gpt-4o-mini");
        assert_eq!(a, b, "同文本重复计数必须一致(命中缓存)");
        // 不同模型(同编码族)不应因缓存串号:分别计算并与首次结果比对
        let c = svc.count_tokens(&text, "gpt-3.5-turbo");
        assert!(c > 0);
        // 再取一次 gpt-4o-mini 仍返回原值(缓存未被 c 的写入破坏)
        assert_eq!(svc.count_tokens(&text, "gpt-4o-mini"), a);
    }

    /// 空文本与超长文本缓存行为正常(边界)
    #[test]
    fn count_cache_handles_empty_and_long_text() {
        let mut svc = TokenService::new();
        let empty = svc.count_tokens("", "gpt-4o-mini");
        assert_eq!(svc.count_tokens("", "gpt-4o-mini"), empty);
        let long = "x".repeat(100_000);
        let n = svc.count_tokens(&long, "gpt-4o-mini");
        assert_eq!(svc.count_tokens(&long, "gpt-4o-mini"), n);
    }
}
