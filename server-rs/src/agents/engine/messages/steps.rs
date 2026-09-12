// 步骤派生:反思失败回退定位(retreat_to_generating_step)、步骤提示词视图
// (with_step_prompt)、按步骤派生生成参数(step_params_for)
// (自 build.rs 拆分迁入,纯代码移动,逻辑不变)
// 可见性说明:导出条目保持 pub(in crate::agents::engine),供 messages/mod.rs
// 以相同可见性再导出,可见范围与拆分前完全一致,未放宽。
use crate::models::types::{GenerationParams, LlmMessage, PlanStep};
use crate::parsing::macros::{expand_macros, MacroCtx};
use crate::tools::registry::ToolRegistry;

/// 反思失败回退:从 idx 往前找到最近一个「会生成内容」的 direct 步骤(下标)。
/// 跳过 generates=false 的步骤(如「理解意图」,不生成也不递增 attempt,不能作为重试锚点)。
/// 找不到返回 None。此函数保证反思重试必有界:回退后必然重新走 direct 生成分支 → attempt 递增。
pub(in crate::agents::engine) fn retreat_to_generating_step(
    steps: &[PlanStep],
    mut idx: usize,
) -> Option<usize> {
    while idx > 0 {
        idx -= 1;
        if steps[idx].action == "direct" && steps[idx].generates != Some(false) {
            return Some(idx);
        }
    }
    None
}

/// 自定义流程步骤消息视图:步骤级系统提示词(宏展开)追加到 system 末尾;
/// 无提示词时返回共享消息的克隆(不修改原数组,保证反思回退后视图可重建)。
pub(in crate::agents::engine) fn with_step_prompt(
    base: &[LlmMessage],
    step: &PlanStep,
    mctx: &mut MacroCtx,
) -> Vec<LlmMessage> {
    let Some(tpl) = step.system_prompt.as_deref() else {
        return base.to_vec();
    };
    if tpl.trim().is_empty() {
        return base.to_vec();
    }
    let mut msgs = base.to_vec();
    let expanded = expand_macros(tpl, mctx);
    if let Some(s) = msgs.first_mut() {
        s.content.push_str(&format!("\n\n[本步指令]\n{expanded}"));
    }
    msgs
}

/// 按步骤派生生成参数:温度/输出上限覆盖全局值;工具按步骤配置解析:
/// None=不使用工具、Some([])=全部工具、Some(list)=白名单。
/// 白名单名称已在流程保存阶段校验,此处只按注册表解析实际定义。
/// 白名单中的工具被视为已授权(白名单即授权语义),不再弹授权框。
pub(in crate::agents::engine) fn step_params_for(
    params: &GenerationParams,
    step: &PlanStep,
    registry: &ToolRegistry,
) -> GenerationParams {
    let mut p = params.clone();
    if let Some(t) = step.temperature {
        p.temperature = t;
    }
    if let Some(m) = step.max_tokens {
        p.max_tokens = m;
    }
    p.tools = match &step.tools {
        None => Vec::new(),
        Some(list) if list.is_empty() => registry.list_definitions(),
        Some(list) => registry
            .list_definitions()
            .into_iter()
            .filter(|t| list.iter().any(|n| n == &t.name))
            .collect(),
    };
    p.tool_choice = match step.tool_choice.as_deref().unwrap_or("auto") {
        "none" => crate::models::types::ToolChoice::None,
        "required" => crate::models::types::ToolChoice::Required,
        "function" => crate::models::types::ToolChoice::Function(
            step.tool_choice_function.clone().unwrap_or_default(),
        ),
        _ => crate::models::types::ToolChoice::Auto,
    };
    p.parallel_tool_calls = step.parallel_tool_calls;
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::planner::make_plan;
    use crate::agents::reflector::reflect;
    use crate::models::types::ToolDefinition;
    use serde_json::json;

    /// 反思失败回退必须落在「会生成内容」的 direct 步骤(regression:旧逻辑停在反思步骤本身
    /// 导致 attempt 永不递增、无限紧密循环,见 2026-08-06 日志 1ms 间隔的 reflect 洪流)
    #[test]
    fn retreat_lands_on_generating_step() {
        // agent / deep plan:反思在 idx=1,应回退到 0(计划生成步骤,generates=true)
        for mode in ["agent", "deep"] {
            let plan = make_plan("你好", mode);
            assert_eq!(
                retreat_to_generating_step(&plan.steps, 1),
                Some(0),
                "mode={mode}"
            );
        }
        // 无生成步骤可回退 → None(调用方应放弃反思而非死循环)
        let steps = vec![PlanStep {
            goal: "r".into(),
            action: "reflect".into(),
            generates: None,
            ..Default::default()
        }];
        assert_eq!(retreat_to_generating_step(&steps, 0), None);
        let steps = vec![PlanStep {
            goal: "理解".into(),
            action: "direct".into(),
            generates: Some(false),
            ..Default::default()
        }];
        assert_eq!(retreat_to_generating_step(&steps, 0), None);
    }
    #[test]
    fn step_params_apply_tool_choice_and_parallel_calls() {
        let registry = ToolRegistry::new();
        registry.register(
            ToolDefinition {
                name: "read".into(),
                description: "读取".into(),
                parameters: json!({}),
            },
            std::sync::Arc::new(|_, _| Box::pin(async { Ok("ok".into()) })),
        );
        let base = GenerationParams {
            temperature: 1.0,
            top_p: 1.0,
            max_tokens: 100,
            stop: None,
            tools: Vec::new(),
            max_tool_rounds: None,
            tool_choice: crate::models::types::ToolChoice::Auto,
            parallel_tool_calls: None,
        };
        let step = PlanStep {
            tools: Some(vec!["read".into()]),
            tool_choice: Some("function".into()),
            tool_choice_function: Some("read".into()),
            parallel_tool_calls: Some(false),
            ..Default::default()
        };
        let params = step_params_for(&base, &step, &registry);
        assert_eq!(params.tools.len(), 1);
        assert_eq!(
            params.tool_choice,
            crate::models::types::ToolChoice::Function("read".into())
        );
        assert_eq!(params.parallel_tool_calls, Some(false));
    }

    /// 反思持续失败时整个循环必须有界(回归:旧实现无限循环,本测试在旧代码上会挂死)
    #[test]
    fn reflect_failure_loop_is_bounded() {
        // 模拟 engine 反思循环语义:反思失败 → 回退 → 重生成(attempt+1)→ 反思。
        // retreat 只在反思失败分支被调用,此时 idx 恒指向反思步骤(plan 最后一步)。
        let plan = make_plan("你好", "agent");
        let max_attempts = 3usize;
        let mut attempt = 0usize;
        let mut reflect_retries = 0usize;
        let reflect_idx = plan.steps.len() - 1;
        let mut iterations = 0usize;
        loop {
            iterations += 1;
            assert!(
                iterations < 100,
                "反思循环未在有限步内停止(回归:旧逻辑死循环)"
            );
            let verdict = reflect("", "你好", attempt, max_attempts, false, None);
            if verdict.passed {
                break;
            }
            if verdict.retry_action == Some("stop")
                || attempt >= max_attempts
                || reflect_retries >= max_attempts
            {
                break; // 放弃反思
            }
            reflect_retries += 1;
            match retreat_to_generating_step(&plan.steps, reflect_idx) {
                Some(target) => {
                    // 重新执行 direct 生成步骤 → attempt 递增(这正是旧代码缺失的环节)
                    if plan.steps[target].generates != Some(false) {
                        attempt += 1;
                    }
                }
                None => break, // 无可回退的生成步骤:放弃(有界)
            }
        }
        assert!(attempt <= max_attempts, "attempt 超出上限: {attempt}");
        assert!(
            reflect_retries <= max_attempts,
            "反思重试超出上限: {reflect_retries}"
        );
        assert_eq!(
            reflect_retries, max_attempts,
            "应恰好重试 {max_attempts} 次后放弃"
        );
    }
}
