// 任务模式宿主能力缝(批次 B.3 依赖倒置;批次 4.2 按 ISP 拆窄接口)。
//
// 任务引擎执行器只按这些**窄接口**调用宿主能力,由 TaskService 一侧实现薄委托
// (一行转调既有方法),从而 task_engine 不再反向依赖 task_service,规则 C 的环被打断。
//
// 拆分原则(批次 4.2):原 `TaskBackend` 是 20 方法的上帝接口,把「读任务/写库/追踪/
// 提示词/流程/事件/终态/生成重试」全部混在一起;消费方按实际所需依赖对应能力面即可。
// `TaskBackend` 保留为**聚合别名**(blanket impl:满足全部窄接口即自动实现),
// 使既有 `Arc<dyn TaskBackend>` 构造点零改动;新代码应优先依赖窄接口。
//
// 方法签名逐字对齐 TaskService 既有方法(返回类型不得漂移,见 db.rs/events.rs/
// executor.rs):写库类保持 bool / Result 返回,异步生成类用 BoxFuture(与
// ModeExecutor 同风格,不引入新依赖)。
//
// ## 读代码须知:这些窄接口的存在理由(2026-09-14 明确,防误读)
//
// **存在理由是「断开 task_engine → task_service 的反向依赖」,不是「为将来多实现预留」。**
// 当前 8 个窄接口**各自只有 1 个实现**(全在 `task_service/backend_impl.rs`),
// 经 blanket `TaskBackend` 以 `Arc<dyn TaskBackend>` 消费——即就「多态」而言,
// 它是一层纯转发,没有第二个实现者,也**不建议**为「万一将来换实现」而继续细拆。
//
// 它的价值是**结构性的**:把「引擎需要宿主提供什么」写成不可绕过的清单,
// 使规则 C(L1 边界门禁)可机器验证,并让引擎与宿主可以独立演进/测试。
// 对比:`ModeExecutor`(task_engine/executor.rs)有 8 个**真实**实现(solo/multi/plan/
// team/custom/legacy/followup),那一处的 trait 拆分才兼具「多态」价值。
//
// 结论:**新增窄接口前先问「是否真的多了一个实现者或一条要断的依赖」**;
// 两个都不是,就不要拆(trait 数量本身不是架构收益)。
use crate::models::types::{
    CharacterRecord, LlmMessage, TaskEventKind, TaskMessageRecord, TaskRecord, TaskStatus,
    TaskStep, TaskSubtaskStatus, ToolDefinition,
};
use crate::services::agent_flow_service::AgentFlowConfig;
use crate::services::settings_service::RuntimeSettings;
use crate::services::task_core::{DeltaBatcher, TaskGenOutput, TaskTerminal, TruncationHeal};
use futures::future::BoxFuture;
use std::time::Duration;
use tokio::sync::watch;

// ==================== 窄接口(1/8):任务/子任务持久化读写 ====================

/// 任务与子任务的持久化读写(task 主表 + task_subtasks 表)。
pub(crate) trait TaskStore: Send + Sync {
    /// 按 id 读取任务记录。
    fn get(&self, id: &str) -> Option<TaskRecord>;

    /// 设置任务状态;返回是否命中并更新。
    fn set_status(&self, id: &str, status: TaskStatus) -> bool;

    /// 覆盖任务计划;返回是否命中并更新。
    fn set_plan(&self, id: &str, plan: &[TaskStep]) -> bool;

    /// 更新子任务状态(result/error 为 None 时保持原值);返回是否命中并更新。
    fn set_subtask_status(
        &self,
        id: &str,
        status: TaskSubtaskStatus,
        result: Option<&str>,
        error: Option<&str>,
    ) -> bool;

    /// 创建子任务行;失败返回错误文本。
    fn create_subtask(
        &self,
        task_id: &str,
        name: &str,
        instruction: &str,
    ) -> Result<String, String>;
}

// ==================== 窄接口(2/8):调用追踪与用量留痕 ====================

/// LLM 调用追踪(task_llm_calls)与用量(task_usage)留痕。
pub(crate) trait TaskTrace: Send + Sync {
    /// 落一次 LLM 调用的 usage。
    fn record_usage(
        &self,
        task_id: &str,
        phase: &str,
        step_index: Option<usize>,
        out: &TaskGenOutput,
    );

    /// 落一次 LLM 调用追踪行。
    #[allow(clippy::too_many_arguments)]
    fn record_llm_call(
        &self,
        task_id: &str,
        phase: &str,
        step_index: Option<usize>,
        model: &str,
        messages: &[LlmMessage],
        response: &str,
        out: Option<&TaskGenOutput>,
        elapsed: Duration,
        status: &str,
    );

    /// 落截断自愈留痕(补落被截断调用行 + 补 usage)。
    /// 入参为中性 DTO(`TruncationHeal`),不再引用 agents 层 SelfHealRecord(批次 4.2)。
    fn record_self_heals(
        &self,
        task_id: &str,
        phase: &str,
        step_index: Option<usize>,
        model: &str,
        messages: &[LlmMessage],
        self_heals: &[TruncationHeal],
    );
}

// ==================== 窄接口(3/8):任务模式有效设置 ====================

/// 任务模式合并后的有效设置快照读取。
pub(crate) trait TaskSettings: Send + Sync {
    /// 任务模式合并后的有效设置快照。
    fn task_settings(&self) -> RuntimeSettings;
}

// ==================== 窄接口(4/8):提示词组装 ====================

/// 执行者提示词组装能力(内置指令/人设/世界书/注入/Agent 提示词)。
pub(crate) trait TaskPromptKit: Send + Sync {
    /// 执行者 system 提示词组装(内置执行者指令 → 人设 → 世界书 → 提示词注入 →
    /// 用户可编辑 Agent 提示词)。
    fn assemble_executor_system_prompt(
        &self,
        settings: &RuntimeSettings,
        character_id: Option<&str>,
        user_goal: &str,
    ) -> String;

    /// 世界书常驻条目文本。
    fn world_context(&self, character_id: Option<&str>) -> String;

    /// 渲染 Agent 系统提示词占位符。
    fn render_agent_prompt(
        &self,
        prompt: &str,
        character: Option<&CharacterRecord>,
        world_text: &str,
        user_goal: &str,
    ) -> String;
}

// ==================== 窄接口(5/8):自定义 Agent 流程访问 ====================

/// 自定义 Agent 流程库访问(custom 模式读取当前启用流程)。
///
/// 批次 4.2 起不再返回 `Arc<Mutex<AgentFlowService>>`(那会把锁纪律泄漏给调用方,
/// 逼迫 task_engine 处理 `lock().unwrap()`):改由宿主侧在能力内部完成「加锁 →
/// 取当前启用流程 → 启用校验 → 注册工具集校验」,只把结果快照交给调用方。
pub(crate) trait TaskFlowAccess: Send + Sync {
    /// 当前启用的 Agent 流程配置(未启用/未配置/校验失败返回错误文本)。
    fn current_flow(&self) -> Result<AgentFlowConfig, String>;
}

// ==================== 窄接口(6/8):任务事件发射 ====================

/// 任务事件广播发射能力。
pub(crate) trait TaskEvents: Send + Sync {
    /// 发射任务事件。
    fn emit_event(
        &self,
        kind: TaskEventKind,
        task_id: &str,
        title: Option<String>,
        status: Option<TaskStatus>,
        detail: Option<String>,
    );

    /// 构造挂在任务事件广播通道上的 delta 攒批器。
    fn delta_batcher(&self, task_id: &str, phase: &str, step_index: Option<usize>) -> DeltaBatcher;
}

// ==================== 窄接口(7/8):执行终态落库 ====================

/// 任务执行终态统一落库能力。
pub(crate) trait TaskTerminalSink: Send + Sync {
    /// 任务执行终态统一落库(按 TaskTerminal 变体分派既有写入形态)。
    fn finalize_terminal(
        &self,
        task_id: &str,
        token: u64,
        terminal: TaskTerminal,
        ended_by_cancel: bool,
    );
}

// ==================== 窄接口(8/8):LLM 单次生成原语 ====================

/// 任务模式 LLM 单次生成原语。
///
/// 批次 4.2:重试与解析**算法**已上移 `task_engine::retry`(那是任务引擎的职责),
/// 本接口只保留「发一次调用」的原语(宿主侧负责提示词组装、连接器调用、调用追踪落库);
/// 引擎侧据此自行决定重试策略与解析判定。
pub(crate) trait TaskGenerator: Send + Sync {
    /// 非流式生成统一出口(含调用追踪/delta 旁路落库)。
    #[allow(clippy::too_many_arguments)]
    fn generate_text<'a>(
        &'a self,
        task_id: &'a str,
        phase: &'a str,
        step_index: Option<usize>,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
        max_tokens: u32,
        temperature: f64,
        top_p: f64,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>>;

    /// 执行单个步骤的单次生成(注入执行者人设/世界书/注入/Agent 提示词)。
    fn generate_step<'a>(
        &'a self,
        task: &'a TaskRecord,
        step: &'a TaskStep,
        step_index: Option<usize>,
        cancel: &'a watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>>;

    /// 带生成参数覆盖的步骤生成(重试时提高 max_tokens 或调整温度)。
    #[allow(clippy::too_many_arguments)]
    fn generate_step_with<'a>(
        &'a self,
        task: &'a TaskRecord,
        step: &'a TaskStep,
        step_index: Option<usize>,
        max_tokens: u32,
        temperature: f64,
        cancel: &'a watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>>;

    /// 汇总的单次生成。
    fn summarize_task<'a>(
        &'a self,
        task: &'a TaskRecord,
        plan: &'a [TaskStep],
        cancel: &'a watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>>;

    /// 带生成参数覆盖的汇总生成。
    #[allow(clippy::too_many_arguments)]
    fn summarize_task_with<'a>(
        &'a self,
        task: &'a TaskRecord,
        plan: &'a [TaskStep],
        max_tokens: u32,
        temperature: f64,
        cancel: &'a watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>>;

    /// 规划的单次生成(含只读侦察;解析与重试在 task_engine::retry)。
    fn plan_task<'a>(
        &'a self,
        task_id: &'a str,
        title: &'a str,
        character_id: Option<&'a str>,
        max_tokens: u32,
        cancel: &'a watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>>;

    /// 规划对话修订的单次生成(解析与重试在 task_engine::retry)。
    #[allow(clippy::too_many_arguments)]
    fn plan_revise<'a>(
        &'a self,
        task: &'a TaskRecord,
        history: &'a [TaskMessageRecord],
        feedback: &'a str,
        max_tokens: u32,
        cancel: &'a watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>>;
}

// ==================== 聚合别名(兼容既有构造点) ====================

/// 宿主能力全集(聚合别名,非上帝接口):同时满足全部窄接口即自动实现。
/// 既有 `Arc<dyn TaskBackend>` 构造点零改动;消费方应优先依赖上面的窄接口。
pub(crate) trait TaskBackend:
    TaskStore
    + TaskTrace
    + TaskSettings
    + TaskPromptKit
    + TaskFlowAccess
    + TaskEvents
    + TaskTerminalSink
    + TaskGenerator
{
}

impl<T> TaskBackend for T where
    T: TaskStore
        + TaskTrace
        + TaskSettings
        + TaskPromptKit
        + TaskFlowAccess
        + TaskEvents
        + TaskTerminalSink
        + TaskGenerator
{
}
