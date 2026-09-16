// 任务模式生成重试/解析算法(批次 4.2 自 task_service/executor.rs 上移 task_engine):
// 空输出分级重试、规划重试、汇总重试、规划对话修订重试——这些是**算法/策略**,
// 属任务引擎职责,不应作为宿主能力暴露(TaskBackend 只负责单次生成原语与落库)。
//
// 宿主侧仅提供单次生成原语(TaskGenerator:generate_step(_with)/summarize_task(_with)/
// plan_task/plan_revise)与落库/设置读取(TaskTrace/TaskSettings);本模块负责
// 「重试几次、预算如何演进、何时解析失败」的判定。逻辑逐行搬迁,行为不变。
use crate::models::types::{TaskRecord, TaskStep};
use crate::services::task_core::{TaskBackend, TaskGenOutput};
use std::time::Duration;
use tokio::sync::watch;

/// 空输出重试前的退避间隔(避免对上游瞬时抖动形成紧循环)。
const EMPTY_RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// 空输出重试时的 max_tokens 翻倍上限(与设置页 max_tokens 上限一致)。
const RETRY_MAX_TOKENS_CAP: u32 = 131_072;

/// 规划调用的初始 max_tokens。推理模型的 reasoning 与正文共用同一预算,
/// 1024 曾被 reasoning 整体吃光导致正文零输出/JSON 半截(2026-08-27 exe 实测),
/// 故起始预算给到 2048。
const PLAN_INITIAL_MAX_TOKENS: u32 = 2048;

/// 规划解析失败的最大尝试次数(截断/空输出每次翻倍预算,纯格式错误同预算重试)。
const PLAN_MAX_ATTEMPTS: u32 = 3;

/// 截断(length)空输出的重试预算:翻倍并封顶 RETRY_MAX_TOKENS_CAP,且不低于
/// `HEAL_BUDGET_FLOOR`(小预算翻倍后仍是「必然被推理烧光」的档位,见该常量注释)。
/// 算法收敛在 `utils::retry::heal_budget_with_floor`(2026-09-15 在下限补齐时引入);
/// 封顶后(结果不大于当前值)回退原预算仍重试一次——本路径是空输出兜底重试,
/// 同预算再试一次好过直接失败(与 chat/engine/team 三处「封顶即放弃」语义不同)。
/// `default_max_tokens` 经 `PUT /api/settings` 校验为 1..=131072;手改 settings.json 可超出
/// 该区间(加载期无钳制),此时保持原预算不缩小——见 `truncated_retry_budget_doubles_and_keeps_cap`。
fn truncated_retry_budget(current: u32) -> u32 {
    crate::utils::retry::heal_budget_with_floor(
        current,
        RETRY_MAX_TOKENS_CAP,
        crate::utils::retry::HEAL_BUDGET_FLOOR,
    )
    .unwrap_or(current)
}

/// 空输出分级重试骨架(步骤/汇总共用;WP2-A 收敛两份逐行同构的重试,逻辑不变):
/// 首次产出非空即返回;空输出退避 EMPTY_RETRY_BACKOFF 后按 finish_reason 分级重试一次——
/// finish_reason=length:max_tokens 翻倍(上限 RETRY_MAX_TOKENS_CAP),温度保持默认;
/// 其他原因/无原因:同 max_tokens,temperature 调至 0.7(沿用 mvu.rs 先例)。
/// 仍空返回带 label 与原因的 Err;网络/超时等硬错误不重试直接透传。
/// call 按 (max_tokens, temperature) 发起重试调用,复用同一 cancel,不重建 system 提示词。
/// (plan_task_retry 不共用本骨架:多轮循环 + parse 判定 + 参数演进/日志规则不同。)
#[allow(clippy::too_many_arguments)] // 输入为「定位信息 + 首调结果 + 重试闭包」三段,拆参反而分散调用点
async fn retry_if_empty_output<F, Fut>(
    deps: &dyn TaskBackend,
    task_id: &str,
    phase: &str,
    step_index: Option<usize>,
    label: &str,
    first: TaskGenOutput,
    cancel: &watch::Receiver<bool>,
    call: F,
) -> Result<TaskGenOutput, String>
where
    F: Fn(u32, f64) -> Fut,
    Fut: std::future::Future<Output = Result<TaskGenOutput, String>>,
{
    if !first.text.trim().is_empty() {
        return Ok(first);
    }
    // 首调空输出的那次调用仍应入账(2026-09-10 实测修复):此前只落 task_llm_calls,
    // 重试成功后只记末次 out,首调 token 从 usage_total 丢失。
    deps.record_usage(task_id, phase, step_index, &first);
    tokio::time::sleep(EMPTY_RETRY_BACKOFF).await;
    if *cancel.borrow() {
        return Err("任务已停止".into());
    }
    let settings = deps.task_settings();
    let reason = first.finish_reason.as_deref().unwrap_or("");
    let retried = if reason == "length" {
        let doubled = truncated_retry_budget(settings.default_max_tokens);
        call(doubled, settings.default_temperature).await
    } else {
        call(settings.default_max_tokens, 0.7).await
    };
    match retried {
        Ok(o) if !o.text.trim().is_empty() => Ok(o),
        Ok(o) => Err(format!(
            "{label}返回空内容(finish_reason={})",
            o.finish_reason.as_deref().unwrap_or("未知")
        )),
        Err(e) => Err(e),
    }
}

/// 生成步骤:空输出按 finish_reason 分级重试一次(仍空返回带原因的 Err)。
/// - finish_reason=length:max_tokens 翻倍(上限 RETRY_MAX_TOKENS_CAP)重试;
/// - 其他原因/无原因:temperature 调至 0.7 重试(沿用 mvu.rs 先例)。
///
/// 重试走 generate_step_with 单参数覆盖变体,复用同一 cancel,不重建 system 提示词。
pub(crate) async fn generate_step_retry(
    deps: &dyn TaskBackend,
    task: &TaskRecord,
    step: &TaskStep,
    step_index: Option<usize>,
    cancel: &watch::Receiver<bool>,
) -> Result<TaskGenOutput, String> {
    let first = deps.generate_step(task, step, step_index, cancel).await?;
    retry_if_empty_output(
        deps,
        &task.id,
        "step",
        step_index,
        "子任务",
        first,
        cancel,
        |max_tokens, temperature| {
            deps.generate_step_with(task, step, step_index, max_tokens, temperature, cancel)
        },
    )
    .await
}

/// 汇总:与步骤同款的分级重试(空输出按 finish_reason 分别加倍 max_tokens 或调温)。
/// 规划(带解析与分级重试):parse_plan 内含截断打捞;仍失败时按 finish_reason 分级——
/// length/空内容 = 预算被推理 token 耗尽,max_tokens 翻倍(上限 RETRY_MAX_TOKENS_CAP);
/// 纯格式错误 = 同预算重试。网络/超时等硬错误不重试直接抛(与步骤重试同款约定)。
/// 共 PLAN_MAX_ATTEMPTS 次尝试;失败文案带末次错误,便于排障。
pub(crate) async fn plan_task_retry(
    deps: &dyn TaskBackend,
    task_id: &str,
    title: &str,
    character_id: Option<&str>,
    cancel: &watch::Receiver<bool>,
) -> Result<(Vec<TaskStep>, TaskGenOutput), String> {
    let mut max_tokens = PLAN_INITIAL_MAX_TOKENS;
    let mut last_err = String::from("规划器未产出有效步骤");
    for attempt in 1..=PLAN_MAX_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(EMPTY_RETRY_BACKOFF).await;
            if *cancel.borrow() {
                return Err("任务已停止".into());
            }
        }
        let out = deps
            .plan_task(task_id, title, character_id, max_tokens, cancel)
            .await?;
        match super::parse::parse_plan(&out.text) {
            Ok(steps) if !steps.is_empty() => return Ok((steps, out)),
            Ok(_) => last_err = "规划器未产出有效步骤".into(),
            Err(e) => last_err = e,
        }
        // 失败 attempt 仍是一次真实调用(已落 task_llm_calls):同步入账 usage,
        // 否则重试场景下 usage_total 少于调用明细求和(2026-09-10 实测修复)。
        deps.record_usage(task_id, "planner", None, &out);
        let reason = out.finish_reason.as_deref().unwrap_or("");
        tracing::warn!(
            attempt = attempt,
            finish_reason = reason,
            max_tokens = max_tokens,
            error = last_err.clone(),
            "任务模式规划输出解析失败,准备重试"
        );
        if reason == "length" || out.text.trim().is_empty() {
            max_tokens = crate::utils::retry::doubled_heal_budget(max_tokens, RETRY_MAX_TOKENS_CAP)
                .unwrap_or(max_tokens);
        }
    }
    Err(format!("{last_err}(已重试 {} 次)", PLAN_MAX_ATTEMPTS - 1))
}

/// 规划对话修订(带解析与分级重试):与 plan_task_retry 同款骨架——
/// 截断/空输出按 finish_reason 翻倍 max_tokens(上限 RETRY_MAX_TOKENS_CAP),
/// 纯格式错误同预算重试;共 PLAN_MAX_ATTEMPTS 次;网络/超时硬错误直接抛。
/// 返回修订后步骤与末次调用产出(usage 落库用)。
pub(crate) async fn plan_revise_retry(
    deps: &dyn TaskBackend,
    task: &TaskRecord,
    history: &[crate::models::types::TaskMessageRecord],
    feedback: &str,
    cancel: &watch::Receiver<bool>,
) -> Result<(Vec<TaskStep>, TaskGenOutput), String> {
    let mut max_tokens = PLAN_INITIAL_MAX_TOKENS;
    let mut last_err = String::from("规划器未产出有效修订计划");
    for attempt in 1..=PLAN_MAX_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(EMPTY_RETRY_BACKOFF).await;
            if *cancel.borrow() {
                return Err("任务已停止".into());
            }
        }
        let out = deps
            .plan_revise(task, history, feedback, max_tokens, cancel)
            .await?;
        match super::parse::parse_plan(&out.text) {
            Ok(steps) if !steps.is_empty() => return Ok((steps, out)),
            Ok(_) => last_err = "规划器未产出有效修订计划".into(),
            Err(e) => last_err = e,
        }
        // 失败 attempt 同步入账 usage(口径同 plan_task_retry;2026-09-10 实测修复)
        deps.record_usage(&task.id, "planner", None, &out);
        let reason = out.finish_reason.as_deref().unwrap_or("");
        tracing::warn!(
            attempt = attempt,
            finish_reason = reason,
            max_tokens = max_tokens,
            error = last_err.clone(),
            "规划对话修订输出解析失败,准备重试"
        );
        if reason == "length" || out.text.trim().is_empty() {
            max_tokens = crate::utils::retry::doubled_heal_budget(max_tokens, RETRY_MAX_TOKENS_CAP)
                .unwrap_or(max_tokens);
        }
    }
    Err(format!("{last_err}(已重试 {} 次)", PLAN_MAX_ATTEMPTS - 1))
}

/// 汇总:与步骤同款的分级重试(空输出按 finish_reason 分别加倍 max_tokens 或调温)。
pub(crate) async fn summarize_task_retry(
    deps: &dyn TaskBackend,
    task: &TaskRecord,
    plan: &[TaskStep],
    cancel: &watch::Receiver<bool>,
) -> Result<TaskGenOutput, String> {
    let first = deps.summarize_task(task, plan, cancel).await?;
    retry_if_empty_output(
        deps,
        &task.id,
        "summary",
        None,
        "汇总",
        first,
        cancel,
        |max_tokens, temperature| {
            deps.summarize_task_with(task, plan, max_tokens, temperature, cancel)
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 截断重试预算:未到封顶时翻倍;命中封顶返回原值,保证「封顶后同预算仍重试一次」
    #[test]
    fn truncated_retry_budget_doubles_and_keeps_cap() {
        // 小预算抬到 HEAL_BUDGET_FLOOR:1024 翻倍只有 2048,仍会被推理整个吃光
        assert_eq!(
            truncated_retry_budget(1024),
            crate::utils::retry::HEAL_BUDGET_FLOOR
        );
        assert_eq!(truncated_retry_budget(16_384), 32_768, "下限之上仍按翻倍");
        assert_eq!(truncated_retry_budget(40000), 80_000);
        assert_eq!(truncated_retry_budget(70_000), RETRY_MAX_TOKENS_CAP);
        assert_eq!(
            truncated_retry_budget(RETRY_MAX_TOKENS_CAP),
            RETRY_MAX_TOKENS_CAP,
            "恰为封顶值:回退原预算,仍发起重试"
        );
        assert_eq!(
            truncated_retry_budget(RETRY_MAX_TOKENS_CAP + 1),
            RETRY_MAX_TOKENS_CAP + 1,
            "设置异常超过封顶(正常路径不可达):保持原预算,不因封顶反而缩小"
        );
        assert_eq!(
            truncated_retry_budget(u32::MAX),
            u32::MAX,
            "饱和相乘不得溢出"
        );
    }
}
