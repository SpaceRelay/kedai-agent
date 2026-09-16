// 子智能体任务服务(agentgo / agentend / todo / read 轮询)
// 状态:pending → running → done | error | ended(agentend 取消)
// 取消机制:内存 watch channel,后台生成任务把它作为 abort 信号
// 任务模式(批次 4.3b):session_id 带 task: 前缀的虚拟会话不落 agent_subtasks 表
// (该表 session_id 外键指向 sessions(id),任务 id 不在其中,写入必 FK 失败),
// 改走内存覆盖层(mem map);进度经任务事件桥(agent_status)观测,口径与
// llm_requests 的 task: 前缀跳过守卫同款。
use super::{log_query_failure, log_read_pool_failure};
use crate::models::db::{now_iso, Db};
use crate::models::types::AgentSubtaskRecord;
use rusqlite::{params, OptionalExtension};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use uuid::Uuid;

/// 终态判定:done / error / ended 之后不再有实质产出,状态与结果到此定稿。
/// 与 `TaskSubtaskStatus::is_terminal` 同口径(两侧各一套类型,语义必须一致)。
fn is_terminal_status(status: &str) -> bool {
    matches!(status, "done" | "error" | "ended")
}

/// `agentend` 的处理结果(2026-09-16 批次 4):旧实现只回 bool,「本次真的中断了活跃任务」
/// 与「任务早就 done/ended,本次是空操作」都返回 true,调用方无法自证。(见 §0.1)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubtaskEndOutcome {
    /// 没有这条记录(拼错 id / 已被清理)
    Missing,
    /// 本次真的中断了活跃任务:status 被置为 ended
    Interrupted { prior_status: String },
    /// 记录已处于终态:不覆盖 status/result,仅补记 finished_at
    AlreadyFinished { prior_status: String },
}

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
        finished_at: row.get(10)?,
    })
}

pub struct AgentSubtaskService {
    db: Arc<Db>,
    /// 任务 id → 取消信号(watch sender;agentend 时 send(true))
    cancels: Mutex<HashMap<String, watch::Sender<bool>>>,
    /// 任务模式内存覆盖层:task: 前缀虚拟 session 的子任务记录(不碰 DB,见文件头)。
    /// 生命周期随进程;任务结束后记录保留供 todo/read 轮询(与 DB 路径读语义一致)。
    mem: Mutex<HashMap<String, AgentSubtaskRecord>>,
}

impl AgentSubtaskService {
    pub fn new(db: Arc<Db>) -> Self {
        AgentSubtaskService {
            db,
            cancels: Mutex::new(HashMap::new()),
            mem: Mutex::new(HashMap::new()),
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
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx);
        let record = AgentSubtaskRecord {
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
            finished_at: String::new(),
        };
        // 任务模式虚拟 session:落内存覆盖层,不写 agent_subtasks 表(FK 守卫)
        if session_id.starts_with("task:") {
            self.mem
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(record.id.clone(), record.clone());
            return Ok(record);
        }
        let conn = self.db.write();
        if let Err(error) = conn.execute(
            "INSERT INTO agent_subtasks (id, session_id, character_id, name, instruction, status, result, error, created_at, updated_at, finished_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', '', '', ?6, ?6, '')",
            params![record.id, session_id, character_id, name, instruction, record.created_at],
        ) {
            self.cancels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&record.id);
            return Err(format!("创建子任务失败: {error}"));
        }
        Ok(record)
    }

    pub fn get(&self, id: &str) -> Option<AgentSubtaskRecord> {
        // 内存覆盖层优先(task: 记录不会出现在 DB,先查可避免一次空查询)
        if let Some(r) = self.mem.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
            return Some(r.clone());
        }
        let conn = self.db.read().ok()?;
        conn.query_row(
            "SELECT id, session_id, character_id, name, instruction, status, result, error, created_at, updated_at, finished_at FROM agent_subtasks WHERE id = ?1",
            params![id],
            row_to_subtask,
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 按 session 前缀列举内存覆盖层记录(可观测性问题⑤,读路径合并用):
    /// task:{id} 覆盖 multi 主 agent 派生子,team 的 task:{id}:main:{n} 全部虚拟
    /// session 一并命中(starts_with 语义,与 end_by_session_prefix 同口径)。
    /// 只读覆盖层、不碰 DB:DB 路径的 session_id 是真实会话(无前缀语义),
    /// 任务侧记录本就只存在于覆盖层。按创建时间升序(+id 兜底),与 list_by_session 一致。
    /// 生命周期随进程:重启后覆盖层丢失,调用方(TaskService::list_subtasks)
    /// 回退为仅 DB 行可见(既定取舍,见其文档注释)。
    pub fn list_by_session_prefix(&self, session_prefix: &str) -> Vec<AgentSubtaskRecord> {
        let mut out: Vec<AgentSubtaskRecord> = self
            .mem
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|r| r.session_id.starts_with(session_prefix))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        out
    }

    pub fn list_by_session(&self, session_id: &str) -> Vec<AgentSubtaskRecord> {
        // 任务模式虚拟 session:只读内存覆盖层(按创建时间升序,与 DB 路径口径一致)
        if session_id.starts_with("task:") {
            let mut out: Vec<AgentSubtaskRecord> = self
                .mem
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|r| r.session_id == session_id)
                .cloned()
                .collect();
            out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
            return out;
        }
        let conn = match self.db.read() {
            Ok(c) => c,
            Err(e) => return log_read_pool_failure("子任务列表", e),
        };
        let mut stmt = match conn
            .prepare("SELECT id, session_id, character_id, name, instruction, status, result, error, created_at, updated_at, finished_at FROM agent_subtasks WHERE session_id = ?1 ORDER BY created_at ASC")
        {
            Ok(s) => s,
            Err(e) => return log_query_failure("子任务列表 prepare", e),
        };
        let query = stmt.query_map(params![session_id], row_to_subtask);
        match query {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => log_query_failure("子任务列表 query_map", e),
        }
    }

    /// 更新状态 + 可选结果/错误;返回更新后的记录。
    /// 进入终态(done/error/ended)时补记 finished_at,但**只补首次**:
    /// 已有的完成时刻不被后续写入改写(否则「done 之后又被置 error」会把完成时间推后,
    /// 调用方据 finished_at 判「何时完成」就失去意义)。
    fn set_status(
        &self,
        id: &str,
        status: &str,
        result: Option<&str>,
        error: Option<&str>,
    ) -> Option<AgentSubtaskRecord> {
        // 内存覆盖层路径(记录存在即走内存,不碰 DB)
        {
            let mut mem = self.mem.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(r) = mem.get_mut(id) {
                r.status = status.to_string();
                if let Some(res) = result {
                    r.result = res.to_string();
                }
                if let Some(err) = error {
                    r.error = err.to_string();
                }
                if is_terminal_status(status) && r.finished_at.is_empty() {
                    r.finished_at = now_iso();
                }
                r.updated_at = now_iso();
                return Some(r.clone());
            }
        }
        let existing = self.get(id)?;
        let new_result = result.map(|s| s.to_string()).unwrap_or(existing.result);
        let new_error = error.map(|s| s.to_string()).unwrap_or(existing.error);
        // 首次进入终态才落时间;非终态(pending/running)保持不动
        let new_finished = if is_terminal_status(status) && existing.finished_at.is_empty() {
            now_iso()
        } else {
            existing.finished_at
        };
        // conn 作用域收窄:UPDATE 执行后立即释放锁,避免末尾 self.get(id)
        // 再次 lock 同一 Mutex<Connection> 造成自死锁(与 session_service 同型约定)
        let n = {
            let conn = self.db.write();
            conn.execute(
                "UPDATE agent_subtasks SET status = ?1, result = ?2, error = ?3, updated_at = ?4, finished_at = ?5 WHERE id = ?6",
                params![status, new_result, new_error, now_iso(), new_finished, id],
            )
            .ok()?
        };
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

    /// 子任务失败但保留部分产出(审计项 B:截断等场景正文仍有价值,不能丢内容)。
    /// 与 set_error 的差异:set_error 只写 error(result 保持空);本方法把 result 与
    /// error 一起落库,状态同为 error。内部转发既有私有 set_status(不复制写库实现)。
    pub fn set_failed(&self, id: &str, result: &str, error: &str) -> Option<AgentSubtaskRecord> {
        self.set_status(id, "error", Some(result), Some(error))
    }

    /// agentend:标记结束并发送取消信号(后台生成若在跑会中断)。
    ///
    /// 终态幂等(2026-09-16 批次 4):记录已 done/error 时**不覆盖** status 与 result,
    /// 只补记缺失的 finished_at。此前无条件 `set_status(id, "ended")` 会把「已完成再被
    /// agentend 召回」的交付物改写成 ended,于是 `ended` 一词同时表示「完成后召回」与
    /// 「中途中断」,调用方无法据 status 判断有无结果(实跑记录第 3 条)。
    /// 返回显式结果而非 bool:让调用方不必再从 prior_status 自行推断(§0.1 自证原则)。
    pub fn end(&self, id: &str) -> SubtaskEndOutcome {
        let outcome = match self.get(id) {
            None => {
                self.cleanup_cancel(id);
                return SubtaskEndOutcome::Missing;
            }
            Some(rec) if is_terminal_status(&rec.status) => {
                // 终态:仅补缺失的 finished_at(条件 UPDATE,天然幂等;不触碰 status/result)
                self.stamp_finished_at_if_missing(id);
                SubtaskEndOutcome::AlreadyFinished {
                    prior_status: rec.status,
                }
            }
            Some(rec) => {
                let prior_status = rec.status;
                if self.set_status(id, "ended", None, None).is_none() {
                    self.cleanup_cancel(id);
                    return SubtaskEndOutcome::Missing;
                }
                SubtaskEndOutcome::Interrupted { prior_status }
            }
        };
        self.cleanup_cancel(id);
        outcome
    }

    /// 只在 finished_at 为空时补记(条件 UPDATE 保证幂等,重复调用不改写首值)。
    /// 内存覆盖层与 DB 两条路径各自处理,与 set_status 同型。
    fn stamp_finished_at_if_missing(&self, id: &str) {
        let now = now_iso();
        {
            let mut mem = self.mem.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(r) = mem.get_mut(id) {
                if r.finished_at.is_empty() {
                    r.finished_at = now;
                    r.updated_at = now_iso();
                }
                return;
            }
        }
        let conn = self.db.write();
        let _ = conn.execute(
            "UPDATE agent_subtasks SET finished_at = ?1 WHERE id = ?2 AND finished_at = ''",
            params![now, id],
        );
    }

    /// 发送取消信号并移除通道(agentend 与记录缺失路径共用)
    fn cleanup_cancel(&self, id: &str) {
        if let Some(tx) = self
            .cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
        {
            let _ = tx.send(true);
        }
    }

    /// 按 session 前缀批量结束(任务模式 stop:task:{id} 前缀覆盖主/子 agent
    /// 全部虚拟 session,含 team 的 task:{id}:main:{n};防孤儿后台任务)。
    /// 返回**真正被中断**的条数(仅计 Interrupted;入参集合已预过滤非终态,
    /// 故计数语义与终态幂等改动前一致),供日志观测。
    pub fn end_by_session_prefix(&self, session_prefix: &str) -> usize {
        let ids: Vec<String> = self
            .mem
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|r| {
                r.session_id.starts_with(session_prefix)
                    && (r.status == "pending" || r.status == "running")
            })
            .map(|r| r.id.clone())
            .collect();
        let mut n = 0;
        for id in ids {
            if matches!(self.end(&id), SubtaskEndOutcome::Interrupted { .. }) {
                n += 1;
            }
        }
        n
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
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    /// 是否已被请求结束(后台任务开始时检查)
    pub fn is_ended(&self, id: &str) -> bool {
        self.get(id).map(|t| t.status == "ended").unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_support::TempDataDir;

    /// 返回 (守卫, 服务):解构绑定按**逆序**析构,守卫在前才活到最后(见 test_support 模块头)
    fn svc() -> (TempDataDir, AgentSubtaskService) {
        let dir = TempDataDir::new("subtask-test");
        let db = Arc::new(Db::open(&dir.join("t.db"), &dir).unwrap());
        (dir, AgentSubtaskService::new(db))
    }

    /// 任务模式内存覆盖层:task: 前缀虚拟 session 的 CRUD 全程不碰 DB
    ///(agent_subtasks.session_id FK 指向 sessions,虚拟 id 直写必失败);
    /// create/get/list/set_status/end 语义与 DB 路径一致。
    #[test]
    fn task_prefixed_session_uses_memory_overlay() {
        let (_dir, s) = svc();
        // 创建(task: 前缀,DB 中无对应 sessions 行——若走 DB 必 FK 失败)
        let r1 = s
            .create("task:t1", "", "子一", "指令一")
            .expect("内存路径创建应成功");
        let r2 = s
            .create("task:t1:main:1", "c", "子二", "指令二")
            .expect("team 主 agent 前缀同样走内存");
        assert_eq!(r1.status, "pending");

        // get/list:内存命中
        assert_eq!(s.get(&r1.id).map(|r| r.name), Some("子一".to_string()));
        let list = s.list_by_session("task:t1");
        assert_eq!(
            list.len(),
            1,
            "list 按 session 精确匹配,不含 team 子的 session"
        );
        assert_eq!(s.list_by_session("task:t1:main:1").len(), 1);

        // 状态推进:running → done(结果写入)
        assert!(s.set_running(&r1.id).is_some());
        assert_eq!(s.get(&r1.id).map(|r| r.status), Some("running".to_string()));
        assert!(s.set_done(&r1.id, "成果").is_some());
        let done = s.get(&r1.id).unwrap();
        assert_eq!(done.status, "done");
        assert_eq!(done.result, "成果");

        // end_by_session_prefix:task:t1 前缀覆盖两个 session;r1 已 done 不再结束
        let n = s.end_by_session_prefix("task:t1");
        assert_eq!(n, 1, "仅 pending 的 r2 被结束");
        assert_eq!(s.get(&r2.id).map(|r| r.status), Some("ended".to_string()));
        assert!(s.is_ended(&r2.id));
        assert!(!s.is_ended(&r1.id), "done 状态不应被误判为 ended");
        assert!(
            !s.get(&r1.id).unwrap().finished_at.is_empty(),
            "set_done 进入终态应记 finished_at"
        );

        // DB 表无写入(覆盖层的核心断言)
        let conn = s.db.read().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_subtasks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "task: 前缀记录不得落 agent_subtasks 表");
    }

    /// 按 session 前缀列举内存覆盖层(可观测性问题⑤):task:{id} 与 team 的
    /// task:{id}:main:{n} 全部虚拟 session 一次取齐,按创建时间升序(+id 兜底);
    /// 只读覆盖层,不碰 DB(DB 路径的 session_id 是真实会话,无前缀语义)。
    #[test]
    fn list_by_session_prefix_covers_team_virtual_sessions() {
        let (_dir, s) = svc();
        let a = s.create("task:t1", "", "主旁子一", "指令一").unwrap();
        let b = s.create("task:t1:main:0", "", "主0子一", "指令二").unwrap();
        let c = s.create("task:t1:main:1", "", "主1子一", "指令三").unwrap();
        // 其他任务的记录不得混入
        let _other = s.create("task:t2", "", "别任务", "指令").unwrap();

        let mut ids: Vec<String> = s
            .list_by_session_prefix("task:t1")
            .into_iter()
            .map(|r| r.id)
            .collect();
        ids.sort();
        let mut want = vec![a.id.clone(), b.id.clone(), c.id.clone()];
        want.sort();
        assert_eq!(ids, want, "前缀列举应覆盖 task:t1 全部虚拟 session");

        // created_at 升序(同毫秒内按 id 字典序兜底,与 list_by_session 口径一致)
        let list = s.list_by_session_prefix("task:t1");
        for w in list.windows(2) {
            assert!(
                (w[0].created_at.as_str(), w[0].id.as_str())
                    <= (w[1].created_at.as_str(), w[1].id.as_str()),
                "排序应为 created_at 升序 + id 兜底: {list:?}"
            );
        }
        // 空前缀不命中(防御:不得把全部覆盖层记录倒出)
        assert!(s.list_by_session_prefix("task:不存在").is_empty());
    }

    /// 普通会话仍走 DB 路径(回归:聊天路径行为不变)
    #[test]
    fn normal_session_still_uses_db() {
        let (_dir, s) = svc();
        // 造真实角色与会话行满足 FK
        let characters = crate::services::character_service::CharacterService::new(
            s.db.clone(),
            _dir.path().to_path_buf(),
        );
        characters.seed_default_character();
        let sessions = crate::services::session_service::SessionService::new(s.db.clone());
        let session = sessions
            .create(crate::services::character_service::BUILTIN_SYSTEM_ID, None)
            .unwrap();
        let r = s
            .create(&session.id, "", "子", "指令")
            .expect("DB 路径创建应成功");
        assert!(s.mem.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
        assert_eq!(s.list_by_session(&session.id).len(), 1);
        assert!(s.set_running(&r.id).is_some());
        assert!(
            s.get(&r.id).unwrap().finished_at.is_empty(),
            "running 期间 finished_at 应保持空串"
        );
        assert_eq!(
            s.end(&r.id),
            SubtaskEndOutcome::Interrupted {
                prior_status: "running".into()
            }
        );
        assert!(s.is_ended(&r.id));
        assert!(
            !s.get(&r.id).unwrap().finished_at.is_empty(),
            "end 中断活跃任务应记 finished_at"
        );
    }

    /// 批次 4(实跑记录第 3 条):`end` 对终态幂等——已完成的任务被 agentend 召回时
    /// status 保持 done、result 不丢,只有真正中途召回才是 ended。
    /// 这修的是「ended 一词同时表示完成后召回与中途中断」的二义。
    #[test]
    fn end_is_idempotent_for_terminal_status_and_stamps_finished_at() {
        let (_dir, s) = svc();
        let r = s.create("task:t1", "", "已完成", "指令").unwrap();
        assert!(r.finished_at.is_empty(), "pending 期间 finished_at 应为空");

        s.set_running(&r.id).unwrap();
        assert!(s.get(&r.id).unwrap().finished_at.is_empty());
        s.set_done(&r.id, "交付物").unwrap();
        let done = s.get(&r.id).unwrap();
        assert!(!done.finished_at.is_empty(), "done 应记 finished_at");
        let finished_at = done.finished_at.clone();

        // 完成后被召回:状态与结果都不动,只回 AlreadyFinished
        assert_eq!(
            s.end(&r.id),
            SubtaskEndOutcome::AlreadyFinished {
                prior_status: "done".into()
            }
        );
        let after = s.get(&r.id).unwrap();
        assert_eq!(after.status, "done", "终态不得被 end 覆盖为 ended");
        assert_eq!(after.result, "交付物", "交付物不得被 end 清空");
        assert_eq!(after.finished_at, finished_at, "已有完成时刻不得被改写");

        // 未命中 id:显式 Missing,不再与成功同形
        assert_eq!(s.end("不存在的-id"), SubtaskEndOutcome::Missing);
    }

    /// 批次 4:error 同为终态,再被召回同样只补时间不改写;DB 路径(relevant)
    #[test]
    fn end_keeps_error_status_and_db_path_stamps_once() {
        let (_dir, s) = svc();
        // DB 路径需要真实会话满足 FK
        let characters = crate::services::character_service::CharacterService::new(
            s.db.clone(),
            _dir.path().to_path_buf(),
        );
        characters.seed_default_character();
        let sessions = crate::services::session_service::SessionService::new(s.db.clone());
        let session = sessions
            .create(crate::services::character_service::BUILTIN_SYSTEM_ID, None)
            .unwrap();
        let r = s.create(&session.id, "", "失败项", "指令").unwrap();
        s.set_running(&r.id).unwrap();
        s.set_error(&r.id, "上游超时").unwrap();
        let failed = s.get(&r.id).unwrap();
        assert_eq!(failed.status, "error");
        assert!(!failed.finished_at.is_empty(), "error 应记 finished_at");
        let finished_at = failed.finished_at.clone();

        assert_eq!(
            s.end(&r.id),
            SubtaskEndOutcome::AlreadyFinished {
                prior_status: "error".into()
            }
        );
        let after = s.get(&r.id).unwrap();
        assert_eq!(after.status, "error");
        assert_eq!(after.error, "上游超时");
        assert_eq!(after.finished_at, finished_at, "重复调用不得改写首次时刻");

        // 再调一次仍幂等(条件 UPDATE 的幂等性)
        assert!(matches!(
            s.end(&r.id),
            SubtaskEndOutcome::AlreadyFinished { .. }
        ));
        assert_eq!(s.get(&r.id).unwrap().finished_at, finished_at);
    }
}
