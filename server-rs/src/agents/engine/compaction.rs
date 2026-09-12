// 上下文压缩(借鉴 deepseek-harness compaction):可逆投影 + LLM 摘要。
// 设计要点:
//   - 原文消息永不删除,摘要存 session_compactions 表;删摘要行即恢复完整历史(可逆)。
//   - 投影(读):存在摘要时,模型可见历史 = [摘要] + 摘要截止点之后的原文。
//   - 生成(写):mode=manual 且请求带 compact 标记,或 mode=auto 且历史 token 超阈值,
//     把「较早历史」压成一条摘要,保留最近 KEEP_RECENT_MESSAGES 条原文。
// 本模块上半为纯函数与提示词文本;引擎侧 LLM 调用与落库方法
// (clear_compaction/compact_session/maybe_compact)自 engine/mod.rs 拆分迁入文件尾 impl 块。
use super::*;
use crate::models::types::MessageRecord;

/// 压缩后保留的最近消息条数默认值(约等于最近 2 轮对话),避免摘要后模型丢失当前语境。
/// 可经设置项 compaction_keep_recent 覆盖(默认 4)。
pub(super) const DEFAULT_KEEP_RECENT_MESSAGES: usize = 4;

/// snip 档保留尾部原文条数(缓存感知管线:陈旧裁剪不碰最近上下文)
pub(super) const SNIP_KEEP_TAIL: usize = 2;

/// snip 档触发水位(先于 compact 档 0.8):历史 token 占上下文窗口比例达到即零成本裁剪
pub(super) const SNIP_THRESHOLD: f64 = 0.6;

/// 投影结果:摘要(可空)+ 保留段的 (role, content) 历史视图。
/// 摘要为 None 表示无压缩,历史视图 = 完整历史(不含 system,交由消息构建层跳过)。
pub(super) struct ProjectedHistory {
    pub summary: Option<String>,
    pub tuples: Vec<(String, String)>,
}

/// 按已存在的压缩摘要投影历史:
///   - 无摘要:返回完整历史(role, content),行为与既有 history_tuples 一致。
///   - 有摘要(upto):返回 [摘要] + id > upto 的原文。
///
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
/// keep_recent 为保留尾部的条数(设置项 compaction_keep_recent,默认 4)。
/// 历史条数不足以保留 keep_recent 条时返回 None(无需压缩)。
pub(super) fn compaction_split(history: &[MessageRecord], keep_recent: usize) -> Option<usize> {
    let len = history.len();
    if len <= keep_recent {
        return None;
    }
    Some(len - keep_recent)
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

/// 增量摘要段(缓存感知管线·改造 B):取 (upto_old, upto_new] 区间的消息。
/// upto_old 为 0 时等价于 [..upto_new](首次压缩兼容);upto_old >= upto_new
/// 返回空段(无新内容,调用方跳过压缩)。
pub(super) fn incremental_segment(
    history: &[MessageRecord],
    upto_old: i64,
    upto_new: i64,
) -> &[MessageRecord] {
    if upto_new <= upto_old {
        return &[];
    }
    let start = history
        .iter()
        .position(|m| m.id > upto_old)
        .unwrap_or(history.len());
    let end = history
        .iter()
        .position(|m| m.id > upto_new)
        .unwrap_or(history.len());
    &history[start..end.max(start)]
}

/// 增量摘要拼接:旧摘要原字节不动(前缀缓存稳定),增量以空行分隔追加在尾部。
/// 旧摘要为空(首次压缩)时仅返回增量;增量为空时旧摘要原样。
pub(super) fn merge_incremental_summary(old_summary: &str, increment: &str) -> String {
    let old = old_summary.trim();
    let inc = increment.trim();
    match (old.is_empty(), inc.is_empty()) {
        (true, _) => inc.to_string(),
        (_, true) => old.to_string(),
        (false, false) => format!("{old}\n\n{inc}"),
    }
}

/// snip 档触发判定(零成本裁剪,先于 LLM 摘要):历史 token 达到窗口 SNIP_THRESHOLD。
/// 纯函数;max_context 为 0(未配置)时保守返回 false。
pub(super) fn should_snip(history_tokens: i64, max_context: u32) -> bool {
    if max_context == 0 {
        return false;
    }
    history_tokens as f64 >= max_context as f64 * SNIP_THRESHOLD
}

/// snip 零成本裁剪(模型可见投影):尾部 SNIP_KEEP_TAIL 条原文保留,
/// 其余 content 字节数超过 max_bytes 的陈旧消息替换为占位符
/// 「[该消息已裁剪:N字符]」(N = 原字符数);含错误特征
/// (error/exception/failed/panic,大小写不敏感)的消息不裁(排障信息优先保留)。
/// 纯投影:返回新 Vec,不修改输入;数据库原文永不动(与可逆投影设计一致)。
pub(super) fn snip_tuples(tuples: &[(String, String)], max_bytes: usize) -> Vec<(String, String)> {
    let n = tuples.len();
    tuples
        .iter()
        .enumerate()
        .map(|(i, (role, content))| {
            let keep_original =
                i + SNIP_KEEP_TAIL >= n || content.len() <= max_bytes || looks_like_error(content);
            if keep_original {
                (role.clone(), content.clone())
            } else {
                (
                    role.clone(),
                    format!("[该消息已裁剪:{}字符]", content.chars().count()),
                )
            }
        })
        .collect()
}

/// 错误特征识别:content 命中任一关键词(大小写不敏感)即视为错误信息,snip 不裁
fn looks_like_error(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    ["error", "exception", "failed", "panic"]
        .iter()
        .any(|kw| lower.contains(kw))
}

// ===== 引擎侧压缩方法(自 engine/mod.rs 拆分迁入,纯代码移动,逻辑不变)=====
// maybe_compact 原为 engine/mod.rs 私有方法,此处为 pub(super)(= 对 engine 可见),
// 可见范围与拆分前一致;clear_compaction/compact_session 本就 pub,未变。
impl AgentEngine {
    /// 阶段 2「上下文收集」:装载字符卡/历史/世界书/变量树与设置快照,产出只读上下文
    /// 清除会话的压缩摘要(撤销压缩,恢复完整原文历史)。返回是否确有摘要被清除。
    pub fn clear_compaction(&self, session_id: &str) -> Result<bool, String> {
        let had = self.sessions.get_compaction(session_id).is_some();
        self.sessions.delete_compaction(session_id)?;
        Ok(had)
    }

    /// 手动压缩(独立端点 /api/chat/compact 调用):对会话较早历史做一次摘要压缩。
    /// 原文消息不删、摘要 upsert 到 session_compactions(可逆);返回是否实际压缩。
    /// mode=off 时拒绝;历史不足 KEEP_RECENT_MESSAGES 条时返回 Ok(false) 无需压缩。
    /// 增量摘要(缓存感知管线·改造 B):旧摘要冻结,只摘要上次截止点之后的新段,
    /// 新行 = 旧摘要(原字节)+ 增量拼接;旧行保留在表中以便回溯。
    pub async fn compact_session(&self, session_id: &str) -> Result<bool, String> {
        // 保留尾部条数:设置项 compaction_keep_recent(load 已钳制 2..=200),
        // 此处再兜底 >= 2,防止异常配置导致压缩后无上下文
        // (设置快照:不留锁跨 await)
        let (mode, keep_recent) = {
            let s = self.settings_snapshot();
            (
                s.compaction_mode.clone(),
                (s.compaction_keep_recent as usize).max(DEFAULT_KEEP_RECENT_MESSAGES.max(2)),
            )
        };
        if mode == "off" {
            return Err("上下文压缩模式为 off,请先在设置中改为 manual 或 auto".into());
        }
        // 同步 SQLite 读取挪进阻塞线程池(DB 并发改造),避免占用 tokio worker
        let (history, compaction) = {
            let sessions = self.sessions.clone();
            let sid = session_id.to_string();
            tokio::task::spawn_blocking(move || {
                (sessions.get_messages(&sid), sessions.get_compaction(&sid))
            })
            .await
            .map_err(|e| format!("读取会话历史失败: {e}"))?
        };
        let Some(to_compact) = compaction_split(&history, keep_recent) else {
            return Ok(false);
        };
        let Some(upto) = upto_message_id(&history, to_compact) else {
            return Ok(false);
        };
        // 增量边界:已有摘要时只压缩 (upto_old, upto] 新段;无新内容跳过
        let (upto_old, old_summary) = compaction.unwrap_or((0, String::new()));
        if upto_old >= upto {
            return Ok(false);
        }
        let segment = incremental_segment(&history, upto_old, upto);
        if segment.is_empty() {
            return Ok(false);
        }
        let (abort, abort_rx) = AbortFlag::new();
        let messages = vec![
            LlmMessage::plain("system", compaction_system_prompt()),
            LlmMessage::plain("user", &compaction_user_text(segment)),
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
        let (increment, _usage) = self.generate_text(&messages, params, abort_rx).await?;
        let increment = increment.trim().to_string();
        if increment.is_empty() {
            return Err("摘要生成返回空内容".into());
        }
        let merged = merge_incremental_summary(&old_summary, &increment);
        self.sessions
            .save_compaction(session_id, upto, &merged, &self.model())?;
        drop(abort);
        Ok(true)
    }

    /// auto 压缩决策(生成主流程内):mode=auto 且历史 token 达到阈值时触发摘要。
    /// mode=manual 由前端独立端点 /api/chat/compact 触发,不经本方法。
    /// 摘要经 generate_text 非流式生成,失败仅告警并跳过,不阻塞生成主流程。
    pub(super) async fn maybe_compact(
        &self,
        session_id: &str,
        tx: &mpsc::Sender<SseEvent>,
        abort: &watch::Receiver<bool>,
    ) {
        let (mode, threshold, max_context, keep_recent) = {
            // 设置快照:不留锁跨 await
            let s = self.settings_snapshot();
            (
                s.compaction_mode.clone(),
                s.compaction_threshold,
                s.max_context_tokens,
                (s.compaction_keep_recent as usize).max(DEFAULT_KEEP_RECENT_MESSAGES.max(2)),
            )
        };
        if mode != "auto" {
            return;
        }
        // 同步 SQLite 读取挪进阻塞线程池(DB 并发改造)
        let (history, compaction) = {
            let sessions = self.sessions.clone();
            let sid = session_id.to_string();
            match tokio::task::spawn_blocking(move || {
                (sessions.get_messages(&sid), sessions.get_compaction(&sid))
            })
            .await
            {
                Ok(v) => v,
                Err(_) => return,
            }
        };
        if history.is_empty() {
            return;
        }
        let tokens = {
            let mut ts = self.token_service.lock().unwrap_or_else(|e| e.into_inner());
            let mut total: i64 = 0;
            for m in &history {
                total += ts.count_tokens(&m.content, &self.model()) + 4;
            }
            total + 2
        };
        if !should_auto_compact(tokens, max_context, threshold) {
            return;
        }
        let Some(to_compact) = compaction_split(&history, keep_recent) else {
            return;
        };
        let Some(upto) = upto_message_id(&history, to_compact) else {
            return;
        };
        // 增量边界(改造 B):已有摘要时只压缩 (upto_old, upto] 新段,无新内容跳过
        let (upto_old, old_summary) = compaction.unwrap_or((0, String::new()));
        if upto_old >= upto {
            return;
        }
        let segment = incremental_segment(&history, upto_old, upto);
        if segment.is_empty() {
            return;
        }
        let _ = tx
            .send(step_evt(
                "压缩历史中…",
                Some("正在把较早对话压成摘要,原文仍保留可恢复".to_string()),
                None,
                None,
            ))
            .await;
        let messages = vec![
            LlmMessage::plain("system", compaction_system_prompt()),
            LlmMessage::plain("user", &compaction_user_text(segment)),
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
        match self.generate_text(&messages, params, abort.clone()).await {
            Ok((increment, _usage)) => {
                let increment = increment.trim().to_string();
                if !increment.is_empty() {
                    let merged = merge_incremental_summary(&old_summary, &increment);
                    if let Err(e) =
                        self.sessions
                            .save_compaction(session_id, upto, &merged, &self.model())
                    {
                        tracing::warn!(error = e, "压缩摘要落库失败");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = e, "压缩摘要生成失败,本轮跳过压缩");
            }
        }
    }
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
        // 6 条,保留 4 条(默认) → 待压缩 2 条
        assert_eq!(
            compaction_split(&history, DEFAULT_KEEP_RECENT_MESSAGES),
            Some(2)
        );

        let short: Vec<MessageRecord> = (1..=3).map(|i| msg(i, "user", "x")).collect();
        assert_eq!(compaction_split(&short, DEFAULT_KEEP_RECENT_MESSAGES), None);
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

    // ===== 增量摘要(缓存感知管线·改造 B) =====
    // 旧摘要文本冻结不再重算:再次压缩只摘要「上次截止点之后的新段」,
    // 新摘要 = 旧摘要(原字节不动)+ 增量摘要拼接注入,使前缀缓存只 miss 尾部。

    /// 二次压缩:新段只取 (upto_old, upto_new] 区间,已摘要过的消息不重复进入
    #[test]
    fn incremental_segment_only_covers_new_range() {
        let history: Vec<MessageRecord> = (1..=6)
            .map(|i| msg(i, "user", &format!("消息{i}")))
            .collect();
        // 首次:upto_old=0 → 前 2 条(id 1、2)
        let first = incremental_segment(&history, 0, 2);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].content, "消息1");
        // 二次:upto_old=2 → 只取 id 3、4
        let second = incremental_segment(&history, 2, 4);
        assert_eq!(second.len(), 2);
        assert_eq!(second[0].content, "消息3");
        assert_eq!(second[1].content, "消息4");
        // 无新内容(upto_old >= upto_new)→ 空段,调用方应跳过压缩
        assert!(incremental_segment(&history, 4, 4).is_empty());
        assert!(incremental_segment(&history, 5, 3).is_empty());
    }

    /// 增量拼接:旧摘要字节原样保留(是完整前缀),增量只追加在尾部
    #[test]
    fn merge_incremental_summary_preserves_old_bytes() {
        let old = "第一段剧情摘要:两人初遇于图书馆。";
        let inc = "第二段增量:他们约定周末再见。";
        let merged = merge_incremental_summary(old, inc);
        assert!(
            merged.starts_with(old),
            "旧摘要必须是新摘要的字节级前缀: {merged}"
        );
        assert!(merged.ends_with(inc), "增量只追加在尾部: {merged}");
        assert!(merged.contains("\n\n"), "两段之间应有分隔");
        // 空旧摘要 → 仅增量(首次压缩)
        assert_eq!(merge_incremental_summary("", inc), inc);
        // 空增量 → 旧摘要原样
        assert_eq!(merge_incremental_summary(old, ""), old);
    }

    /// 二次压缩后旧行保留可回溯:get_compaction 取 upto 最大行,旧行 summary 不变
    #[test]
    fn second_compaction_keeps_old_row_and_appends() {
        use crate::models::db::Db;
        use crate::services::session_service::SessionService;
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!("kedai-compaction-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("kedai.db"), &dir).unwrap());
        let svc = SessionService::new(db);
        {
            let conn = rusqlite::Connection::open(dir.join("kedai.db")).unwrap();
            conn.execute(
                "INSERT INTO characters (id, name, chara_name, description, file_path, data_raw, created_at)
                 VALUES ('c1', 'c', 'c', '', '', '{}', '')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, character_id, title, created_at, updated_at)
                 VALUES ('s1', 'c1', 't', '', '')",
                [],
            )
            .unwrap();
        }
        let first = "第一次摘要文本。";
        svc.save_compaction("s1", 2, first, "mock").unwrap();
        // 二次压缩:新行 = 旧摘要字节 + 增量
        let increment = "第二次增量摘要。";
        let merged = merge_incremental_summary(first, increment);
        svc.save_compaction("s1", 4, &merged, "mock").unwrap();

        let (upto, latest) = svc.get_compaction("s1").expect("应能读到最新摘要");
        assert_eq!(upto, 4);
        assert!(
            latest.starts_with(first),
            "最新摘要应以旧摘要为前缀: {latest}"
        );
        assert!(latest.ends_with(increment));
        // 旧行仍在且字节未变(可回溯)
        let conn = rusqlite::Connection::open(dir.join("kedai.db")).unwrap();
        let old_row: String = conn
            .query_row(
                "SELECT summary FROM session_compactions WHERE session_id='s1' AND upto_message_id=2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_row, first, "旧摘要行必须原样保留");
        drop(conn);
        std::fs::remove_dir_all(dir).ok();
    }

    // ===== snip 零成本裁剪档(缓存感知管线) =====
    // 在触发 LLM 摘要之前(水位 0.6,先于 compact 0.8)零成本裁掉陈旧的
    // 超长消息:尾部 2 条原文保留,含错误特征的保留,其余超长替换为占位符。
    // 只影响模型可见投影,不修改数据库原文(与可逆投影设计一致)。

    /// keep_recent 参数化:压缩分界按设置保留尾部 N 条原文
    #[test]
    fn compaction_split_uses_configured_keep_recent() {
        let history: Vec<MessageRecord> = (1..=6).map(|i| msg(i, "user", "x")).collect();
        // 默认 4(设置缺省值):待压缩 2 条
        assert_eq!(
            compaction_split(&history, DEFAULT_KEEP_RECENT_MESSAGES),
            Some(2)
        );
        // 自定义 2:待压缩 4 条
        assert_eq!(compaction_split(&history, 2), Some(4));
        // 保留数 >= 历史条数 → 无需压缩
        assert_eq!(compaction_split(&history, 6), None);
    }

    /// snip 裁剪:尾部 2 条原文保留,前面的超长消息替换为占位符
    #[test]
    fn snip_replaces_stale_long_messages_but_keeps_tail() {
        let long = "长".repeat(100); // UTF-8 下 300 字节 > 100 字节阈值
        let tuples = vec![
            ("user".to_string(), long.clone()),
            ("assistant".to_string(), "短回复".to_string()),
            ("user".to_string(), long.clone()),
            ("assistant".to_string(), "尾部保留一".to_string()),
            ("user".to_string(), "尾部保留二".to_string()),
        ];
        let snipped = snip_tuples(&tuples, 100);
        assert_eq!(snipped.len(), 5, "条数不变(占位符替换而非删除)");
        // 前面的超长消息 → 占位符(标注原字符数)
        assert!(
            snipped[0].1.starts_with("[该消息已裁剪:"),
            "超长陈旧消息应替换为占位符: {}",
            snipped[0].1
        );
        assert!(
            snipped[0].1.contains("100"),
            "占位符应含原字符数: {}",
            snipped[0].1
        );
        assert_eq!(snipped[1].1, "短回复", "未超阈值的消息不裁");
        assert_eq!(snipped[2].1, snipped[0].1, "另一个超长陈旧消息同样替换");
        // 尾部 2 条原文保留
        assert_eq!(snipped[3].1, "尾部保留一");
        assert_eq!(snipped[4].1, "尾部保留二");
        // 纯投影:输入不被修改(原文可恢复)
        assert_eq!(tuples[0].1, long, "snip 不得修改输入(数据库原文不动)");
    }

    /// snip 裁剪:含错误特征(error/exception/failed/panic,大小写不敏感)的超长消息不裁
    #[test]
    fn snip_preserves_error_featured_messages() {
        let long_err = format!("{}然后工具报错 error: something failed", "长".repeat(100));
        let long_panic = format!("{}Traceback panic!", "长".repeat(100));
        let tuples = vec![
            ("user".to_string(), long_err),
            ("user".to_string(), long_panic),
            ("user".to_string(), "尾部".to_string()),
            ("user".to_string(), "尾部二".to_string()),
        ];
        let snipped = snip_tuples(&tuples, 100);
        assert_eq!(
            snipped[0].1, tuples[0].1,
            "含 error/failed 的超长消息必须原文保留"
        );
        assert_eq!(snipped[1].1, tuples[1].1, "含 panic 的超长消息必须原文保留");
    }

    /// 尾部不足 2 条时全部保留
    #[test]
    fn snip_keeps_all_when_history_short() {
        let long = "长".repeat(100);
        let tuples = vec![
            ("user".to_string(), long.clone()),
            ("user".to_string(), long),
        ];
        let snipped = snip_tuples(&tuples, 100);
        assert_eq!(snipped, tuples, "不超过尾部 2 条时全部原文保留");
    }

    /// snip 水位:0.6 触发(先于 compact 0.8),max_context 为 0 不触发
    #[test]
    fn should_snip_respects_threshold() {
        assert!(should_snip(6100, 10_000), "0.61 >= 0.6 应触发 snip");
        assert!(!should_snip(5900, 10_000), "0.59 < 0.6 不触发");
        assert!(!should_snip(9000, 0), "未配置上限不触发");
    }
}
