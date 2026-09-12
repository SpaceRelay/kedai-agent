// 会话与消息服务(与 Node 版 session.service.ts 对齐)
use super::log_query_failure;
use crate::models::db::{now_iso, Db};
use crate::models::types::{MessageRecord, SessionRecord, SessionWithCharacter, StMessage};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

pub struct SessionService {
    db: Arc<Db>,
}

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        id: row.get(0)?,
        character_id: row.get(1)?,
        title: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn row_to_message(row: &rusqlite::Row) -> rusqlite::Result<MessageRecord> {
    let extra: String = row.get(4)?;
    Ok(MessageRecord {
        id: row.get(0)?,
        session_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        extra: serde_json::from_str(&extra).unwrap_or(Value::Object(Default::default())),
        created_at: row.get(5)?,
    })
}

/// 会话 + 角色名(联表):0 id,1 character_id,2 title,3 created_at,4 updated_at,5 chara_name
fn row_to_session_with_char(row: &rusqlite::Row) -> rusqlite::Result<SessionWithCharacter> {
    Ok(SessionWithCharacter {
        id: row.get(0)?,
        character_id: row.get(1)?,
        title: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        character_name: row.get(5)?,
    })
}

impl SessionService {
    pub fn new(db: Arc<Db>) -> Self {
        SessionService { db }
    }

    pub fn create(&self, character_id: &str, title: Option<&str>) -> Result<SessionRecord, String> {
        let now = now_iso();
        let id = Uuid::new_v4().to_string();
        let title = title.unwrap_or("新会话");
        let conn = self.db.write();
        conn.execute(
            "INSERT INTO sessions (id, character_id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, character_id, title, now],
        )
        .map_err(|e| format!("创建会话失败: {e}"))?;
        Ok(SessionRecord {
            id,
            character_id: character_id.to_string(),
            title: title.to_string(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn list_by_character(&self, character_id: &str) -> Vec<SessionRecord> {
        let conn = self.db.read().expect("获取只读连接失败");
        // prepare/参数绑定失败属 schema 级异常:记 warn 回退空列表,不在阻塞线程 panic
        // (与 filter_map 丢弃坏行的既有 best-effort 语义一致)
        let mut stmt = match conn
            .prepare_cached("SELECT id, character_id, title, created_at, updated_at FROM sessions WHERE character_id = ?1 ORDER BY updated_at DESC")
        {
            Ok(s) => s,
            Err(e) => return log_query_failure("会话列表(按角色) prepare", e),
        };
        let query = stmt.query_map(params![character_id], row_to_session);
        match query {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => log_query_failure("会话列表(按角色) query_map", e),
        }
    }

    /// 全部会话(联表角色名),按 updated_at DESC —— 聊天记录面板
    pub fn list_all(&self) -> Vec<SessionWithCharacter> {
        let conn = self.db.read().expect("获取只读连接失败");
        let mut stmt = match conn.prepare(
            "SELECT s.id, s.character_id, s.title, s.created_at, s.updated_at, c.chara_name \
                 FROM sessions s LEFT JOIN characters c ON c.id = s.character_id \
                 ORDER BY s.updated_at DESC",
        ) {
            Ok(s) => s,
            Err(e) => return log_query_failure("会话列表(全部) prepare", e),
        };
        let query = stmt.query_map([], row_to_session_with_char);
        match query {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => log_query_failure("会话列表(全部) query_map", e),
        }
    }

    /// 会话消息数量(聊天记录面板显示)
    pub fn message_count(&self, session_id: &str) -> i64 {
        self.db
            .read()
            .expect("获取只读连接失败")
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .unwrap_or(0)
    }

    pub fn get(&self, id: &str) -> Option<SessionRecord> {
        let conn = self.db.read().ok()?;
        conn.query_row(
            "SELECT id, character_id, title, created_at, updated_at FROM sessions WHERE id = ?1",
            params![id],
            row_to_session,
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn touch(&self, id: &str) {
        let conn = self.db.write();
        let _ = conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now_iso(), id],
        );
    }

    pub fn delete(&self, id: &str) -> bool {
        let conn = self.db.write();
        conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    /// 取该角色最新会话,无则创建
    pub fn ensure_session(&self, character_id: &str) -> Result<SessionRecord, String> {
        let list = self.list_by_character(character_id);
        match list.into_iter().next() {
            Some(s) => Ok(s),
            None => self.create(character_id, None),
        }
    }

    pub fn add_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        extra: Value,
    ) -> Result<MessageRecord, String> {
        let now = now_iso();
        let extra_str = serde_json::to_string(&extra).unwrap_or_else(|_| "{}".to_string());
        // 注意:conn(MutexGuard)必须在调用 self.touch(再取锁)之前释放
        let id = {
            let conn = self.db.write();
            conn.execute(
                "INSERT INTO messages (session_id, role, content, extra, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session_id, role, content, extra_str, now],
            )
            .map_err(|e| format!("写入消息失败: {e}"))?;
            conn.last_insert_rowid()
        };
        self.touch(session_id);
        Ok(MessageRecord {
            id,
            session_id: session_id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            extra,
            created_at: now,
        })
    }

    pub fn get_messages(&self, session_id: &str) -> Vec<MessageRecord> {
        let conn = self.db.read().expect("获取只读连接失败");
        let mut stmt = match conn
            .prepare_cached("SELECT id, session_id, role, content, extra, created_at FROM messages WHERE session_id = ?1 ORDER BY id ASC")
        {
            Ok(s) => s,
            Err(e) => return log_query_failure("会话消息列表 prepare", e),
        };
        let query = stmt.query_map(params![session_id], row_to_message);
        match query {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => log_query_failure("会话消息列表 query_map", e),
        }
    }

    /// 保存/覆盖会话的上下文压缩摘要(按 (session_id, upto_message_id) 幂等 upsert)。
    /// 摘要覆盖到 upto_message_id(含)为止的历史;原文消息不删除,删该行即可恢复完整历史。
    pub fn save_compaction(
        &self,
        session_id: &str,
        upto_message_id: i64,
        summary: &str,
        model: &str,
    ) -> Result<(), String> {
        let conn = self.db.write();
        conn.execute(
            "INSERT INTO session_compactions (session_id, upto_message_id, summary, model, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id, upto_message_id) DO UPDATE SET
               summary = excluded.summary,
               model = excluded.model,
               created_at = excluded.created_at",
            params![session_id, upto_message_id, summary, model, now_iso()],
        )
        .map_err(|e| format!("保存压缩摘要失败: {e}"))?;
        Ok(())
    }

    /// 读取该会话最新一条压缩摘要(按 upto_message_id 最大),返回 (upto_message_id, summary)。
    pub fn get_compaction(&self, session_id: &str) -> Option<(i64, String)> {
        let conn = self.db.read().ok()?;
        conn.query_row(
            "SELECT upto_message_id, summary FROM session_compactions WHERE session_id = ?1 ORDER BY upto_message_id DESC LIMIT 1",
            params![session_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 删除会话的压缩摘要(可逆:删摘要即恢复完整原文历史)。
    pub fn delete_compaction(&self, session_id: &str) -> Result<(), String> {
        let conn = self.db.write();
        conn.execute(
            "DELETE FROM session_compactions WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(|e| format!("删除压缩摘要失败: {e}"))?;
        Ok(())
    }

    /// 保存一次 LLM 请求快照(第四点·主题 A):记录真正下发给模型的完整消息数组 JSON。
    pub fn save_llm_request(
        &self,
        session_id: &str,
        run_id: &str,
        seq: i64,
        payload: &str,
        model: &str,
    ) -> Result<(), String> {
        let conn = self.db.write();
        conn.execute(
            "INSERT INTO llm_requests (session_id, run_id, seq, payload, model, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session_id, run_id, seq, payload, model, now_iso()],
        )
        .map_err(|e| format!("保存 LLM 请求快照失败: {e}"))?;
        Ok(())
    }

    /// 记录一次 LLM 请求的 usage 缓存统计(缓存感知管线):
    /// 已有该 (session_id, run_id, seq) 快照行时 UPDATE 缓存列(payload 不动);
    /// 无既有行(快照开关关闭)时插入轻量行(payload 空串),保证缓存观测
    /// 不依赖 llm_request_log 调试开关。失败由调用方决定是否告警。
    #[allow(clippy::too_many_arguments)]
    pub fn save_llm_cache_usage(
        &self,
        session_id: &str,
        run_id: &str,
        seq: i64,
        model: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
        cache_hit_tokens: i64,
        cache_miss_tokens: i64,
    ) -> Result<(), String> {
        let conn = self.db.write();
        let updated = conn
            .execute(
                "UPDATE llm_requests SET
                   prompt_cache_hit_tokens = ?4,
                   prompt_cache_miss_tokens = ?5,
                   prompt_tokens = ?6,
                   completion_tokens = ?7
                 WHERE session_id = ?1 AND run_id = ?2 AND seq = ?3",
                params![
                    session_id,
                    run_id,
                    seq,
                    cache_hit_tokens,
                    cache_miss_tokens,
                    prompt_tokens,
                    completion_tokens
                ],
            )
            .map_err(|e| format!("更新 LLM 缓存统计失败: {e}"))?;
        if updated == 0 {
            conn.execute(
                "INSERT INTO llm_requests
                   (session_id, run_id, seq, payload, model, created_at,
                    prompt_cache_hit_tokens, prompt_cache_miss_tokens, prompt_tokens, completion_tokens)
                 VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    session_id,
                    run_id,
                    seq,
                    model,
                    now_iso(),
                    cache_hit_tokens,
                    cache_miss_tokens,
                    prompt_tokens,
                    completion_tokens
                ],
            )
            .map_err(|e| format!("保存 LLM 缓存统计失败: {e}"))?;
        }
        Ok(())
    }

    /// 裁剪会话的 LLM 请求快照,仅保留最近 keep 条(按 id 升序删最旧)。
    pub fn prune_llm_requests(&self, session_id: &str, keep: i64) -> Result<(), String> {
        if keep < 0 {
            return Ok(());
        }
        let conn = self.db.write();
        conn.execute(
            "DELETE FROM llm_requests WHERE session_id = ?1 AND id NOT IN (
               SELECT id FROM llm_requests WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2
             )",
            params![session_id, keep],
        )
        .map_err(|e| format!("裁剪 LLM 请求快照失败: {e}"))?;
        Ok(())
    }

    /// 更新消息内容,保留原有 extra 并标记 { edited: true }。
    /// 原实现整体覆盖 extra 会抹掉 first_mes / status_slot.last / extra.mvu 快照等关键附加数据,
    /// 编辑首楼后状态栏替换锚点丢失、变量快照无法回放。
    pub fn update_message(
        &self,
        session_id: &str,
        id: i64,
        content: &str,
    ) -> Option<MessageRecord> {
        let before = self.get_message(session_id, id)?;
        let mut extra = match before.extra.as_object() {
            Some(obj) => obj.clone(),
            None => serde_json::Map::new(),
        };
        extra.insert("edited".to_string(), serde_json::json!(true));
        let extra_str = serde_json::to_string(&Value::Object(extra)).ok()?;
        // 注意:conn(MutexGuard)必须在再次调用 self.get_message 之前释放,避免重入死锁
        let affected = {
            let conn = self.db.write();
            conn.execute(
                "UPDATE messages SET content = ?1, extra = ?2 WHERE session_id = ?3 AND id = ?4",
                params![content, extra_str, session_id, id],
            )
            .ok()?
        };
        if affected == 0 {
            return None;
        }
        self.get_message(session_id, id)
    }

    /// 仅更新消息内容,保留原 extra(编辑语义的 update_message 会清空 extra;
    /// 两步生成首楼状态栏替换等场景需要保留 first_mes/status_slot 等附加数据)
    pub fn update_message_content(
        &self,
        session_id: &str,
        id: i64,
        content: &str,
    ) -> Option<MessageRecord> {
        let affected = {
            let conn = self.db.write();
            conn.execute(
                "UPDATE messages SET content = ?1 WHERE session_id = ?2 AND id = ?3",
                params![content, session_id, id],
            )
            .ok()?
        };
        if affected == 0 {
            return None;
        }
        self.get_message(session_id, id)
    }

    /// 整行更新:content + 完整 extra 整体替换(供「生成新版本」原地更新原 assistant
    /// 消息行,swipes 版本数组挂靠;不标记 edited,区别于编辑语义的 update_message)。
    pub fn update_message_full(
        &self,
        session_id: &str,
        id: i64,
        content: &str,
        extra: Value,
    ) -> Option<MessageRecord> {
        let extra_str = serde_json::to_string(&extra).ok()?;
        let affected = {
            let conn = self.db.write();
            conn.execute(
                "UPDATE messages SET content = ?1, extra = ?2 WHERE session_id = ?3 AND id = ?4",
                params![content, extra_str, session_id, id],
            )
            .ok()?
        };
        if affected == 0 {
            return None;
        }
        self.get_message(session_id, id)
    }

    pub fn get_message(&self, session_id: &str, id: i64) -> Option<MessageRecord> {
        let conn = self.db.read().ok()?;
        conn.query_row(
            "SELECT id, session_id, role, content, extra, created_at FROM messages WHERE session_id = ?1 AND id = ?2",
            params![session_id, id],
            row_to_message,
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 合并消息 extra:patch 的字段写入 extra(深层对象按顶层键合并),返回更新后的消息。
    /// 供 mvu 变量快照(extra.mvu)等附加数据持久化,不影响原有 extra 字段。
    pub fn merge_message_extra(
        &self,
        session_id: &str,
        id: i64,
        patch: Value,
    ) -> Option<MessageRecord> {
        let existing = self.get_message(session_id, id)?;
        let mut extra = existing.extra;
        if let (Some(obj), Some(patch_obj)) = (extra.as_object_mut(), patch.as_object()) {
            for (k, v) in patch_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
        let extra_str = serde_json::to_string(&extra).ok()?;
        {
            let conn = self.db.write();
            let n = conn
                .execute(
                    "UPDATE messages SET extra = ?1 WHERE session_id = ?2 AND id = ?3",
                    params![extra_str, session_id, id],
                )
                .ok()?;
            if n == 0 {
                return None;
            }
        }
        self.get_message(session_id, id)
    }

    pub fn delete_message(&self, session_id: &str, id: i64) -> bool {
        let conn = self.db.write();
        conn.execute(
            "DELETE FROM messages WHERE session_id = ?1 AND id = ?2",
            params![session_id, id],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    /// 截断:删除该会话中 id 大于 anchor_id 的全部消息(anchor 本身保留)。
    /// 用于「编辑用户消息后重发」:保留被编辑消息、丢弃其后的所有上下文。
    pub fn truncate_messages_after(&self, session_id: &str, anchor_id: i64) -> usize {
        let conn = self.db.write();
        conn.execute(
            "DELETE FROM messages WHERE session_id = ?1 AND id > ?2",
            params![session_id, anchor_id],
        )
        .unwrap_or(0)
    }

    pub fn clear_messages(&self, session_id: &str) {
        let conn = self.db.write();
        let _ = conn.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            params![session_id],
        );
    }

    /// 导出(SillyTavern 兼容):StMessage 数组
    pub fn export_chat(&self, session_id: &str) -> Vec<StMessage> {
        self.get_messages(session_id)
            .into_iter()
            .map(|m| StMessage {
                id: Some(m.id),
                role: m.role,
                content: m.content,
                extra: Some(m.extra),
            })
            .collect()
    }

    /// 导入:先清空再逐条追加,id 重新分配;返回导入条数
    pub fn import_chat(&self, session_id: &str, messages: Vec<StMessage>) -> Result<usize, String> {
        self.clear_messages(session_id);
        let mut count = 0;
        for m in messages {
            let role = m.role.trim().to_lowercase();
            if !matches!(role.as_str(), "user" | "assistant" | "system") {
                return Err("消息格式无效,须为 {role, content}".into());
            }
            if m.content.is_empty() {
                return Err("消息格式无效,须为 {role, content}".into());
            }
            let extra = m.extra.unwrap_or_else(|| Value::Object(Default::default()));
            self.add_message(session_id, &role, &m.content, extra)?;
            count += 1;
        }
        Ok(count)
    }

    // ===== 会话变量(酒馆宏 {{setvar}}/{{addvar}}/{{getvar}} 持久化) =====

    /// 读取会话全部变量 → HashMap
    pub fn load_session_vars(&self, session_id: &str) -> HashMap<String, String> {
        let conn = match self.db.read() {
            Ok(c) => c,
            Err(_) => return HashMap::new(),
        };
        let mut stmt =
            match conn.prepare("SELECT key, value FROM session_vars WHERE session_id = ?1") {
                Ok(s) => s,
                Err(_) => return HashMap::new(),
            };
        let rows = match stmt.query_map(params![session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            Ok(r) => r,
            Err(_) => return HashMap::new(),
        };
        let mut map = HashMap::new();
        for r in rows.flatten() {
            map.insert(r.0, r.1);
        }
        map
    }

    /// 全量覆写会话变量(宏展开后由引擎调用);返回写入条数
    pub fn save_session_vars(&self, session_id: &str, vars: &HashMap<String, String>) -> usize {
        let conn = self.db.write();
        let now = now_iso();
        let _ = conn.execute(
            "DELETE FROM session_vars WHERE session_id = ?1",
            params![session_id],
        );
        let mut count = 0;
        for (k, v) in vars {
            let r = conn.execute(
                "INSERT OR REPLACE INTO session_vars (session_id, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)",
                params![session_id, k, v, now],
            );
            if r.is_ok() {
                count += 1;
            }
        }
        count
    }

    // ===== 酒馆助手变量树(assistant 插件 stat_data,会话级 JSON) =====

    /// 读取会话酒馆助手变量树;无记录返回空树
    pub fn load_assistant_vars(
        &self,
        session_id: &str,
    ) -> crate::parsing::assistant::AssistantVars {
        let Ok(conn) = self.db.read() else {
            return crate::parsing::assistant::AssistantVars::new();
        };
        let raw: Option<String> = conn
            .query_row(
                "SELECT data_raw FROM session_assistant_vars WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .ok();
        match raw {
            Some(s) => crate::parsing::assistant::AssistantVars::from_json(&s),
            None => crate::parsing::assistant::AssistantVars::new(),
        }
    }

    /// 保存会话酒馆助手变量树(整树覆写)。
    /// 返回 Err 使写失败可感知(FK 违约/磁盘满等):内存已应用而落库失败会导致
    /// 刷新后变量回滚且无任何告警——调用方必须记录或上抛。
    pub fn save_assistant_vars(
        &self,
        session_id: &str,
        vars: &crate::parsing::assistant::AssistantVars,
    ) -> Result<(), String> {
        let conn = self.db.write();
        let now = now_iso();
        conn.execute(
            "INSERT OR REPLACE INTO session_assistant_vars (session_id, data_raw, updated_at) VALUES (?1, ?2, ?3)",
            params![session_id, vars.to_json(), now],
        )
        .map_err(|e| format!("保存变量树失败(session {session_id}): {e}"))?;
        Ok(())
    }

    // ===== 7 作用域变量(计划二 · scope_variables 表) =====

    /// 读取指定作用域原始数据(JSON);无记录返回 None。
    pub fn load_scope_variables(&self, scope: &str, scope_id: &str) -> Option<serde_json::Value> {
        let conn = self.db.read().ok()?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT data_raw FROM scope_variables WHERE scope = ?1 AND scope_id = ?2",
                params![scope, scope_id],
                |row| row.get(0),
            )
            .ok();
        raw.and_then(|s| serde_json::from_str(&s).ok())
    }

    /// 保存指定作用域原始数据(整树覆写;INSERT OR REPLACE)。
    pub fn save_scope_variables(&self, scope: &str, scope_id: &str, data: &serde_json::Value) {
        let conn = self.db.write();
        let now = now_iso();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO scope_variables (scope, scope_id, data_raw, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![scope, scope_id, data.to_string(), now],
        );
    }

    /// 批量落库非 chat 作用域(引擎收尾;scope, scope_id, data_raw 元组)。
    pub fn save_scope_variables_batch(&self, entries: Vec<(String, String, String)>) -> usize {
        let conn = self.db.write();
        let now = now_iso();
        let mut count = 0;
        for (scope, scope_id, data_raw) in entries {
            let r = conn.execute(
                "INSERT OR REPLACE INTO scope_variables (scope, scope_id, data_raw, updated_at) VALUES (?1, ?2, ?3, ?4)",
                params![scope, scope_id, data_raw, now],
            );
            if r.is_ok() {
                count += 1;
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::db::Db;

    fn service() -> (SessionService, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("kedai-session-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("kedai.db"), &dir).unwrap());
        let svc = SessionService::new(db);
        // llm_requests/session_compactions 均外键引用 sessions,测试需先建 character + session
        {
            let conn = svc.db.write();
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
        (svc, dir)
    }

    #[test]
    fn save_llm_request_persists_payload() {
        let (svc, dir) = service();
        svc.save_llm_request("s1", "run1", 0, r#"{"role":"system"}"#, "m")
            .unwrap();

        let db = svc.db.clone();
        let conn = db.write();
        let (payload, model): (String, String) = conn
            .query_row(
                "SELECT payload, model FROM llm_requests WHERE session_id='s1' AND seq=0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(payload, r#"{"role":"system"}"#);
        assert_eq!(model, "m");
        drop(conn);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn prune_llm_requests_keeps_only_recent() {
        let (svc, dir) = service();
        // 插入 seq 0..4 共 5 条
        for seq in 0..5 {
            svc.save_llm_request("s1", "run1", seq, "x", "m").unwrap();
        }
        svc.prune_llm_requests("s1", 2).unwrap();

        let db = svc.db.clone();
        let conn = db.write();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM llm_requests WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "应仅保留最近 2 条");
        // 保留的应是 seq 最大的两条(3、4)
        let max_seq: i64 = conn
            .query_row(
                "SELECT MAX(seq) FROM llm_requests WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let min_seq: i64 = conn
            .query_row(
                "SELECT MIN(seq) FROM llm_requests WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(max_seq, 4);
        assert_eq!(min_seq, 3);
        drop(conn);
        std::fs::remove_dir_all(dir).ok();
    }

    /// 缓存 usage 落库:已有请求快照行(seq 匹配)时 UPDATE 缓存列,payload 不被覆盖
    #[test]
    fn save_llm_cache_usage_updates_existing_row() {
        let (svc, dir) = service();
        svc.save_llm_request("s1", "run1", 0, r#"{"role":"system"}"#, "m")
            .unwrap();
        svc.save_llm_cache_usage("s1", "run1", 0, "m", 1000, 200, 700, 300)
            .unwrap();

        let db = svc.db.clone();
        let conn = db.write();
        let (hit, miss, prompt, completion, payload): (i64, i64, i64, i64, String) = conn
            .query_row(
                "SELECT prompt_cache_hit_tokens, prompt_cache_miss_tokens, prompt_tokens, completion_tokens, payload
                 FROM llm_requests WHERE session_id='s1' AND run_id='run1' AND seq=0",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!((hit, miss, prompt, completion), (700, 300, 1000, 200));
        assert_eq!(
            payload, r#"{"role":"system"}"#,
            "缓存列更新不应覆盖 payload"
        );
        drop(conn);
        std::fs::remove_dir_all(dir).ok();
    }

    /// 缓存 usage 落库:快照开关关闭(无既有行)时插入轻量行(payload 为空串),
    /// 保证缓存观测数据不依赖 llm_request_log 调试开关
    #[test]
    fn save_llm_cache_usage_inserts_light_row_when_missing() {
        let (svc, dir) = service();
        svc.save_llm_cache_usage("s1", "run2", 3, "m", 500, 80, 0, 500)
            .unwrap();

        let db = svc.db.clone();
        let conn = db.write();
        let (hit, miss, payload): (i64, i64, String) = conn
            .query_row(
                "SELECT prompt_cache_hit_tokens, prompt_cache_miss_tokens, payload
                 FROM llm_requests WHERE session_id='s1' AND run_id='run2' AND seq=3",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((hit, miss), (0, 500));
        assert_eq!(payload, "", "无快照开关时应插入轻量行(payload 空)");
        drop(conn);
        std::fs::remove_dir_all(dir).ok();
    }
}
