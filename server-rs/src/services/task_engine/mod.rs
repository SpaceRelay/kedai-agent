// 任务引擎(批次 4 六模式):模式执行器底座 + legacy 派发 + 五模式执行器实现。
// 设计依据 docs/任务引擎六模式.md:
// - 复用聊天引擎 execute_generation/run_tool_loop,不建影子 sessions 行;
// - runs 互斥只在 AgentEngine::run() 内登记,直调 run_tool_loop 天然绕开,
//   任务级互斥由 TaskService 的 cancel token 机制承担(register_cancel 在
//   TaskService::run/approve 内完成,本模块沿用其 cancel 接收端与 token);
// - multi/team/custom 执行器于批次 4.3b 填入同一 ModeExecutor 缝。
// 模块地图:
//   context.rs   TaskRunContext(目标/设置快照/执行者/取消通道)
//   sink.rs      引擎事件 → 任务事件桥(mpsc + drain,kind=agent_status)
//   executor.rs  ModeExecutor trait 与 TaskOutcome(含终态覆盖与 usage 入参助手)
//   solo.rs      单主 agent 工具自循环(run_agent_loop 共享骨架:solo/multi/team 复用)
//   multi.rs     solo 主循环 + 子 agent 工具化(agentgo 链路)
//   plan.rs      只规划不执行(planned 待批准)+ 批准续跑执行器(ApprovedPlanExecutor)
//   team.rs      自动拓扑多主 agent 并行 + 审计终审升华
//   custom.rs    AgentFlowConfig 步骤序列轻量执行器
pub(crate) mod context;
pub(crate) mod custom;
pub(crate) mod executor;
pub(crate) mod multi;
pub(crate) mod plan;
pub(crate) mod sink;
pub(crate) mod solo;
pub(crate) mod team;
pub(crate) mod tool_policy;

use crate::agents::engine::AgentEngine;
use crate::models::types::{TaskRecord, TaskRunMode, TaskStatus, TaskStep};
use crate::services::task_service::TaskService;
use context::TaskRunContext;
use custom::CustomExecutor;
use executor::ModeExecutor;
use multi::MultiExecutor;
use plan::{ApprovedPlanExecutor, PlanExecutor};
use solo::SoloExecutor;
use std::sync::Arc;
use team::TeamExecutor;
use tokio::sync::watch;

/// 任务引擎:持有任务服务与聊天引擎,按 task_mode 派发后台执行。
/// 轻量句柄(两个 Arc),在 TaskService::run/approve 处即时构造,无状态。
pub struct TaskEngine {
    svc: Arc<TaskService>,
    engine: Arc<AgentEngine>,
}

/// 执行器成功后的收尾方式:
/// - Complete:写结果 + done 终态(solo / approve 续跑);
/// - AwaitApproval:plan 模式的计划产出轮——planned 终态由执行器写入;计划清单文本
///   落 result(批次 R1:planned 态 result 语义 = 待批准的计划清单,供批准前预览)
///   后清理执行登记。
enum OnSuccess {
    Complete,
    AwaitApproval,
}

impl TaskEngine {
    pub(crate) fn new(svc: Arc<TaskService>, engine: Arc<AgentEngine>) -> Arc<Self> {
        Arc::new(TaskEngine { svc, engine })
    }

    /// 模式派发入口(run):spawn 后台执行;cancel/token 沿用 TaskService 已登记的本轮执行。
    pub(crate) fn run_mode(
        self: &Arc<Self>,
        task: &TaskRecord,
        cancel: watch::Receiver<bool>,
        token: u64,
    ) {
        let executor: Option<Box<dyn ModeExecutor>> = match task.task_mode {
            TaskRunMode::Solo => Some(Box::new(SoloExecutor::new(
                self.svc.clone(),
                self.engine.clone(),
            ))),
            TaskRunMode::Plan => Some(Box::new(PlanExecutor::new(self.svc.clone()))),
            TaskRunMode::Multi => Some(Box::new(MultiExecutor::new(
                self.svc.clone(),
                self.engine.clone(),
            ))),
            TaskRunMode::Team => Some(Box::new(TeamExecutor::new(
                self.svc.clone(),
                self.engine.clone(),
            ))),
            TaskRunMode::Custom => Some(Box::new(CustomExecutor::new(
                self.svc.clone(),
                self.engine.clone(),
            ))),
            // run() 已拦截 legacy;此处为防御兜底(不应到达)
            TaskRunMode::Legacy => None,
        };
        let Some(executor) = executor else {
            self.svc
                .finalize_mode_run(&task.id, token, false, Some("该模式将在后续批次开放"));
            return;
        };
        let on_success = match task.task_mode {
            TaskRunMode::Plan => OnSuccess::AwaitApproval,
            _ => OnSuccess::Complete,
        };
        self.spawn_run(task, executor, None, on_success, cancel, token);
    }

    /// approve 续跑入口(plan 模式批准后):**只认已批准计划,与 task_mode 无关**——
    /// 续跑语义是「按已批准计划执行」,若按 Plan 派发会再规划一遍回到 planned 死循环
    ///(批次 4.3 回归)。plan 非空 → ApprovedPlanExecutor 逐步骤执行(每步独立
    /// run_agent_loop + SUMMARIZER_PROMPT 汇总);plan 为空(防御兜底)→ SoloExecutor
    /// 纯 goal 续跑。goal 为「目标 + 已批准计划」组合文本,作各步骤消息的整体上下文。
    pub(crate) fn run_approved(
        self: &Arc<Self>,
        task: &TaskRecord,
        goal: String,
        plan: Vec<TaskStep>,
        cancel: watch::Receiver<bool>,
        token: u64,
    ) {
        let executor: Box<dyn ModeExecutor> = if plan.is_empty() {
            Box::new(SoloExecutor::new(self.svc.clone(), self.engine.clone()))
        } else {
            Box::new(ApprovedPlanExecutor::new(
                self.svc.clone(),
                self.engine.clone(),
                plan,
            ))
        };
        self.spawn_run(
            task,
            executor,
            Some(goal),
            OnSuccess::Complete,
            cancel,
            token,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_run(
        self: &Arc<Self>,
        task: &TaskRecord,
        executor: Box<dyn ModeExecutor>,
        goal: Option<String>,
        on_success: OnSuccess,
        cancel: watch::Receiver<bool>,
        token: u64,
    ) {
        let this = Arc::clone(self);
        let task = task.clone();
        tokio::spawn(async move {
            this.run_inner(task, executor, goal, on_success, cancel, token)
                .await;
        });
    }

    async fn run_inner(
        self: Arc<Self>,
        task: TaskRecord,
        executor: Box<dyn ModeExecutor>,
        goal: Option<String>,
        on_success: OnSuccess,
        cancel: watch::Receiver<bool>,
        token: u64,
    ) {
        let ctx = TaskRunContext {
            task_id: task.id.clone(),
            goal: goal.unwrap_or_else(|| task.title.clone()),
            settings: self.svc.task_settings(),
            character_id: task.character_id.clone(),
            cancel: cancel.clone(),
        };
        match executor.run(ctx).await {
            Ok(outcome) => {
                // 完成日志(对齐引擎 finish 口径:整轮 token 累计;任务侧 usage 面板
                // 数据源为 task_llm_calls,本日志仅供排障)
                tracing::info!(
                    task_id = task.id.clone(),
                    mode = task.task_mode.as_str(),
                    total_tokens = outcome.usage.total_tokens,
                    "任务模式执行完成"
                );
                match on_success {
                    // plan 计划产出轮:计划清单文本落 result 后清理执行登记(批次 R1)
                    OnSuccess::AwaitApproval => {
                        self.svc
                            .planned_mode_run(&task.id, token, outcome.text.trim())
                    }
                    OnSuccess::Complete => self.svc.complete_mode_run(
                        &task.id,
                        token,
                        outcome.text.trim(),
                        // 执行器可覆盖成功终态(team/custom 有失败主/步骤但有产出 → partial)
                        outcome.status.unwrap_or(TaskStatus::Done),
                        // partial 的可解释原因(team 审计/终审未通过或子目标失败);
                        // Done 时执行器给 None,写空串清掉上一轮残留 error
                        outcome.error.as_deref(),
                    ),
                }
            }
            // 取消(stop)时 ended_by_cancel=true → ended;否则 error(与 legacy 收尾同语义)
            Err(e) => self
                .svc
                .finalize_mode_run(&task.id, token, *cancel.borrow(), Some(&e)),
        }
    }
}
