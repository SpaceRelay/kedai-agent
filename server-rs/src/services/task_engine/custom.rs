// custom 模式:复用 AgentFlowService 当前启用流程(AgentFlowConfig 步骤序列,
// L2 既有资产)配轻量 step 执行器(批次 4.3b,docs/任务引擎六模式.md 第一节)。
// 语义:逐 step 顺序执行,上一步输出作为下一步输入;只吃 steps+goal,
// 不依赖角色卡/聊天历史(角色类占位符渲染为空,{{char}} 等宏原文不泄漏)。
// 工具:step.tools=None 走 generate_text 纯生成;Some([]) = 按 task_tool_policy 编译的
// 全部工具,Some(list) = 与步骤白名单取交(只能收窄);均经 run_tool_loop 的 ToolGate
// 闸门,恒不等待授权(任务模式名单外工具立即拒绝)。
// 反思步骤(action=reflect)按契约不携带用户 system_prompt,统一用内置
// CUSTOM_REFLECT_PROMPT;首版不做 reflect 回退循环(判定结论作为文本流向下一步)。
use super::context::TaskRunContext;
use super::executor::{ModeExecutor, TaskOutcome};
use super::sink;
use crate::agents::engine::executor::run_tool_loop;
use crate::agents::engine::{AbortFlag, AgentEngine};
use crate::agents::state_machine::StateMachine;
use crate::models::types::{
    GenerationParams, LlmMessage, PlanStep, TaskStatus, TaskStep, TaskStepStatus, TokenUsage,
    ToolChoice, ToolContext,
};
use crate::services::agent_flow_service::AgentFlowConfig;
use crate::services::prompt_kit::untrusted_boundary;
use crate::services::task_service::prompt::{
    CUSTOM_REFLECT_PROMPT, EXECUTOR_PROMPT, TASK_INTERNAL_PLAN_PROMPT,
};
use crate::services::task_service::{TaskGenOutput, TaskService};
use futures::future::BoxFuture;
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

/// custom 执行器:任务服务(流程库/落库/追踪)+ 聊天引擎(带工具步骤的工具循环)。
pub(crate) struct CustomExecutor {
    svc: Arc<TaskService>,
    engine: Arc<AgentEngine>,
}

impl CustomExecutor {
    pub(crate) fn new(svc: Arc<TaskService>, engine: Arc<AgentEngine>) -> Self {
        CustomExecutor { svc, engine }
    }

    /// 读取当前启用流程配置(克隆后立即释放锁,禁持引用跨 .await)。
    fn current_flow(&self) -> Result<AgentFlowConfig, String> {
        let flow = self.svc.agent_flow();
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

    /// 组装步骤 system:内置基础指令(direct 生成=执行者 / direct 非生成=内部规划 /
    /// reflect=内置反思)+ 步骤提示词(用户可编辑 → 宏渲染 + untrusted 包裹)+ 任务目标上下文(untrusted 包裹)。
    /// generates=false 的 direct 步骤用内部规划指令:其语义是「只做分析规划、不产出正文」,
    /// 与步骤自身定位一致(此前复用执行者指令会吐出完整正文,2026-09-10 实测修复)。
    fn build_step_system(&self, step: &PlanStep, goal: &str) -> String {
        let mut sys = match step.action.as_str() {
            "reflect" => String::from(CUSTOM_REFLECT_PROMPT),
            _ if step.generates == Some(false) => String::from(TASK_INTERNAL_PLAN_PROMPT),
            _ => String::from(EXECUTOR_PROMPT),
        };
        if let Some(prompt) = &step.system_prompt {
            if !prompt.trim().is_empty() {
                // 步骤提示词为用户可编辑配置:与 agent_system_prompt 同款宏渲染
                //(无角色卡/聊天历史:character=None,角色类占位符置空不泄漏宏原文)
                let rendered = self.svc.render_agent_prompt(prompt, None, "", goal);
                if !rendered.trim().is_empty() {
                    sys.push_str(&format!(
                        "\n\n{}",
                        untrusted_boundary("flow_step", &rendered)
                    ));
                }
            }
        }
        sys.push_str(&format!(
            "\n\n任务目标:\n{}",
            untrusted_boundary("task_goal", goal)
        ));
        sys
    }

    /// 带工具步骤:run_tool_loop + 步骤白名单;返回 (正文, 整轮 usage)。
    /// 调用追踪在本函数内落(phase=step;对齐 generate_text 的统一出口语义:
    /// 成功/空/中断/错误均落一行 task_llm_calls)。
    async fn run_step_with_tools(
        &self,
        ctx: &TaskRunContext,
        step_index: usize,
        step: &PlanStep,
        messages: &mut Vec<LlmMessage>,
        whitelist: &[String],
    ) -> Result<(String, TokenUsage), String> {
        let settings = &ctx.settings;
        // 任务模式工具策略:先按策略编译候选集(默认拒绝危险工具、剔除元工具),
        // 再与步骤白名单取交——步骤白名单只能收窄,不能突破任务策略放行危险工具。
        let policy = super::tool_policy::compile(
            &settings.task_tool_policy,
            &settings.task_tool_allowlist,
            &self.engine.tool_registry(),
        );
        // Some([]) = 策略全量集;Some(list) = 策略集 ∩ 步骤白名单
        let tools: Vec<_> = if whitelist.is_empty() {
            policy.defs
        } else {
            policy
                .defs
                .into_iter()
                .filter(|d| whitelist.iter().any(|w| w == &d.name))
                .collect()
        };
        // 闸门名单与下发工具一致:名单外立即拒绝(任务模式无 UI 授权上下文)
        let gate_list: Vec<String> = tools.iter().map(|d| d.name.clone()).collect();
        let gate = crate::agents::engine::executor::ToolGate::listed(&gate_list);
        let tool_choice = match step.tool_choice.as_deref() {
            Some("none") => ToolChoice::None,
            Some("required") => ToolChoice::Required,
            Some("function") => step
                .tool_choice_function
                .clone()
                .map(ToolChoice::Function)
                .unwrap_or_default(),
            _ => ToolChoice::Auto,
        };
        let params = GenerationParams {
            temperature: step.temperature.unwrap_or(settings.default_temperature),
            top_p: settings.default_top_p,
            max_tokens: step.max_tokens.unwrap_or(settings.default_max_tokens),
            stop: None,
            tools,
            max_tool_rounds: Some(settings.max_tool_rounds),
            tool_choice,
            parallel_tool_calls: step.parallel_tool_calls,
        };
        let session_id = format!("task:{}:step:{}", ctx.task_id, step_index + 1);
        let mut state_machine = StateMachine::new(&session_id);
        let run_id = Uuid::new_v4().to_string();
        let (flag, _flag_rx) = AbortFlag::new();
        let tool_ctx = ToolContext {
            session_id: session_id.clone(),
            character_id: String::new(), // custom 不吃角色卡
            agent_depth: 0,
        };
        // 事件桥(批次 R4 携 phase/step_index):custom 工具步骤的调用追踪口径为
        // phase=step + 本步骤下标(与下方 record_llm_call 一致)
        let (tx, drain) = sink::spawn(
            self.svc.clone(),
            ctx.task_id.clone(),
            "主 agent",
            "step",
            Some(step_index),
        );
        let mut total_usage = TokenUsage::default();
        let started = Instant::now();
        let result = run_tool_loop(
            &self.engine,
            &mut state_machine,
            None,
            &session_id,
            messages,
            &params,
            &tool_ctx,
            &tx,
            &ctx.cancel,
            &flag,
            &mut total_usage,
            &run_id,
            gate,
        )
        .await;
        drop(tx);
        let _ = drain.await;

        let elapsed = started.elapsed();
        let model = self.engine.model();
        match result {
            Ok(res) if !res.interrupted => {
                // 截断自愈留痕落库(问题①,与 solo.rs run_agent_loop 同口径):
                // 被截断的那次调用补落一行 status=error,再落最终行
                for heal in &res.self_heals {
                    let heal_out = TaskGenOutput {
                        text: String::new(),
                        finish_reason: heal.finish_reason.clone(),
                        prompt_tokens: heal.prompt_tokens,
                        completion_tokens: heal.completion_tokens,
                        reasoning_tokens: 0,
                        reasoning_chars: 0,
                        tool_calls: Vec::new(),
                    };
                    self.svc.record_llm_call(
                        &ctx.task_id,
                        "step",
                        Some(step_index),
                        &model,
                        messages,
                        &format!(
                            "(截断自愈){},输出上限翻倍至 {} 重发",
                            heal.note, heal.retried_max_tokens
                        ),
                        Some(&heal_out),
                        std::time::Duration::ZERO,
                        "error",
                    );
                    // 补落被截断那次调用的 usage(2026-09-10 实测修复,口径同 solo.rs)
                    self.svc
                        .record_usage(&ctx.task_id, "step", Some(step_index), &heal_out);
                }
                let text = res.content.trim().to_string();
                let status = if text.is_empty() { "empty" } else { "ok" };
                let out = TaskGenOutput {
                    text: text.clone(),
                    // 上游 finish_reason 经 run_tool_loop 末轮透出(可观测性问题①)
                    finish_reason: res.finish_reason.clone(),
                    prompt_tokens: total_usage.prompt_tokens,
                    completion_tokens: total_usage.completion_tokens,
                    reasoning_tokens: 0,
                    reasoning_chars: 0,
                    tool_calls: Vec::new(),
                };
                self.svc.record_llm_call(
                    &ctx.task_id,
                    "step",
                    Some(step_index),
                    &model,
                    messages,
                    &text,
                    Some(&out),
                    elapsed,
                    status,
                );
                Ok((text, total_usage))
            }
            Ok(_) => {
                self.svc.record_llm_call(
                    &ctx.task_id,
                    "step",
                    Some(step_index),
                    &model,
                    messages,
                    "(已中断)",
                    None,
                    elapsed,
                    "error",
                );
                Err("任务已停止".into())
            }
            Err(e) => {
                self.svc.record_llm_call(
                    &ctx.task_id,
                    "step",
                    Some(step_index),
                    &model,
                    messages,
                    &e,
                    None,
                    elapsed,
                    "error",
                );
                Err(e)
            }
        }
    }

    async fn run_inner(&self, ctx: TaskRunContext) -> Result<TaskOutcome, String> {
        let cfg = self.current_flow()?;
        let steps: Vec<PlanStep> = cfg.steps.iter().filter(|s| s.enabled).cloned().collect();
        if steps.is_empty() {
            return Err("当前 Agent 流程没有启用的步骤".into());
        }
        let svc = &self.svc;
        svc.set_status(&ctx.task_id, TaskStatus::Running);

        // 进度模型 = plan 步骤(前端 custom 渲染:plan 步骤 + 状态徽标)
        let mut plan: Vec<TaskStep> = steps
            .iter()
            .map(|s| TaskStep {
                name: s.name.clone(),
                goal: s.goal.clone(),
                status: TaskStepStatus::Pending,
                result: String::new(),
            })
            .collect();
        svc.set_plan(&ctx.task_id, &plan);

        let mut prev_output = String::new();
        // 最近一次「生成正文」(direct + generates)步骤的产出,即最终结果
        let mut draft = String::new();
        let mut any_error = false;
        let mut total = TokenUsage::default();

        for (i, step) in steps.iter().enumerate() {
            if *ctx.cancel.borrow() {
                return Err("任务已停止".into());
            }
            plan[i].status = TaskStepStatus::Running;
            svc.set_plan(&ctx.task_id, &plan);

            let sys = self.build_step_system(step, &ctx.goal);
            // 上一步输出作为下一步输入;任务目标贯穿每一步的 user 消息
            //(同时进 system 上下文),保证后续步骤始终可见原始目标
            let user = if i == 0 {
                ctx.goal.clone()
            } else {
                format!(
                    "任务目标:\n{}\n\n上一步「{}」产出:\n{}",
                    ctx.goal,
                    steps[i - 1].name,
                    prev_output
                )
            };
            let mut messages = vec![
                LlmMessage::plain("system", &sys),
                LlmMessage::plain("user", &user),
            ];

            let result: Result<(String, TaskGenOutput), String> = match &step.tools {
                // 无工具步骤:纯生成统一出口(generate_text 自带调用追踪落库)
                None => {
                    let settings = &ctx.settings;
                    svc.generate_text(
                        &ctx.task_id,
                        "step",
                        Some(i),
                        messages,
                        Vec::new(),
                        step.max_tokens.unwrap_or(settings.default_max_tokens),
                        step.temperature.unwrap_or(settings.default_temperature),
                        settings.default_top_p,
                        ctx.cancel.clone(),
                    )
                    .await
                    .map(|out| {
                        let text = out.text.trim().to_string();
                        (text, out)
                    })
                }
                // 工具步骤:run_tool_loop(白名单自动放行),调用追踪在步骤函数内落
                Some(list) => self
                    .run_step_with_tools(&ctx, i, step, &mut messages, list)
                    .await
                    .map(|(text, usage)| {
                        let out = TaskGenOutput {
                            text: text.clone(),
                            finish_reason: None,
                            prompt_tokens: usage.prompt_tokens,
                            completion_tokens: usage.completion_tokens,
                            reasoning_tokens: 0,
                            reasoning_chars: 0,
                            tool_calls: Vec::new(),
                        };
                        (text, out)
                    }),
            };

            match result {
                Ok((text, out)) => {
                    total.prompt_tokens += out.prompt_tokens;
                    total.completion_tokens += out.completion_tokens;
                    total.total_tokens += out.prompt_tokens + out.completion_tokens;
                    // usage 落库:逐步骤一行(phase=step,对齐 legacy 按调用计口径)
                    svc.record_usage(&ctx.task_id, "step", Some(i), &out);
                    if text.is_empty() {
                        any_error = true;
                        plan[i].status = TaskStepStatus::Error;
                        plan[i].result = format!("步骤「{}」返回空内容", step.name);
                        prev_output.clear();
                    } else {
                        plan[i].status = TaskStepStatus::Done;
                        // generates=false 的内部规划步骤:产出仅供后续步骤参考,
                        // 加标注区分于面向用户的成果(不参与 draft 选拔)。
                        if step.action == "direct" && step.generates == Some(false) {
                            plan[i].result = format!("(内部规划)\n{text}");
                        } else {
                            plan[i].result = text.clone();
                        }
                        // 仅「生成正文」的 direct 步骤产出进入最终成果
                        if step.action == "direct" && step.generates == Some(true) {
                            draft = text.clone();
                        }
                        prev_output = text;
                    }
                }
                Err(e) => {
                    if *ctx.cancel.borrow() {
                        return Err("任务已停止".into());
                    }
                    any_error = true;
                    plan[i].status = TaskStepStatus::Error;
                    plan[i].result = e;
                    prev_output.clear();
                }
            }
            svc.set_plan(&ctx.task_id, &plan);
        }

        if draft.is_empty() {
            return Err("自定义流程未产出任何成果(生成步骤全部失败或为空)".into());
        }
        // 含 error 步骤但成果已产出 → partial(对齐 legacy WP3 语义)
        let status = if any_error {
            Some(TaskStatus::Partial)
        } else {
            None
        };
        Ok(TaskOutcome {
            text: draft,
            usage: total,
            status,
            error: None,
        })
    }
}

impl ModeExecutor for CustomExecutor {
    fn run<'a>(&'a self, ctx: TaskRunContext) -> BoxFuture<'a, Result<TaskOutcome, String>> {
        Box::pin(async move { self.run_inner(ctx).await })
    }
}
