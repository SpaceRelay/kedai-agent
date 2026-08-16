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
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_generation(
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
    {
        let log_enabled = engine
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .llm_request_log;
        if log_enabled {
            if let Ok(payload) = serde_json::to_string(messages) {
                let seq = LLM_REQUEST_SEQ.fetch_add(1, Ordering::Relaxed);
                if let Err(e) = engine.sessions.save_llm_request(
                    session_id,
                    run_id,
                    seq,
                    &payload,
                    &engine.model(),
                ) {
                    logger::warn(
                        "LLM 请求快照落库失败",
                        &[("error", Value::String(e))],
                    );
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
                    if process_chunk(chunk, &mut content, &mut reasoning, &mut tool_calls, &mut usage, tx, abort, flag).await? {
                        return Ok(ExecutorResult { content, usage, interrupted: true, tool_calls, reasoning });
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
            });
        }
        return Err(e);
    }

    Ok(ExecutorResult {
        content,
        usage,
        interrupted: *abort.borrow(),
        tool_calls,
        reasoning,
    })
}

#[allow(clippy::too_many_arguments)]
async fn process_chunk(
    chunk: LlmStreamChunk,
    content: &mut String,
    reasoning: &mut String,
    tool_calls: &mut Vec<ToolCallArgs>,
    usage: &mut TokenUsage,
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
                    render_kind: crate::tools::registry::render_kind_for(&call.name).map(|s| s.to_string()),
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
        } => {
            usage.prompt_tokens += prompt_tokens;
            usage.completion_tokens += completion_tokens;
            usage.total_tokens += total_tokens;
            usage.prompt_cache_hit_tokens += prompt_cache_hit_tokens;
        }
    }
    Ok(false)
}

/// 执行结果(executor 输出);content/usage/interrupted 由父模块 run() 主流程读取
pub(super) struct ExecutorResult {
    pub(super) content: String,
    pub(super) usage: TokenUsage,
    pub(super) interrupted: bool,
    /// 模型本轮请求的工具调用(AGENT 模式由 run_tool_loop 执行)
    tool_calls: Vec<ToolCallArgs>,
    /// 本轮思考模式推理内容(回传用)
    reasoning: String,
}

/// AGENT 模式工具循环:生成 → 有 tool_calls 则逐个执行并回填消息 → 重新生成,
/// 轮次上限默认 32(params.max_tool_rounds 可调,settings 页配置);无 tool_calls 时返回最终正文。
/// 每轮 usage 已累加进 total_usage。
/// SSE 语义:模型发出调用时由 execute_generation 推送 ToolCall,执行完成后此处推送 ToolResult。
/// 轮次语义(修复原 8 轮 off-by-one):第 N 轮(含 N=上限)生成的工具调用照常执行,
/// 只是执行完后停止再发起新的模型请求——工具调用不会被静默丢弃,卡片不会无终态悬挂。
/// 白名单模式:步骤配置了 tools 白名单时,名单内工具自动放行(不再弹授权框)。
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_tool_loop(
    engine: &AgentEngine,
    state_machine: &mut StateMachine,
    agent_session: &AgentSessionRecord,
    session_id: &str,
    llm_messages: &mut Vec<LlmMessage>,
    params: &GenerationParams,
    tool_ctx: &ToolContext,
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
    total_usage: &mut TokenUsage,
    run_id: &str,
    // 步骤白名单(custom 模式):名单内工具自动放行,None = 非白名单模式
    step_whitelist: Option<&[String]>,
) -> Result<ExecutorResult, String> {
    // 轮次上限:缺省 32(settings 可调);至少 1 轮,防止配置异常导致死循环
    let max_rounds = params.max_tool_rounds.unwrap_or(32).max(1) as usize;
    let mut round = 0usize;
    loop {
        let result = execute_generation(engine, session_id, run_id, llm_messages, params, tx, abort, flag).await?;
        total_usage.prompt_tokens += result.usage.prompt_tokens;
        total_usage.completion_tokens += result.usage.completion_tokens;
        total_usage.total_tokens += result.usage.total_tokens;
        total_usage.prompt_cache_hit_tokens += result.usage.prompt_cache_hit_tokens;
        if result.interrupted {
            return Ok(result);
        }
        if result.tool_calls.is_empty() {
            return Ok(ExecutorResult {
                content: result.content,
                usage: TokenUsage::default(),
                interrupted: false,
                tool_calls: Vec::new(),
                reasoning: String::new(),
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
        // 1) 裁决(顺序,无副作用):白名单工具自动放行,其余走权限裁决
        let mut executed: Vec<ExecutedTool> = result
            .tool_calls
            .iter()
            .map(|call| {
                let registered = engine.tool_registry.get(&call.name).is_some();
                let custom_authorized = step_whitelist.is_some_and(|whitelist| {
                    whitelist.is_empty() || whitelist.iter().any(|name| name == &call.name)
                });
                let (bypass_mode, bypass_blacklisted) = {
                    let settings = engine.settings.lock().unwrap_or_else(|e| e.into_inner());
                    (
                        settings.bypass_mode,
                        settings
                            .bypass_blacklist
                            .iter()
                            .any(|name| name == &call.name),
                    )
                };
                let permission = engine.tool_registry.permissions().decide_with_policy(
                    &call.name,
                    tool_ctx,
                    registered,
                    custom_authorized,
                    bypass_mode,
                    bypass_blacklisted,
                );
                crate::utils::logger::info(
                    "tool_permission",
                    &[
                        ("tool", Value::String(call.name.clone())),
                        (
                            "risk",
                            Value::String(format!("{:?}", permission.risk).to_lowercase()),
                        ),
                        ("allowed", Value::Bool(permission.allowed)),
                        ("result", Value::String(permission.reason.clone())),
                    ],
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
                )
                .await?;
                if interrupted {
                    return Ok(ExecutorResult {
                        content: String::new(),
                        usage: TokenUsage::default(),
                        interrupted: true,
                        tool_calls: Vec::new(),
                        reasoning: String::new(),
                    });
                }
            }
        }
        // 3) 收尾(按原调用顺序):状态机/持久化/SSE 推送/模型消息回填。
        //    ToolResult 与 ToolAuthorizationRequired 均在此按序推送,保证前端配对稳定。
        for e in &executed {
            let _ = state_machine.transition(AgentState::ToolCall, session_id);
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
            let _ = state_machine.transition(AgentState::Executing, session_id);
            let _ = engine.agent_sessions.update(
                &agent_session.id,
                Some("executing"),
                None,
                None,
                None,
            );
            // 授权已在执行前等待;此处统一发送最终结果,允许与拒绝都按 call_id 收束状态。
            send_event(
                SseEvent::ToolResult {
                    name: e.call.name.clone(),
                    output: e.output.clone(),
                    call_id: Some(e.call.id.clone()),
                    render_kind: crate::tools::registry::render_kind_for(&e.call.name).map(|s| s.to_string()),
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
async fn execute_serial_tool(
    engine: &AgentEngine,
    e: &mut ExecutedTool,
    tool_ctx: &ToolContext,
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
    run_id: &str,
) -> Result<bool, String> {
    if *abort.borrow() {
        return Ok(true);
    }
    if !e.permission.allowed && engine.tool_registry.get(&e.call.name).is_some() {
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
        let resolved = tokio::select! {
            decision = tokio::time::timeout(std::time::Duration::from_secs(300), receiver) => {
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
                engine.tool_registry.permissions().authorize(
                    &e.call.name,
                    "session",
                    &tool_ctx.session_id,
                )?;
                e.permission = crate::tools::permissions::PermissionDecision::allowed(
                    e.permission.risk,
                    "用户授权当前会话".into(),
                );
            }
            Ok(crate::tools::permissions::PendingAuthorizationDecision::AllowRole) => {
                engine.tool_registry.permissions().authorize(
                    &e.call.name,
                    "role",
                    &tool_ctx.character_id,
                )?;
                e.permission = crate::tools::permissions::PermissionDecision::allowed(
                    e.permission.risk,
                    "用户授权当前角色".into(),
                );
            }
            Ok(crate::tools::permissions::PendingAuthorizationDecision::Deny) => {
                e.output = json!({ "error": "用户拒绝工具调用", "code": "tool_authorization_denied" });
                return Ok(false);
            }
            Err((code, message)) => {
                e.output = json!({ "error": message, "code": code });
                return Ok(false);
            }
        }
    }
    let (output, duration_ms) = execute_call(engine, &e.call, &e.permission, tool_ctx, tx, abort, flag).await;
    e.output = output;
    e.duration_ms = duration_ms;
    Ok(false)
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
