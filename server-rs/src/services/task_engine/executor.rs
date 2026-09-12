// 模式执行器抽象:每种任务模式一个实现,共享 TaskRunContext 输入与 TaskOutcome 输出。
// 风格对齐 tools 的 ToolExecutor(BoxFuture);能力缝雏形(docs/任务引擎六模式.md 第六节)。
use super::context::TaskRunContext;
use crate::models::types::{TaskStatus, TokenUsage};
use crate::services::task_service::TaskGenOutput;
use futures::future::BoxFuture;

/// 模式执行器产物:最终文本 + 整轮 token 累计(任务侧调用追踪/结果落库用)。
pub(crate) struct TaskOutcome {
    pub text: String,
    pub usage: TokenUsage,
    /// 成功终态覆盖:None = Done;team/custom 存在失败主/步骤但有产出时为
    /// Some(Partial)(对齐 legacy「有产出则 partial」语义,批次 4.3b)。
    pub status: Option<TaskStatus>,
    /// partial 终态的可解释原因(如「以下子目标执行失败:…」「审计/终审未通过」)。
    /// None = Done 或无补充说明。写入 tasks.error 供前端在状态行展示——旧实现
    /// partial 时 error 恒为空,用户只看到一个与「执行中」同色的黄标、不知为何没完成
    ///(实跑问题 2 观感残留)。仅成功收尾(complete_mode_run)消费。
    pub error: Option<String>,
}

/// 模式执行器(solo/plan/multi/team/custom 共用同一缝)。
pub(crate) trait ModeExecutor: Send + Sync {
    fn run<'a>(&'a self, ctx: TaskRunContext) -> BoxFuture<'a, Result<TaskOutcome, String>>;
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
