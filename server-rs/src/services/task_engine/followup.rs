// followup(终态追加指令)执行器:原三段式 followup 后台执行主体的机械搬迁,
// 上 `ModeExecutor` 缝(批次 M3),消除「绕过 ModeExecutor 直调
// solo::run_agent_loop」的第三条执行路径。
//
// 语义不变:solo 单轮 run_agent_loop(工具集按 task_tool_policy 编译,不重新规划)→ 产出落
// assistant 消息(kind=followup,与首轮的 kind=result 区分)→ `append` 以
// 「追加 N」段附加进 result,`replace` 以「修订 N」段整体替换 → 终态映射
// (原 partial 保持 partial,其余回 done)。
//
// 收尾需「写 result 的文本 ≠ 落消息的文本」且消息 kind 非 result,故不走
// Complete 而用 TaskTerminal::Followup,由引擎 finalize_terminal 转调
// TaskService::complete_followup_run(守门与取消清理语义与 complete_mode_run 一致)。
// **永不返回 Err**。
use super::context::TaskRunContext;
use super::executor::{usage_as_output, ModeExecutor};
use super::solo::{run_agent_loop, AgentLoopCall};
use crate::agents::engine::AgentEngine;
use crate::models::types::{TaskFollowupMode, TaskStatus, TokenUsage};
use crate::services::task_core::{TaskBackend, TaskTerminal};
use futures::future::BoxFuture;
use std::sync::Arc;

/// followup 执行参数(构造时注入;goal 走 TaskRunContext.goal,不在此处重复)。
pub(crate) struct FollowupParams {
    /// 用户追加/修订指令原文(消息落库与「追加 N」段概要取用)
    pub instruction: String,
    /// 本轮序号(第 N 次追加/修订)
    pub followup_no: usize,
    /// 上轮 result 快照(append 模式拼接基底)
    pub prev_result: String,
    /// 上轮是否 partial(决定本轮终态是否保持 partial)
    pub prev_partial: bool,
    /// append(默认)/ replace
    pub mode: TaskFollowupMode,
}

pub(crate) struct FollowupExecutor {
    svc: Arc<dyn TaskBackend>,
    engine: Arc<AgentEngine>,
    params: FollowupParams,
}

impl FollowupExecutor {
    pub(crate) fn new(
        svc: Arc<dyn TaskBackend>,
        engine: Arc<AgentEngine>,
        params: FollowupParams,
    ) -> Self {
        FollowupExecutor {
            svc,
            engine,
            params,
        }
    }
}

impl ModeExecutor for FollowupExecutor {
    fn run<'a>(
        &'a self,
        ctx: TaskRunContext,
    ) -> BoxFuture<'a, Result<(TaskTerminal, TokenUsage), String>> {
        Box::pin(async move {
            let svc = &self.svc;
            let task_id = ctx.task_id.as_str();
            let cancel = ctx.cancel;

            let Some(task) = svc.get(task_id) else {
                return Ok((TaskTerminal::Noop, TokenUsage::default()));
            };

            let call = AgentLoopCall {
                task_id: ctx.task_id.clone(),
                session_id: format!("task:{}", ctx.task_id),
                goal: ctx.goal.clone(),
                settings: ctx.settings.clone(),
                character_id: task.character_id.clone(),
                phase: "agent",
                step_index: None,
                label: "主 agent".into(),
            };
            let result =
                run_agent_loop(svc.clone(), self.engine.clone(), call, cancel.clone()).await;

            match result {
                Ok((text, usage)) => {
                    svc.record_usage(task_id, "agent", None, &usage_as_output(&usage));
                    // 取消优先于写结果:stop 后产出不再落库(与 legacy 同口径)
                    if *cancel.borrow() {
                        return Ok((TaskTerminal::Failed { error: None }, usage));
                    }
                    let text = text.trim();
                    let brief: String = self.params.instruction.chars().take(80).collect();
                    let brief = if self.params.instruction.chars().count() > 80 {
                        format!("{brief}…")
                    } else {
                        brief
                    };
                    let section = if self.params.mode == TaskFollowupMode::Replace {
                        // replace:整体替换,段标「修订 N」,旧 result 不再保留
                        format!("**修订 {}:**{brief}\n\n{text}", self.params.followup_no)
                    } else {
                        format!("**追加 {}:**{brief}\n\n{text}", self.params.followup_no)
                    };
                    let new_result = if self.params.mode == TaskFollowupMode::Replace {
                        section
                    } else {
                        // prev_result 为入口快照:追加期间仅本执行可写 result
                        //(is_current_run 守门;stop 只动状态不动 result),快照即当前
                        let prev = self.params.prev_result.trim_end();
                        if prev.is_empty() {
                            section
                        } else {
                            format!("{prev}\n\n---\n\n{section}")
                        }
                    };
                    let status = if self.params.prev_partial {
                        TaskStatus::Partial
                    } else {
                        TaskStatus::Done
                    };
                    // 终态值:msg_text 落 kind=followup 消息,new_result 写 result
                    //(引擎 finalize_terminal 转调 complete_followup_run,守门一致)
                    Ok((
                        TaskTerminal::Followup {
                            msg_text: text.to_string(),
                            new_result,
                            status,
                        },
                        usage,
                    ))
                }
                Err(e) => Ok((
                    TaskTerminal::Failed { error: Some(e) },
                    TokenUsage::default(),
                )),
            }
        })
    }
}
