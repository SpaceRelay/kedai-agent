// 上下文裁剪:trim_to_context(按 token 预算丢弃旧消息、极端情况截断 system)
// 与受保护头部长度(protected_head_len,供裁剪保护首条 system 与摘要槽/记忆槽)
// (自 messages.rs 拆分迁入,纯代码移动,逻辑不变)
// R3b:trim_tool_history(工具循环历史回灌上限,keep-recent + 预算双闸门,
// 旧轮 tool 结果原地替换为短摘要,tool_calls/tool_call_id 配对不破坏)。
// 可见性说明:trim_to_context 原 pub(super)(= 对 engine 可见)改为
// pub(in crate::agents::engine),供 messages/mod.rs 以相同可见性再导出,范围不变。
use crate::models::types::LlmMessage;

/// R3b:已摘要 tool 消息的幂等标记前缀(替换内容以此开头;
/// 再次扫描时凭此前缀跳过,不重复摘要、不占 keep_rounds 名额)。
/// 集成测试(tasks.rs 工具循环回灌用例)按「已省略」子串断言,改动需同步。
pub(in crate::agents::engine) const TOOL_HISTORY_SUMMARY_PREFIX: &str = "(较早工具结果已省略:";

/// 按上下文窗口上限裁剪:始终保留 system(角色设定)与摘要槽(独立 system 消息,
/// 缓存感知管线·改造 A),从最旧的 user/assistant 起丢弃,直到总 token 不超过预算;
/// 极端情况下(仅剩 system 仍超)截断 system 内容,并优先保留尾部注入文本
/// (protected_tail 字符),避免注入先于角色设定被切掉。
pub(in crate::agents::engine) fn trim_to_context(
    messages: &mut Vec<LlmMessage>,
    max_context: Option<u32>,
    token_service: &mut crate::services::token_service::TokenService,
    model: &str,
    protected_tail: usize,
) {
    let Some(budget) = max_context else { return };
    if budget == 0 || messages.len() <= 1 {
        return;
    }
    // 受保护头部:首条 system + 摘要槽(存在时)——裁剪从其后开始
    let head = protected_head_len(messages);
    let mut total: i64 = token_service.count_message_tokens(messages, model);
    // 从最旧消息(head 起)丢弃,直到不超预算或仅剩受保护头部;idx 保持不变(remove 后自动前移)
    let idx = head;
    while total > budget as i64 && idx < messages.len() {
        let cost = token_service.count_tokens(&messages[idx].content, model) + 4;
        messages.remove(idx);
        total -= cost;
    }
    // 仍超预算(极长角色设定):按预算约 80% 截断 system,保留尾部注入块
    if total > budget as i64 && messages.len() == head && head > 0 {
        let sys = &mut messages[0];
        let text = sys.content.clone();
        let n: usize = ((budget as f64 * 0.8) as usize).max(200);
        let total_chars = text.chars().count();
        let keep_tail = protected_tail.min(total_chars);
        if keep_tail > 0 {
            // 核心运行时契约位于 system 开头，注入/步骤约束位于末尾；两端都必须保留。
            // 即使 protected_tail 大于估算字符预算，也至少保留一段头部契约，宁可少裁一点，
            // 不得生成“只剩不可信素材/尾部要求、系统权限规则消失”的提示词。
            let min_head = (n / 3)
                .clamp(80, 800)
                .min(total_chars.saturating_sub(keep_tail));
            let max_head = n.saturating_sub(keep_tail).max(min_head);
            let head: String = text.chars().take(max_head).collect();
            let tail: String = text.chars().skip(total_chars - keep_tail).collect();
            sys.content = format!("{head}\n\n[上下文裁剪：中间非核心内容已省略]\n\n{tail}");
            tracing::warn!(
                budget = budget,
                protected_chars = keep_tail,
                "上下文超限:系统提示词被截断(保留尾部注入块)"
            );
        } else {
            sys.content = text.chars().take(n).collect();
            tracing::warn!(budget = budget, "上下文超限:系统提示词被截断");
        }
    }
}

/// 消息数组中受裁剪保护的头部条数:首条 system(恒保护)+ 开头连续的
/// 槽位 system 消息(摘要槽/记忆槽,存在则保护)。
/// trim_to_context 从其后开始丢弃旧消息,摘要槽与记忆槽不会被裁掉。
pub(super) fn protected_head_len(messages: &[LlmMessage]) -> usize {
    let mut n = usize::from(!messages.is_empty());
    for m in messages.iter().skip(1) {
        if m.role == "system" {
            n += 1;
        } else {
            break;
        }
    }
    n
}

/// R3b:工具循环历史回灌上限(2026-09-02 实测修复:run_tool_loop 每轮全量回灌
/// assistant/tool 交替历史,3 步任务 prompt 3,119→26,679 token 无界膨胀)。
///
/// 机制(keep-recent + 预算双闸门,均在「轮」粒度操作):
/// - 轮分组:assistant(tool_calls 非空)起一轮,其后连续 role="tool" 消息归属该轮;
/// - keep-recent:完整轮数超过 keep_rounds 时,最老轮的 tool 结果原地替换为短摘要
///   (TOOL_HISTORY_SUMMARY_PREFIX 前缀,含工具名与原输出长度);
/// - 预算(budget_tokens > 0):估算总 token 超预算时继续摘要更老完整轮,直到达标;
///   0 = 禁用预算闸门(仅 keep-recent);
/// - 保底:最近 1 轮始终完整(模型必须看到最新工具结果才能续推)。
///
/// 不误伤纪律:只改 tool 消息内容与旧轮 assistant 的 reasoning_content;
/// system/首条 user/非工具循环消息不动;tool_calls 与 tool_call_id 全部保留
/// (OpenAI assistant(tool_calls)↔tool 配对不破坏,严格后端不报 400);
/// 已摘要消息凭前缀标记跳过(幂等,重复调用不再改动)。
/// 与聊天主链路 compaction(compaction.rs)不同层:本函数只作用于工具循环的
/// 消息数组,不动摘要槽/记忆槽/protected_tail 注入语义。
pub(in crate::agents::engine) fn trim_tool_history(
    messages: &mut [LlmMessage],
    keep_rounds: usize,
    budget_tokens: u32,
    token_service: &mut crate::services::token_service::TokenService,
    model: &str,
) {
    if messages.len() <= 2 {
        return;
    }
    // 1) 轮分组(按出现顺序):assistant 带非空 tool_calls = 一轮起点
    let mut rounds: Vec<(usize, Vec<usize>)> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        if m.role == "assistant" && m.tool_calls.as_ref().is_some_and(|cs| !cs.is_empty()) {
            rounds.push((i, Vec::new()));
        } else if m.role == "tool" {
            if let Some(last) = rounds.last_mut() {
                last.1.push(i);
            }
        }
    }
    if rounds.is_empty() {
        return;
    }
    // 2) 完整轮 = 尚有 tool 消息未带摘要前缀的轮(无 tool 消息的轮视为完整,
    //    摘要对它无意义,keep 计数时占位——实际 run_tool_loop 每轮恒有 tool 消息)
    let full: Vec<usize> = (0..rounds.len())
        .filter(|&i| {
            let (_, tools) = &rounds[i];
            tools.is_empty()
                || tools
                    .iter()
                    .any(|&t| !messages[t].content.starts_with(TOOL_HISTORY_SUMMARY_PREFIX))
        })
        .collect();
    if full.len() <= 1 {
        return;
    }
    // 3) keep-recent 闸门:从最老完整轮起摘要,直到完整轮数 <= keep_rounds
    //    (full 下标游标 cursor 指向下一轮待摘要;full 末位 = 最近完整轮,保底不摘)
    let keep = keep_rounds.max(1);
    let mut cursor = 0usize;
    let mut excess = full.len().saturating_sub(keep);
    while excess > 0 && cursor + 1 < full.len() {
        summarize_round(messages, &rounds[full[cursor]]);
        cursor += 1;
        excess -= 1;
    }
    // 4) 预算闸门(0 = 禁用):超预算继续摘要更老完整轮;保底最近 1 轮完整
    if budget_tokens > 0 {
        let mut total = token_service.count_message_tokens(messages, model);
        while total > budget_tokens as i64 && cursor + 1 < full.len() {
            summarize_round(messages, &rounds[full[cursor]]);
            cursor += 1;
            total = token_service.count_message_tokens(messages, model);
        }
    }
}

/// 摘要一轮:该轮每条未摘要的 tool 消息内容替换为短摘要(含工具名与原输出长度),
/// 该轮 assistant 的 reasoning_content 清空(推理对后续轮无用且占预算);
/// tool_calls/tool_call_id 不动(配对完整)。已摘要消息跳过(幂等)。
fn summarize_round(messages: &mut [LlmMessage], round: &(usize, Vec<usize>)) {
    let (a_idx, tool_idxs) = round;
    for &ti in tool_idxs {
        if messages[ti]
            .content
            .starts_with(TOOL_HISTORY_SUMMARY_PREFIX)
        {
            continue;
        }
        // 工具名:按 tool_call_id 从该轮 assistant 的 tool_calls 反查;查不到用 id 兜底
        let call_id = messages[ti].tool_call_id.clone().unwrap_or_default();
        let name = messages[*a_idx]
            .tool_calls
            .as_ref()
            .and_then(|cs| cs.iter().find(|c| c.id == call_id))
            .map(|c| c.name.clone())
            .unwrap_or_else(|| call_id.clone());
        let chars = messages[ti].content.chars().count();
        messages[ti].content =
            format!("{TOOL_HISTORY_SUMMARY_PREFIX}工具 \"{name}\" 原输出约 {chars} 字符)");
    }
    messages[*a_idx].reasoning_content = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::engine::messages::inject::{MEMORY_SLOT_MARKER, SUMMARY_SLOT_MARKER};

    /// trim_to_context:超预算时 system 截断必须保留尾部注入块(注入不能先于角色设定被切掉)
    #[test]
    fn trim_keeps_protected_inject_tail() {
        let mut ts = crate::services::token_service::TokenService::new();
        // 超长角色设定 + 尾部注入文本(简单模式「请将回复控制在 99 字以内。」)
        let long_desc = "长".repeat(2000);
        let inject_text = "\n\n请将回复控制在 99 字以内。".to_string();
        let sys_content = format!(
            "你是角色「测试」的扮演者与文学创作者,与用户进行沉浸式角色扮演 / 文学创作。\n\n角色设定:\n{long_desc}\n\n要求:以\"能否被称为一段好小说\"为最低验收标准。{inject_text}"
        );
        let mut messages = vec![
            LlmMessage::plain("system", &sys_content),
            LlmMessage::plain("user", "你好"),
        ];
        let protected_tail = inject_text.chars().count();
        // 极小预算 → 丢弃历史后仍超 → 截断 system,但尾部注入必须保留
        trim_to_context(
            &mut messages,
            Some(50),
            &mut ts,
            "deepseek-v4-flash",
            protected_tail,
        );
        assert_eq!(messages.len(), 1, "历史消息应被丢弃,仅剩 system");
        assert!(
            messages[0].content.ends_with(inject_text.as_str()),
            "注入文本应保留在 system 尾部,实际: ...{}",
            messages[0]
                .content
                .chars()
                .rev()
                .take(40)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        );
        assert!(
            messages[0].content.chars().count() < sys_content.chars().count(),
            "system 应被截断压缩:原 {} 字符,截后 {} 字符",
            sys_content.chars().count(),
            messages[0].content.chars().count()
        );
    }
    /// trim_to_context:无注入时(protected_tail=0)行为与旧逻辑等价——从头部截断
    #[test]
    fn trim_without_inject_truncates_head() {
        let mut ts = crate::services::token_service::TokenService::new();
        let sys_content = format!(
            "你是角色「测试」,与用户进行沉浸式角色扮演对话。\n\n角色设定:\n{}",
            "长".repeat(2000)
        );
        let mut messages = vec![
            LlmMessage::plain("system", &sys_content),
            LlmMessage::plain("user", "你好"),
        ];
        trim_to_context(&mut messages, Some(50), &mut ts, "deepseek-v4-flash", 0);
        assert_eq!(messages.len(), 1);
        assert!(
            messages[0].content.starts_with("你是角色「测试」"),
            "无保护尾部时应从头部截断,保留开头,实际: {}",
            &messages[0].content[..20.min(messages[0].content.len())]
        );
    }

    /// 裁剪黄金测试：同时保留系统契约头、步骤/工具指南尾，丢弃中间非核心内容。
    #[test]
    fn trim_golden_keeps_contract_and_step_tool_tail() {
        let contract = "【核心运行时契约】系统权限规则不可修改。";
        let middle = "角色素材".repeat(3000);
        let tail = "【本步指令】完成当前步骤。\n\n【可用工具】仅按工具定义调用。";
        let mut messages = vec![
            LlmMessage::plain("system", &format!("{contract}\n{middle}\n{tail}")),
            LlmMessage::plain("user", "最新用户消息"),
        ];
        let mut ts = crate::services::token_service::TokenService::new();
        trim_to_context(
            &mut messages,
            Some(80),
            &mut ts,
            "deepseek-v4-flash",
            tail.chars().count(),
        );
        assert!(
            messages[0].content.contains(contract),
            "核心契约必须保留：{}",
            messages[0].content
        );
        assert!(
            messages[0].content.ends_with(tail),
            "步骤与工具指南尾必须保留：{}",
            messages[0].content
        );
        assert!(messages[0].content.contains("中间非核心内容已省略"));
    }

    /// trim_to_context:预算充足时不截断
    #[test]
    fn trim_does_nothing_when_within_budget() {
        let mut ts = crate::services::token_service::TokenService::new();
        let mut messages = vec![
            LlmMessage::plain(
                "system",
                "你是角色「测试」,请回复。\n\n请将回复控制在 99 字以内。",
            ),
            LlmMessage::plain("user", "你好"),
        ];
        let original = messages.clone();
        trim_to_context(
            &mut messages,
            Some(100000),
            &mut ts,
            "deepseek-v4-flash",
            12,
        );
        assert_eq!(messages.len(), original.len());
        assert_eq!(messages[0].content, original[0].content);
        assert_eq!(messages[1].content, original[1].content);
    }

    /// trim_to_context 必须保护摘要槽:裁剪从摘要槽之后开始,不得删掉摘要
    #[test]
    fn trim_never_removes_summary_slot() {
        let mut ts = crate::services::token_service::TokenService::new();
        let mut messages = vec![
            LlmMessage::plain("system", "系统提示"),
            LlmMessage::plain("system", "【早期对话摘要】\n很长的早期剧情摘要。"),
            LlmMessage::plain("user", "较早消息"),
            LlmMessage::plain("assistant", "较早回复"),
            LlmMessage::plain("user", "最新消息"),
        ];
        let head = protected_head_len(&messages);
        assert_eq!(head, 2, "system + 摘要槽都应受保护");
        trim_to_context(&mut messages, Some(30), &mut ts, "deepseek-v4-flash", 0);
        assert!(
            messages.iter().any(|m| m.content.contains("早期剧情摘要")),
            "极小预算下摘要槽也不得被裁掉: {:?}",
            messages
                .iter()
                .map(|m| m.content.chars().take(20).collect::<String>())
                .collect::<Vec<_>>()
        );
    }

    /// trim_to_context 必须保护记忆槽:极小预算下摘要槽与记忆槽都不得被裁掉
    #[test]
    fn trim_never_removes_memory_slot() {
        let mut ts = crate::services::token_service::TokenService::new();
        let mut messages = vec![
            LlmMessage::plain("system", "系统提示"),
            LlmMessage::plain("system", "【早期对话摘要】\n早期剧情摘要。"),
            LlmMessage::plain("system", "【角色长期记忆】\n- 用户害怕打雷"),
            LlmMessage::plain("user", "较早消息"),
            LlmMessage::plain("user", "最新消息"),
        ];
        assert_eq!(
            protected_head_len(&messages),
            3,
            "system+摘要槽+记忆槽都应受保护"
        );
        trim_to_context(&mut messages, Some(30), &mut ts, "deepseek-v4-flash", 0);
        assert!(
            messages
                .iter()
                .any(|m| m.content.starts_with(MEMORY_SLOT_MARKER)),
            "极小预算下记忆槽也不得被裁掉: {:?}",
            messages
                .iter()
                .map(|m| m.content.chars().take(15).collect::<String>())
                .collect::<Vec<_>>()
        );
        assert!(
            messages
                .iter()
                .any(|m| m.content.starts_with(SUMMARY_SLOT_MARKER)),
            "摘要槽保护不受记忆槽影响"
        );
    }

    // ===== R3b:trim_tool_history(工具循环历史回灌上限)=====

    /// 一轮工具循环消息:assistant(带 tool_calls + reasoning)+ 一条 tool 结果
    fn tool_round(name: &str, call_id: &str, output: &str) -> Vec<LlmMessage> {
        vec![
            LlmMessage {
                role: "assistant".into(),
                content: String::new(),
                reasoning_content: Some(format!("第{call_id}轮推理")),
                tool_calls: Some(vec![crate::models::types::ToolCallArgs {
                    id: call_id.into(),
                    name: name.into(),
                    arguments: "{}".into(),
                }]),
                tool_call_id: None,
            },
            LlmMessage {
                role: "tool".into(),
                content: output.into(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some(call_id.into()),
            },
        ]
    }

    /// system + user + n 轮工具循环(每轮 tool 输出 output_len 字符)
    fn loop_messages(rounds: usize, output_len: usize) -> Vec<LlmMessage> {
        let mut msgs = vec![
            LlmMessage::plain("system", "系统提示"),
            LlmMessage::plain("user", "任务目标"),
        ];
        for i in 0..rounds {
            msgs.extend(tool_round(
                "read",
                &format!("call-{i}"),
                &format!("第{i}轮{}", "果".repeat(output_len)),
            ));
        }
        msgs
    }

    /// 第 idx 轮是否完整(tool 输出未被摘要)
    fn round_intact(messages: &[LlmMessage], idx: usize) -> bool {
        messages.iter().any(|m| {
            m.tool_call_id.as_deref() == Some(format!("call-{idx}").as_str())
                && m.role == "tool"
                && !m.content.starts_with(TOOL_HISTORY_SUMMARY_PREFIX)
        })
    }

    /// keep-recent:超出 keep_rounds 的最老轮被摘要,最近 K 轮完整;system/user 不动
    #[test]
    fn tool_history_keeps_recent_k_rounds() {
        let mut ts = crate::services::token_service::TokenService::new();
        let mut messages = loop_messages(4, 10);
        trim_tool_history(&mut messages, 2, 0, &mut ts, "deepseek-v4-flash");
        assert!(!round_intact(&messages, 0), "最老轮应被摘要: {messages:?}");
        assert!(!round_intact(&messages, 1), "次老轮应被摘要: {messages:?}");
        assert!(round_intact(&messages, 2), "倒数第二轮应完整保留");
        assert!(round_intact(&messages, 3), "最近一轮应完整保留");
        assert_eq!(messages[0].content, "系统提示", "system 不得被动");
        assert_eq!(messages[1].content, "任务目标", "user 目标不得被动");
        // 摘要文本可读:带工具名与原输出长度
        let summarized = messages
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call-0"))
            .unwrap();
        assert!(
            summarized.content.contains("read"),
            "摘要应含工具名: {}",
            summarized.content
        );
        assert!(
            summarized.content.contains("省略"),
            "摘要应含省略标记: {}",
            summarized.content
        );
    }

    /// 预算闸门:keep 之内仍超预算时,从最老轮继续摘要,但保底最近 1 轮完整
    #[test]
    fn tool_history_budget_summarizes_beyond_keep() {
        let mut ts = crate::services::token_service::TokenService::new();
        let mut messages = loop_messages(4, 2000);
        // keep=3 本来允许 3 轮完整;极小预算应继续摘要更老轮,只剩最近 1 轮
        trim_tool_history(&mut messages, 3, 60, &mut ts, "deepseek-v4-flash");
        assert!(!round_intact(&messages, 0));
        assert!(!round_intact(&messages, 1));
        assert!(
            !round_intact(&messages, 2),
            "预算超限时 keep 之内的轮也应被摘要"
        );
        assert!(round_intact(&messages, 3), "保底:最近一轮始终完整");
    }

    /// 幂等 + 配对完整:二次调用结果不变;tool_call_id/tool_calls 配对不破坏;
    /// 被摘要轮的 assistant reasoning_content 清空(推理内容对后续轮无用且占预算)
    #[test]
    fn tool_history_idempotent_and_preserves_pairing() {
        let mut ts = crate::services::token_service::TokenService::new();
        let mut messages = loop_messages(4, 10);
        trim_tool_history(&mut messages, 2, 0, &mut ts, "deepseek-v4-flash");
        let once = messages.clone();
        trim_tool_history(&mut messages, 2, 0, &mut ts, "deepseek-v4-flash");
        assert_eq!(
            serde_json::to_string(&messages).unwrap(),
            serde_json::to_string(&once).unwrap(),
            "二次调用不得再改动(幂等)"
        );
        // 配对:每条 tool 消息的 tool_call_id 都能在某条 assistant 的 tool_calls 里找到
        for m in &messages {
            if m.role == "tool" {
                let cid = m.tool_call_id.as_deref().unwrap();
                let paired = messages.iter().any(|a| {
                    a.role == "assistant"
                        && a.tool_calls
                            .as_ref()
                            .is_some_and(|cs| cs.iter().any(|c| c.id == cid))
                });
                assert!(paired, "tool 消息 {cid} 的 assistant 配对不得被破坏");
            }
        }
        // 被摘要轮的 assistant:tool_calls 保留(结构配对),reasoning 清空
        let a0 = messages
            .iter()
            .find(|a| {
                a.tool_calls
                    .as_ref()
                    .is_some_and(|cs| cs.iter().any(|c| c.id == "call-0"))
            })
            .unwrap();
        assert!(a0.tool_calls.is_some(), "被摘要轮的 tool_calls 不得删除");
        assert!(
            a0.reasoning_content.is_none(),
            "被摘要轮的 reasoning_content 应清空(省预算)"
        );
    }

    /// 关闭语义:轮数未超 keep 且预算禁用(0)时原样不动;无工具循环时无操作;
    /// 非工具循环消息(尾部 assistant 文本)不受摘要影响
    #[test]
    fn tool_history_disabled_or_within_keep_is_noop() {
        let mut ts = crate::services::token_service::TokenService::new();
        // 轮数 2 <= keep 4,预算 0 = 禁用 → 不动
        let mut messages = loop_messages(2, 5000);
        let original = messages.clone();
        trim_tool_history(&mut messages, 4, 0, &mut ts, "deepseek-v4-flash");
        assert_eq!(
            serde_json::to_string(&messages).unwrap(),
            serde_json::to_string(&original).unwrap(),
            "未超 keep 且预算禁用时不得改动"
        );
        // 无工具循环(纯 system+user)→ 无操作
        let mut plain = vec![
            LlmMessage::plain("system", "系统提示"),
            LlmMessage::plain("user", "目标"),
        ];
        trim_tool_history(&mut plain, 1, 1, &mut ts, "deepseek-v4-flash");
        assert_eq!(plain.len(), 2);
        assert_eq!(plain[1].content, "目标");
        // 尾部非工具 assistant 文本(模型最终回复途中)不受摘要影响
        let mut with_tail = loop_messages(4, 10);
        with_tail.push(LlmMessage::plain("assistant", "正在整理最终答案"));
        trim_tool_history(&mut with_tail, 2, 0, &mut ts, "deepseek-v4-flash");
        assert_eq!(
            with_tail.last().unwrap().content,
            "正在整理最终答案",
            "非工具循环的尾部 assistant 消息不得被动"
        );
    }
}
