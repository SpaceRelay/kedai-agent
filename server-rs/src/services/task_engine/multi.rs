// multi 模式:solo 主循环 + 子 agent 工具化(批次 4.3b)。
// 主 agent 与 solo 同一套 system 组装/run_tool_loop 调用(复用 solo.rs 的
// run_agent_loop 共享骨架,工具清单 = tool_registry 全量);与 solo 的差异在
// 子 agent 链路可用性:agentgo 排出的子 agent 走 run_tool_loop + 白名单工具
//(tools/agent_tools_agent.rs 的 run_subtask 工具化路径),任务侧经 task: 前缀
// 虚拟 session 的内存覆盖层落库(FK 守卫),进度/结果经事件桥与 task_llm_calls
//(phase=subagent)观测。子 agent LLM 调用落库在 run_subtask 内完成,本文件只管主循环。
use super::context::TaskRunContext;
use super::executor::{usage_as_output, ModeExecutor, TaskOutcome};
use super::solo::{run_agent_loop, AgentLoopCall};
use crate::agents::engine::AgentEngine;
use crate::models::types::TaskStatus;
use crate::services::task_service::TaskService;
use futures::future::BoxFuture;
use std::sync::Arc;

/// multi 执行器:主 agent 工具自循环(工具全量,含 agentgo 子 agent 编排)。
pub(crate) struct MultiExecutor {
    svc: Arc<TaskService>,
    engine: Arc<AgentEngine>,
}

impl MultiExecutor {
    pub(crate) fn new(svc: Arc<TaskService>, engine: Arc<AgentEngine>) -> Self {
        MultiExecutor { svc, engine }
    }
}

impl ModeExecutor for MultiExecutor {
    fn run<'a>(&'a self, ctx: TaskRunContext) -> BoxFuture<'a, Result<TaskOutcome, String>> {
        Box::pin(async move {
            // multi 无独立规划阶段:进入即执行(与 solo 同口径)
            self.svc.set_status(&ctx.task_id, TaskStatus::Running);
            let call = AgentLoopCall {
                task_id: ctx.task_id.clone(),
                session_id: format!("task:{}", ctx.task_id),
                goal: ctx.goal.clone(),
                settings: ctx.settings.clone(),
                character_id: ctx.character_id.clone(),
                phase: "agent",
                step_index: None,
                label: "主 agent".into(),
            };
            let (text, usage) = run_agent_loop(
                self.svc.clone(),
                self.engine.clone(),
                call,
                ctx.cancel.clone(),
            )
            .await?;
            // usage 落库(phase=agent,主循环整轮累计;子 agent 各行在 run_subtask 内落)
            self.svc
                .record_usage(&ctx.task_id, "agent", None, &usage_as_output(&usage));
            Ok(TaskOutcome {
                text,
                usage,
                status: None,
                error: None,
            })
        })
    }
}
