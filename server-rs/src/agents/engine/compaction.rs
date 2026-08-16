// 上下文压缩(借鉴 deepseek-harness compaction):可逆投影 + LLM 摘要。
// 设计要点:
//   - 原文消息永不删除,摘要存 session_compactions 表;删摘要行即恢复完整历史(可逆)。
//   - 投影(读):存在摘要时,模型可见历史 = [摘要] + 摘要截止点之后的原文。
//   - 生成(写):mode=manual 且请求带 compact 标记,或 mode=auto 且历史 token 超阈值,
//     把「较早历史」压成一条摘要,保留最近 KEEP_RECENT_MESSAGES 条原文。
// 本模块只提供纯函数与提示词文本;LLM 调用与落库由 engine 完成(见 maybe_compact)。
use crate::models::types::MessageRecord;

/// 压缩后保留的最近消息条数(约等于最近 2 轮对话),避免摘要后模型丢失当前语境。
pub(super) const KEEP_RECENT_MESSAGES: usize = 4;

/// 投影结果:摘要(可空)+ 保留段的 (role, content) 历史视图。
/// 摘要为 None 表示无压缩,历史视图 = 完整历史(不含 system,交由消息构建层跳过)。
pub(super) struct ProjectedHistory {
    pub summary: Option<String>,
    pub tuples: Vec<(String, String)>,
}

/// 按已存在的压缩摘要投影历史:
///   - 无摘要:返回完整历史(role, content),行为与既有 history_tuples 一致。
///   - 有摘要(upto):返回 [摘要] + id > upto 的原文。
/// 摘要作为独立字段返回(不塞进 tuples),由消息构建层拼入 system,避免被历史循环跳过。
pub(super) fn project_history(
    history: &[MessageRecord],
    compaction: Option<(i64, String)>,
) -> ProjectedHistory {
    match compaction {
        None => ProjectedHistory {
            summary: None,
            tuples: history
                .iter()
                .map(|m| (m.role.clone(), m.content.clone()))
                .collect(),
        },
        Some((upto, summary)) => ProjectedHistory {
            summary: Some(summary),
            tuples: history
                .iter()
                .filter(|m| m.id > upto)
                .map(|m| (m.role.clone(), m.content.clone()))
                .collect(),
        },
    }
}

/// 计算压缩分界:返回「待压缩段」的消息条数(前 n 条进摘要,后 (len-n) 条保留原文)。
/// 历史条数不足以保留 KEEP_RECENT_MESSAGES 条时返回 None(无需压缩)。
pub(super) fn compaction_split(history: &[MessageRecord]) -> Option<usize> {
    let len = history.len();
    if len <= KEEP_RECENT_MESSAGES {
        return None;
    }
    Some(len - KEEP_RECENT_MESSAGES)
}

/// 待压缩段(前 to_compact 条)的截止消息 id:即这段最后一条消息的自增 id。
/// 摘要覆盖该 id(含)之前的全部历史;投影时保留 id > upto 的消息。
pub(super) fn upto_message_id(history: &[MessageRecord], to_compact: usize) -> Option<i64> {
    if to_compact == 0 || to_compact > history.len() {
        return None;
    }
    history.get(to_compact - 1).map(|m| m.id)
}

/// auto 模式触发判定:历史 token 数是否达到上下文窗口上限的 threshold 比例。
/// 纯函数,便于单测;max_context 为 0 或阈值越界时保守返回 false(不触发)。
pub(super) fn should_auto_compact(history_tokens: i64, max_context: u32, threshold: f32) -> bool {
    if max_context == 0 || !(0.5..=0.95).contains(&threshold) {
        return false;
    }
    history_tokens as f64 >= max_context as f64 * threshold as f64
}

/// 压缩摘要的系统指令:强调保留人物关系、关键事件、当前情境与未完成伏笔,
/// 要求输出平实可续写的摘要正文(无编号、无元评论)。
pub(super) fn compaction_system_prompt() -> &'static str {
    "你是对话历史的压缩助手。把下面的早期对话压缩成一段简洁、忠实、可续写的摘要,\
     用于替代原文注入给角色扮演模型。必须保留:人物关系与立场、已发生的关键事件与因果、\
     当前情境与未完成的伏笔/承诺、角色间的约定。用第三人称平实叙述,不要编号、\
     不要复述原文、不要添加评论或「摘要如下」等元文本。只输出摘要正文。"
}

/// 待压缩历史文本(供 LLM 摘要):按「角色: 内容」逐条拼接,role 映射为可读称呼。
pub(super) fn compaction_user_text(history_segment: &[MessageRecord]) -> String {
    history_segment
        .iter()
        .map(|m| {
            let speaker = match m.role.as_str() {
                "user" => "用户",
                "assistant" => "角色",
                other => other,
            };
            format!("{speaker}: {}", m.content)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(id: i64, role: &str, content: &str) -> MessageRecord {
        MessageRecord {
            id,
            session_id: "s".into(),
            role: role.into(),
            content: content.into(),
            extra: json!({}),
            created_at: String::new(),
        }
    }

    #[test]
    fn project_history_without_compaction_returns_full() {
        let history = vec![
            msg(1, "user", "你好"),
            msg(2, "assistant", "你好呀"),
            msg(3, "system", "系统消息"),
        ];
        let projected = project_history(&history, None);
        assert!(projected.summary.is_none());
        assert_eq!(projected.tuples.len(), 3);
        assert_eq!(projected.tuples[0], ("user".into(), "你好".into()));
    }

    #[test]
    fn project_history_with_compaction_keeps_only_recent() {
        let history = vec![
            msg(1, "user", "a"),
            msg(2, "assistant", "b"),
            msg(3, "user", "c"),
            msg(4, "assistant", "d"),
            msg(5, "user", "e"),
        ];
        // 摘要覆盖到 id=2(前 2 条压缩),保留 id>2 的 3 条
        let projected = project_history(&history, Some((2, "摘要".into())));
        assert_eq!(projected.summary.as_deref(), Some("摘要"));
        assert_eq!(projected.tuples.len(), 3);
        assert_eq!(projected.tuples[0], ("user".into(), "c".into()));
        assert!(projected.tuples.iter().all(|(_, c)| c != "a" && c != "b"));
    }

    #[test]
    fn compaction_split_keeps_recent_windows() {
        let history: Vec<MessageRecord> = (1..=6).map(|i| msg(i, "user", "x")).collect();
        // 6 条,保留 4 条 → 待压缩 2 条
        assert_eq!(compaction_split(&history), Some(2));

        let short: Vec<MessageRecord> = (1..=3).map(|i| msg(i, "user", "x")).collect();
        assert_eq!(compaction_split(&short), None);
    }

    #[test]
    fn upto_message_id_is_last_of_compacted_segment() {
        let history: Vec<MessageRecord> = (1..=6).map(|i| msg(i, "user", "x")).collect();
        // 压缩前 2 条 → 截止 id = 2
        assert_eq!(upto_message_id(&history, 2), Some(2));
        assert_eq!(upto_message_id(&history, 0), None);
        assert_eq!(upto_message_id(&history, 7), None);
    }

    #[test]
    fn should_auto_compact_respects_threshold() {
        // 8100/10000 = 0.81 > 0.8 触发;7900 < 0.8 不触发
        // (避开恰好等于阈值的边界,f32 精度下 0.8 略大于 0.8)
        assert!(should_auto_compact(8100, 10_000, 0.8));
        assert!(!should_auto_compact(7900, 10_000, 0.8));
        // 越界阈值保守不触发
        assert!(!should_auto_compact(9000, 10_000, 1.0));
        assert!(!should_auto_compact(9000, 0, 0.8));
    }

    #[test]
    fn compaction_user_text_maps_roles() {
        let history = vec![msg(1, "user", "你好"), msg(2, "assistant", "你好呀")];
        let text = compaction_user_text(&history);
        assert!(text.contains("用户: 你好"));
        assert!(text.contains("角色: 你好呀"));
    }
}
