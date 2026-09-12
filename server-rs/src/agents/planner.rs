// 规划器(与 Node 版 planner.ts 对齐)
use crate::models::types::{Plan, PlanStep};

/// fast: 单步直接生成;deep: 理解意图(不生成) → 生成草稿(生成) → 反思;
/// agent: 同 deep 结构,但执行阶段启用完整 function calling 工具循环(模型可多轮自主调用工具);
/// custom: 自定义流程(见 make_custom_plan),由设置中编辑的步骤序列驱动。
pub fn make_plan(user_input: &str, mode: &str) -> Plan {
    if mode == "agent" {
        let _ = user_input;
        Plan {
            steps: vec![
                PlanStep {
                    goal: "制定写作计划并生成回复".into(),
                    action: "direct".into(),
                    generates: Some(true),
                    system_prompt: Some(
                        "【本步指令·计划并回复】开始输出正文前,先在草稿区撰写一份 200 字以内的写作计划\
                         (段落结构、核心要点、情节走向、节奏安排),随后严格按该计划输出正文;\
                         正文中不得包含计划本身,不得出现「计划」「草稿」等字样。\
                         以 {{char}} 的视角与口吻输出:视角遵循系统提示词与用户要求\
                         (用户未指定时以角色自身视角叙述);服从用户字数与风格要求,用户提问必须正面回答。\
                         你可以通过 function calling 自主调用下方列出的工具:\
                         需要随机数用 role、联网查最新信息用 search、读世界书/角色资料用 read、写对话气泡或文件用 write、\
                         需要后台并行处理子任务用 agentgo;需要时再调用,不必每轮都调。"
                            .into(),
                    ),
                    ..Default::default()
                },
                PlanStep {
                    goal: "反思输出质量与连贯性".into(),
                    action: "reflect".into(),
                    generates: None,
                    ..Default::default()
                },
            ],
            summary: "Agent 模式:计划 + 工具调用 + 生成 + 反思,可多轮调用工具。".into(),
        }
    } else if mode == "deep" {
        Plan {
            steps: vec![
                PlanStep {
                    goal: "制定写作计划并生成草稿".into(),
                    action: "direct".into(),
                    generates: Some(true),
                    system_prompt: Some(
                        "【本步指令·计划并起草】开始输出正文前,先在草稿区撰写一份 200 字以内的写作计划\
                         (段落结构、核心要点、情节走向、节奏安排),随后严格按该计划输出正文草稿;\
                         正文中不得包含计划本身,不得出现「计划」「草稿」等字样。\
                         以 {{char}} 的视角与口吻输出:视角遵循系统提示词与用户要求\
                         (用户未指定时以角色自身视角叙述);服从用户字数与风格要求,用户提问必须正面回答。\
                         本步为草稿,反思步骤会检查质量,必要时回到本步重生成。"
                            .into(),
                    ),
                    ..Default::default()
                },
                PlanStep {
                    goal: "反思输出质量与连贯性".into(),
                    action: "reflect".into(),
                    generates: None,
                    ..Default::default()
                },
            ],
            summary: "深度模式:先写计划,再生成草稿,最后反思优化。".into(),
        }
    } else {
        let _ = user_input;
        Plan {
            steps: vec![PlanStep {
                goal: "根据上下文生成回复".into(),
                action: "direct".into(),
                generates: Some(true),
                ..Default::default()
            }],
            summary: "快速模式:直接生成回复".into(),
        }
    }
}

/// 自定义流程(custom 模式):按用户配置的步骤序列构建计划。
/// 过滤 disabled 步骤;兜底校验至少一个会生成的 direct 步骤(由 PUT /api/agent-flows
/// 的 validate_flow 强制,此处防御外部手改配置后再次兜底)。
pub fn make_custom_plan(steps: &[PlanStep]) -> Result<Plan, String> {
    let active: Vec<PlanStep> = steps.iter().filter(|s| s.enabled).cloned().collect();
    if active.is_empty() {
        return Err("自定义流程为空:请先在设置中启用至少一个步骤".into());
    }
    if !active
        .iter()
        .any(|s| s.action == "direct" && s.generates == Some(true))
    {
        return Err("自定义流程缺少生成步骤:至少需要一个「生成正文」的 direct 步骤".into());
    }
    let summary = format!("自定义流程:共 {} 步", active.len());
    Ok(Plan {
        steps: active,
        summary,
    })
}

/// 判断输入是否像计算式:明确「计算」指令、含数字的「算一下」口语,或数字+运算符算式。
/// 「算一下」不再无条件触发——口语如「算一下我欠你多少人情」不含数字,不应打断角色扮演。
pub fn looks_like_calculation(input: &str) -> bool {
    // 明确的「计算」指令:直接触发
    if input.contains("计算") {
        return true;
    }
    // 「算一下」等口语:仅当句中同时含数字才触发(避免「算一下我欠你多少人情」误判)
    if input.contains("算一下") && input.bytes().any(|b| b.is_ascii_digit()) {
        return true;
    }
    // 其余:数字+运算符 的算式
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            // 找到数字后,跳过它及空白,看是否跟运算符
            let mut j = i;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b'.') {
                j += 1;
            }
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && matches!(bytes[j], b'+' | b'-' | b'*' | b'/') {
                return true;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    false
}

/// 提取计算表达式:去掉中文词/问号/句号后,匹配加减乘除表达式,去空格返回
pub fn extract_expression(input: &str) -> String {
    let cleaned: String = input
        .chars()
        .filter(|c| {
            // 注意:小数点 '.' 必须保留(小数算式 12.5*2),仅过滤中文句号与标点
            !matches!(
                c,
                '计' | '算' | '一' | '下' | '？' | '?' | '。' | ':' | '：' | ' ' | '　'
            )
        })
        .collect();
    // 匹配:括号子表达式或数字,以运算符连接,至少一次
    let re = regex_multi_expr(&cleaned);
    if let Some(m) = re {
        return m;
    }
    cleaned
}

/// 手写扫描:匹配 (?:\([^)]+\)|-?\d+(?:\.\d+)?)(?:\s*[+\-*/]\s*(?:\([^)]+\)|-?\d+(?:\.\d+)?))+
fn regex_multi_expr(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some((end, _)) = parse_operand(&bytes[i..]) {
            // 尝试扩展操作数链
            let mut j = i + end;
            let mut ops = 0;
            loop {
                // 跳过空白
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && matches!(bytes[j], b'+' | b'-' | b'*' | b'/') {
                    j += 1;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if let Some((len2, _)) = parse_operand(&bytes[j..]) {
                        j += len2;
                        ops += 1;
                        continue;
                    }
                }
                break;
            }
            if ops >= 1 {
                let expr = &s[i..j];
                let no_space: String = expr.chars().filter(|c| !c.is_whitespace()).collect();
                return Some(no_space);
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

/// 解析一个操作数:括号子表达式 或 带可选负号的数字;返回(长度, 是否括号)
fn parse_operand(s: &[u8]) -> Option<(usize, bool)> {
    if s.is_empty() {
        return None;
    }
    if s[0] == b'(' {
        let mut depth = 0;
        for (k, &b) in s.iter().enumerate() {
            if b == b'(' {
                depth += 1;
            } else if b == b')' {
                depth -= 1;
                if depth == 0 {
                    return Some((k + 1, true));
                }
            }
        }
        return None;
    }
    let mut k = 0;
    if s[0] == b'-' {
        k = 1;
    }
    let mut seen_digit = false;
    while k < s.len() && (s[k].is_ascii_digit() || s[k] == b'.') {
        if s[k].is_ascii_digit() {
            seen_digit = true;
        }
        k += 1;
    }
    if seen_digit {
        Some((k, false))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fast_plan() {
        let p = make_plan("你好", "fast");
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].action, "direct");
        assert_eq!(p.summary, "快速模式:直接生成回复");
    }

    #[test]
    fn test_deep_plan() {
        let p = make_plan("你好", "deep");
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.steps[1].action, "reflect");
    }

    #[test]
    fn test_agent_plan_step_prompts() {
        let p = make_plan("你好", "agent");
        // 计划生成步 + 反思步(理解意图步已废弃,不再有不生成的 direct 步骤)
        assert_eq!(p.steps.len(), 2);
        let gen = &p.steps[0];
        assert_eq!(gen.generates, Some(true));
        let sp = gen.system_prompt.as_deref().unwrap_or("");
        assert!(
            sp.contains("200 字") && sp.contains("计划"),
            "生成步应先写 ≤200 字计划再输出正文: {sp}"
        );
        assert!(
            sp.contains("function calling"),
            "生成步应提 function calling: {sp}"
        );
        assert!(
            sp.contains("role") && sp.contains("search"),
            "生成步应含工具名: {sp}"
        );
        // 反思步:reflect 不带 system_prompt
        assert_eq!(p.steps[1].action, "reflect");
        assert!(p.steps[1].system_prompt.is_none());
    }

    #[test]
    fn test_deep_plan_step_prompts() {
        let p = make_plan("你好", "deep");
        assert_eq!(p.steps.len(), 2);
        assert!(
            p.steps[0]
                .system_prompt
                .as_deref()
                .unwrap_or("")
                .contains("200 字")
                && p.steps[0]
                    .system_prompt
                    .as_deref()
                    .unwrap_or("")
                    .contains("计划"),
            "deep 生成步应先写计划再输出正文"
        );
        assert_eq!(p.steps[0].generates, Some(true));
        assert!(p.steps[1].system_prompt.is_none());
    }

    #[test]
    fn test_looks_like_calculation() {
        assert!(looks_like_calculation("帮我算一下 12*34"));
        assert!(looks_like_calculation("计算 1 + 2"));
        assert!(looks_like_calculation("1+2"));
        assert!(!looks_like_calculation("你好世界"));
        // 口语「算一下」无数字:不触发(避免打断角色扮演)
        assert!(!looks_like_calculation("算一下我欠你多少人情"));
        assert!(!looks_like_calculation("你算一下这事该怎么办"));
        // 口语「算一下」含数字:触发
        assert!(looks_like_calculation("帮我算一下 12*34 等于多少"));
    }

    #[test]
    fn test_extract_expression() {
        assert_eq!(extract_expression("帮我算一下 12*34"), "12*34");
        assert_eq!(extract_expression("计算 (1+2)*3"), "(1+2)*3");
        assert_eq!(extract_expression("1 + 2"), "1+2");
    }

    /// 回归:小数点不能被过滤(12.5*2 = 25,过滤后 125*2 = 250 算错)
    #[test]
    fn extract_expression_keeps_decimal_point() {
        assert_eq!(extract_expression("帮我算一下 12.5*2"), "12.5*2");
        assert_eq!(extract_expression("计算 3.14*4"), "3.14*4");
        assert_eq!(extract_expression("0.5+0.25"), "0.5+0.25");
    }

    #[test]
    fn custom_plan_filters_disabled_steps() {
        let steps = vec![
            PlanStep {
                id: "a".into(),
                name: "理解".into(),
                enabled: false,
                goal: "理解意图".into(),
                action: "direct".into(),
                generates: Some(false),
                ..Default::default()
            },
            PlanStep {
                id: "b".into(),
                name: "生成".into(),
                enabled: true,
                goal: "生成正文".into(),
                action: "direct".into(),
                generates: Some(true),
                ..Default::default()
            },
            PlanStep {
                id: "c".into(),
                name: "反思".into(),
                enabled: true,
                goal: "检查质量".into(),
                action: "reflect".into(),
                generates: None,
                ..Default::default()
            },
        ];
        let plan = make_custom_plan(&steps).unwrap();
        assert_eq!(plan.steps.len(), 2, "disabled 步骤应被过滤");
        assert_eq!(plan.steps[0].id, "b");
        assert_eq!(plan.steps[1].action, "reflect");
        assert_eq!(plan.summary, "自定义流程:共 2 步");
    }

    #[test]
    fn custom_plan_empty_rejected() {
        assert!(make_custom_plan(&[]).is_err());
        let disabled = vec![PlanStep {
            id: "a".into(),
            name: "禁用".into(),
            enabled: false,
            goal: "g".into(),
            action: "direct".into(),
            generates: Some(true),
            ..Default::default()
        }];
        assert!(make_custom_plan(&disabled).is_err());
    }

    #[test]
    fn custom_plan_missing_generating_step_rejected() {
        let steps = vec![
            PlanStep {
                id: "a".into(),
                name: "理解".into(),
                enabled: true,
                goal: "理解意图".into(),
                action: "direct".into(),
                generates: Some(false),
                ..Default::default()
            },
            PlanStep {
                id: "b".into(),
                name: "反思".into(),
                enabled: true,
                goal: "检查质量".into(),
                action: "reflect".into(),
                generates: None,
                ..Default::default()
            },
        ];
        assert!(make_custom_plan(&steps).is_err());
    }
}
