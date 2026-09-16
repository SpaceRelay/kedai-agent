// 任务主表/子任务表读写:行映射(row_to_task/row_to_subtask)、状态写入
// (reset_task/set_status/set_plan/set_result/set_error)、usage 落库(record_usage)、
// 子任务 CRUD(create_subtask/set_subtask_status/list_subtasks)。
// 自 task_service.rs 拆分迁入,纯代码移动,逻辑不变;依赖经 `use super::*` 取自 mod.rs。
use super::*;

/// 孤儿任务启动恢复(问题④,2026-08-31 实测:服务进程被停后,任务永远停在
/// running 态):上次进程退出时遗留在 running/planning 的任务,执行上下文已随
/// 进程消亡、永不再推进,启动时统一置 ended 终态 + error 文本「服务重启,任务中断」。
/// planned 不动:计划已产出待批准,approve 续跑语义可跨重启存活;
/// pending(未启动)与终态不动。
/// 返回被恢复任务的 id 列表(供 TaskService 在 DB 写成功后逐个发射 status 事件);
/// 幂等:重复执行零命中(状态已是 ended,不再匹配 IN 条件)。
pub(super) fn recover_orphan_tasks(db: &Db) -> Vec<String> {
    let conn = db.write();
    let ids: Vec<String> = match conn
        .prepare_cached("SELECT id FROM tasks WHERE status IN ('running', 'planning') ORDER BY created_at ASC, rowid ASC")
        .map(|mut stmt| {
            stmt.query_map([], |row| row.get(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
        }) {
        Ok(Ok(ids)) => ids,
        Ok(Err(e)) => {
            tracing::warn!(error = e.to_string(), "孤儿任务恢复查询失败");
            return Vec::new();
        }
        Err(e) => {
            tracing::warn!(error = e.to_string(), "孤儿任务恢复 prepare 失败");
            return Vec::new();
        }
    };
    if ids.is_empty() {
        return ids;
    }
    let changed = conn
        .execute(
            "UPDATE tasks SET status = 'ended', error = '服务重启,任务中断', updated_at = ?1 WHERE status IN ('running', 'planning')",
            params![now_iso()],
        )
        .unwrap_or(0);
    if changed > 0 {
        tracing::info!(
            count = changed,
            "服务启动:遗留执行中任务已标记中断(孤儿恢复)"
        );
    }
    ids
}

/// 任务主表列:0 id, 1 title, 2 status, 3 plan, 4 result, 5 error, 6 character_id, 7 created_at, 8 updated_at, 9 task_mode
pub(super) fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<TaskRecord> {
    let plan_str: String = row.get(3)?;
    let plan = serde_json::from_str(&plan_str).unwrap_or_default();
    let status: String = row.get(2)?;
    let task_mode: String = row.get(9)?;
    Ok(TaskRecord {
        id: row.get(0)?,
        title: row.get(1)?,
        // DB 读取容错:未知值记 warn 回退 Pending,不 panic 不丢行
        status: TaskStatus::from_str_lossy(&status),
        plan,
        result: row.get(4)?,
        error: row.get(5)?,
        character_id: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        // DB 读取容错:未知模式记 warn 回退 Legacy(与 status 同款约定)
        task_mode: TaskRunMode::from_str_lossy(&task_mode),
    })
}

pub(super) const TASK_COLS: &str =
    "id, title, status, plan, result, error, character_id, created_at, updated_at, task_mode";

/// 按字符截断(中文安全,不切 char 边界;调用追踪摘要用)
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// 任务消息行映射(批次 R2):role/kind 原样透传(CHECK 约束由建表保证;
/// 读取侧宽容,旧行默认 kind='normal' 由迁移 DEFAULT 兜底)。
fn row_to_task_message(row: &rusqlite::Row) -> rusqlite::Result<TaskMessageRecord> {
    Ok(TaskMessageRecord {
        id: row.get(0)?,
        task_id: row.get(1)?,
        role: row.get(2)?,
        kind: row.get(3)?,
        content: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn row_to_subtask(row: &rusqlite::Row) -> rusqlite::Result<TaskSubtaskRecord> {
    let status: String = row.get(4)?;
    Ok(TaskSubtaskRecord {
        id: row.get(0)?,
        task_id: row.get(1)?,
        name: row.get(2)?,
        instruction: row.get(3)?,
        // DB 读取容错:未知值记 warn 回退 Pending,不 panic 不丢行
        status: TaskSubtaskStatus::from_str_lossy(&status),
        result: row.get(5)?,
        error: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        finished_at: row.get(9)?,
    })
}

impl TaskService {
    /// 同步落库统一让出点(2026-09-16 性能批次 P-6)。
    ///
    /// 任务引擎经 `tokio::spawn` 跑在 async worker 上,而这些方法做的是同步 rusqlite
    /// 写(唯一写连接 + busy_timeout 最长 5s + fsync),直接执行会卡住整个 worker,
    /// 连带该 worker 上排队的其他请求一起停摆。语义与判定规则见 `utils::blocking`。
    /// 全部写方法都经此包装,是「任务侧与聊天侧落库纪律一致」的单点保证
    /// (聊天侧同类写入早用 spawn_blocking,见 engine/run_finish.rs:94)。
    ///
    /// 注意:`record_self_heals` 不包——它不直接取写锁,而是委托给下方已包装的
    /// `record_llm_call`/`record_usage`;包了只会形成多余的嵌套。
    #[inline]
    fn blocking<T>(f: impl FnOnce() -> T) -> T {
        crate::utils::blocking::park_worker(f)
    }

    // ===== 状态写入(内部) =====

    pub(super) fn reset_task(&self, id: &str) -> bool {
        Self::blocking(|| {
            let conn = self.db.write();
            let changed = conn
                .execute(
                    "UPDATE tasks SET status = 'planning', plan = '[]', result = '', error = '', updated_at = ?1 WHERE id = ?2",
                    params![now_iso(), id],
                )
                .map(|n| n > 0)
                .unwrap_or(false);
            if changed {
                // 重跑入口即进入规划态,与 set_status 同款 kind="status" 事件
                self.emit_event(
                    TaskEventKind::Status,
                    id,
                    None,
                    Some(TaskStatus::Planning),
                    Some("任务进入规划阶段".into()),
                );
            }
            changed
        })
    }

    /// pub(crate):任务引擎(task_engine)solo/plan 执行器推进任务状态用。
    pub(crate) fn set_status(&self, id: &str, status: TaskStatus) -> bool {
        Self::blocking(|| {
            let conn = self.db.write();
            let changed = conn
                .execute(
                    "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
                    params![status.as_str(), now_iso(), id],
                )
                .map(|n| n > 0)
                .unwrap_or(false);
            if changed {
                self.emit_event(
                    TaskEventKind::Status,
                    id,
                    None,
                    Some(status),
                    Some(format!("任务状态更新为 {}", status.as_str())),
                );
            }
            changed
        })
    }

    /// pub(crate):任务引擎 plan 执行器落库计划用(写库成功后发射 kind=plan 事件)。
    pub(crate) fn set_plan(&self, id: &str, plan: &[TaskStep]) -> bool {
        Self::blocking(|| {
            let json = serde_json::to_string(plan).unwrap_or_else(|_| "[]".into());
            let conn = self.db.write();
            let changed = conn
                .execute(
                    "UPDATE tasks SET plan = ?1, updated_at = ?2 WHERE id = ?3",
                    params![json, now_iso(), id],
                )
                .map(|n| n > 0)
                .unwrap_or(false);
            if changed {
                self.emit_event(
                    TaskEventKind::Plan,
                    id,
                    None,
                    None,
                    Some(format!("执行计划已更新(共 {} 步)", plan.len())),
                );
            }
            changed
        })
    }

    /// 写入最终结果并置终态:全部步骤成功为 done;含 error 步骤为 partial(部分完成)。
    pub(super) fn set_result(&self, id: &str, result: &str, status: TaskStatus) -> bool {
        self.set_result_with_error(id, result, status, None)
    }

    /// set_result 的可解释版本:partial 终态可携带原因文本写入 tasks.error(供前端
    /// 状态行展示「为什么不是完成」);error 为 None 时清空旧 error(重跑成功后不残留
    /// 上一轮的错误提示)。其余语义与 set_result 一致。
    pub(super) fn set_result_with_error(
        &self,
        id: &str,
        result: &str,
        status: TaskStatus,
        error: Option<&str>,
    ) -> bool {
        Self::blocking(|| {
            let conn = self.db.write();
            let changed = conn
                .execute(
                    "UPDATE tasks SET result = ?1, status = ?2, error = ?3, updated_at = ?4 WHERE id = ?5",
                    params![
                        result,
                        status.as_str(),
                        error.unwrap_or(""),
                        now_iso(),
                        id
                    ],
                )
                .map(|n| n > 0)
                .unwrap_or(false);
            drop(conn);
            if changed {
                self.emit_event(
                    TaskEventKind::Status,
                    id,
                    None,
                    Some(status),
                    Some("任务已产出最终结果".into()),
                );
            }
            changed
        })
    }

    /// planned 态写入计划清单文本(批次 R1,plan 模式):只更新 result 列,不动状态
    /// (planned 终态由 PlanExecutor 先行写入,本方法随其后)。
    /// planned 态 result 的语义是「待批准的计划清单」:供用户在批准前预览完整计划;
    /// 批准续跑完成后由终态 result(汇总文本 + 「## 最终计划」段)整体覆盖。
    /// 写库成功后发射既有 kind="status" 事件(detail 与终态 set_result 文案区分;
    /// 不引入新事件 kind;WP4 纪律:仅 DB 写成功后发射)。
    pub(crate) fn set_planned_result(&self, id: &str, result: &str) -> bool {
        Self::blocking(|| {
            let conn = self.db.write();
            let changed = conn
                .execute(
                    "UPDATE tasks SET result = ?1, updated_at = ?2 WHERE id = ?3",
                    params![result, now_iso(), id],
                )
                .map(|n| n > 0)
                .unwrap_or(false);
            if changed {
                self.emit_event(
                    TaskEventKind::Status,
                    id,
                    None,
                    Some(TaskStatus::Planned),
                    Some("计划清单已写入,待批准".into()),
                );
            }
            changed
        })
    }

    pub(super) fn set_error(&self, id: &str, error: &str) -> bool {
        Self::blocking(|| {
            let conn = self.db.write();
            let changed = conn
                .execute(
                    "UPDATE tasks SET error = ?1, status = 'error', updated_at = ?2 WHERE id = ?3",
                    params![error, now_iso(), id],
                )
                .map(|n| n > 0)
                .unwrap_or(false);
            if changed {
                // detail 截断防超长错误文本撑大事件帧
                let detail: String = error.chars().take(120).collect();
                self.emit_event(
                    TaskEventKind::Status,
                    id,
                    None,
                    Some(TaskStatus::Error),
                    Some(detail),
                );
            }
            changed
        })
    }

    // ===== 任务 usage(WP4)=====

    /// 任务模式 usage 落库:每次 LLM 调用(规划/步骤/汇总)写一行 task_usage。
    /// model 取合并设置后的有效模型(与设置页口径一致;启动日志 env model 可能失真,见 WP6)。
    /// pub(crate):任务引擎(task_engine)六模式执行器统一经本出口落 usage(批次 4.3b;
    /// phase 对齐 legacy 口径:planner/agent/subagent/step/audit/summary,面板按 task_id 求和)。
    pub(crate) fn record_usage(
        &self,
        task_id: &str,
        phase: &str,
        step_index: Option<usize>,
        out: &TaskGenOutput,
    ) {
        Self::blocking(|| {
            let model = self.task_settings().model;
            let conn = self.db.write();
            let result = conn.execute(
                "INSERT INTO task_usage (id, task_id, phase, step_index, model, prompt_tokens, completion_tokens, reasoning_tokens, created_at)              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    Uuid::new_v4().to_string(),
                    task_id,
                    phase,
                    step_index.map(|i| i as i64),
                    model,
                    out.prompt_tokens,
                    out.completion_tokens,
                    out.reasoning_tokens,
                    now_iso(),
                ],
            );
            if let Err(e) = result {
                tracing::warn!(
                    op = "task_usage insert",
                    error = e.to_string(),
                    "任务 usage 落库失败"
                );
            } else {
                self.emit_event(
                    TaskEventKind::Usage,
                    task_id,
                    None,
                    None,
                    Some(format!(
                        "已记录 {phase} 阶段 token 用量(prompt {} / completion {})",
                        out.prompt_tokens, out.completion_tokens
                    )),
                );
            }
        })
    }

    // ===== 任务 LLM 调用追踪(批次 3)=====

    /// 任务侧每次 LLM 调用落一行 task_llm_calls(提示词/响应摘要 + 耗时 + 状态);
    /// 写库成功后发射 kind=llm_call 事件(WP4 纪律:仅 DB 写入成功后发射)。
    /// out 为 None 表示调用失败(超时/上游错误),token 记 0。
    /// finish_reason(可观测性问题①):取自 TaskGenOutput(流式 Finish 块聚合),
    /// None/空 = 未知,落库为 ''(旧行兼容);事件同步携带('' 省略字段)。
    /// 摘要截断:每条消息 role + 正文截 800 字符,整体 4000;响应截 2000。
    /// pub(crate):任务引擎(task_engine)solo 等执行器同样经本统一出口落调用追踪。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_llm_call(
        &self,
        task_id: &str,
        phase: &str,
        step_index: Option<usize>,
        model: &str,
        messages: &[LlmMessage],
        response: &str,
        out: Option<&TaskGenOutput>,
        elapsed: Duration,
        status: &str,
    ) {
        Self::blocking(|| {
            let mut prompt_summary = String::new();
            for m in messages {
                if !prompt_summary.is_empty() {
                    prompt_summary.push('\n');
                }
                prompt_summary.push_str(&m.role);
                prompt_summary.push_str(": ");
                prompt_summary.push_str(&truncate_chars(&m.content, 800));
            }
            let (p, c, r) = out
                .map(|o| (o.prompt_tokens, o.completion_tokens, o.reasoning_tokens))
                .unwrap_or((0, 0, 0));
            // finish_reason:None(失败/工具循环旧路径/上游未下发)与 Some(空串)统一落 ''
            let finish_reason = out.and_then(|o| o.finish_reason.as_deref()).unwrap_or("");
            let conn = self.db.write();
            let result = conn.execute(
                "INSERT INTO task_llm_calls (id, task_id, phase, step_index, model, prompt_summary, response_summary, prompt_tokens, completion_tokens, reasoning_tokens, elapsed_ms, status, created_at, finish_reason) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    Uuid::new_v4().to_string(),
                    task_id,
                    phase,
                    step_index.map(|i| i as i64),
                    model,
                    truncate_chars(&prompt_summary, 4000),
                    truncate_chars(response, 2000),
                    p,
                    c,
                    r,
                    elapsed.as_millis() as i64,
                    status,
                    now_iso(),
                    finish_reason,
                ],
            );
            if let Err(e) = result {
                tracing::warn!(
                    op = "task_llm_calls insert",
                    error = e.to_string(),
                    "任务 LLM 调用追踪落库失败"
                );
            } else {
                let step = step_index
                    .map(|i| format!(" #{}", i + 1))
                    .unwrap_or_default();
                // phase/step_index 随事件透出(批次 R4):前端据此清对应流式缓冲
                self.emit_llm_call(
                    task_id,
                    phase,
                    step_index,
                    format!("{phase}{step} · {model} · {} tokens", p + c),
                    Some(finish_reason.to_string()),
                );
            }
        })
    }

    /// 截断自愈留痕批量落库(问题①的单一实现):solo.rs 与 custom.rs 各自复用过
    /// 一段逐行同构的循环,现收拢于此——被截断的那次调用补落一行 status=error
    /// (response_summary 标注触发原因与重发预算),并补落其 usage。
    ///
    /// 为何要补 usage:被截断那次同样消耗 token,只记最终行会让 usage_total
    /// 少于调用明细求和(2026-09-10 实测修复口径)。
    /// 调用情况面板据此看到完整「截断 → 提高预算重发」链路,实际调用次数可考。
    ///
    /// 批次 4.2:入参为 task_core 中性 DTO `TruncationHeal`(不再引用 agents 层
    /// SelfHealRecord),转换在 task_engine 边界完成(见 task_engine::executor::to_self_heals)。
    pub(crate) fn record_self_heals(
        &self,
        task_id: &str,
        phase: &str,
        step_index: Option<usize>,
        model: &str,
        messages: &[LlmMessage],
        self_heals: &[crate::services::task_core::TruncationHeal],
    ) {
        for heal in self_heals {
            let heal_out = TaskGenOutput {
                text: String::new(),
                finish_reason: heal.finish_reason.clone(),
                prompt_tokens: heal.prompt_tokens,
                completion_tokens: heal.completion_tokens,
                reasoning_tokens: 0,
                reasoning_chars: 0,
                tool_calls: Vec::new(),
            };
            self.record_llm_call(
                task_id,
                phase,
                step_index,
                model,
                messages,
                &format!(
                    "(截断自愈){},输出上限翻倍至 {} 重发",
                    heal.note, heal.retried_max_tokens
                ),
                Some(&heal_out),
                Duration::ZERO,
                "error",
            );
            self.record_usage(task_id, phase, step_index, &heal_out);
        }
    }

    /// 调用追踪全量拉取(「调用情况」面板打开/事件重连时补拉),按发生顺序升序。
    pub(crate) fn list_llm_calls(&self, task_id: &str) -> Vec<TaskLlmCallRecord> {
        let Some(conn) = read_or_log(&self.db, "任务调用追踪 read") else {
            return Vec::new();
        };
        let mut stmt = match conn.prepare_cached(
            "SELECT id, task_id, phase, step_index, model, prompt_summary, response_summary, prompt_tokens, completion_tokens, reasoning_tokens, elapsed_ms, status, created_at, finish_reason \
                 FROM task_llm_calls WHERE task_id = ?1 ORDER BY created_at ASC, rowid ASC",
        ) {
            Ok(s) => s,
            Err(e) => return log_query_failure("任务调用追踪 prepare", e),
        };
        let query = stmt.query_map(params![task_id], |row| {
            Ok(TaskLlmCallRecord {
                id: row.get(0)?,
                task_id: row.get(1)?,
                phase: row.get(2)?,
                step_index: row.get(3)?,
                model: row.get(4)?,
                prompt_summary: row.get(5)?,
                response_summary: row.get(6)?,
                prompt_tokens: row.get(7)?,
                completion_tokens: row.get(8)?,
                reasoning_tokens: row.get(9)?,
                elapsed_ms: row.get(10)?,
                status: row.get(11)?,
                created_at: row.get(12)?,
                // 列由迁移保证存在(NOT NULL DEFAULT '');旧行读出 '' = 未知
                finish_reason: row.get(13)?,
            })
        });
        match query {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => log_query_failure("任务调用追踪 query_map", e),
        }
    }

    // ===== 任务消息(批次 R2:followup 追加指令 / plan_chat 规划对话) =====

    /// 任务消息落库:用户追加指令/规划对话的双方发言持久化(task_messages 表)。
    /// 落库成功即返回记录;不发射事件——消息增量经 status/plan 事件驱动前端
    /// 重拉详情(GET 响应携带 messages 全量)对齐,不引入新事件 kind。
    pub(crate) fn add_task_message(
        &self,
        task_id: &str,
        role: &str,
        kind: &str,
        content: &str,
    ) -> Result<TaskMessageRecord, String> {
        Self::blocking(|| {
            let record = TaskMessageRecord {
                id: Uuid::new_v4().to_string(),
                task_id: task_id.to_string(),
                role: role.to_string(),
                kind: kind.to_string(),
                content: content.to_string(),
                created_at: now_iso(),
            };
            let conn = self.db.write();
            conn.execute(
                "INSERT INTO task_messages (id, task_id, role, kind, content, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    record.id,
                    record.task_id,
                    record.role,
                    record.kind,
                    record.content,
                    record.created_at
                ],
            )
            .map_err(|e| format!("任务消息落库失败: {e}"))?;
            Ok(record)
        })
    }

    /// 任务消息全量列表(详情响应 messages 字段),按发生顺序升序
    /// (created_at 同毫秒时 rowid 兜底,与调用追踪同口径)。
    pub(crate) fn list_task_messages(&self, task_id: &str) -> Vec<TaskMessageRecord> {
        let Some(conn) = read_or_log(&self.db, "任务消息 read") else {
            return Vec::new();
        };
        let mut stmt = match conn.prepare_cached(
            "SELECT id, task_id, role, kind, content, created_at \
                 FROM task_messages WHERE task_id = ?1 ORDER BY created_at ASC, rowid ASC",
        ) {
            Ok(s) => s,
            Err(e) => return log_query_failure("任务消息 prepare", e),
        };
        // query_map 结果先绑定局部变量再 match(与 list_llm_calls 同口径):
        // 尾表达式直接 match 会触发 E0597(MappedRows 借用 stmt 的临时值 drop 顺序)
        let query = stmt.query_map(params![task_id], row_to_task_message);
        match query {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => log_query_failure("任务消息 query_map", e),
        }
    }

    /// 第 N 次追加的序号:kind=followup 的 user 消息计数。
    /// 约定在用户指令落库之后调用,返回值含本条在内(首轮追加 = 1)。
    pub(crate) fn followup_count(&self, task_id: &str) -> usize {
        let Some(conn) = read_or_log(&self.db, "追加计数 read") else {
            return 0;
        };
        conn.query_row(
            "SELECT COUNT(*) FROM task_messages WHERE task_id = ?1 AND kind = 'followup' AND role = 'user'",
            params![task_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n as usize)
        .unwrap_or(0)
    }

    // ===== 子任务 =====

    pub(crate) fn create_subtask(
        &self,
        task_id: &str,
        name: &str,
        instruction: &str,
    ) -> Result<String, String> {
        Self::blocking(|| {
            let id = Uuid::new_v4().to_string();
            let now = now_iso();
            let conn = self.db.write();
            conn.execute(
                "INSERT INTO task_subtasks (id, task_id, name, instruction, status, result, error, created_at, updated_at, finished_at) \
                 VALUES (?1, ?2, ?3, ?4, 'running', '', '', ?5, ?5, '')",
                params![id, task_id, name, instruction, now],
            )
            .map_err(|e| format!("创建子任务失败: {e}"))?;
            self.emit_event(
                TaskEventKind::Subtask,
                task_id,
                None,
                None,
                Some(format!("子任务「{name}」开始执行")),
            );
            Ok(id)
        })
    }

    pub(crate) fn set_subtask_status(
        &self,
        id: &str,
        status: TaskSubtaskStatus,
        result: Option<&str>,
        error: Option<&str>,
    ) -> bool {
        Self::blocking(|| {
            let conn = self.db.write();
            // 保持未提供字段不变:先读旧值(顺带取 task_id 供事件发射,避免额外查库)
            // finished_at 一并读出:终态只记首次(见下),不能无条件 now_iso() 覆盖
            let existing = conn
                .query_row(
                    "SELECT task_id, result, error, finished_at FROM task_subtasks WHERE id = ?1",
                    params![id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()
                .ok()
                .flatten();
            let (task_id, res, err, finished) = existing
                .map(|(t, r, e, f)| (Some(t), r, e, f))
                .unwrap_or((None, String::new(), String::new(), String::new()));
            let res = result.map(|s| s.to_string()).unwrap_or(res);
            let err = error.map(|s| s.to_string()).unwrap_or(err);
            // 首次进入终态才记 finished_at;pending/running 保持空串,重复置终态不改写首值
            let finished_at = if status.is_terminal() && finished.is_empty() {
                now_iso()
            } else {
                finished
            };
            let changed = conn
                .execute(
                    "UPDATE task_subtasks SET status = ?1, result = ?2, error = ?3, updated_at = ?4, finished_at = ?5 WHERE id = ?6",
                    params![status.as_str(), res, err, now_iso(), finished_at, id],
                )
                .map(|n| n > 0)
                .unwrap_or(false);
            if changed {
                if let Some(tid) = task_id {
                    self.emit_event(
                        TaskEventKind::Subtask,
                        &tid,
                        None,
                        None,
                        Some(format!("子任务状态更新为 {}", status.as_str())),
                    );
                }
            }
            changed
        })
    }

    /// 子任务列表(读路径合并,可观测性问题⑤):DB(task_subtasks 表,legacy
    /// 逐步执行的子任务) + agent_subtask_service 内存覆盖层(multi/team 经 agentgo
    /// 排出的子 agent,task:{id} / task:{id}:main:{n} 虚拟 session 记录,批次 4.3b
    /// 起因 FK 守卫不落 agent_subtasks 表)。同一 id 覆盖层优先;合并后按
    /// created_at 升序(+id 字典序兜底,与同毫秒创建的团队子 agent 排序稳定)。
    /// 既定取舍:覆盖层随进程存活,重启后内存记录丢失,subtasks 仅剩 DB 行可见
    ///(multi/team 子 agent 为进程内执行单元,重启即中断,无持久化价值)。
    pub(super) fn list_subtasks(&self, task_id: &str) -> Vec<TaskSubtaskRecord> {
        // 覆盖层记录先收(task: 前缀虚拟 session 全部子 agent);
        // AgentSubtaskRecord → TaskSubtaskRecord 形状映射(status 走容错解析)
        let overlay: Vec<TaskSubtaskRecord> = self
            .agent_subtasks
            .list_by_session_prefix(&format!("task:{task_id}"))
            .into_iter()
            .map(|r| TaskSubtaskRecord {
                id: r.id,
                task_id: task_id.to_string(),
                name: r.name,
                instruction: r.instruction,
                status: TaskSubtaskStatus::from_str_lossy(&r.status),
                result: r.result,
                error: r.error,
                created_at: r.created_at,
                updated_at: r.updated_at,
                finished_at: r.finished_at,
            })
            .collect();
        let overlay_ids: std::collections::HashSet<String> =
            overlay.iter().map(|r| r.id.clone()).collect();

        let mut out = overlay;
        let Some(conn) = read_or_log(&self.db, "子任务列表 read") else {
            return out;
        };
        let mut stmt = match conn.prepare_cached(
            "SELECT id, task_id, name, instruction, status, result, error, created_at, updated_at, finished_at \
                 FROM task_subtasks WHERE task_id = ?1 ORDER BY created_at ASC",
        ) {
            Ok(s) => s,
            Err(e) => return log_query_failure("子任务列表 prepare", e),
        };
        let query = stmt.query_map(params![task_id], row_to_subtask);
        match query {
            Ok(rows) => {
                // 同一 id 覆盖层优先:DB 行与覆盖层冲突时丢弃 DB 行
                out.extend(
                    rows.filter_map(|r| r.ok())
                        .filter(|r| !overlay_ids.contains(r.id.as_str())),
                );
            }
            Err(e) => return log_query_failure("子任务列表 query_map", e),
        }
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_support::TempDataDir;

    /// 孤儿任务启动恢复(问题④):遗留 running/planning 置 ended + error 文本
    /// 「服务重启,任务中断」;planned(待批准可续跑)/pending/终态不动;
    /// 幂等(重复执行零命中);模拟「重开」(drop 后重新 open 同一库文件)后
    /// 再插一行 running 仍能恢复——恢复语义跨进程成立。
    #[test]
    fn recover_orphan_tasks_marks_interrupted_once() {
        let dir = TempDataDir::new("task-recover");
        let db_path = dir.join("kedai.db");
        {
            let db = Db::open(&db_path, &dir).expect("开库失败");
            {
                let conn = db.write();
                for (id, status) in [
                    ("t-running", "running"),
                    ("t-planning", "planning"),
                    ("t-planned", "planned"),
                    ("t-pending", "pending"),
                    ("t-done", "done"),
                ] {
                    conn.execute(
                        "INSERT INTO tasks (id, title, status, created_at, updated_at) \
                         VALUES (?1, '目标', ?2, 'c', 'u')",
                        params![id, status],
                    )
                    .unwrap();
                }
            }
            let mut ids = recover_orphan_tasks(&db);
            ids.sort();
            assert_eq!(
                ids,
                vec!["t-planning".to_string(), "t-running".to_string()],
                "running/planning 应被恢复"
            );
            let status_error_of = |db: &Db, id: &str| -> (String, String) {
                db.read()
                    .unwrap()
                    .query_row(
                        "SELECT status, error FROM tasks WHERE id = ?1",
                        params![id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap()
            };
            let (st, err) = status_error_of(&db, "t-running");
            assert_eq!(st, "ended", "running 应置 ended");
            assert!(err.contains("服务重启"), "error 文本应标注中断原因: {err}");
            assert_eq!(status_error_of(&db, "t-planning").0, "ended");
            // planned/pending/done 不动
            assert_eq!(
                status_error_of(&db, "t-planned").0,
                "planned",
                "planned 待批准可跨重启续跑,不动"
            );
            assert_eq!(status_error_of(&db, "t-pending").0, "pending");
            assert_eq!(status_error_of(&db, "t-done").0, "done");
            // 幂等:第二次执行零命中
            assert!(recover_orphan_tasks(&db).is_empty(), "重复执行应零命中");
        }
        // 重开(模拟服务重启):同一库文件再 open,既有行不重复恢复;
        // 新插一行 running → 恢复 → 已终结(「插一行 running → 重开 → 断言已终结」)
        let db2 = Db::open(&db_path, &dir).expect("重开库失败");
        assert!(
            recover_orphan_tasks(&db2).is_empty(),
            "重开后不应重复恢复既有行"
        );
        db2.write()
            .execute(
                "INSERT INTO tasks (id, title, status, created_at, updated_at) \
                 VALUES ('t-orphan', '孤儿', 'running', 'c', 'u')",
                [],
            )
            .unwrap();
        assert_eq!(recover_orphan_tasks(&db2), vec!["t-orphan".to_string()]);
        let (st, err): (String, String) = db2
            .read()
            .unwrap()
            .query_row(
                "SELECT status, error FROM tasks WHERE id = 't-orphan'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(st, "ended", "重开后新孤儿应被终结");
        assert!(err.contains("服务重启"), "error 文本应标注中断原因: {err}");
        drop(db2);
    }
}
