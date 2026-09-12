// 生成执行:计算器启发式工具触发、单次流式生成与 AGENT 模式工具循环
use super::*;
use std::sync::atomic::{AtomicI64, Ordering};

/// 进程级 LLM 请求快照序号(单调递增;第四点·主题 A 的 llm_requests.seq 用,
/// 跨 run 也单调,比「run 内自增」更强,prune 按 id 删除不受影响)。
static LLM_REQUEST_SEQ: AtomicI64 = AtomicI64::new(0);

/// 工具触发(每个生成周期最多一次;失败不致命)
#[allow(clippy::too_many_arguments)]
pub(super) async fn maybe_run_tool(
    engine: &AgentEngine,
    state_machine: &mut StateMachine,
    tool_triggered: &mut bool,
    agent_session_id: &str,
    session_id: &str,
    user_input: &str,
    tool_ctx: &ToolContext,
    llm_messages: &mut Vec<LlmMessage>,
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
) -> Result<(), String> {
    if *tool_triggered || !looks_like_calculation(user_input) {
        return Ok(());
    }
    *tool_triggered = true;
    let _ = state_machine.transition(AgentState::ToolCall, session_id);
    let _ = engine
        .agent_sessions
        .update(agent_session_id, Some("tool_call"), None, None, None);
    let expression = extract_expression(user_input);
    send_event(
        SseEvent::ToolCall {
            name: "calculator".into(),
            input: json!({ "expression": expression }),
            call_id: None,
            render_kind: Some("generic".into()),
        },
        tx,
        abort,
        flag,
    )
    .await?;

    let start = std::time::Instant::now();
    let output: Value = match engine
        .tool_registry
        .execute(
            "calculator",
            &json!({ "expression": expression }).to_string(),
            tool_ctx.clone(),
        )
        .await
    {
        Ok(r) => serde_json::from_str(&r).unwrap_or(Value::Null),
        Err(e) => {
            let _ = send_event(
                step_evt("工具调用失败,改用直接生成", Some(e.clone()), None, None),
                tx,
                abort,
                flag,
            )
            .await;
            json!({ "error": e })
        }
    };
    let _ = engine.agent_sessions.add_tool_call(
        agent_session_id,
        "calculator",
        json!({ "expression": expression }),
        output.clone(),
        start.elapsed().as_millis() as i64,
    );
    send_event(
        SseEvent::ToolResult {
            name: "calculator".into(),
            output: output.clone(),
            call_id: None,
            render_kind: Some("generic".into()),
        },
        tx,
        abort,
        flag,
    )
    .await?;
    // 计算结果回填到 LLM 消息(否则模型看不到结果,只能瞎猜答案,工具对提示无实际作用)。
    // 以 user 角色的中性旁白注入,让模型当作「外部提供的事实」而非自己的发言,
    // 避免「[计算器结果]」元文本污染正文、破坏角色扮演沉浸感。
    if let Some(result) = output.get("result") {
        llm_messages.push(LlmMessage {
            role: "user".into(),
            content: format!("(旁白:计算结果为 {result})"),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        });
    }
    let _ = state_machine.transition(AgentState::Executing, session_id);
    let _ = engine
        .agent_sessions
        .update(agent_session_id, Some("executing"), None, None, None);
    Ok(())
}
/// 流式执行 LLM 生成(与 Node 版 executor.ts executeGeneration 对齐)
/// pub(crate):任务引擎 custom 模式(批次 4.3b)按步骤直调;聊天路径行为不变。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_generation(
    engine: &AgentEngine,
    session_id: &str,
    run_id: &str,
    messages: &[LlmMessage],
    params: &GenerationParams,
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
) -> Result<ExecutorResult, String> {
    // LLM 请求快照(第四点·主题 A):开关开启时,把真正下发的完整消息数组落盘,
    // 供回放/调试「模型到底看到了什么」。失败仅告警,不阻塞生成。
    // seq 总是分配:缓存观测(usage 落库)不依赖快照开关。
    let seq = LLM_REQUEST_SEQ.fetch_add(1, Ordering::Relaxed);
    // 任务模式(session_id 带 task: 前缀)跳过 llm_requests 落库:该表 FK 到 sessions(id),
    // 虚拟 id 会 FK 失败刷 warn;任务侧调用追踪统一走 task_llm_calls(批次 3 起)。
    let is_task_run = session_id.starts_with("task:");
    {
        // 设置快照:不留锁跨 await(锁在 settings_snapshot 内即释放)
        let log_enabled = engine.settings_snapshot().llm_request_log;
        if log_enabled && !is_task_run {
            if let Ok(payload) = serde_json::to_string(messages) {
                if let Err(e) = engine.sessions.save_llm_request(
                    session_id,
                    run_id,
                    seq,
                    &payload,
                    &engine.model(),
                ) {
                    tracing::warn!(error = e, "LLM 请求快照落库失败");
                }
            }
        }
    }
    let mut content = String::new();
    let mut usage = TokenUsage::default();
    // 按 index 聚合后的完整工具调用(由连接器在流结束时输出)
    let mut tool_calls: Vec<ToolCallArgs> = Vec::new();
    // 思考模式推理内容(多轮工具调用需随 assistant 消息回传)
    let mut reasoning = String::new();
    // 上游 finish_reason(stop/length 等;可观测性问题①):聚合到 ExecutorResult,
    // 任务模式经 run_tool_loop 一路带到 task_llm_calls 落库点;聊天路径不消费本字段。
    let mut finish_reason: Option<String> = None;

    let connector = engine.connector.read().await;
    let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel();
    let generate = connector.generate_stream(messages, params.clone(), abort.clone(), chunk_tx);
    tokio::pin!(generate);
    let mut generation_result = None;

    loop {
        tokio::select! {
            result = &mut generate, if generation_result.is_none() => {
                generation_result = Some(result);
            }
            chunk = chunk_rx.recv() => match chunk {
                Some(chunk) => {
                    if process_chunk(chunk, &mut content, &mut reasoning, &mut tool_calls, &mut usage, &mut finish_reason, tx, abort, flag).await? {
                        return Ok(ExecutorResult { content, usage, interrupted: true, tool_calls, reasoning, finish_reason, self_heals: Vec::new() });
                    }
                }
                None => break,
            }
        }
        if generation_result.is_some() && chunk_rx.is_empty() {
            break;
        }
    }
    if let Some(Err(e)) = generation_result {
        if *abort.borrow() {
            return Ok(ExecutorResult {
                content,
                usage,
                interrupted: true,
                tool_calls,
                reasoning,
                finish_reason,
                self_heals: Vec::new(),
            });
        }
        return Err(e);
    }

    // 缓存观测落库(缓存感知管线):每轮请求的命中/未命中 token 记入 llm_requests,
    // 与快照开关解耦;失败仅告警,不影响生成结果。
    // 任务模式(task: 前缀)同样跳过:llm_requests FK 到 sessions 表,任务追踪走 task_llm_calls。
    if !is_task_run {
        if let Err(e) = engine.sessions.save_llm_cache_usage(
            session_id,
            run_id,
            seq,
            &engine.model(),
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.prompt_cache_hit_tokens,
            usage.prompt_cache_miss_tokens,
        ) {
            tracing::warn!(error = e, "LLM 缓存统计落库失败");
        }
    }

    Ok(ExecutorResult {
        content,
        usage,
        interrupted: *abort.borrow(),
        tool_calls,
        reasoning,
        finish_reason,
        self_heals: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn process_chunk(
    chunk: LlmStreamChunk,
    content: &mut String,
    reasoning: &mut String,
    tool_calls: &mut Vec<ToolCallArgs>,
    usage: &mut TokenUsage,
    finish_reason: &mut Option<String>,
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
) -> Result<bool, String> {
    if *abort.borrow() {
        return Ok(true);
    }
    match chunk {
        LlmStreamChunk::Token(text) => {
            content.push_str(&text);
            send_event(SseEvent::Token { text }, tx, abort, flag).await?;
        }
        LlmStreamChunk::Reasoning(rc) => reasoning.push_str(&rc),
        LlmStreamChunk::ToolCall(call) => {
            if !call.id.is_empty() && !call.name.is_empty() {
                tool_calls.push(call.clone());
            }
            send_event(
                SseEvent::ToolCall {
                    name: if call.name.is_empty() {
                        "unknown".into()
                    } else {
                        call.name.clone()
                    },
                    input: Value::String(call.arguments),
                    call_id: (!call.id.is_empty()).then_some(call.id),
                    render_kind: crate::tools::registry::render_kind_for(&call.name)
                        .map(|s| s.to_string()),
                },
                tx,
                abort,
                flag,
            )
            .await?;
        }
        LlmStreamChunk::Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            prompt_cache_hit_tokens,
            prompt_cache_miss_tokens,
            ..
        } => {
            usage.prompt_tokens += prompt_tokens;
            usage.completion_tokens += completion_tokens;
            usage.total_tokens += total_tokens;
            usage.prompt_cache_hit_tokens += prompt_cache_hit_tokens;
            usage.prompt_cache_miss_tokens += prompt_cache_miss_tokens;
        }
        // finish_reason 聚合到结果(可观测性问题①):任务模式据此落 task_llm_calls,
        // 区分「正常收尾(stop)」与「max_tokens 截断(length)」;聊天引擎不据此动作
        LlmStreamChunk::Finish { reason } => *finish_reason = Some(reason),
    }
    Ok(false)
}

/// 执行结果(executor 输出);content/usage/interrupted 由父模块 run() 主流程读取
/// pub(crate):任务引擎(task_engine)直调 run_tool_loop 后读取本结构(批次 4.2)。
pub(crate) struct ExecutorResult {
    pub(crate) content: String,
    pub(crate) usage: TokenUsage,
    pub(crate) interrupted: bool,
    /// 模型本轮请求的工具调用(AGENT 模式由 run_tool_loop 执行)
    pub(crate) tool_calls: Vec<ToolCallArgs>,
    /// 本轮思考模式推理内容(回传用)
    pub(crate) reasoning: String,
    /// 上游 finish_reason(stop/length 等;可观测性问题①):任务模式落库用,
    /// 上游未下发/中断未完成时为 None;聊天路径不消费本字段
    pub(crate) finish_reason: Option<String>,
    /// 本轮内发生的截断自愈记录(问题①,2026-08-31 deepseek 实测修复):
    /// 仅 run_tool_loop 的单轮自愈路径产出,其余构造点恒空;聊天路径不消费。
    pub(crate) self_heals: Vec<SelfHealRecord>,
}

/// 截断自愈记录(问题①):单轮生成被 max_tokens 截断到不可用(空正文/半截
/// tool_call JSON)时「翻倍预算原样重发一次」的留痕。供任务模式(run_agent_loop /
/// custom 步骤)把被截断的那次调用补落 task_llm_calls,调用情况面板可见
/// 「截断 → 提高预算重发 → 成功/失败」完整链路。
pub(crate) struct SelfHealRecord {
    /// 触发原因描述(落 response_summary;如「返回空内容(已达 token 上限)」
    /// 「工具参数 JSON 截断(工具 "calculator")」)
    pub(crate) note: String,
    /// 被截断调用的 token 用量(Err 形态拿不到,记 0)
    pub(crate) prompt_tokens: i64,
    pub(crate) completion_tokens: i64,
    /// 被截断调用的 finish_reason(Ok 形态恒 Some("length");
    /// Err 形态 finish_reason 随连接器错误丢失,None)
    pub(crate) finish_reason: Option<String>,
    /// 重发使用的 max_tokens(翻倍后,上限 TRUNCATION_HEAL_MAX_TOKENS_CAP)
    pub(crate) retried_max_tokens: u32,
}

/// 截断自愈重发的 max_tokens 上限(问题①):与 task_service 空输出重试同款
/// 「翻倍+封顶」口径;单轮产出(一次 tool_call 或一段正文)8192 足够宽裕,
/// 封顶防止异常上游把单轮预算顶到设置页上限(65536)空烧 token。
const TRUNCATION_HEAL_MAX_TOKENS_CAP: u32 = 8192;

/// 计算自愈重发的 max_tokens(翻倍+封顶);已封顶返回 None(重发无意义,走原错误路径)
fn doubled_heal_budget(current: u32) -> Option<u32> {
    let doubled = (current.saturating_mul(2)).min(TRUNCATION_HEAL_MAX_TOKENS_CAP);
    (doubled > current).then_some(doubled)
}

/// Ok 形态的截断判定(问题①):finish_reason=length 且该轮产出不可用——
/// 任一 tool_call 的 arguments 非空但非法 JSON(参数被预算切成半截,执行必败),
/// 或空正文且无 tool_call(推理烧光预算,正文为零)。普通文本截断(非空正文、
/// 无/合法工具调用)不算:半截文本也是产出,维持既有「截断仍 done」语义
/// (task_llm_call_finish_reason_marks_length_truncation 锁定)。
/// 空正文但带合法 tool_call 也不算:模型本轮只想调工具,工具调用本身就是产出
/// (heal_cause_ignores_healthy_results 锁定),误触发自愈会白白重发一轮。
fn truncation_heal_cause(res: &ExecutorResult) -> Option<String> {
    if res.finish_reason.as_deref() != Some("length") {
        return None;
    }
    // 先查半截 tool_call(成因更具体:正文为空也可能是预算被工具参数烧光)
    if let Some(bad) = res.tool_calls.iter().find(|c| {
        !c.arguments.trim().is_empty() && serde_json::from_str::<Value>(&c.arguments).is_err()
    }) {
        return Some(format!("工具参数 JSON 截断(工具 \"{}\")", bad.name));
    }
    if res.content.trim().is_empty() && res.tool_calls.is_empty() {
        return Some("返回空内容(已达 token 上限)".into());
    }
    None
}

/// Err 形态的截断判定(问题①):真实连接器(openai_compatible)在 finish_reason=length
/// 时把半截 tool_call 留到流尾 flush 校验,报「工具 "X" 的 arguments 不是合法 JSON」,
/// finish_reason 随错误丢失,只能按该专属错误文案判定。
/// (跨层文案耦合点:connectors/openai_compatible/sse_parser.rs flush_tool_calls,
/// 该文案变更时此处须同步。)
fn is_truncated_tool_call_error(err: &str) -> bool {
    err.contains("arguments 不是合法 JSON")
}

/// AGENT 模式工具循环:生成 → 有 tool_calls 则逐个执行并回填消息 → 重新生成,
/// 轮次上限默认 32(params.max_tool_rounds 可调,settings 页配置);无 tool_calls 时返回最终正文。
/// 每轮 usage 已累加进 total_usage。
/// 截断自愈(问题①):单轮生成被 max_tokens 截断到不可用(空正文 / 半截 tool_call
/// JSON / 连接器流尾 flush 校验报错)时,本轮 max_tokens 翻倍(上限
/// TRUNCATION_HEAL_MAX_TOKENS_CAP)原样重发一次,重发仍失败才透出原结果;
/// 触发自愈的记录经 ExecutorResult.self_heals 透出(任务模式补落 task_llm_calls)。
/// SSE 语义:模型发出调用时由 execute_generation 推送 ToolCall,执行完成后此处推送 ToolResult。
/// 轮次语义(修复原 8 轮 off-by-one):第 N 轮(含 N=上限)生成的工具调用照常执行,
/// 只是执行完后停止再发起新的模型请求——工具调用不会被静默丢弃,卡片不会无终态悬挂。
/// 白名单模式:步骤配置了 tools 白名单时,名单内工具自动放行(不再弹授权框)。
/// agent_session 为 None 表示任务模式(不建影子 agent_sessions 行,
/// 工具授权闸门:替代裸白名单,把「名单内放行」与「名单外如何处理」分开表达。
/// - `whitelist`:None = 非白名单模式(未放行工具走授权等待);
///   Some([]) = 全量放行;Some(list) = 仅名单内工具放行。
/// - `no_ui_authorization`:true = 未放行工具立即拒绝并回灌错误,不进入授权等待。
///   任务模式没有 UI 授权上下文,若走等待会空等 300 秒超时,故必须置 true。
#[derive(Clone, Copy)]
pub(crate) struct ToolGate<'a> {
    pub whitelist: Option<&'a [String]>,
    pub no_ui_authorization: bool,
}

impl<'a> ToolGate<'a> {
    /// 非白名单模式,未放行则等待授权(聊天 agent 路径)
    pub(crate) fn wait() -> Self {
        Self {
            whitelist: None,
            no_ui_authorization: false,
        }
    }

    /// 显式白名单,不等待(任务 custom 步骤 / 子 agent)
    pub(crate) fn listed(whitelist: &'a [String]) -> Self {
        Self {
            whitelist: Some(whitelist),
            no_ui_authorization: true,
        }
    }

    /// 该工具是否被名单放行(空名单 = 全量放行)
    fn authorizes(&self, name: &str) -> bool {
        self.whitelist
            .is_some_and(|wl| wl.is_empty() || wl.iter().any(|n| n == name))
    }

    /// 该工具是否被闸门硬性排除。仅对「有名单 + 不等待授权」的路径成立(任务模式):
    /// 名单是能力的硬边界,不在名单内的工具必须拒绝——否则文件规则可能因「宽松模式
    /// 写文件放行」而放过被任务策略排除的危险工具(模型幻觉调用即越权)。
    /// 聊天路径(no_ui_authorization=false)不硬性排除:名单外工具走授权等待,与改造前一致。
    fn excludes(&self, name: &str) -> bool {
        self.no_ui_authorization && self.whitelist.is_some() && !self.authorizes(name)
    }
}

/// 跳过状态/工具调用落库;docs/任务引擎六模式.md 第三节);聊天路径恒 Some,行为不变。
/// pub(crate):任务引擎 solo/custom 模式直调(批次 4.2 起)。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_tool_loop(
    engine: &AgentEngine,
    state_machine: &mut StateMachine,
    agent_session: Option<&AgentSessionRecord>,
    session_id: &str,
    llm_messages: &mut Vec<LlmMessage>,
    params: &GenerationParams,
    tool_ctx: &ToolContext,
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
    total_usage: &mut TokenUsage,
    run_id: &str,
    // 授权闸门(替代 step_whitelist):见 ToolGate 文档
    gate: ToolGate<'_>,
) -> Result<ExecutorResult, String> {
    // 轮次上限:缺省 32(settings 可调);至少 1 轮,防止配置异常导致死循环
    let max_rounds = params.max_tool_rounds.unwrap_or(32).max(1) as usize;
    let mut round = 0usize;
    // 截断自愈留痕(问题①):各轮触发的自愈记录,随最终 ExecutorResult 透出;
    // Err 传播(?)时丢弃——失败路径由调用方落 error 行,截断细节含在错误文案内
    let mut self_heals: Vec<SelfHealRecord> = Vec::new();
    loop {
        // ===== 工具历史回灌上限(R3b,2026-09-02 实测修复:每轮全量回灌
        // assistant/tool 交替历史,3 步任务 prompt 3,119→26,679 token 无界膨胀)=====
        // 每轮生成前:最老轮 tool 结果原地摘要化(保留最近 K 轮完整 + token 预算
        // 双闸门;配对不破坏、幂等)。仅存在工具循环历史时触发;聊天主链路
        // compaction(compaction.rs)/protected_tail 注入语义不受影响(不同层)。
        if llm_messages.iter().any(|m| m.role == "tool") {
            let (keep_rounds, budget_tokens) = {
                // 设置快照:不留锁跨 await
                let s = engine.settings_snapshot();
                (
                    s.tool_history_keep_rounds as usize,
                    s.tool_history_budget_tokens,
                )
            };
            let summarized_count = |msgs: &[LlmMessage]| {
                msgs.iter()
                    .filter(|m| m.content.starts_with(TOOL_HISTORY_SUMMARY_PREFIX))
                    .count()
            };
            let before = summarized_count(llm_messages);
            let model = engine.model();
            {
                let mut ts = engine
                    .token_service
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                trim_tool_history(llm_messages, keep_rounds, budget_tokens, &mut ts, &model);
            }
            let after = summarized_count(llm_messages);
            if after > before {
                tracing::info!(
                    session_id = session_id.to_string(),
                    newly_summarized = after - before,
                    keep_rounds = keep_rounds,
                    budget_tokens = budget_tokens,
                    "工具循环历史回灌截断(旧轮 tool 结果已摘要化)"
                );
            }
        }
        // ===== 截断自愈(问题①,2026-08-31 deepseek 实测:max_tokens 被推理/长参数
        // 烧光,tool_call 参数 JSON 被切成半截直接判步骤 error)=====
        // 单轮内最多自愈一次:该轮被 max_tokens 截断到不可用时,把本轮 max_tokens 翻倍
        // (上限 TRUNCATION_HEAL_MAX_TOKENS_CAP)原样重发;重发仍失败(同形态再现)
        // 才透出原结果走既有错误路径。与 task_service 的空输出分级重试是同问题不同层:
        // 那边管无工具纯生成(plan/step/summary),这边管工具循环内的单轮。
        let mut attempt_params = params.clone();
        let mut healed: Option<SelfHealRecord> = None;
        let result = loop {
            let one = execute_generation(
                engine,
                session_id,
                run_id,
                llm_messages,
                &attempt_params,
                tx,
                abort,
                flag,
            )
            .await;
            // 已自愈过(重发仍失败)或预算已封顶:透出原结果,不再重发
            if healed.is_some() {
                break one;
            }
            let Some(next_budget) = doubled_heal_budget(attempt_params.max_tokens) else {
                break one;
            };
            match one {
                Ok(res) => {
                    let Some(cause) = truncation_heal_cause(&res) else {
                        break Ok(res);
                    };
                    // 中断轮不自愈(用户停止语义优先;interrupted 时 finish_reason
                    // 也可能残留 length,不得误判)
                    if res.interrupted {
                        break Ok(res);
                    }
                    tracing::info!(
                        session_id = session_id.to_string(),
                        cause = cause.clone(),
                        max_tokens = attempt_params.max_tokens,
                        retry_max_tokens = next_budget,
                        "工具循环单轮截断,提高输出上限原样重发(截断自愈)"
                    );
                    let _ = send_event(
                        step_evt(
                            "截断自愈",
                            Some(format!(
                                "{cause},输出上限 {}→{next_budget} 重发",
                                attempt_params.max_tokens
                            )),
                            None,
                            None,
                        ),
                        tx,
                        abort,
                        flag,
                    )
                    .await;
                    healed = Some(SelfHealRecord {
                        note: cause,
                        prompt_tokens: res.usage.prompt_tokens,
                        completion_tokens: res.usage.completion_tokens,
                        finish_reason: res.finish_reason.clone(),
                        retried_max_tokens: next_budget,
                    });
                    attempt_params.max_tokens = next_budget;
                    continue;
                }
                Err(e) => {
                    if !is_truncated_tool_call_error(&e) {
                        break Err(e);
                    }
                    tracing::info!(
                        session_id = session_id.to_string(),
                        max_tokens = attempt_params.max_tokens,
                        retry_max_tokens = next_budget,
                        "工具调用参数 JSON 截断,提高输出上限原样重发(截断自愈)"
                    );
                    let _ = send_event(
                        step_evt(
                            "截断自愈",
                            Some(format!(
                                "工具参数 JSON 截断,输出上限 {}→{next_budget} 重发",
                                attempt_params.max_tokens
                            )),
                            None,
                            None,
                        ),
                        tx,
                        abort,
                        flag,
                    )
                    .await;
                    healed = Some(SelfHealRecord {
                        note: "工具参数 JSON 截断".into(),
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        finish_reason: None,
                        retried_max_tokens: next_budget,
                    });
                    attempt_params.max_tokens = next_budget;
                    continue;
                }
            }
        };
        // Err 传播前包装截断错误文案(问题③):自愈重发仍失败时,错误带「截断」定性,
        // 任务模式据此把可读的失败原因写进步骤 result(而非连接器原始半截 JSON)
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                if let Some(h) = healed {
                    self_heals.push(h);
                    return Err(format!(
                        "工具参数 JSON 截断(已达 token 上限,提高预算重发仍失败): {e}"
                    ));
                }
                return Err(e);
            }
        };
        if let Some(h) = healed {
            self_heals.push(h);
        }
        total_usage.prompt_tokens += result.usage.prompt_tokens;
        total_usage.completion_tokens += result.usage.completion_tokens;
        total_usage.total_tokens += result.usage.total_tokens;
        total_usage.prompt_cache_hit_tokens += result.usage.prompt_cache_hit_tokens;
        total_usage.prompt_cache_miss_tokens += result.usage.prompt_cache_miss_tokens;
        if result.interrupted {
            return Ok(ExecutorResult {
                self_heals: std::mem::take(&mut self_heals),
                ..result
            });
        }
        if result.tool_calls.is_empty() {
            return Ok(ExecutorResult {
                content: result.content,
                usage: TokenUsage::default(),
                interrupted: false,
                tool_calls: Vec::new(),
                reasoning: String::new(),
                // 末轮(无工具调用)的 finish_reason 透出:任务模式落库截断标记用
                finish_reason: result.finish_reason,
                self_heals: std::mem::take(&mut self_heals),
            });
        }
        round += 1;
        // 本轮是否已达上限:是则执行完本轮工具后停止,不再发起新的模型请求
        let last_round = round >= max_rounds;
        // 回填 OpenAI 标准结构:先追加一条 assistant 消息,携带本轮完整 tool_calls[] 与
        // reasoning_content,再逐条追加 tool 结果消息。旧实现为每个 call 单独追加一条
        // assistant(tool_calls=[call]),违反 OpenAI 多工具调用格式,并行工具调用时可能被
        // 严格后端拒绝(400)或丢失 reasoning_content 对应关系。
        let reasoning = (!result.reasoning.is_empty()).then_some(result.reasoning.clone());
        llm_messages.push(LlmMessage {
            role: "assistant".into(),
            content: String::new(),
            reasoning_content: reasoning,
            tool_calls: Some(result.tool_calls.clone()),
            tool_call_id: None,
        });
        // ===== 本轮工具执行 =====
        // 1) 裁决(顺序,无副作用):白名单工具自动放行,其余走三档授权模式裁决
        // (任务模式无 UI 授权上下文时,未放行工具在下方执行阶段直接拒绝,不空等)
        let mut executed: Vec<ExecutedTool> = result
            .tool_calls
            .iter()
            .map(|call| {
                let registered = engine.tool_registry.get(&call.name).is_some();
                let custom_authorized = gate.authorizes(&call.name);
                let (mode, always_required) = {
                    // 设置快照:不留锁跨 await
                    let settings = engine.settings_snapshot();
                    (
                        settings.authorization_mode,
                        settings
                            .bypass_blacklist
                            .iter()
                            .any(|name| name == &call.name),
                    )
                };
                // 操作分类:读/写/删文件与系统路径区域,决定三档矩阵的走向
                let origin = engine
                    .tool_registry
                    .origin_of(&call.name)
                    .unwrap_or(crate::tools::action_class::ToolOrigin::Builtin);
                let action = crate::tools::action_class::classify(
                    &call.name,
                    &call.arguments,
                    origin,
                );
                let permission = if gate.excludes(&call.name) {
                    // 任务模式名单是硬边界:名单外工具直接拒绝,不进入三档文件规则
                    // (否则宽松模式会放过被任务策略排除的写类工具)
                    crate::tools::permissions::PermissionDecision {
                        allowed: false,
                        risk: engine
                            .tool_registry
                            .permissions()
                            .risk_for(&call.name),
                        reason: "该工具不在当前任务策略允许的工具清单内".into(),
                    }
                } else {
                    engine.tool_registry.permissions().decide_with_policy(
                        &call.name,
                        tool_ctx,
                        registered,
                        custom_authorized,
                        mode,
                        &action,
                        always_required,
                    )
                };
                tracing::info!(
                    tool = call.name.clone(),
                    risk = format!("{:?}", permission.risk).to_lowercase(),
                    allowed = permission.allowed,
                    result = permission.reason.clone(),
                    "tool_permission"
                );
                ExecutedTool {
                    call: call.clone(),
                    permission,
                    output: Value::Null,
                    duration_ms: 0,
                }
            })
            .collect();
        // 2) 执行:按模型顺序分组调度——连续 Safe 工具组内并发(join_all 保序),
        //    非 Safe/未注册工具单独成组、串行执行(含授权等待),组间串行,
        //    保证授权语义与副作用顺序稳定。
        let parallel_safe: Vec<bool> = executed
            .iter()
            .map(|e| {
                engine.tool_registry.get(&e.call.name).is_some()
                    && engine.tool_registry.permissions().risk_for(&e.call.name)
                        == crate::tools::permissions::ToolRisk::Safe
            })
            .collect();
        let groups = split_parallel_groups(&parallel_safe);
        for group in groups {
            if group.len() > 1 || parallel_safe[group[0]] {
                // 并发组:组内全部为 Safe 工具,并发执行并按原顺序收集
                let futures: Vec<_> = group
                    .iter()
                    .map(|&idx| {
                        let e = &executed[idx];
                        let call = &e.call;
                        let perm = &e.permission;
                        let ctx = tool_ctx.clone();
                        async move { execute_call(engine, call, perm, &ctx, tx, abort, flag).await }
                    })
                    .collect();
                let outputs = futures::future::join_all(futures).await;
                for (&idx, out) in group.iter().zip(outputs) {
                    executed[idx].output = out.0;
                    executed[idx].duration_ms = out.1;
                }
            } else {
                // 串行组:单个非 Safe/未注册工具,含授权等待
                let interrupted = execute_serial_tool(
                    engine,
                    &mut executed[group[0]],
                    tool_ctx,
                    tx,
                    abort,
                    flag,
                    run_id,
                    gate,
                )
                .await?;
                if interrupted {
                    return Ok(ExecutorResult {
                        content: String::new(),
                        usage: TokenUsage::default(),
                        interrupted: true,
                        tool_calls: Vec::new(),
                        reasoning: String::new(),
                        // 工具执行期中断:生成未完成,finish_reason 不适用
                        finish_reason: None,
                        self_heals: std::mem::take(&mut self_heals),
                    });
                }
            }
        }
        // 3) 收尾(按原调用顺序):状态机/持久化/SSE 推送/模型消息回填。
        //    ToolResult 与 ToolAuthorizationRequired 均在此按序推送,保证前端配对稳定。
        for e in &executed {
            let _ = state_machine.transition(AgentState::ToolCall, session_id);
            // 任务模式 agent_session=None(不建影子会话行):跳过 agent_sessions 落库;
            // 聊天路径恒 Some,落库顺序与原实现逐字节一致。
            if let Some(agent_session) = agent_session {
                let _ = engine.agent_sessions.update(
                    &agent_session.id,
                    Some("tool_call"),
                    None,
                    None,
                    None,
                );
                let input_val: Value = serde_json::from_str(&e.call.arguments)
                    .unwrap_or_else(|_| Value::String(e.call.arguments.clone()));
                let _ = engine.agent_sessions.add_tool_call(
                    &agent_session.id,
                    &e.call.name,
                    input_val,
                    e.output.clone(),
                    e.duration_ms,
                );
            }
            let _ = state_machine.transition(AgentState::Executing, session_id);
            if let Some(agent_session) = agent_session {
                let _ = engine.agent_sessions.update(
                    &agent_session.id,
                    Some("executing"),
                    None,
                    None,
                    None,
                );
            }
            // 授权已在执行前等待;此处统一发送最终结果,允许与拒绝都按 call_id 收束状态。
            send_event(
                SseEvent::ToolResult {
                    name: e.call.name.clone(),
                    output: e.output.clone(),
                    call_id: Some(e.call.id.clone()),
                    render_kind: crate::tools::registry::render_kind_for(&e.call.name)
                        .map(|s| s.to_string()),
                },
                tx,
                abort,
                flag,
            )
            .await?;
            // tool 结果消息:与循环前的 assistant(tool_calls) 成对,供下一轮生成参考
            llm_messages.push(LlmMessage {
                role: "tool".into(),
                content: e.output.to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some(e.call.id.clone()),
            });
        }
        // 已达轮次上限:本轮工具已全部执行(含终态推送),停止发起新的模型请求并输出当前结果
        if last_round {
            send_event(
                step_evt(
                    "达到工具调用轮次上限",
                    Some(format!(
                        "已执行 {round} 轮工具调用,停止继续调用工具,输出当前结果"
                    )),
                    None,
                    None,
                ),
                tx,
                abort,
                flag,
            )
            .await?;
            return Ok(ExecutorResult {
                content: result.content,
                usage: TokenUsage::default(),
                interrupted: false,
                tool_calls: Vec::new(),
                reasoning: String::new(),
                finish_reason: result.finish_reason,
                self_heals: std::mem::take(&mut self_heals),
            });
        }
    }
}

/// 单轮中一个待执行/已执行的工具调用(裁决结果 + 执行输出 + 耗时)
struct ExecutedTool {
    call: ToolCallArgs,
    permission: crate::tools::permissions::PermissionDecision,
    output: Value,
    /// 工具执行耗时(毫秒;未执行时为 0)
    duration_ms: i64,
}

/// 按模型顺序把工具分成「并行组」:连续 Safe 工具为同一组(组内并发),
/// 非 Safe/未注册工具各自成组(串行屏障)。返回每组在 executed 里的下标集合。
fn split_parallel_groups(parallel_safe: &[bool]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (idx, &safe) in parallel_safe.iter().enumerate() {
        if safe {
            // Safe 且上一组也是 Safe 组 → 并入,否则新开一组
            if let Some(last) = groups.last_mut() {
                if parallel_safe[last[0]] {
                    last.push(idx);
                    continue;
                }
            }
            groups.push(vec![idx]);
        } else {
            // 非 Safe → 单独成组(屏障)
            groups.push(vec![idx]);
        }
    }
    groups
}

/// 串行执行单个工具调用(含未授权时的等待授权)。返回是否被中止。
/// 与并发组不同,非 Safe/未注册工具必须走此路径,保证授权语义与副作用顺序稳定。
/// `gate.no_ui_authorization = true`(任务模式)时,未放行工具直接拒绝并回灌错误,
/// 不进入 300 秒授权等待——任务模式没有 UI 授权上下文,等待必然超时。
#[allow(clippy::too_many_arguments)]
async fn execute_serial_tool(
    engine: &AgentEngine,
    e: &mut ExecutedTool,
    tool_ctx: &ToolContext,
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
    run_id: &str,
    gate: ToolGate<'_>,
) -> Result<bool, String> {
    if *abort.borrow() {
        return Ok(true);
    }
    if !e.permission.allowed && engine.tool_registry.get(&e.call.name).is_some() {
        // 任务模式(无 UI 授权上下文):立即拒绝,不空等
        if gate.no_ui_authorization {
            e.output = json!({
                "error": e.permission.reason,
                "code": "tool_policy_denied"
            });
            return Ok(false);
        }
        let receiver = engine.tool_registry.permissions().begin_wait(
            run_id,
            &e.call.id,
            &tool_ctx.session_id,
            &e.call.name,
        );
        send_event(
            SseEvent::ToolAuthorizationRequired {
                name: e.call.name.clone(),
                risk: e.permission.risk,
                reason: e.permission.reason.clone(),
                run_id: run_id.to_string(),
                call_id: e.call.id.clone(),
            },
            tx,
            abort,
            flag,
        )
        .await?;
        let mut abort_wait = abort.clone();
        let timeout_secs = {
            let s = engine.settings_snapshot();
            s.tool_authorization_timeout_secs as u64
        };
        let resolved = tokio::select! {
            decision = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), receiver) => {
                match decision {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(_)) => Err(("authorization_disconnected", "授权通道已断开")),
                    Err(_) => Err(("authorization_timeout", "等待授权超时")),
                }
            }
            changed = abort_wait.changed() => {
                let _ = changed;
                Err(("authorization_disconnected", "生成连接已断开"))
            }
        };
        engine
            .tool_registry
            .permissions()
            .cancel_wait(run_id, &e.call.id);
        match resolved {
            Ok(crate::tools::permissions::PendingAuthorizationDecision::AllowOnce) => {
                e.permission = crate::tools::permissions::PermissionDecision::allowed(
                    e.permission.risk,
                    "用户允许本次调用".into(),
                );
            }
            Ok(crate::tools::permissions::PendingAuthorizationDecision::AllowSession) => {
                // 授权落盘失败(如匿名会话/磁盘异常)不应中断整轮生成:降级为「仅本次允许」
                // 并留痕,否则用户看到的是生成失败而非授权失败。
                match engine.tool_registry.permissions().authorize(
                    &e.call.name,
                    "session",
                    &tool_ctx.session_id,
                ) {
                    Ok(()) => {
                        e.permission = crate::tools::permissions::PermissionDecision::allowed(
                            e.permission.risk,
                            "用户授权当前会话".into(),
                        );
                    }
                    Err(err) => {
                        tracing::warn!(tool = e.call.name, error = %err, "会话授权落盘失败,降级为仅本次允许");
                        e.permission = crate::tools::permissions::PermissionDecision::allowed(
                            e.permission.risk,
                            format!("用户允许本次调用(会话授权未保存:{err})"),
                        );
                    }
                }
            }
            Ok(crate::tools::permissions::PendingAuthorizationDecision::AllowRole) => {
                match engine.tool_registry.permissions().authorize(
                    &e.call.name,
                    "role",
                    &tool_ctx.character_id,
                ) {
                    Ok(()) => {
                        e.permission = crate::tools::permissions::PermissionDecision::allowed(
                            e.permission.risk,
                            "用户授权当前角色".into(),
                        );
                    }
                    Err(err) => {
                        tracing::warn!(tool = e.call.name, error = %err, "角色授权落盘失败,降级为仅本次允许");
                        e.permission = crate::tools::permissions::PermissionDecision::allowed(
                            e.permission.risk,
                            format!("用户允许本次调用(角色授权未保存:{err})"),
                        );
                    }
                }
            }
            Ok(crate::tools::permissions::PendingAuthorizationDecision::Deny) => {
                e.output =
                    json!({ "error": "用户拒绝工具调用", "code": "tool_authorization_denied" });
                emit_authorization_outcome(tx, abort, flag, &e.call.name, "已拒绝").await;
                return Ok(false);
            }
            Err((code, message)) => {
                e.output = json!({ "error": message, "code": code });
                emit_authorization_outcome(tx, abort, flag, &e.call.name, message).await;
                return Ok(false);
            }
        }
    }
    let (output, duration_ms) =
        execute_call(engine, &e.call, &e.permission, tool_ctx, tx, abort, flag).await;
    e.output = output;
    e.duration_ms = duration_ms;
    Ok(false)
}

/// 授权终态事件(拒绝/超时/断开):原实现只回灌 error JSON,前端工具卡片显示为
/// 普通失败,用户无法区分「授权被拒」与「工具报错」。发一条 step 事件补足可见性。
async fn emit_authorization_outcome(
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
    tool: &str,
    outcome: &str,
) {
    let _ = send_event(
        step_evt(
            &format!("工具 {tool} 未执行"),
            Some(format!("授权结果:{outcome}")),
            None,
            None,
        ),
        tx,
        abort,
        flag,
    )
    .await;
}

/// 执行单个工具调用(已裁决)。未授权不执行:未注册 → 错误 JSON;已注册 → 错误 JSON
/// (授权事件由收尾阶段按序推送)。执行失败 → 结构化错误 JSON + step 提示,不终止整轮。
/// 返回 (输出, 耗时毫秒)。
#[allow(clippy::too_many_arguments)]
async fn execute_call(
    engine: &AgentEngine,
    call: &ToolCallArgs,
    permission: &crate::tools::permissions::PermissionDecision,
    tool_ctx: &ToolContext,
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
) -> (Value, i64) {
    if !permission.allowed {
        return (
            if engine.tool_registry.get(&call.name).is_none() {
                json!({ "error": format!("工具未注册:{}", call.name), "code": "tool_not_registered" })
            } else {
                json!({ "error": permission.reason, "code": "tool_authorization_required" })
            },
            0,
        );
    }
    let start = std::time::Instant::now();
    // 白名单放行:已裁决执行,避免 execute 二次权限裁决拒绝白名单危险工具
    // (旧实现外层 allowed=true 但 execute 内部再裁决,敏感/危险工具实际仍被拒)
    let out = match engine
        .tool_registry
        .execute_with_decision(&call.name, &call.arguments, tool_ctx.clone(), permission)
        .await
    {
        Ok(r) => serde_json::from_str(&r).unwrap_or(Value::String(r)),
        Err(e) => {
            let _ = send_event(
                step_evt(
                    &format!("工具 {} 调用失败", call.name),
                    Some(e.clone()),
                    None,
                    None,
                ),
                tx,
                abort,
                flag,
            )
            .await;
            json!({ "error": e })
        }
    };
    (out, start.elapsed().as_millis() as i64)
}

// ===== 非流式静默生成(自 engine/mod.rs 拆分迁入,纯代码移动,逻辑不变)=====
impl AgentEngine {
    /// 阶段六 6g-1:非流式静默生成(复刻 generate_reflect_advice 模式)。
    /// 供后端脚本 TavernHelper.generate 与外部调用;不入聊天记录、不推 SSE。
    pub async fn generate_text(
        &self,
        messages: &[LlmMessage],
        params: GenerationParams,
        abort: watch::Receiver<bool>,
    ) -> Result<(String, TokenUsage), String> {
        if *abort.borrow() {
            return Err("生成已中断".into());
        }
        let connector = self.connector.read().await;
        let chunks = connector.generate(messages, params, abort).await?;
        drop(connector);
        let mut out = String::new();
        let mut usage = TokenUsage::default();
        for chunk in chunks {
            match chunk {
                LlmStreamChunk::Token(t) => out.push_str(&t),
                LlmStreamChunk::Usage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    prompt_cache_hit_tokens,
                    prompt_cache_miss_tokens,
                    ..
                } => {
                    usage.prompt_tokens += prompt_tokens;
                    usage.completion_tokens += completion_tokens;
                    usage.total_tokens += total_tokens;
                    usage.prompt_cache_hit_tokens += prompt_cache_hit_tokens;
                    usage.prompt_cache_miss_tokens += prompt_cache_miss_tokens;
                }
                _ => {}
            }
        }
        if out.trim().is_empty() {
            Err("生成返回空内容".into())
        } else {
            Ok((out, usage))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 任务模式名单是硬边界:名单外工具必须被闸门排除。
    /// 回归用例——曾因三档文件规则(宽松模式写文件放行)而放过被策略排除的 write。
    #[test]
    fn task_gate_hard_excludes_tools_outside_whitelist() {
        let allowed = vec!["read".to_string(), "search".to_string()];
        let gate = ToolGate::listed(&allowed);
        assert!(gate.excludes("write"), "名单外 write 必须被排除");
        assert!(!gate.excludes("read"), "名单内 read 不应被排除");
    }

    /// 聊天路径不硬性排除名单外工具:走授权等待(与改造前行为一致)
    #[test]
    fn chat_gate_does_not_hard_exclude() {
        let allowed = vec!["read".to_string()];
        let chat = ToolGate {
            whitelist: Some(&allowed),
            no_ui_authorization: false,
        };
        assert!(!chat.excludes("write"), "聊天路径名单外工具应走授权等待");
    }

    /// 非白名单模式(whitelist=None)不排除任何工具
    #[test]
    fn wait_gate_excludes_nothing() {
        assert!(!ToolGate::wait().excludes("write"));
    }

    /// 空名单 = 全量放行(任务策略 all),不排除任何工具
    #[test]
    fn empty_whitelist_authorizes_all() {
        let gate = ToolGate::listed(&[]);
        assert!(!gate.excludes("write"));
        assert!(gate.authorizes("write"));
    }

    #[test]
    fn split_parallel_groups_groups_safe_runs() {
        // [safe, safe, danger, safe] → [[0,1],[2],[3]]
        let flags = vec![true, true, false, true];
        assert_eq!(
            split_parallel_groups(&flags),
            vec![vec![0, 1], vec![2], vec![3]]
        );
    }

    #[test]
    fn split_parallel_groups_all_safe_single_group() {
        let flags = vec![true, true, true];
        assert_eq!(split_parallel_groups(&flags), vec![vec![0, 1, 2]]);
    }

    #[test]
    fn split_parallel_groups_no_safe_each_solo() {
        let flags = vec![false, false];
        assert_eq!(split_parallel_groups(&flags), vec![vec![0], vec![1]]);
    }

    #[test]
    fn split_parallel_groups_empty() {
        assert!(split_parallel_groups(&[]).is_empty());
    }

    // ===== 截断自愈(问题①)判定纯函数 =====

    fn mk_result(
        content: &str,
        finish: Option<&str>,
        tool_calls: Vec<(&str, &str)>,
    ) -> ExecutorResult {
        ExecutorResult {
            content: content.into(),
            usage: TokenUsage::default(),
            interrupted: false,
            tool_calls: tool_calls
                .into_iter()
                .map(|(name, arguments)| ToolCallArgs {
                    id: "c1".into(),
                    name: name.into(),
                    arguments: arguments.into(),
                })
                .collect(),
            reasoning: String::new(),
            finish_reason: finish.map(|s| s.into()),
            self_heals: Vec::new(),
        }
    }

    /// 半截 tool_call JSON + finish=length → 触发,成因指向工具参数截断
    #[test]
    fn heal_cause_detects_truncated_tool_call() {
        let r = mk_result(
            "",
            Some("length"),
            vec![("calculator", "{\"expression\": \"12*3")],
        );
        let cause = truncation_heal_cause(&r).expect("半截 tool_call 应触发自愈");
        assert!(cause.contains("工具参数 JSON 截断"), "{cause}");
        assert!(cause.contains("calculator"), "{cause}");
        // 正文非空但 tool_call 半截:同样触发(工具调用不可执行)
        let r2 = mk_result(
            "思考残余",
            Some("length"),
            vec![("agentgo", "{\"tasks\": [")],
        );
        assert!(truncation_heal_cause(&r2).is_some());
    }

    /// 空正文 + finish=length(推理烧光预算)→ 触发,成因为空内容
    #[test]
    fn heal_cause_detects_empty_content_length() {
        let r = mk_result("", Some("length"), vec![]);
        let cause = truncation_heal_cause(&r).unwrap();
        assert!(cause.contains("空内容"), "{cause}");
    }

    /// 不自愈的形态:普通文本截断(半截文本也是产出,既有语义)、正常收尾、
    /// 合法 tool_call、无 finish_reason
    #[test]
    fn heal_cause_ignores_healthy_results() {
        // 半截文本截断:不触发(维持「截断仍 done」语义)
        assert!(
            truncation_heal_cause(&mk_result("这段成果被截断,后半", Some("length"), vec![]))
                .is_none()
        );
        // 正常 stop:不触发
        assert!(truncation_heal_cause(&mk_result("", Some("stop"), vec![])).is_none());
        // length 但 tool_call 参数完整:正常执行,不触发
        assert!(truncation_heal_cause(&mk_result(
            "",
            Some("length"),
            vec![("calculator", "{\"expression\":\"1+1\"}")],
        ))
        .is_none());
        // 无 finish_reason:不触发
        assert!(truncation_heal_cause(&mk_result("", None, vec![])).is_none());
    }

    /// Err 形态判定:仅匹配连接器半截 tool_call flush 的专属文案
    #[test]
    fn heal_err_matches_connector_truncation_wording() {
        assert!(is_truncated_tool_call_error(
            "工具 \"agentgo\" 的 arguments 不是合法 JSON: {\"tasks\": ["
        ));
        assert!(!is_truncated_tool_call_error("上游连接超时"));
        assert!(!is_truncated_tool_call_error("生成已中断"));
    }

    /// 预算翻倍+封顶:1024→2048;4096→8192 封顶;8192 不再重发(None)
    #[test]
    fn heal_budget_doubles_with_cap() {
        assert_eq!(doubled_heal_budget(1024), Some(2048));
        assert_eq!(doubled_heal_budget(4096), Some(8192));
        assert_eq!(doubled_heal_budget(8192), None, "已封顶不重发");
        assert_eq!(doubled_heal_budget(u32::MAX), None, "饱和相乘不得溢出");
    }
}
