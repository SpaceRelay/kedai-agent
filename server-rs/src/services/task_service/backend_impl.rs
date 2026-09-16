// TaskBackend 宿主侧实现(批次 B.3 依赖倒置;批次 4.2 拆窄接口):一行转调 TaskService
// 既有方法,本体零改动、零行为变化。任务引擎只依赖 task_core 的窄接口,
// 真实读写能力仍全部落在 TaskService/各子模块。
//
// 批次 4.2 变更:
// - 按 ISP 分别实现 TaskStore/TaskTrace/TaskSettings/TaskPromptKit/TaskFlowAccess/
//   TaskEvents/TaskTerminalSink/TaskGenerator(不再实现聚合别名 TaskBackend,该别名由
//   task_core 的 blanket impl 自动满足);
// - `record_self_heals` 改收中性 DTO `TruncationHeal`,不再引用 agents 层类型;
// - `agent_flow()`(返回 Arc<Mutex<AgentFlowService>>)收敛为 `current_flow()`,
//   加锁与校验在能力内部完成,不再泄漏锁纪律。
use super::{TaskGenOutput, TaskService};
use crate::models::types::{
    CharacterRecord, LlmMessage, TaskEventKind, TaskMessageRecord, TaskRecord, TaskStatus,
    TaskStep, TaskSubtaskStatus, ToolDefinition,
};
use crate::services::agent_flow_service::AgentFlowConfig;
use crate::services::settings_service::RuntimeSettings;
use crate::services::task_core::{
    DeltaBatcher, TaskEvents, TaskFlowAccess, TaskGenerator, TaskPromptKit, TaskSettings,
    TaskStore, TaskTerminal, TaskTerminalSink, TaskTrace, TruncationHeal,
};
use futures::future::BoxFuture;
use std::time::Duration;
use tokio::sync::watch;

// ==================== TaskStore:任务/子任务持久化读写 ====================

impl TaskStore for TaskService {
    fn get(&self, id: &str) -> Option<TaskRecord> {
        TaskService::get(self, id)
    }

    fn set_status(&self, id: &str, status: TaskStatus) -> bool {
        TaskService::set_status(self, id, status)
    }

    fn set_plan(&self, id: &str, plan: &[TaskStep]) -> bool {
        TaskService::set_plan(self, id, plan)
    }

    fn set_subtask_status(
        &self,
        id: &str,
        status: TaskSubtaskStatus,
        result: Option<&str>,
        error: Option<&str>,
    ) -> bool {
        TaskService::set_subtask_status(self, id, status, result, error)
    }

    fn create_subtask(
        &self,
        task_id: &str,
        name: &str,
        instruction: &str,
    ) -> Result<String, String> {
        TaskService::create_subtask(self, task_id, name, instruction)
    }
}

// ==================== TaskTrace:调用追踪与用量留痕 ====================

impl TaskTrace for TaskService {
    fn record_usage(
        &self,
        task_id: &str,
        phase: &str,
        step_index: Option<usize>,
        out: &TaskGenOutput,
    ) {
        TaskService::record_usage(self, task_id, phase, step_index, out)
    }

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
    ) {
        TaskService::record_llm_call(
            self, task_id, phase, step_index, model, messages, response, out, elapsed, status,
        )
    }

    fn record_self_heals(
        &self,
        task_id: &str,
        phase: &str,
        step_index: Option<usize>,
        model: &str,
        messages: &[LlmMessage],
        self_heals: &[TruncationHeal],
    ) {
        TaskService::record_self_heals(
            self, task_id, phase, step_index, model, messages, self_heals,
        )
    }
}

// ==================== TaskSettings:任务模式有效设置 ====================

impl TaskSettings for TaskService {
    fn task_settings(&self) -> RuntimeSettings {
        TaskService::task_settings(self)
    }
}

// ==================== TaskPromptKit:提示词组装 ====================

impl TaskPromptKit for TaskService {
    fn assemble_executor_system_prompt(
        &self,
        settings: &RuntimeSettings,
        character_id: Option<&str>,
        user_goal: &str,
    ) -> String {
        TaskService::assemble_executor_system_prompt(self, settings, character_id, user_goal)
    }

    fn world_context(&self, character_id: Option<&str>) -> String {
        TaskService::world_context(self, character_id)
    }

    fn render_agent_prompt(
        &self,
        prompt: &str,
        character: Option<&CharacterRecord>,
        world_text: &str,
        user_goal: &str,
    ) -> String {
        TaskService::render_agent_prompt(self, prompt, character, world_text, user_goal)
    }
}

// ==================== TaskFlowAccess:自定义 Agent 流程访问 ====================

impl TaskFlowAccess for TaskService {
    fn current_flow(&self) -> Result<AgentFlowConfig, String> {
        // 加锁与校验在能力内部完成(批次 4.2:不把 Arc<Mutex<_>> 交给调用方);
        // 逻辑与原 task_engine/custom.rs::current_flow 逐行等价(克隆后立即释放锁)
        let flow = TaskService::agent_flow(self);
        let guard = flow.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = guard
            .get()
            .cloned()
            .ok_or("请先在设置中启用一个 Agent 流程")?;
        if !cfg.enabled {
            return Err("当前 Agent 流程未启用,请在设置中开启后再运行 custom 模式".into());
        }
        // 执行前按启动时注册工具集校验(与保存时同一 validate_flow)
        guard.validate(&cfg)?;
        Ok(cfg)
    }
}

// ==================== TaskEvents:任务事件发射 ====================

impl TaskEvents for TaskService {
    fn emit_event(
        &self,
        kind: TaskEventKind,
        task_id: &str,
        title: Option<String>,
        status: Option<TaskStatus>,
        detail: Option<String>,
    ) {
        TaskService::emit_event(self, kind, task_id, title, status, detail)
    }

    fn delta_batcher(&self, task_id: &str, phase: &str, step_index: Option<usize>) -> DeltaBatcher {
        TaskService::delta_batcher(self, task_id, phase, step_index)
    }
}

// ==================== TaskTerminalSink:执行终态落库 ====================

impl TaskTerminalSink for TaskService {
    fn finalize_terminal(
        &self,
        task_id: &str,
        token: u64,
        terminal: TaskTerminal,
        ended_by_cancel: bool,
    ) {
        TaskService::finalize_terminal(self, task_id, token, terminal, ended_by_cancel)
    }
}

// ==================== TaskGenerator:LLM 生成与分级重试 ====================

impl TaskGenerator for TaskService {
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
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>> {
        Box::pin(TaskService::generate_text(
            self,
            task_id,
            phase,
            step_index,
            messages,
            tools,
            max_tokens,
            temperature,
            top_p,
            cancel,
        ))
    }

    fn generate_step<'a>(
        &'a self,
        task: &'a TaskRecord,
        step: &'a TaskStep,
        step_index: Option<usize>,
        cancel: &'a watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>> {
        Box::pin(TaskService::generate_step(
            self, task, step, step_index, cancel,
        ))
    }

    fn generate_step_with<'a>(
        &'a self,
        task: &'a TaskRecord,
        step: &'a TaskStep,
        step_index: Option<usize>,
        max_tokens: u32,
        temperature: f64,
        cancel: &'a watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>> {
        Box::pin(TaskService::generate_step_with(
            self,
            task,
            step,
            step_index,
            max_tokens,
            temperature,
            cancel,
        ))
    }

    fn summarize_task<'a>(
        &'a self,
        task: &'a TaskRecord,
        plan: &'a [TaskStep],
        cancel: &'a watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>> {
        Box::pin(TaskService::summarize_task(self, task, plan, cancel))
    }

    fn summarize_task_with<'a>(
        &'a self,
        task: &'a TaskRecord,
        plan: &'a [TaskStep],
        max_tokens: u32,
        temperature: f64,
        cancel: &'a watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>> {
        Box::pin(TaskService::summarize_task_with(
            self,
            task,
            plan,
            max_tokens,
            temperature,
            cancel,
        ))
    }

    fn plan_task<'a>(
        &'a self,
        task_id: &'a str,
        title: &'a str,
        character_id: Option<&'a str>,
        max_tokens: u32,
        cancel: &'a watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>> {
        Box::pin(TaskService::plan_task(
            self,
            task_id,
            title,
            character_id,
            max_tokens,
            cancel,
        ))
    }

    fn plan_revise<'a>(
        &'a self,
        task: &'a TaskRecord,
        history: &'a [TaskMessageRecord],
        feedback: &'a str,
        max_tokens: u32,
        cancel: &'a watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<TaskGenOutput, String>> {
        Box::pin(TaskService::plan_revise(
            self, task, history, feedback, max_tokens, cancel,
        ))
    }
}
