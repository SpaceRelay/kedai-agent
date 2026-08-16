// 禁词替换工具(censor_text):对文本做「禁词 → 替换词」同义改写。
// 由引擎在生成收尾时调用(deep/agent/custom 模式兜底),也可供模型 function calling 使用。
// 纯文本处理、无副作用 → 权限风险 Safe(见 permissions.rs default_risk)。
use serde_json::{json, Value};

/// 禁词条目(与 prompt_inject_service::BannedWordEntry 字段一致,供工具参数反序列化)
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CensorEntry {
    #[serde(default)]
    pub word: String,
    #[serde(default)]
    pub replacement: String,
}

/// 对 text 做精确子串同义替换:每条 entry 的 word 出现处替换为 replacement。
/// 空 word 跳过;空 replacement 等价于删除该词。替换顺序按 entries 传入顺序。
pub fn censor_text(text: &str, entries: &[CensorEntry]) -> String {
    let mut out = text.to_string();
    for e in entries {
        let word = e.word.trim();
        if word.is_empty() {
            continue;
        }
        if out.contains(word) {
            out = out.replace(word, &e.replacement);
        }
    }
    out
}

/// 注册 censor_text 工具(纯函数,不依赖 ToolDeps)
pub fn register_censor_tool(registry: &crate::tools::registry::ToolRegistry) {
    registry.register(
        crate::models::types::ToolDefinition {
            name: "censor_text".into(),
            description: "对文本进行禁词替换:把出现禁词的位置改写为含义相近、更得体的替换词(同义改写,保留上下文语义)".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "待处理的原文(通常是模型生成的回复正文)" },
                    "entries": {
                        "type": "array",
                        "description": "禁词 → 替换词 映射列表",
                        "items": {
                            "type": "object",
                            "properties": {
                                "word": { "type": "string", "description": "禁词(精确子串匹配)" },
                                "replacement": { "type": "string", "description": "替换词(含义相近、更得体;空串 = 删除)" }
                            },
                            "required": ["word", "replacement"]
                        }
                    }
                },
                "required": ["text", "entries"]
            }),
        },
        std::sync::Arc::new(
            |args: Value, _ctx: crate::models::types::ToolContext| {
                Box::pin(async move {
                    let text = args
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let entries: Vec<CensorEntry> = args
                        .get("entries")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default();
                    if text.is_empty() {
                        return Ok(String::new());
                    }
                    let result = censor_text(&text, &entries);
                    // 无任何替换时仍返回原文,让调用方明确知道处理结果
                    Ok(result)
                })
            },
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(word: &str, replacement: &str) -> CensorEntry {
        CensorEntry {
            word: word.to_string(),
            replacement: replacement.to_string(),
        }
    }

    #[test]
    fn replaces_single_word() {
        let e = [entry("笨蛋", "傻瓜")];
        assert_eq!(censor_text("你真是个笨蛋。", &e), "你真是个傻瓜。");
    }

    #[test]
    fn replaces_multiple_words_in_order() {
        let e = [entry("笨蛋", "傻瓜"), entry("笨蛋", "可爱鬼")];
        // 第二条的 word 已被第一条替换,不再命中 → 输出只有第一次替换
        assert_eq!(censor_text("笨蛋你好", &e), "傻瓜你好");
    }

    #[test]
    fn no_match_keeps_text() {
        let e = [entry("脏话", "好话")];
        assert_eq!(censor_text("今天天气不错。", &e), "今天天气不错。");
    }

    #[test]
    fn empty_replacement_removes_word() {
        let e = [entry("废话", "")];
        assert_eq!(censor_text("别废话了快说。", &e), "别了快说。");
    }

    #[test]
    fn empty_word_skipped() {
        let e = [entry("", "x")];
        assert_eq!(censor_text("原文", &e), "原文");
    }

    #[test]
    fn multiple_occurrences_replaced() {
        let e = [entry("笨蛋", "傻瓜")];
        assert_eq!(censor_text("笨蛋和笨蛋", &e), "傻瓜和傻瓜");
    }
}
