// Agent 会话服务(与 Node 版 agent-session.service.ts 对齐)
use super::log_query_failure;
use crate::models::db::{now_iso, Db};
use crate::models::types::{AgentSessionRecord, ToolCallRecord};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

pub struct AgentSessionService {
    db: Arc<Db>,
}

fn row_to_agent_session(row: &rusqlite::Row) -> rusqlite::Result<AgentSessionRecord> {
    let plan: String = row.get(3)?;
    let steps: String = row.get(4)?;
    Ok(AgentSessionRecord {
        id: row.get(0)?,
        session_id: row.get(1)?,
        state: row.get(2)?,
        plan: serde_json::from_str(&plan).unwrap_or_default(),
        steps: serde_json::from_str(&steps).unwrap_or_default(),
        step_index: row.get(5)?,
        agent_mode: row.get(6)?,
        started_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn row_to_tool_call(row: &rusqlite::Row) -> rusqlite::Result<ToolCallRecord> {
    let input: String = row.get(3)?;
    let output: String = row.get(4)?;
    Ok(ToolCallRecord {
        id: row.get(0)?,
        agent_session_id: row.get(1)?,
        name: row.get(2)?,
        input: serde_json::from_str(&input).unwrap_or(Value::Null),
        output: serde_json::from_str(&output).unwrap_or(Value::Null),
        duration_ms: row.get(5)?,
        created_at: row.get(6)?,
    })
}

impl AgentSessionService {
    pub fn new(db: Arc<Db>) -> Self {
        AgentSessionService { db }
    }

    pub fn create(&self, session_id: &str, agent_mode: &str) -> Result<AgentSessionRecord, String> {
        let now = now_iso();
        let id = Uuid::new_v4().to_string();
        let conn = self.db.write();
        conn.execute(
            "INSERT INTO agent_sessions (id, session_id, state, plan, steps, step_index, agent_mode, started_at, updated_at) VALUES (?1, ?2, 'idle', '[]', '[]', 0, ?3, ?4, ?4)",
            params![id, session_id, agent_mode, now],
        )
        .map_err(|e| format!("创建 Agent 会话失败: {e}"))?;
        Ok(AgentSessionRecord {
            id,
            session_id: session_id.to_string(),
            state: "idle".into(),
            plan: vec![],
            steps: vec![],
            step_index: 0,
            agent_mode: agent_mode.to_string(),
            started_at: now.clone(),
            updated_at: now,
        })
    }

    /// 只允许更新 state/plan/step_index/steps 四列 + updated_at
    pub fn update(
        &self,
        id: &str,
        state: Option<&str>,
        plan: Option<&Vec<String>>,
        step_index: Option<i64>,
        steps: Option<&Vec<ToolCallRecord>>,
    ) -> Result<AgentSessionRecord, String> {
        let existing = self.get(id).ok_or_else(|| "Agent 会话不存在".to_string())?;
        let new_state = state.unwrap_or(&existing.state).to_string();
        let new_plan = plan.cloned().unwrap_or(existing.plan.clone());
        let new_step = step_index.unwrap_or(existing.step_index);
        let new_steps = steps.cloned().unwrap_or(existing.steps.clone());
        let plan_str = serde_json::to_string(&new_plan).unwrap_or_else(|_| "[]".into());
        let steps_str = serde_json::to_string(&new_steps).unwrap_or_else(|_| "[]".into());
        let now = now_iso();
        let conn = self.db.write();
        conn.execute(
            "UPDATE agent_sessions SET state = ?1, plan = ?2, step_index = ?3, steps = ?4, updated_at = ?5 WHERE id = ?6",
            params![new_state, plan_str, new_step, steps_str, now, id],
        )
        .map_err(|e| format!("更新 Agent 会话失败: {e}"))?;
        Ok(AgentSessionRecord {
            id: id.to_string(),
            session_id: existing.session_id,
            state: new_state,
            plan: new_plan,
            steps: new_steps,
            step_index: new_step,
            agent_mode: existing.agent_mode,
            started_at: existing.started_at,
            updated_at: now,
        })
    }

    pub fn get(&self, id: &str) -> Option<AgentSessionRecord> {
        let conn = self.db.read().ok()?;
        conn.query_row(
            "SELECT id, session_id, state, plan, steps, step_index, agent_mode, started_at, updated_at FROM agent_sessions WHERE id = ?1",
            params![id],
            row_to_agent_session,
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 按 session 找最新一条
    pub fn find_by_session(&self, session_id: &str) -> Option<AgentSessionRecord> {
        let conn = self.db.read().ok()?;
        conn.query_row(
            "SELECT id, session_id, state, plan, steps, step_index, agent_mode, started_at, updated_at FROM agent_sessions WHERE session_id = ?1 ORDER BY updated_at DESC LIMIT 1",
            params![session_id],
            row_to_agent_session,
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn add_tool_call(
        &self,
        agent_session_id: &str,
        name: &str,
        input: Value,
        output: Value,
        duration_ms: i64,
    ) -> Result<ToolCallRecord, String> {
        let now = now_iso();
        let id = Uuid::new_v4().to_string();
        let input_str = serde_json::to_string(&input).unwrap_or_else(|_| "{}".into());
        let output_str = serde_json::to_string(&output).unwrap_or_else(|_| "{}".into());
        let conn = self.db.write();
        conn.execute(
            "INSERT INTO tool_calls (id, agent_session_id, name, input, output, duration_ms, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, agent_session_id, name, input_str, output_str, duration_ms, now],
        )
        .map_err(|e| format!("写入工具调用失败: {e}"))?;
        Ok(ToolCallRecord {
            id,
            agent_session_id: agent_session_id.to_string(),
            name: name.to_string(),
            input,
            output,
            duration_ms,
            created_at: now,
        })
    }

    pub fn list_tool_calls(&self, agent_session_id: &str) -> Vec<ToolCallRecord> {
        let conn = self.db.read().expect("获取只读连接失败");
        let mut stmt = match conn
            .prepare("SELECT id, agent_session_id, name, input, output, duration_ms, created_at FROM tool_calls WHERE agent_session_id = ?1 ORDER BY created_at ASC")
        {
            Ok(s) => s,
            Err(e) => return log_query_failure("工具调用列表 prepare", e),
        };
        let query = stmt.query_map(params![agent_session_id], row_to_tool_call);
        match query {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => log_query_failure("工具调用列表 query_map", e),
        }
    }

    pub fn delete_by_session(&self, session_id: &str) {
        let conn = self.db.write();
        let _ = conn.execute(
            "DELETE FROM agent_sessions WHERE session_id = ?1",
            params![session_id],
        );
    }
}
