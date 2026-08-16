// 子智能体任务服务(agentgo / agentend / todo / read 轮询)
// 状态:pending → running → done | error | ended(agentend 取消)
// 取消机制:内存 watch channel,后台生成任务把它作为 abort 信号
use crate::models::db::{now_iso, Db};
use crate::models::types::AgentSubtaskRecord;
use rusqlite::{params, OptionalExtension};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use uuid::Uuid;

fn row_to_subtask(row: &rusqlite::Row) -> rusqlite::Result<AgentSubtaskRecord> {
    Ok(AgentSubtaskRecord {
        id: row.get(0)?,
        session_id: row.get(1)?,
        character_id: row.get(2)?,
        name: row.get(3)?,
        instruction: row.get(4)?,
        status: row.get(5)?,
        result: row.get(6)?,
        error: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

pub struct AgentSubtaskService {
    db: Arc<Db>,
    /// 任务 id → 取消信号(watch sender;agentend 时 send(true))
    cancels: Mutex<HashMap<String, watch::Sender<bool>>>,
}

impl AgentSubtaskService {
    pub fn new(db: Arc<Db>) -> Self {
        AgentSubtaskService {
            db,
            cancels: Mutex::new(HashMap::new()),
        }
    }

    pub fn create(
        &self,
        session_id: &str,
        character_id: &str,
        name: &str,
        instruction: &str,
    ) -> Result<AgentSubtaskRecord, String> {
        let now = now_iso();
        let id = Uuid::new_v4().to_string();
        let (tx, _rx) = watch::channel(false);
        self.cancels.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), tx);
        let conn = self.db.conn();
        if let Err(error) = conn.execute(
            "INSERT INTO agent_subtasks (id, session_id, character_id, name, instruction, status, result, error, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', '', '', ?6, ?6)",
            params![id, session_id, character_id, name, instruction, now],
        ) {
            self.cancels.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return Err(format!("创建子任务失败: {error}"));
        }
        Ok(AgentSubtaskRecord {
            id,
            session_id: session_id.to_string(),
            character_id: character_id.to_string(),
            name: name.to_string(),
            instruction: instruction.to_string(),
            status: "pending".into(),
            result: String::new(),
            error: String::new(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub fn get(&self, id: &str) -> Option<AgentSubtaskRecord> {
        let conn = self.db.conn();
        conn.query_row(
            "SELECT id, session_id, character_id, name, instruction, status, result, error, created_at, updated_at FROM agent_subtasks WHERE id = ?1",
            params![id],
            row_to_subtask,
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn list_by_session(&self, session_id: &str) -> Vec<AgentSubtaskRecord> {
        let conn = self.db.conn();
        let mut stmt = conn
            .prepare("SELECT id, session_id, character_id, name, instruction, status, result, error, created_at, updated_at FROM agent_subtasks WHERE session_id = ?1 ORDER BY created_at ASC")
            .unwrap();
        stmt.query_map(params![session_id], row_to_subtask)
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    /// 更新状态 + 可选结果/错误;返回更新后的记录
    fn set_status(
        &self,
        id: &str,
        status: &str,
        result: Option<&str>,
        error: Option<&str>,
    ) -> Option<AgentSubtaskRecord> {
        let existing = self.get(id)?;
        let new_result = result.map(|s| s.to_string()).unwrap_or(existing.result);
        let new_error = error.map(|s| s.to_string()).unwrap_or(existing.error);
        let conn = self.db.conn();
        let n = conn
            .execute(
                "UPDATE agent_subtasks SET status = ?1, result = ?2, error = ?3, updated_at = ?4 WHERE id = ?5",
                params![status, new_result, new_error, now_iso(), id],
            )
            .ok()?;
        if n == 0 {
            return None;
        }
        self.get(id)
    }

    pub fn set_running(&self, id: &str) -> Option<AgentSubtaskRecord> {
        self.set_status(id, "running", None, None)
    }

    pub fn set_done(&self, id: &str, result: &str) -> Option<AgentSubtaskRecord> {
        self.set_status(id, "done", Some(result), None)
    }

    pub fn set_error(&self, id: &str, error: &str) -> Option<AgentSubtaskRecord> {
        self.set_status(id, "error", None, Some(error))
    }

    /// agentend:标记结束并发送取消信号(后台生成若在跑会中断)
    pub fn end(&self, id: &str) -> bool {
        let ok = self.set_status(id, "ended", None, None).is_some();
        if let Some(tx) = self.cancels.lock().unwrap_or_else(|e| e.into_inner()).remove(id) {
            let _ = tx.send(true);
        }
        ok
    }

    /// 注册后台任务的取消通道;返回接收端
    pub fn register_cancel(&self, id: &str) -> watch::Receiver<bool> {
        let mut cancels = self.cancels.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = cancels.get(id) {
            return tx.subscribe();
        }
        let (tx, rx) = watch::channel(false);
        cancels.insert(id.to_string(), tx);
        rx
    }

    /// 后台任务完成时清理取消通道
    pub fn unregister_cancel(&self, id: &str) {
        self.cancels.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    }

    /// 是否已被请求结束(后台任务开始时检查)
    pub fn is_ended(&self, id: &str) -> bool {
        self.get(id).map(|t| t.status == "ended").unwrap_or(true)
    }
}
