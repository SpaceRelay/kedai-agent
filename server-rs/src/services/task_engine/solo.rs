// solo 模式:单主 agent 工具自循环——目标直接进 run_tool_loop,工具集按
// task_tool_policy 编译(默认 deny_dangerous:危险级与元工具除外、bash 例外),
// 步数上限 max_tool_rounds(docs/功能.md 第一节)。
// 复用聊天引擎 run_tool_loop,不建影子 sessions 行(session_id 用 task: 前缀虚拟 id,
// llm_requests 落库在引擎侧据此跳过;任务侧追踪走 task_llm_calls,phase=agent)。
// run_agent_loop 为「单主 agent 工具自循环」共享骨架:solo/multi 执行器与
// team 模式的各主 agent 复用同一实现(批次 4.3b),差异仅在运行身份/追踪字段。
use super::context::TaskRunContext;
use super::executor::{usage_as_output, ModeExecutor};
use super::sink;
use crate::agents::engine::executor::run_tool_loop;
use crate::agents::engine::{AbortFlag, AgentEngine};
use crate::agents::state_machine::{AgentState, StateMachine};
use crate::models::types::{
    GenerationParams, LlmMessage, TaskStatus, TokenUsage, ToolChoice, ToolContext,
};
use crate::services::settings_service::RuntimeSettings;
use crate::services::task_core::{TaskBackend, TaskTerminal};
use futures::future::BoxFuture;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::watch;
use uuid::Uuid;

/// 单主 agent 工具自循环的入参(owned,便于 tokio::spawn/JoinSet 跨 'static 边界)。
pub(crate) struct AgentLoopCall {
    /// 任务 id(调用追踪/usage 落库/事件桥用)
    pub task_id: String,
    /// 运行身份(虚拟 session_id):solo/multi = task:{id};team 主 agent = task:{id}:main:{n}
    pub session_id: String,
    /// user 消息正文(solo/multi = 任务目标;team 主 = 总体目标 + 该主分工文本)
    pub goal: String,
    /// 任务模式有效设置快照(执行全程读快照,与 legacy 同语义)
    pub settings: RuntimeSettings,
    /// 执行者人设角色 id(空 = 通用执行者)
    pub character_id: Option<String>,
    /// 调用追踪 phase(solo/multi/team 主 agent 均为 "agent")
    pub phase: &'static str,
    /// 调用追踪 step_index(team/plan 续跑 = 子目标/步骤在 plan 中的全局下标;
    /// solo/multi = None)
    pub step_index: Option<usize>,
    /// 事件桥文案称谓(「主 agent」/team 的「主 agent N」)
    pub label: String,
}

/// 单主 agent 工具自循环(solo/multi 执行器主体;team 各主 agent 复用):
/// system 组装(内置执行者指令 → 人设 → 世界书 → 提示词注入 → Agent 提示词,
/// 外部段落逐一 untrusted 包裹)→ run_tool_loop(工具按 task_tool_policy 编译 +
/// ToolGate 闸门,恒不等待授权:名单外工具立即拒绝)→ 调用追踪统一出口落 task_llm_calls(成功/空/中断/错误均一行)。
/// 成功返回 (正文, 整轮累计 usage);中断/错误/空内容返回 Err。
pub(crate) async fn run_agent_loop(
    svc: Arc<dyn TaskBackend>,
    engine: Arc<AgentEngine>,
    call: AgentLoopCall,
    cancel: watch::Receiver<bool>,
) -> Result<(String, TokenUsage), String> {
    let settings = &call.settings;

    // system 提示词组装:统一走 TaskBackend::assemble_executor_system_prompt
    // (单一实现,宿主侧 prompt.rs);拼装顺序与 untrusted 包裹纪律同 legacy,
    // 勿在本文件复制实现(WP7)。
    let sys =
        svc.assemble_executor_system_prompt(settings, call.character_id.as_deref(), &call.goal);

    let mut messages = vec![
        LlmMessage::plain("system", &sys),
        LlmMessage::plain("user", &call.goal),
    ];

    // 工具按任务策略下发(批次授权改造):默认拒绝危险工具;元工具不入正文列表。
    // 任务模式无 UI 授权上下文,闸门恒 no_ui_authorization = true:
    // 名单外工具立即拒绝并回灌错误,不会空等 300 秒授权超时。
    let policy = super::tool_policy::compile(
        &settings.task_tool_policy,
        &settings.task_tool_allowlist,
        &engine.tool_registry(),
    );
    // 闸门名单先取出(allowed 借用生命周期需覆盖整个工具循环),再取走 defs
    let allowed = policy.allowed;
    let params = GenerationParams {
        temperature: settings.default_temperature,
        top_p: settings.default_top_p,
        max_tokens: settings.default_max_tokens,
        stop: None,
        tools: policy.defs,
        max_tool_rounds: Some(settings.max_tool_rounds),
        tool_choice: ToolChoice::Auto,
        parallel_tool_calls: None,
    };
    let gate = crate::agents::engine::executor::ToolGate::listed(&allowed);

    // 运行身份:虚拟 session_id(task: 前缀,不建 sessions/agent_sessions 影子行);
    // 状态迁移校验:非法迁移记 warn 日志(StateMachine 会返回 Err,不得静默丢弃);
    // run_id 标识本轮(工具授权等待的 key,白名单模式下用不到)。
    let mut state_machine = StateMachine::new(&call.session_id);
    let run_id = Uuid::new_v4().to_string();
    // AbortFlag 仅供 send_event 的「通道断开」置位;中断信号本体是任务取消通道
    // cancel(register_cancel 登记,stop 经 signal_cancel 触发)。
    let (flag, _flag_rx) = AbortFlag::new();
    let tool_ctx = ToolContext {
        session_id: call.session_id.clone(),
        character_id: call.character_id.clone().unwrap_or_default(),
        agent_depth: 0,
    };
    // 事件桥:引擎事件 → 任务事件(agent_status);drain 持续消费到 tx drop。
    // phase/step_index 随桥传入(批次 R4):Token 攒批 delta 携带调用归属,
    // 与下方 record_llm_call 落库行同口径(前端按 key 对齐暂态与权威)
    let (tx, drain) = sink::spawn(
        svc.clone(),
        call.task_id.clone(),
        &call.label,
        call.phase,
        call.step_index,
    );
    let mut total_usage = TokenUsage::default();
    let started = Instant::now();
    // Idle → Executing 为任务/工具路径的合法首迁(无独立规划阶段,见 state_machine.rs);
    // 仍记录非法迁移,避免静默吞掉状态机校验结果。
    if let Err(e) = state_machine.transition(AgentState::Executing, &call.session_id) {
        tracing::warn!(error = %e, session = %call.session_id, "状态迁移被拒");
    }
    let result = run_tool_loop(
        &engine,
        &mut state_machine,
        None, // 任务模式无 agent_sessions 行:跳过状态/工具调用落库
        &call.session_id,
        &mut messages,
        &params,
        &tool_ctx,
        &tx,
        &cancel,
        &flag,
        &mut total_usage,
        &run_id,
        gate,
    )
    .await;
    // 先关通道再等 drain 收尾,保证进度事件全部转发完毕
    drop(tx);
    let _ = drain.await;

    // 调用追踪统一出口:成功/空/中断/错误均落一行 task_llm_calls;
    // token 用整轮累计 total_usage;messages 此时含完整工具循环历史,摘要自截断。
    let elapsed = started.elapsed();
    let model = engine.model();
    match result {
        Ok(res) if !res.interrupted => {
            // 截断自愈留痕落库(问题①):被截断的调用补落一行 + 补 usage,
            // 统一实现见 TaskService::record_self_heals(与 custom 共用)。
            svc.record_self_heals(
                &call.task_id,
                call.phase,
                call.step_index,
                &model,
                &messages,
                &super::executor::to_self_heals(&res.self_heals),
            );
            let text = res.content.trim().to_string();
            let status = if text.is_empty() { "empty" } else { "ok" };
            // token/字段构造统一走 usage_as_output(批次 B.4 单一出处);text 由
            // record_llm_call 的 response 参数单独承载,finish_reason 是本调用特有的
            // 诊断(问题①)故单独覆盖——上游未下发时为 None,落库 ''。
            let mut out = usage_as_output(&total_usage);
            out.finish_reason = res.finish_reason.clone();
            svc.record_llm_call(
                &call.task_id,
                call.phase,
                call.step_index,
                &model,
                &messages,
                &text,
                Some(&out),
                elapsed,
                status,
            );
            if text.is_empty() {
                // 空内容错误带 finish_reason(问题③):区分「已达 token 上限」(推理
                // 烧光预算,实测主因)与其他成因,与 legacy retry_if_empty_output
                // 的文案口径对齐,步骤 result 落库后可读
                if let Err(e) = state_machine.transition(AgentState::Error, &call.session_id) {
                    tracing::warn!(error = %e, session = %call.session_id, "状态迁移被拒");
                }
                let reason = res.finish_reason.as_deref().unwrap_or("未知");
                return Err(format!("{}返回空内容(finish_reason={reason})", call.label));
            }
            if let Err(e) = state_machine.transition(AgentState::Finished, &call.session_id) {
                tracing::warn!(error = %e, session = %call.session_id, "状态迁移被拒");
            }
            Ok((text, total_usage))
        }
        Ok(_) => {
            // 中断(用户 stop):与 legacy 的 connector Err 同款记 error 行
            svc.record_llm_call(
                &call.task_id,
                call.phase,
                call.step_index,
                &model,
                &messages,
                "(已中断)",
                None,
                elapsed,
                "error",
            );
            if let Err(e) = state_machine.transition(AgentState::Interrupted, &call.session_id) {
                tracing::warn!(error = %e, session = %call.session_id, "状态迁移被拒");
            }
            Err("任务已停止".into())
        }
        Err(e) => {
            // 对外契约是字符串错误(任务追踪列/步骤 result),分类在此落回文案;
            // 分类只服务聊天路径的 SSE 错误终态。
            let msg = e.message().to_string();
            svc.record_llm_call(
                &call.task_id,
                call.phase,
                call.step_index,
                &model,
                &messages,
                &msg,
                None,
                elapsed,
                "error",
            );
            if let Err(e) = state_machine.transition(AgentState::Error, &call.session_id) {
                tracing::warn!(error = %e, session = %call.session_id, "状态迁移被拒");
            }
            Err(msg)
        }
    }
}

/// solo 执行器:持有任务后端(提示词助手/状态落库/调用追踪)与聊天引擎(工具循环)。
pub(crate) struct SoloExecutor {
    svc: Arc<dyn TaskBackend>,
    engine: Arc<AgentEngine>,
}

impl SoloExecutor {
    pub(crate) fn new(svc: Arc<dyn TaskBackend>, engine: Arc<AgentEngine>) -> Self {
        SoloExecutor { svc, engine }
    }

    async fn run_inner(&self, ctx: TaskRunContext) -> Result<(TaskTerminal, TokenUsage), String> {
        // solo 无规划阶段:进入即执行(run 入口 reset_task 已置 planning,此处推进到 running)
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
        // usage 落库(批次 4.3b 口径补齐:solo 整轮一行,phase=agent;approve 续跑
        // 自 2026-08 起由 ApprovedPlanExecutor 逐步落库,本执行器仅作 plan 为空时的兜底)
        self.svc
            .record_usage(&ctx.task_id, "agent", None, &usage_as_output(&usage));
        // 成功终态:result 文本 + done(无覆盖状态/原因)
        Ok((
            TaskTerminal::Complete {
                result: text,
                status: TaskStatus::Done,
                error: None,
            },
            usage,
        ))
    }
}

impl ModeExecutor for SoloExecutor {
    fn run<'a>(
        &'a self,
        ctx: TaskRunContext,
    ) -> BoxFuture<'a, Result<(TaskTerminal, TokenUsage), String>> {
        Box::pin(async move { self.run_inner(ctx).await })
    }
}
