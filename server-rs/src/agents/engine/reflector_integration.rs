// 反思集成:按配置的反思提示词调用 LLM 判定草稿质量(失败回退机械规则由主流程处理),
// 以及反思未通过放弃重试时的改进建议自动生成(≤200 token,由引擎调用而非用户手填)。
use super::*;

/// 反思失败建议的输出上限(模型自行约束 ≤200 token,采样再截一刀)
pub(super) const REFLECT_ADVICE_MAX_TOKENS: u32 = 200;

/// LLM 反思:按配置的反思提示词调用模型检查草稿质量。
/// 消息 = system(反思提示词)+ user(用户输入 + 剥离变量块后的草稿);
/// 低温度 0.3、输出上限 512,判定 token 计入本轮 total_usage。
/// 返回 (判定结果, usage);调用失败、中断或输出无法解析返回 None(调用方回退机械规则,
/// 保证反思路径在模型异常时仍可判定且重试计数有界)。
pub(super) async fn reflect_with_llm(
    engine: &AgentEngine,
    prompt: &str,
    user_input: &str,
    draft: &str,
    abort: &watch::Receiver<bool>,
) -> Option<(ReflectionResult, TokenUsage)> {
    if *abort.borrow() {
        return None;
    }
    let messages = vec![
        LlmMessage::plain("system", prompt),
        LlmMessage::plain(
            "user",
            &format!("[用户输入]\n{user_input}\n\n[模型草稿]\n{draft}"),
        ),
    ];
    let params = GenerationParams {
        temperature: 0.3,
        top_p: 1.0,
        max_tokens: 512,
        stop: None,
        tools: Vec::new(),
        max_tool_rounds: None,
        tool_choice: crate::models::types::ToolChoice::Auto,
        parallel_tool_calls: None,
    };
    let connector = engine.connector.read().await.clone();
    let chunks = connector
        .generate(&messages, params, abort.clone())
        .await
        .ok()?;
    drop(connector);
    let mut out = String::new();
    let mut usage = TokenUsage::default();
    for chunk in chunks {
        if *abort.borrow() {
            return None;
        }
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
    parse_reflect_verdict(&out).map(|v| (v, usage))
}

/// 反思工具循环(第四点·阶段 C):反思模型带文本修正工具(censor_text / revise_passage),
/// 发现禁词或不合理段落时自主调用工具定点修正,再给出 PASS/FAIL 判定。
/// 最多 3 轮工具循环;无工具调用即解析最终判定;工具修正后的正文经返回值回传。
/// 返回 (判定, 修正后的正文 Option, usage);任何异常/中断返回 None(调用方回退机械规则,
/// 保证反思路径永远可判定、有界)。
pub(super) async fn reflect_with_tools(
    engine: &AgentEngine,
    prompt: &str,
    user_input: &str,
    draft: &str,
    tool_ctx: &crate::models::types::ToolContext,
    abort: &watch::Receiver<bool>,
) -> Option<(ReflectionResult, Option<String>, TokenUsage)> {
    if *abort.borrow() {
        return None;
    }
    // 反思可用的工具:文本修正(禁词替换/定点修订)+ 只读检索(read,
    // 供反思时查世界书/资料佐证判定);仅取已注册的(缺失时退回无工具反思)。
    // 常量见 tools::tool_sets(REFLECT)。
    let mut names: Vec<&str> = crate::tools::tool_sets::REFLECT.to_vec();
    names.push("read");
    let tools: Vec<ToolDefinition> = names
        .iter()
        .filter_map(|name| engine.tool_registry.get(name).map(|t| t.definition))
        .collect();
    if tools.is_empty() {
        return reflect_with_llm(engine, prompt, user_input, draft, abort)
            .await
            .map(|(v, u)| (v, None, u));
    }

    let mut messages = vec![
        LlmMessage::plain(
            "system",
            &format!(
                "{prompt}\n\n若发现正文存在禁词、逻辑矛盾、表述不当或需要改写的段落,请先调用 \
                 censor_text(替换禁词)或 revise_passage(定点修改段落)修正正文,再输出 PASS 或 FAIL 判定。"
            ),
        ),
        LlmMessage::plain(
            "user",
            &format!("[用户输入]\n{user_input}\n\n[模型草稿]\n{draft}"),
        ),
    ];
    let mut revised: Option<String> = None;
    let mut total_usage = TokenUsage::default();

    for _round in 0..3 {
        if *abort.borrow() {
            return None;
        }
        let params = GenerationParams {
            temperature: 0.3,
            top_p: 1.0,
            max_tokens: 512,
            stop: None,
            tools: tools.clone(),
            max_tool_rounds: None,
            tool_choice: crate::models::types::ToolChoice::Auto,
            parallel_tool_calls: None,
        };
        let connector = engine.connector.read().await.clone();
        let chunks = connector
            .generate(&messages, params, abort.clone())
            .await
            .ok()?;
        drop(connector);

        let mut out = String::new();
        let mut tool_calls: Vec<ToolCallArgs> = Vec::new();
        for chunk in chunks {
            if *abort.borrow() {
                return None;
            }
            match chunk {
                LlmStreamChunk::Token(t) => out.push_str(&t),
                LlmStreamChunk::ToolCall(call) => {
                    if !call.id.is_empty() && !call.name.is_empty() {
                        tool_calls.push(call);
                    }
                }
                LlmStreamChunk::Usage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    prompt_cache_hit_tokens,
                    prompt_cache_miss_tokens,
                    ..
                } => {
                    total_usage.prompt_tokens += prompt_tokens;
                    total_usage.completion_tokens += completion_tokens;
                    total_usage.total_tokens += total_tokens;
                    total_usage.prompt_cache_hit_tokens += prompt_cache_hit_tokens;
                    total_usage.prompt_cache_miss_tokens += prompt_cache_miss_tokens;
                }
                _ => {}
            }
        }

        if tool_calls.is_empty() {
            // 无工具调用:最终判定文本
            return parse_reflect_verdict(&out).map(|v| (v, revised, total_usage));
        }

        // 有工具调用:回填 assistant(tool_calls) 与 tool 结果,继续下一轮
        messages.push(LlmMessage {
            role: "assistant".into(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
        });
        for call in &tool_calls {
            let output = engine
                .tool_registry
                .execute(&call.name, &call.arguments, tool_ctx.clone())
                .await
                .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"));
            // 只有文本修正类工具的输出才是修正后的正文(read 是检索类,输出是资料,
            // 混入会污染正文);修正类「最后一次调用胜出」。
            if crate::tools::tool_sets::REFLECT.contains(&call.name.as_str()) {
                revised = Some(output.clone());
            }
            messages.push(LlmMessage {
                role: "tool".into(),
                content: output,
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some(call.id.clone()),
            });
        }
    }
    // 3 轮工具循环仍未给出判定:回退 None(机械规则)
    None
}

/// 生成反思失败改进建议(自动注入,内容不由用户决定):
/// 引擎以反思判定的失败原因 + 用户输入 + 草稿摘要为上下文,调用模型产出针对性
/// 改进建议。输出上限 REFLECT_ADVICE_MAX_TOKENS(≤200 token),低温度保证稳定;
/// 硬截断到 300 字符兜底(长模型超限时也不注入过大建议)。
/// 返回 (建议文本, usage);调用失败、中断或输出为空返回 None(建议为可选增强,
/// 缺失时主流程仅使用可选的用户补充说明,不阻塞流程)。
pub(super) async fn generate_reflect_advice(
    engine: &AgentEngine,
    reason: &str,
    user_input: &str,
    draft: &str,
    abort: &watch::Receiver<bool>,
) -> Option<(String, TokenUsage)> {
    if *abort.borrow() {
        return None;
    }
    let messages = vec![
        LlmMessage::plain(
            "system",
            "你是写作质量改进顾问。依据反思反馈与草稿,给出一段简短、具体的改进建议\
             (不超过 200 token):指出问题所在并给出可操作的重写方向。\
             只输出建议正文,不要编号、不要复述原文、不要额外解释。",
        ),
        LlmMessage::plain(
            "user",
            &format!(
                "[反思反馈]\n{}\n\n[用户输入]\n{}\n\n[模型草稿(可能截断)]\n{}",
                reason,
                user_input,
                truncate_str(draft, 3000)
            ),
        ),
    ];
    let params = GenerationParams {
        temperature: 0.3,
        top_p: 1.0,
        max_tokens: REFLECT_ADVICE_MAX_TOKENS,
        stop: None,
        tools: Vec::new(),
        max_tool_rounds: None,
        tool_choice: crate::models::types::ToolChoice::Auto,
        parallel_tool_calls: None,
    };
    let connector = engine.connector.read().await.clone();
    let chunks = connector
        .generate(&messages, params, abort.clone())
        .await
        .ok()?;
    drop(connector);
    let mut out = String::new();
    let mut usage = TokenUsage::default();
    for chunk in chunks {
        if *abort.borrow() {
            return None;
        }
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
    let text = out.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some((truncate_str(&text, 300), usage))
    }
}

/// 按字符数截断(字符边界安全,不截断多字节字符)
fn truncate_str(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_chars).collect()
    }
}

// ===== 反思失败建议组装(自 engine/mod.rs 拆分迁入,纯代码移动,逻辑不变)=====
// 原为 engine/mod.rs 私有函数,此处为 pub(super)(= 对 engine 可见),范围一致。
/// 生成反思失败建议并拼为注入文本(位置0 内容,自动而非用户决定):
/// 调用 LLM 产出 ≤200 token 的针对性改进建议(失败/空则仅保留用户补充说明),
/// 可选的用户补充说明附加在其后;建议与补充均为空时返回 None(不注入)。
/// 生成产生的 usage 累加进 total_usage。注入边(user/assistant)由构建期
/// reflect_advice_role 决定,与本函数无关。
pub(super) async fn build_reflect_advice(
    engine: &AgentEngine,
    reason: &str,
    user_input: &str,
    draft: &str,
    supplement: &str,
    abort: &watch::Receiver<bool>,
    total_usage: &mut TokenUsage,
) -> Option<String> {
    let mut text = String::new();
    if let Some((advice, u)) =
        generate_reflect_advice(engine, reason, user_input, draft, abort).await
    {
        total_usage.prompt_tokens += u.prompt_tokens;
        total_usage.completion_tokens += u.completion_tokens;
        total_usage.total_tokens += u.total_tokens;
        total_usage.prompt_cache_hit_tokens += u.prompt_cache_hit_tokens;
        total_usage.prompt_cache_miss_tokens += u.prompt_cache_miss_tokens;
        text.push_str("[反思反馈]\n");
        text.push_str(advice.trim());
    }
    let supplement = supplement.trim();
    if !supplement.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n[补充要求]\n");
        }
        text.push_str(supplement);
    }
    (!text.is_empty()).then_some(text)
}
