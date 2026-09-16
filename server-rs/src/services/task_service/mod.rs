// 任务模式(task 工作台)服务:任务主表 CRUD + 后台执行引擎。
// 执行流程:planning(LLM 规划拆解)→ running(逐步派子智能体执行)→ done(LLM 汇总);
// 出错 → error;stop → ended。子任务存 task_subtasks 表(独立于 agent_subtasks,
// 因后者的 session_id 外键指向 sessions 表,而任务 id 不在其中);
// multi/team 经 agentgo 排出的子 agent 记录走 agent_subtask_service 的 task: 前缀
// 内存覆盖层(批次 4.3b,FK 守卫),list_subtasks 读路径合并两者(可观测性问题⑤;
// 覆盖层随进程存活,重启后仅 DB 行可见——既定取舍,见 db.rs::list_subtasks)。
// 模块地图(巨型文件拆分后,纯代码移动,逻辑不变):
//   本文件      TaskService 结构体、任务 CRUD、usage 累计
//   db.rs       任务主表/子任务表读写与 usage 落库
//   events.rs   任务事件 broadcast 通道与发射(WP4 SSE 实时化)
//   cancel.rs   取消信号与执行 token 登记
//   prompt.rs   提示词组装(任务设置/世界书/注入/Agent 系统提示词/人设)
//   executor.rs 后台执行引擎(run/stop、LLM 单次生成原语、后台主体)
use super::log_query_failure;
use crate::connectors::Connector;
use crate::models::db::{now_iso, Db, PooledRead};
use crate::models::types::{
    CharacterRecord, GenerationParams, LlmMessage, LlmStreamChunk, SseEvent, TaskEventKind,
    TaskFollowupMode, TaskLlmCallRecord, TaskMessageRecord, TaskRecord, TaskRunMode, TaskStatus,
    TaskStep, TaskSubtaskRecord, TaskSubtaskStatus, ToolCallArgs, ToolChoice, ToolContext,
    ToolDefinition,
};
use crate::services::agent_flow_service::AgentFlowService;
use crate::services::agent_subtask_service::AgentSubtaskService;
use crate::services::character_service::CharacterService;
use crate::services::prompt_inject_service::PromptInjectService;
use crate::services::settings_service::{AppMode, RuntimeSettings};
use crate::services::world_book_service::WorldBookService;
use rusqlite::{params, OptionalExtension};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, RwLock};
use uuid::Uuid;

// 按职责拆分的子模块(纯代码移动):DB 读写 / 取消信号 / 提示词组装 / 后台执行
pub(crate) mod backend_impl;
pub(crate) mod cancel;
pub(crate) mod db;
pub(crate) mod events;
pub(crate) mod executor;
pub(crate) mod prompt;

use self::db::{row_to_task, TASK_COLS};

/// 任务模式单次 LLM 调用总时长上限(涵盖共享读锁排队 + 整个流式生成)。
/// 超时报错让任务进入 error 终态,防止上游停滞或锁队列饿死导致任务永久停在
/// planning/步骤 running(层内另有 SSE 流空闲看门狗,此为兜底;正常规划/生成
/// 均在数十秒量级,5 分钟上限足够宽裕)。
const TASK_LLM_TOTAL_TIMEOUT: Duration = Duration::from_secs(300);

// 空输出分级重试与规划解析的算法/常量(EMPTY_RETRY_BACKOFF / RETRY_MAX_TOKENS_CAP /
// PLAN_INITIAL_MAX_TOKENS / PLAN_MAX_ATTEMPTS / parse_plan)已于批次 4.2 上移
// task_engine::retry + task_engine::parse(任务引擎职责,非宿主能力)。

/// 规划器只读侦察白名单(问题②,2026-08-31 实测:计划模式下模型只写计划、
/// 不调用工具收集信息,对「测试 agent 框架能力」这类目标只能凭空编造步骤):
/// 规划阶段允许模型先调用只读工具收集与目标相关的信息,再产出计划 JSON。
/// 严禁写操作(write/replace/create/memory_write/update_variables)与编排类
/// (agentgo/agentend)——写操作违背 plan 模式「只规划不执行」零副作用纪律,
/// 编排类会把规划阶段变成实际执行。
/// 常量本体收敛在 tools::tool_sets::READONLY_SCOUT(单一出处,避免多处漂移)。
const PLANNER_SCOUT_TOOLS: &[&str] = crate::tools::tool_sets::READONLY_SCOUT;

/// 规划器侦察轮上限:工具调用最多 2 轮,随后最终轮不带工具强制产出计划 JSON
/// (计划契约不变);侦察是增强环节,轮数封底防模型沉迷收集迟迟不出计划。
const PLANNER_SCOUT_MAX_ROUNDS: usize = 2;

/// 任务模式单次 LLM 调用的完整产出:正文 + 诊断信息(批次 B.3 搬迁至 task_core,
/// 本处再导出保持既有调用方零改动)。
pub(crate) use crate::services::task_core::TaskGenOutput;

pub struct TaskService {
    db: Arc<Db>,
    characters: Arc<CharacterService>,
    connector: Arc<RwLock<Connector>>,
    /// 聊天引擎(solo/plan 等六模式执行器复用 execute_generation/run_tool_loop;
    /// 批次 4.2 注入,engine 不依赖 tasks,无循环)
    engine: Arc<crate::agents::engine::AgentEngine>,
    /// 自定义 Agent 执行流程库(custom 模式读取当前启用流程;批次 4.3b 注入,
    /// 与 AppState 共享同一实例,克隆配置后即释放锁,不持引用跨 .await)
    flow: Arc<Mutex<AgentFlowService>>,
    /// 子智能体任务(stop 时按 task: 前缀结束任务模式子 agent,防孤儿后台任务)
    agent_subtasks: Arc<AgentSubtaskService>,
    /// 运行期设置(读任务模式的生成参数);与 AppState 共享同一实例
    settings: Arc<Mutex<RuntimeSettings>>,
    /// 世界书(注入执行者人设的世界观设定)
    world_books: Arc<WorldBookService>,
    /// 提示词注入配置(简单合成文本 / 复杂 system 楼层)
    prompt_inject: Arc<Mutex<PromptInjectService>>,
    /// 任务 id → (取消信号, 本次执行 token)。token 用于区分同一任务的先后执行:
    /// stop 后立即重跑时,旧后台任务退出不得删除/覆盖新任务的取消条目与状态。
    cancels: Mutex<HashMap<String, (watch::Sender<bool>, u64)>>,
    /// 任务事件广播通道(WP4):DB 写入成功后发射 SseEvent::Task,
    /// GET /api/tasks/events 订阅转发为 SSE
    events: tokio::sync::broadcast::Sender<SseEvent>,
}

impl TaskService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<Db>,
        characters: Arc<CharacterService>,
        connector: Arc<RwLock<Connector>>,
        settings: Arc<Mutex<RuntimeSettings>>,
        world_books: Arc<WorldBookService>,
        prompt_inject: Arc<Mutex<PromptInjectService>>,
        engine: Arc<crate::agents::engine::AgentEngine>,
        flow: Arc<Mutex<AgentFlowService>>,
        agent_subtasks: Arc<AgentSubtaskService>,
    ) -> Self {
        let svc = TaskService {
            db,
            characters,
            connector,
            engine,
            flow,
            agent_subtasks,
            settings,
            world_books,
            prompt_inject,
            cancels: Mutex::new(HashMap::new()),
            events: tokio::sync::broadcast::channel(events::EVENTS_CAPACITY).0,
        };
        // 孤儿任务启动恢复(问题④):遗留 running/planning 任务置 ended 终态
        // + error 文本;DB 写成功后逐个发射 status 事件(WP4 纪律:先写库后发射)
        for id in db::recover_orphan_tasks(&svc.db) {
            svc.emit_event(
                TaskEventKind::Status,
                &id,
                None,
                Some(TaskStatus::Ended),
                Some("服务重启,任务中断".into()),
            );
        }
        svc
    }

    // ===== CRUD =====

    pub fn create(
        &self,
        title: &str,
        character_id: Option<&str>,
        mode: TaskRunMode,
    ) -> Result<TaskRecord, String> {
        let title = title.trim();
        if title.is_empty() {
            return Err("任务目标不能为空".into());
        }
        let id = Uuid::new_v4().to_string();
        let now = now_iso();
        let cid = character_id.map(|s| s.to_string());
        {
            // 写锁作用域:INSERT 完成后立即释放——下方 add_task_message 会再次取
            // db.write(),若仍持锁则自死锁(非重入锁)
            let conn = self.db.write();
            conn.execute(
                "INSERT INTO tasks (id, title, status, plan, result, error, character_id, created_at, updated_at, task_mode) \
                 VALUES (?1, ?2, 'pending', '[]', '', '', ?3, ?4, ?4, ?5)",
                params![id, title, cid, now, mode.as_str()],
            )
            .map_err(|e| format!("创建任务失败: {e}"))?;
        }
        // 用户目标落 task_messages(role=user,kind=goal):任务模式的对话记录区
        // 按 user/assistant 逐轮气泡呈现,目标作为首条用户发言(实跑问题 1)。
        // 落库失败不阻断创建(消息是展示层增强,任务本身已建好)。
        let _ = self.add_task_message(&id, "user", "goal", title);
        self.emit_event(
            TaskEventKind::Created,
            &id,
            Some(title.to_string()),
            Some(TaskStatus::Pending),
            Some("任务已创建".into()),
        );
        Ok(TaskRecord {
            id,
            title: title.to_string(),
            status: TaskStatus::Pending,
            plan: Vec::new(),
            result: String::new(),
            error: String::new(),
            character_id: cid,
            created_at: now.clone(),
            updated_at: now,
            // 创建入口由调用方(API)严格解析用户所选模式;缺省 legacy(行为与旧版一致)
            task_mode: mode,
        })
    }

    pub fn list(&self) -> Vec<TaskRecord> {
        let Some(conn) = read_or_log(&self.db, "任务列表 read") else {
            return Vec::new();
        };
        let mut stmt = match conn.prepare_cached(&format!(
            "SELECT {TASK_COLS} FROM tasks ORDER BY created_at DESC"
        )) {
            Ok(s) => s,
            Err(e) => return log_query_failure("任务列表 prepare", e),
        };
        let query = stmt.query_map([], row_to_task);
        match query {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => log_query_failure("任务列表 query_map", e),
        }
    }

    pub fn get(&self, id: &str) -> Option<TaskRecord> {
        self.db
            .read()
            .ok()?
            .query_row(
                &format!("SELECT {TASK_COLS} FROM tasks WHERE id = ?1"),
                params![id],
                row_to_task,
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn get_with_subtasks(&self, id: &str) -> Option<(TaskRecord, Vec<TaskSubtaskRecord>)> {
        let task = self.get(id)?;
        let subtasks = self.list_subtasks(id);
        Some((task, subtasks))
    }

    /// 删除任务(子任务经外键 ON DELETE CASCADE 一并删除);运行中则先取消。
    pub fn delete(&self, id: &str) -> bool {
        self.signal_cancel(id);
        self.remove_cancel_entry(id);
        let deleted = self
            .db
            .write()
            .execute("DELETE FROM tasks WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false);
        if deleted {
            self.emit_event(
                TaskEventKind::Deleted,
                id,
                None,
                None,
                Some("任务已删除".into()),
            );
        }
        deleted
    }

    /// 单任务 token 累计(prompt / completion / reasoning)。
    pub fn usage_total(&self, task_id: &str) -> (i64, i64, i64) {
        self.db
            .read()
            .ok()
            .and_then(|conn| {
                conn.query_row(
                    "SELECT COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COALESCE(SUM(reasoning_tokens),0)                      FROM task_usage WHERE task_id = ?1",
                    params![task_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .ok()
                .flatten()
            })
            .unwrap_or((0, 0, 0))
    }

    /// 自定义流程库句柄(custom 执行器读取当前启用配置用;批次 4.3b)。
    /// 调用方须 lock 后立即克隆配置并释放 guard,禁持引用跨 .await。
    pub(crate) fn agent_flow(&self) -> Arc<Mutex<AgentFlowService>> {
        self.flow.clone()
    }

    /// 全部任务的 token 累计(侧栏「全局累计」)。
    pub fn all_usage_total(&self) -> (i64, i64, i64) {
        self.db
            .read()
            .ok()
            .and_then(|conn| {
                conn.query_row(
                    "SELECT COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COALESCE(SUM(reasoning_tokens),0)                      FROM task_usage",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .ok()
                .flatten()
            })
            .unwrap_or((0, 0, 0))
    }
}

/// 只读连接获取失败兜底(与 log_query_failure 同款语义,2026-08 裸 unwrap 审计纪律):
/// 记 warn 并返回 None,调用方回退空列表。Db::read 的错误为 String(连接池层),
/// 与 log_query_failure 的 rusqlite::Error 不同源,故单列本 helper。
fn read_or_log(db: &Db, op: &str) -> Option<PooledRead> {
    match db.read() {
        Ok(conn) => Some(conn),
        Err(e) => {
            tracing::warn!(op = op, error = e, "DB 只读连接获取失败,回退空列表");
            None
        }
    }
}
