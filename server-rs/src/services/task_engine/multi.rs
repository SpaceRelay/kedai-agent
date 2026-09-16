// multi 模式:与 solo 同一套主 agent 工具自循环(批次 B.4 收敛为直接委托)。
//
// 历史上本文件内联复制了 solo 的「置 running → 组装 AgentLoopCall → run_agent_loop →
// record_usage」四步,与 solo.rs 行为零差异(同 task:{id} 虚拟 session、phase=agent、
// step_index=None、label「主 agent」、同一 run_agent_loop)。批次 B.4 收敛为 newtype
// 持有 SoloExecutor,run 直接委托——保留本类型以承载扩展缝(multi 的子 agent 工具化
// 差异在 agentgo/run_subtask 链路,主循环无差异)与日志模式名。
use super::context::TaskRunContext;
use super::executor::ModeExecutor;
use super::solo::SoloExecutor;
use crate::agents::engine::AgentEngine;
use crate::models::types::TokenUsage;
use crate::services::task_core::{TaskBackend, TaskTerminal};
use futures::future::BoxFuture;
use std::sync::Arc;

/// multi 执行器:主 agent 工具自循环(工具集按 task_tool_policy 编译,含 agentgo 子 agent 编排)。
pub(crate) struct MultiExecutor {
    inner: SoloExecutor,
}

impl MultiExecutor {
    pub(crate) fn new(svc: Arc<dyn TaskBackend>, engine: Arc<AgentEngine>) -> Self {
        Self {
            inner: SoloExecutor::new(svc, engine),
        }
    }
}

impl ModeExecutor for MultiExecutor {
    fn run<'a>(
        &'a self,
        ctx: TaskRunContext,
    ) -> BoxFuture<'a, Result<(TaskTerminal, TokenUsage), String>> {
        self.inner.run(ctx)
    }
}
