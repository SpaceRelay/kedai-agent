// 反思器(与 Node 版 reflector.ts 对齐)
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectionResult {
    pub passed: bool,
    pub reason: String,
    /// retry | adjust | stop
    pub retry_action: Option<&'static str>,
}

const MIN_CHARS: usize = 2;
/// 结尾中间标点(截断信号);感叹号/省略号不算
const TRAILING_PUNCT: &[char] = &[',', '，', ';', '；', ':', '：', '、'];

/// 解析 LLM 反思输出:首个单词精确为 PASS/通过 = 通过;FAIL/不通过 = 失败(retry)。
/// 其他输出/空返回 None——由调用方回退机械规则,保证反思路径在模型异常时仍可判定。
pub fn parse_reflect_verdict(output: &str) -> Option<ReflectionResult> {
    let first = output.trim().lines().next()?.split_whitespace().next()?;
    let reason = output.trim().to_string();
    if first == "PASS" || first == "通过" {
        Some(ReflectionResult {
            passed: true,
            reason,
            retry_action: None,
        })
    } else if first == "FAIL" || first == "不通过" {
        Some(ReflectionResult {
            passed: false,
            reason,
            retry_action: Some("retry"),
        })
    } else {
        None
    }
}

pub fn reflect(
    content: &str,
    user_input: &str,
    attempt: usize,
    max_attempts: usize,
    has_updates: bool,
    min_chars: Option<usize>,
) -> ReflectionResult {
    let trimmed = content.trim();
    // 规则 1:空或过短(Node 版按 UTF-16 码元计数,Rust 用 char 数对齐);
    // has_updates=true(内容含 mvu <UpdateVariable> 补丁)时放行——纯变量更新也是有效输出
    if !has_updates && trimmed.chars().count() < MIN_CHARS {
        let action = if attempt >= max_attempts {
            "stop"
        } else {
            "retry"
        };
        return ReflectionResult {
            passed: false,
            reason: "输出为空或过短,缺少实质内容".into(),
            retry_action: Some(action),
        };
    }
    // 规则 1b:简单模式字数要求(如「输出约 1200 字」)明显不达标时判失败重生成。
    // 下限取要求的 60%,避免把「接近目标」误杀;has_updates 时放行(纯变量更新)。
    if !has_updates {
        if let Some(min) = min_chars {
            let lower = (min as f64 * 0.6) as usize;
            if trimmed.chars().count() < lower {
                let action = if attempt >= max_attempts {
                    "stop"
                } else {
                    "retry"
                };
                return ReflectionResult {
                    passed: false,
                    reason: format!("输出字数明显不足(要求约 {min} 字,实际不足 {lower} 字)"),
                    retry_action: Some(action),
                };
            }
        }
    }
    // 规则 2:以中间标点结尾(截断)
    if let Some(last) = trimmed.chars().last() {
        if TRAILING_PUNCT.contains(&last) {
            let action = if attempt >= max_attempts {
                "adjust"
            } else {
                "retry"
            };
            return ReflectionResult {
                passed: false,
                reason: "输出疑似被截断(以中间标点结尾)".into(),
                retry_action: Some(action),
            };
        }
    }
    // 规则 3:用户提问而回复无问句标记且内容过短;has_updates 时放行(变量更新响应无需回问句)
    if !has_updates
        && (user_input.contains('?') || user_input.contains('？'))
        && !trimmed.contains('?')
        && !trimmed.contains('？')
        && trimmed.chars().count() < 30
    {
        return ReflectionResult {
            passed: false,
            reason: "用户提出疑问但回复未回应,且内容过短".into(),
            retry_action: Some("retry"),
        };
    }
    ReflectionResult {
        passed: true,
        reason: "质量检查通过".into(),
        retry_action: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pass() {
        let r = reflect("这是一段正常的回复内容", "你好", 0, 3, false, None);
        assert!(r.passed);
    }

    #[test]
    fn test_empty() {
        let r = reflect("", "你好", 0, 3, false, None);
        assert!(!r.passed);
        assert_eq!(r.retry_action, Some("retry"));
    }

    #[test]
    fn test_short() {
        let r = reflect("好", "你好", 0, 3, false, None);
        assert!(!r.passed);
    }

    #[test]
    fn test_truncated() {
        let r = reflect("内容被截断了,", "你好", 1, 3, false, None);
        assert!(!r.passed);
        assert_eq!(r.retry_action, Some("retry"));
    }

    #[test]
    fn test_truncated_max_attempts() {
        let r = reflect("内容被截断了,", "你好", 3, 3, false, None);
        assert!(!r.passed);
        assert_eq!(r.retry_action, Some("adjust"));
    }

    #[test]
    fn test_question_unanswered() {
        let r = reflect("好的", "你在吗?", 0, 3, false, None);
        assert!(!r.passed);
    }

    /// 简单模式字数要求:明显偏短时判失败并重生成;接近目标时通过
    #[test]
    fn test_word_count_below_threshold_fails() {
        // 要求约 1200 字:不足 720 字(60%)→ FAIL 重生成
        let r = reflect("短回复只有几百字。", "你好", 0, 3, false, Some(1200));
        assert!(!r.passed);
        assert_eq!(r.retry_action, Some("retry"));
        assert!(r.reason.contains("1200"), "reason: {}", r.reason);
        // 达到 720 字下限 → 通过
        let long = "长".repeat(800);
        let r = reflect(&long, "你好", 0, 3, false, Some(1200));
        assert!(r.passed);
        // 达到上限后放弃重试(stop)
        let r = reflect("还是太短。", "你好", 3, 3, false, Some(1200));
        assert!(!r.passed);
        assert_eq!(r.retry_action, Some("stop"));
    }

    /// mvu 补丁放行:字数要求不误杀纯变量更新
    #[test]
    fn test_word_count_ignored_with_updates() {
        let r = reflect("", "你好", 0, 3, true, Some(1200));
        assert!(r.passed);
    }

    /// mvu 补丁放行:内容含 <UpdateVariable> 补丁时,空/短正文不再判失败
    #[test]
    fn test_pure_update_passes() {
        // 剥离后的正文为空,但带补丁 → 视为有效输出
        let r = reflect("", "你好", 0, 3, true, None);
        assert!(r.passed);
    }

    /// mvu 补丁放行:用户提问 + 无问句短回复,但带补丁 → 不误判重试
    #[test]
    fn test_update_with_question_passes() {
        let r = reflect("好的", "现在几点?", 0, 3, true, None);
        assert!(r.passed);
    }

    /// 截断标点规则不因补丁放行:内容仍以中间标点结尾 → 判失败
    #[test]
    fn test_update_still_checks_truncation() {
        let r = reflect("内容被截断了,", "你好", 0, 3, true, None);
        assert!(!r.passed);
    }

    /// LLM 反思输出解析:PASS / FAIL / 通过 / 不通过 / 无法解析
    #[test]
    fn test_parse_verdict_pass() {
        let r = parse_reflect_verdict("PASS\n人设连贯,内容完整").unwrap();
        assert!(r.passed);
        assert_eq!(r.retry_action, None);
    }

    #[test]
    fn test_parse_verdict_fail() {
        let r = parse_reflect_verdict("FAIL\n回复过短,未回应提问").unwrap();
        assert!(!r.passed);
        assert_eq!(r.retry_action, Some("retry"));
    }

    #[test]
    fn test_parse_verdict_chinese_markers() {
        assert!(parse_reflect_verdict("通过\n质量可以").unwrap().passed);
        assert!(!parse_reflect_verdict("不通过\n字数不足").unwrap().passed);
    }

    #[test]
    fn test_parse_verdict_unparseable() {
        // 空输出 / 无标记文本 → None(调用方回退机械规则)
        assert!(parse_reflect_verdict("").is_none());
        assert!(parse_reflect_verdict("这段草稿看起来不错").is_none());
        // 首行之前允许空白行
        assert!(parse_reflect_verdict("\n\nPASS\nok").unwrap().passed);
        // 前缀过宽不匹配:「通过分析…」不是精确标记,必须回退
        assert!(parse_reflect_verdict("通过分析,但需改进").is_none());
        assert!(parse_reflect_verdict("PASSES 不算标记").is_none());
    }
}
