// 收尾落库:发送/接收统计(ST-Prompt-Template 兼容)、重生成原地更新(swipes 版本
// 数组)、Token 累计统计落库(record_usage/record_usage_sync)、run 登记表清理
// (finish_run/finish_run_generation)。自 engine/mod.rs 拆分迁入,纯代码移动,逻辑不变。
// 依赖经 `use super::*` 取自 engine/mod.rs(与 executor/mvu 等子模块同一约定)。
use super::*;

impl AgentEngine {
    /// 发送统计(ST-Prompt-Template 兼容):LAST_SEND_TOKENS / LAST_SEND_CHARS 写入会话
    /// 宏变量并落库,供模板 `{{getvar::LAST_SEND_TOKENS}}` 读取。写入宏表而非变量树,
    /// 避免污染 {{format_message_variable}} 的状态输出(原版即「不参与树渲染的特殊变量」)。
    /// 对应 run_body 内「发送统计」段;L2 中层定位:SSE 链路旁路的状态落库封装。
    pub(super) fn record_send_stats(
        &self,
        session_vars: &mut HashMap<String, String>,
        llm_messages: &[LlmMessage],
        context_tokens: i64,
        session_id: &str,
    ) {
        let send_chars: usize = llm_messages.iter().map(|m| m.content.chars().count()).sum();
        session_vars.insert("LAST_SEND_TOKENS".to_string(), context_tokens.to_string());
        session_vars.insert("LAST_SEND_CHARS".to_string(), send_chars.to_string());
        self.sessions.save_session_vars(session_id, session_vars);
    }

    /// 接收统计(ST-Prompt-Template 兼容):LAST_RECEIVE_TOKENS / LAST_RECEIVE_CHARS 写入
    /// 会话宏变量并落库,下一轮模板可用 {{getvar::LAST_RECEIVE_TOKENS}} 读取。
    /// 先重新加载最新宏表再写回,与运行时序保持一致。
    /// 对应 run_body 内「接收统计」段;L2 中层定位:SSE 链路旁路的状态落库封装。
    pub(super) fn record_receive_stats(
        &self,
        session_id: &str,
        completion_tokens: i64,
        clean_content: &str,
    ) {
        let mut vars = self.sessions.load_session_vars(session_id);
        vars.insert(
            "LAST_RECEIVE_TOKENS".to_string(),
            completion_tokens.to_string(),
        );
        vars.insert(
            "LAST_RECEIVE_CHARS".to_string(),
            clean_content.chars().count().to_string(),
        );
        self.sessions.save_session_vars(session_id, &vars);
    }

    /// 阶段六 6f:重生成时原地更新原 assistant 消息行,不新增行。
    /// 旧内容并入 extra.swipes 版本数组(含激活版本,元素 {swipe_id, content, ts}),
    /// swipe_id 指向新版本;与最后一条版本相同则不重复追加(避免重复生成同一文本)。
    /// 返回更新后的消息记录;锚点不存在或更新失败返回 Err。
    pub(super) fn upsert_regenerated_message(
        &self,
        session_id: &str,
        regenerate_id: i64,
        content: &str,
        extra: &mut Value,
        ts: i64,
    ) -> Result<MessageRecord, String> {
        let original = self
            .sessions
            .get_message(session_id, regenerate_id)
            .ok_or_else(|| "重生成锚点消息不存在".to_string())?;
        // 已有版本数组则沿用(可能来自更早的重生成),否则以原内容为首版本
        let mut swipes: Vec<Value> = match original.extra.get("swipes").and_then(|s| s.as_array()) {
            Some(arr) => arr.clone(),
            None => vec![json!({ "swipe_id": 0, "content": original.content, "ts": ts })],
        };
        let last_content = swipes
            .last()
            .and_then(|s| s.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        if last_content != content {
            swipes.push(json!({ "swipe_id": swipes.len(), "content": content, "ts": ts }));
        }
        let swipe_id = swipes.len() - 1;
        extra["swipes"] = json!(swipes);
        extra["swipe_id"] = json!(swipe_id);
        self.sessions
            .update_message_full(session_id, regenerate_id, content, extra.clone())
            .ok_or_else(|| "重生成消息更新失败".to_string())
    }

    /// Token 累计统计落库:会话级 session_usage 累加 + 全局 global_usage 累加,
    /// 与 Node 版 usage 持久化语义一致。
    /// 对应 run_body 内「Token 累计统计」段;L2 中层定位:落库侧的单一职责封装。
    /// 同步 SQLite 写入经 spawn_blocking 挪进阻塞线程池(DB 并发改造),逻辑不变。
    pub(super) async fn record_usage(&self, session_id: &str, total_usage: &TokenUsage) {
        let db = self.db.clone();
        let session_id = session_id.to_string();
        let usage = total_usage.clone();
        let _ = tokio::task::spawn_blocking(move || {
            Self::record_usage_sync(&db, &session_id, &usage);
        })
        .await;
    }

    /// record_usage 的同步实现(阻塞线程内执行)
    fn record_usage_sync(db: &Db, session_id: &str, total_usage: &TokenUsage) {
        let total = total_usage.prompt_tokens + total_usage.completion_tokens;
        let now = crate::models::db::now_iso();
        let conn = db.write();
        let _ = conn.execute(
            "INSERT INTO session_usage (session_id, total_prompt, total_completion, total_tokens, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id) DO UPDATE SET
               total_prompt = total_prompt + ?2,
               total_completion = total_completion + ?3,
               total_tokens = total_tokens + ?4,
               updated_at = ?5",
            rusqlite::params![
                session_id,
                total_usage.prompt_tokens,
                total_usage.completion_tokens,
                total,
                now
            ],
        );
        let _ = conn.execute(
            "UPDATE global_usage SET
               total_prompt = total_prompt + ?1,
               total_completion = total_completion + ?2,
               total_tokens = total_tokens + ?3,
               updated_at = ?4
             WHERE id = 1",
            rusqlite::params![
                total_usage.prompt_tokens,
                total_usage.completion_tokens,
                total,
                now
            ],
        );
    }

    pub(super) fn finish_run(&self, session_id: &str, run_id: uuid::Uuid) {
        finish_run_generation(
            &mut self.runs.lock().unwrap_or_else(|e| e.into_inner()),
            session_id,
            run_id,
        );
    }
}

// finish_run 的纯函数内核;pub(super) 供 engine/mod.rs 的 run_generation_tests 复用
pub(super) fn finish_run_generation(
    runs: &mut HashMap<String, RunHandle>,
    session_id: &str,
    run_id: uuid::Uuid,
) {
    if runs.get(session_id).is_some_and(|run| run.run_id == run_id) {
        runs.remove(session_id);
    }
}
