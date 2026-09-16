// 模式执行器抽象:每种任务模式一个实现,共享 TaskRunContext 输入。
// 风格对齐 tools 的 ToolExecutor(BoxFuture);能力缝雏形(docs/功能.md 第六节)。
// 产出为「终态值 + 整轮 token 累计」:执行器只描述到达什么终态(task_core::TaskTerminal),
// 落库由 TaskService::finalize_terminal 统一消费(批次 B 依赖倒置,规则 C 断环)。
use super::context::TaskRunContext;
use crate::agents::engine::executor::SelfHealRecord;
use crate::models::types::TokenUsage;
use crate::services::task_core::{TaskGenOutput, TaskTerminal, TruncationHeal};
use futures::future::BoxFuture;

/// 模式执行器(solo/plan/multi/team/custom/legacy/followup 共用同一缝)。
/// `Ok((terminal, usage))` = 已到达终态(usage 仅供完成日志);`Err` = 引擎兜底失败收尾。
pub(crate) trait ModeExecutor: Send + Sync {
    fn run<'a>(
        &'a self,
        ctx: TaskRunContext,
    ) -> BoxFuture<'a, Result<(TaskTerminal, TokenUsage), String>>;
}

/// agents 层截断自愈记录 → task_core 中性 DTO(批次 4.2 依赖倒置边界)。
/// task_core 的能力缝不得引用 agents 层类型,故转换落在已依赖 agents 的 task_engine 侧;
/// 字段一一对应,不得漂移。solo/custom 两处调用点共用本转换。
pub(crate) fn to_self_heals(records: &[SelfHealRecord]) -> Vec<TruncationHeal> {
    records
        .iter()
        .map(|h| TruncationHeal {
            note: h.note.clone(),
            prompt_tokens: h.prompt_tokens,
            completion_tokens: h.completion_tokens,
            finish_reason: h.finish_reason.clone(),
            retried_max_tokens: h.retried_max_tokens,
        })
        .collect()
}

/// 由整轮累计 TokenUsage 构造 record_usage 入参(六模式执行器统一口径;
/// reasoning 无分项观测记 0,详情以 task_llm_calls 行为准)。
pub(crate) fn usage_as_output(usage: &TokenUsage) -> TaskGenOutput {
    TaskGenOutput {
        text: String::new(),
        finish_reason: None,
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        reasoning_tokens: 0,
        reasoning_chars: 0,
        tool_calls: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 中性 DTO 转换必须逐字段保真(批次 4.2 重构护栏):record_self_heals 落库
    /// 依据的 note/token/finish_reason/retried_max_tokens 一个都不能丢或错位。
    #[test]
    fn to_self_heals_preserves_every_field() {
        let records = vec![
            SelfHealRecord {
                note: "工具参数 JSON 截断(工具 \"calculator\")".into(),
                prompt_tokens: 111,
                completion_tokens: 222,
                finish_reason: Some("length".into()),
                retried_max_tokens: 4096,
            },
            SelfHealRecord {
                note: "返回空内容(已达 token 上限)".into(),
                prompt_tokens: 0,
                completion_tokens: 0,
                finish_reason: None,
                retried_max_tokens: 8192,
            },
        ];
        let neutral = to_self_heals(&records);
        assert_eq!(neutral.len(), 2);
        assert_eq!(neutral[0].note, records[0].note);
        assert_eq!(neutral[0].prompt_tokens, 111);
        assert_eq!(neutral[0].completion_tokens, 222);
        assert_eq!(neutral[0].finish_reason.as_deref(), Some("length"));
        assert_eq!(neutral[0].retried_max_tokens, 4096);
        assert_eq!(neutral[1].note, records[1].note);
        assert_eq!(neutral[1].finish_reason, None);
        assert_eq!(neutral[1].retried_max_tokens, 8192);
    }

    /// 空输入 → 空输出(不 panic,不凭空造行)
    #[test]
    fn to_self_heals_empty_is_empty() {
        assert!(to_self_heals(&[]).is_empty());
    }
}
