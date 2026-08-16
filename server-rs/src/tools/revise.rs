// 段落修订工具(revise_passage):对文本做「定位片段 → 替换为新文本」的定点修改。
// 供反思/审查阶段模型自主调用:发现不合逻辑段落、表述不当等,精准替换而非整体重生成。
// 纯文本处理、无副作用 → 权限风险 Safe(见 permissions.rs default_risk)。
use serde_json::{json, Value};

/// 对 text 做定点子串替换:把 find 出现处替换为 replace。
/// find 为空返回错误;text 中找不到 find 时原样返回(不报错,避免打断反思流程)。
pub fn revise_passage(text: &str, find: &str, replace: &str) -> Result<String, String> {
    if find.trim().is_empty() {
        return Err("find 不能为空:需指定要修改的原文片段".into());
    }
    if text.contains(find) {
        Ok(text.replace(find, replace))
    } else {
        Ok(text.to_string())
    }
}

/// 注册 revise_passage 工具(纯函数,不依赖 ToolDeps)
pub fn register_revise_passage_tool(registry: &crate::tools::registry::ToolRegistry) {
    registry.register(
        crate::models::types::ToolDefinition {
            name: "revise_passage".into(),
            description: "定点修改正文中的某段文本:把原文片段(find)替换为新文本(replace)。\
                用于修正不合逻辑的段落、不当表述或需要改写的句子,而非整体重生成。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "待修改的正文全文" },
                    "find": { "type": "string", "description": "要修改的原文片段(需与正文精确匹配)" },
                    "replace": { "type": "string", "description": "替换后的新文本" }
                },
                "required": ["text", "find", "replace"]
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
                    let find = args
                        .get("find")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let replace = args
                        .get("replace")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let result = revise_passage(&text, &find, &replace)?;
                    Ok(result)
                })
            },
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_found_passage() {
        assert_eq!(
            revise_passage("他走进了屋子,然后坐了下来。", "坐了下来", "站了一会儿又坐下").unwrap(),
            "他走进了屋子,然后站了一会儿又坐下。"
        );
    }

    #[test]
    fn missing_find_returns_original() {
        assert_eq!(revise_passage("原文内容", "不存在的片段", "x").unwrap(), "原文内容");
    }

    #[test]
    fn empty_find_errors() {
        assert!(revise_passage("原文", "", "x").is_err());
        assert!(revise_passage("原文", "   ", "x").is_err());
    }

    #[test]
    fn replaces_all_occurrences() {
        assert_eq!(revise_passage("他笑了笑,然后笑了笑。", "笑了笑", "点了点头").unwrap(), "他点了点头,然后点了点头。");
    }
}
