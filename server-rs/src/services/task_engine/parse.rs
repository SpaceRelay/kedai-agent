// 计划 JSON 解析(批次 4.2 自 task_service/parse.rs 上移 task_engine):
// parse_plan 四级从严到宽尝试(整体解析/剥 markdown 代码块/取数组子串/截断打捞),
// salvage_step_objects 字符串感知地打捞配平步骤对象。
// 纯文本算法、零宿主依赖,归任务引擎所有(重试与解析是任务引擎职责,不是宿主能力);
// 宿主 task_service 不再持有本逻辑。
use crate::models::types::TaskStep;

/// 从 LLM 输出解析计划 JSON 数组。按从严到宽四级尝试:
/// 1) 整体解析;2) 剥 markdown 代码块;3) 首个 '[' 到末个 ']' 的子串(容忍前言/后记);
/// 4) 截断打捞:逐个提取配平的 {...} 对象(推理模型预算耗尽时尾部常缺一半,
///    报「EOF while parsing a string」,已完成的步骤对象仍可用)。全部失败才报错。
///
/// 所有层级统一过滤空 name+goal 的步骤(serde 默认值会让 `[{}]` 也"解析成功",
/// 空步骤会把空调 goal 发给执行器,必须拦截)。
pub(crate) fn parse_plan(text: &str) -> Result<Vec<TaskStep>, String> {
    fn non_empty(steps: Vec<TaskStep>) -> Vec<TaskStep> {
        steps
            .into_iter()
            .filter(|s| !s.name.trim().is_empty() || !s.goal.trim().is_empty())
            .collect()
    }
    let t = text.trim();
    if let Ok(steps) = serde_json::from_str::<Vec<TaskStep>>(t).map(non_empty) {
        if !steps.is_empty() {
            return Ok(steps);
        }
    }
    let stripped = t
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(steps) = serde_json::from_str::<Vec<TaskStep>>(stripped).map(non_empty) {
        if !steps.is_empty() {
            return Ok(steps);
        }
    }
    if let (Some(lo), Some(hi)) = (t.find('['), t.rfind(']')) {
        if lo < hi {
            if let Ok(steps) = serde_json::from_str::<Vec<TaskStep>>(&t[lo..=hi]).map(non_empty) {
                if !steps.is_empty() {
                    return Ok(steps);
                }
            }
        }
    }
    let salvaged = salvage_step_objects(t);
    if !salvaged.is_empty() {
        tracing::info!(
            steps = salvaged.len(),
            "任务模式计划 JSON 被截断,已打捞完整步骤对象"
        );
        return Ok(salvaged);
    }
    // 保持与原实现一致的报错形态(集成测试/用户提示均按此前缀识别);
    // 语法合法但过滤后无有效步骤(如 [{}])也要报错,不得返回空计划。
    match serde_json::from_str::<Vec<TaskStep>>(stripped) {
        Ok(_) => Err("解析计划失败: 规划输出不含任何有效步骤".into()),
        Err(e) => Err(format!("解析计划失败: {e}")),
    }
}

/// 从(可能被截断的)文本中打捞配平的 JSON 对象并解析为步骤。
/// 字符串/转义感知,避免把 goal 文本里的 {} 误当结构符;空 name+goal 的对象丢弃
/// (serde 默认值会让任意 `{}`/`{"foo":1}` 都"解析成功",必须过滤)。
fn salvage_step_objects(text: &str) -> Vec<TaskStep> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    let mut start: Option<usize> = None;
    for (idx, ch) in text.char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start.take() {
                        if let Ok(step) =
                            serde_json::from_str::<TaskStep>(&text[s..idx + ch.len_utf8()])
                        {
                            if !step.name.trim().is_empty() || !step.goal.trim().is_empty() {
                                out.push(step);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::types::TaskStepStatus;

    /// 合法 JSON 数组直接解析
    #[test]
    fn parse_plan_plain_json() {
        let steps =
            parse_plan(r#"[{"name":"步骤一","goal":"做一"},{"name":"步骤二","goal":"做二"}]"#)
                .expect("合法 JSON 应解析成功");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].name, "步骤一");
        assert_eq!(
            steps[0].status,
            TaskStepStatus::Pending,
            "status 应有 serde 默认值"
        );
    }

    /// markdown 代码块包裹
    #[test]
    fn parse_plan_fenced() {
        let text = "```json\n[{\"name\":\"一\",\"goal\":\"g\"}]\n```";
        assert_eq!(parse_plan(text).unwrap().len(), 1);
    }

    /// 数组外有前言/后记:取首个 '[' 到末个 ']' 子串
    #[test]
    fn parse_plan_prose_wrapped() {
        let text = "好的,计划如下:\n[{\"name\":\"一\",\"goal\":\"g\"}]\n以上。";
        let steps = parse_plan(text).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].goal, "g");
    }

    /// 截断打捞:2 个完整步骤对象 + 半截字符串(2026-08-27 exe 实测报错的形态,
    /// 「EOF while parsing a string」),已完成对象应恢复,半截对象丢弃
    #[test]
    fn parse_plan_truncated_salvage() {
        let text = r#"[{"name":"收集意象","goal":"收集秋天意象"}, {"name":"写初稿","goal":"写出初稿"}, {"name":"润色","goal":"润"#;
        let steps = parse_plan(text).expect("截断但有完整对象应打捞成功");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1].name, "写初稿");
    }

    /// 打捞时字符串内的花括号不得误当结构符
    #[test]
    fn parse_plan_salvage_ignores_braces_in_strings() {
        let text = r#"[{"name":"模板","goal":"输出 {a} 与 {b} 两个占位符"}, {"name":"x"#;
        let steps = parse_plan(text).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].goal, "输出 {a} 与 {b} 两个占位符");
    }

    /// 零个完整对象(开头即截断)必须报错,交给重试/降级
    #[test]
    fn parse_plan_truncated_zero_objects_errors() {
        let text = r#"[{"name":"收"#;
        let err = parse_plan(text).unwrap_err();
        assert!(
            err.starts_with("解析计划失败"),
            "报错形态应保持原前缀: {err}"
        );
    }

    /// 空输出报错(重试路径据此判断空内容)
    #[test]
    fn parse_plan_empty_errors() {
        assert!(parse_plan("").is_err());
        assert!(parse_plan("   \n  ").is_err());
    }

    /// 纯垃圾文本报错,且 `{}` 之类的空对象不得被打捞成步骤
    #[test]
    fn parse_plan_garbage_errors() {
        assert!(parse_plan("这不是 JSON").is_err());
        assert!(
            parse_plan("[{}, {\"foo\":1}]").is_err(),
            "空对象应被过滤,不得静默成功"
        );
    }
}
