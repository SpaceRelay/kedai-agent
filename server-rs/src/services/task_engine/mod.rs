// 任务引擎(批次 4 六模式):模式执行器底座 + legacy 派发 + 五模式执行器实现。
// 设计依据 docs/功能.md:
// - 复用聊天引擎 execute_generation/run_tool_loop,不建影子 sessions 行;
// - runs 互斥只在 AgentEngine::run() 内登记,直调 run_tool_loop 天然绕开,
//   任务级互斥由 TaskService 的 cancel token 机制承担(register_cancel 在
//   TaskService::run/approve 内完成,本模块沿用其 cancel 接收端与 token);
// - multi/team/custom 执行器于批次 4.3b 填入同一 ModeExecutor 缝。
// 模块地图:
//   context.rs   TaskRunContext(目标/设置快照/执行者/取消通道)
//   sink.rs      引擎事件 → 任务事件桥(mpsc + drain,kind=agent_status)
//   executor.rs  ModeExecutor trait(产出 TaskTerminal 终态值 + usage)
//   solo.rs      单主 agent 工具自循环(run_agent_loop 共享骨架:solo/multi/team 复用)
//   multi.rs     solo 主循环 + 子 agent 工具化(agentgo 链路)
//   plan.rs      只规划不执行(planned 待批准)+ 批准续跑执行器(ApprovedPlanExecutor)
//   team.rs      自动拓扑多主 agent 并行 + 审计终审升华
//   custom.rs    AgentFlowConfig 步骤序列轻量执行器
//   legacy.rs    三段式(规划/逐步/汇总)执行器:自原三段式后台执行主体机械搬迁
//   followup.rs  终态追加指令执行器:自原 followup 后台执行主体机械搬迁
//   retry.rs     空输出/规划分级重试算法(批次 4.2 自 task_service 上移)
//   parse.rs     计划 JSON 解析(批次 4.2 自 task_service 上移)
pub(crate) mod context;
pub(crate) mod custom;
pub(crate) mod executor;
pub(crate) mod followup;
pub(crate) mod legacy;
pub(crate) mod multi;
pub(crate) mod parse;
pub(crate) mod plan;
pub(crate) mod retry;
pub(crate) mod sink;
pub(crate) mod solo;
pub(crate) mod team;
pub(crate) mod tool_policy;

use crate::agents::engine::AgentEngine;
use crate::models::types::{TaskRecord, TaskRunMode, TaskStep};
use crate::services::task_core::{TaskBackend, TaskTerminal};
use context::TaskRunContext;
use custom::CustomExecutor;
use executor::ModeExecutor;
use followup::{FollowupExecutor, FollowupParams};
use legacy::LegacyExecutor;
use multi::MultiExecutor;
use plan::{ApprovedPlanExecutor, PlanExecutor};
use solo::SoloExecutor;
use std::sync::Arc;
use team::TeamExecutor;
use tokio::sync::watch;
use tracing::Instrument;

/// 任务引擎:持有任务后端与聊天引擎,按 task_mode 派发后台执行。
/// 轻量句柄(两个 Arc),在 TaskService::run/approve 处即时构造,无状态。
pub struct TaskEngine {
    svc: Arc<dyn TaskBackend>,
    engine: Arc<AgentEngine>,
}

impl TaskEngine {
    pub(crate) fn new(svc: Arc<dyn TaskBackend>, engine: Arc<AgentEngine>) -> Arc<Self> {
        Arc::new(TaskEngine { svc, engine })
    }

    /// 模式派发入口(run):spawn 后台执行;cancel/token 沿用 TaskService 已登记的本轮执行。
    pub(crate) fn run_mode(
        self: &Arc<Self>,
        task: &TaskRecord,
        cancel: watch::Receiver<bool>,
        token: u64,
    ) {
        let executor: Box<dyn ModeExecutor> = match task.task_mode {
            TaskRunMode::Legacy => Box::new(LegacyExecutor::new(self.svc.clone())),
            TaskRunMode::Solo => Box::new(SoloExecutor::new(self.svc.clone(), self.engine.clone())),
            TaskRunMode::Plan => Box::new(PlanExecutor::new(self.svc.clone())),
            TaskRunMode::Multi => {
                Box::new(MultiExecutor::new(self.svc.clone(), self.engine.clone()))
            }
            TaskRunMode::Team => Box::new(TeamExecutor::new(self.svc.clone(), self.engine.clone())),
            TaskRunMode::Custom => {
                Box::new(CustomExecutor::new(self.svc.clone(), self.engine.clone()))
            }
        };
        self.spawn_run(task, executor, None, cancel, token);
    }

    /// followup(终态追加指令)派发入口:goal 由 TaskService 组装为「原目标 +
    /// 上轮 result + 追加指令」;params 携带消息文本与 append/replace 语义。
    /// 收尾由 FollowupExecutor 返回 TaskTerminal::Followup,引擎统一落库。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_followup(
        self: &Arc<Self>,
        task: &TaskRecord,
        goal: String,
        params: FollowupParams,
        cancel: watch::Receiver<bool>,
        token: u64,
    ) {
        let executor: Box<dyn ModeExecutor> = Box::new(FollowupExecutor::new(
            self.svc.clone(),
            self.engine.clone(),
            params,
        ));
        self.spawn_run(task, executor, Some(goal), cancel, token);
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
        self.spawn_run(task, executor, Some(goal), cancel, token);
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_run(
        self: &Arc<Self>,
        task: &TaskRecord,
        executor: Box<dyn ModeExecutor>,
        goal: Option<String>,
        cancel: watch::Receiver<bool>,
        token: u64,
    ) {
        let this = Arc::clone(self);
        let task = task.clone();
        // span 不跨 tokio::spawn 自动继承:显式建 span + `.instrument(...)` 闭合。
        // 任务执行的两条入口(run / 批准续跑)都汇到此处,挂上 taskId 后执行器内
        // (含 service 层)所有日志自动携带该字段;若调用链源自 HTTP 请求,父链还含
        // http_request span,requestId 随之穿透。既有手工传参(task.id)保持不变。
        let run_span = tracing::info_span!("task_run", taskId = task.id.as_str());
        tokio::spawn(
            async move {
                this.run_inner(task, executor, goal, cancel, token).await;
            }
            .instrument(run_span),
        );
    }

    async fn run_inner(
        self: Arc<Self>,
        task: TaskRecord,
        executor: Box<dyn ModeExecutor>,
        goal: Option<String>,
        cancel: watch::Receiver<bool>,
        token: u64,
    ) {
        let ctx = TaskRunContext {
            task_id: task.id.clone(),
            token,
            goal: goal.unwrap_or_else(|| task.title.clone()),
            settings: self.svc.task_settings(),
            character_id: task.character_id.clone(),
            cancel: cancel.clone(),
        };
        // 单一收尾出口(批次 B 依赖倒置):执行器返回终态值,引擎按值分派落库;
        // Err 分支兜底为 Failed(ended_by_cancel 以取消通道求值)。收尾判定所需的
        // 取消态在此处快照,消费方不得再次 borrow。
        match executor.run(ctx).await {
            Ok((terminal, usage)) => {
                tracing::info!(
                    task_id = task.id.clone(),
                    mode = task.task_mode.as_str(),
                    total_tokens = usage.total_tokens,
                    "任务模式执行完成"
                );
                self.svc
                    .finalize_terminal(&task.id, token, terminal, *cancel.borrow());
            }
            Err(e) => self.svc.finalize_terminal(
                &task.id,
                token,
                TaskTerminal::Failed { error: Some(e) },
                *cancel.borrow(),
            ),
        }
    }
}
