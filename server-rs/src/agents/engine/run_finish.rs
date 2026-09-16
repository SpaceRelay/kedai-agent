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
        let sid = session_id.to_string();
        let usage = total_usage.clone();
        // 阻塞任务失败(线程池关闭/任务 panic)此前被 `let _ =` 吞掉:用量静默丢失,
        // 调用方无从察觉。改为留痕(不阻断生成收尾;2026-09-13 批次 3.7)。
        if let Err(e) = tokio::task::spawn_blocking(move || {
            Self::record_usage_sync(&db, &sid, &usage);
        })
        .await
        {
            tracing::warn!(
                session_id,
                error = e.to_string(),
                "Token 用量落库阻塞任务失败"
            );
        }
    }

    /// record_usage 的同步实现(阻塞线程内执行)。
    ///
    /// 两条累加 SQL 必须**同事务**(2026-09-13 批次 3.7):此前各自独立提交且都用
    /// `let _ =` 吞错,第二条(global_usage)失败时会留下「会话用量已加、全局未加」的
    /// 永久偏差,且无任何日志。现在开事务→两条 SQL→commit,任一步失败即回滚并留痕,
    /// 保证会话用量与全局用量始终同进同退。
    fn record_usage_sync(db: &Db, session_id: &str, total_usage: &TokenUsage) {
        let total = total_usage.prompt_tokens + total_usage.completion_tokens;
        let now = crate::models::db::now_iso();
        // transaction() 需要 &mut Connection,故写连接取 mut 守卫
        let mut conn = db.write();
        let tx = match conn.transaction() {
            Ok(tx) => tx,
            Err(e) => {
                tracing::warn!(
                    session_id,
                    error = e.to_string(),
                    "Token 用量落库开启事务失败,本次用量未记录"
                );
                return;
            }
        };
        if let Err(e) = tx.execute(
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
        ) {
            // tx 在此处 drop → 默认回滚行为,不会留下半截用量
            tracing::warn!(
                session_id,
                error = e.to_string(),
                "会话用量写入失败(事务已回滚)"
            );
            return;
        }
        if let Err(e) = tx.execute(
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
        ) {
            tracing::warn!(
                session_id,
                error = e.to_string(),
                "全局用量写入失败(事务已回滚)"
            );
            return;
        }
        if let Err(e) = tx.commit() {
            tracing::warn!(
                session_id,
                error = e.to_string(),
                "Token 用量落库提交失败(事务已回滚)"
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::db::Db;
    use crate::models::types::TokenUsage;
    use crate::utils::test_support::TempDataDir;

    /// 临时库:预置 characters + sessions(session_usage 有 FK 约束,必须先有会话行)。
    /// 返回 (守卫, 库):解构绑定按**逆序**析构,守卫在前才活到最后(见 test_support 模块头)
    fn test_db(tag: &str) -> (TempDataDir, Db) {
        let dir = TempDataDir::new(&format!("usage-{tag}"));
        let db = Db::open(&dir.join("kedai.db"), &dir).unwrap();
        {
            let conn = db.write();
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
        (dir, db)
    }

    /// 失败回滚(2026-09-13 批次 3.7):第二条 SQL(global_usage)失败时,
    /// session_usage 不得新增行——此前两条独立提交 + `let _ =` 吞错会留下
    /// 「会话已加、全局未加/半截」的不可观测偏差。
    /// 制造失败的方式:删掉 global_usage 表(UPDATE 必然报 no such table)。
    #[test]
    fn record_usage_sync_rolls_back_when_global_update_fails() {
        let (_dir, db) = test_db("rollback");
        {
            let conn = db.write();
            conn.execute_batch("DROP TABLE global_usage").unwrap();
        }
        let usage = TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            ..Default::default()
        };
        AgentEngine::record_usage_sync(&db, "s1", &usage);

        let conn = db.write();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_usage WHERE session_id = 's1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n, 0,
            "global_usage 写入失败 → session_usage 必须一并回滚,不得留半截用量"
        );
        drop(conn);
    }

    /// 成功路径一致性:连续两次落库后 session_usage 与 global_usage 累加值相等
    #[test]
    fn record_usage_sync_keeps_session_and_global_in_step() {
        let (_dir, db) = test_db("ok");
        let usage = TokenUsage {
            prompt_tokens: 12,
            completion_tokens: 8,
            ..Default::default()
        };
        AgentEngine::record_usage_sync(&db, "s1", &usage);
        AgentEngine::record_usage_sync(&db, "s1", &usage);

        let conn = db.write();
        let s: (i64, i64, i64) = conn
            .query_row(
                "SELECT total_prompt, total_completion, total_tokens FROM session_usage WHERE session_id = 's1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        let g: (i64, i64, i64) = conn
            .query_row(
                "SELECT total_prompt, total_completion, total_tokens FROM global_usage WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(s, (24, 16, 40), "两次累加后的会话用量");
        assert_eq!(g, (24, 16, 40), "全局用量与会话用量同进同退");
        drop(conn);
    }
}
