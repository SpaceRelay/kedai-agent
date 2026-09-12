// 幂等 DDL 升级:新增表 DDL 常量与 ensure_* 补列/补表函数(缺失才执行,已存在跳过)。
use rusqlite::Connection;

/// 计划二新增的 7 作用域变量表。合并前对两侧各补一次 DDL,避免旧版本数据库
/// (无此表)与新版本数据库合并时因「基线缺少源表」停止(§4.3)。
pub(super) const SCOPE_VARIABLES_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS scope_variables (
  scope      TEXT NOT NULL,
  scope_id   TEXT NOT NULL DEFAULT '',
  data_raw   TEXT NOT NULL DEFAULT '{}',
  updated_at TEXT NOT NULL,
  PRIMARY KEY (scope, scope_id)
)"#;
/// 阶段三新增的用户脚本表(ScriptTree 全局脚本)。合并前对两侧各补一次 DDL,
/// 与 SCOPE_VARIABLES_DDL 同理,避免旧库与新库合并时因「基线缺少源表」停止(§4.3)。
pub(super) const USER_SCRIPTS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS user_scripts (
  id         TEXT PRIMARY KEY,
  scope      TEXT NOT NULL,
  owner_id   TEXT NOT NULL DEFAULT '',
  data_json  TEXT NOT NULL DEFAULT '[]',
  updated_at TEXT NOT NULL,
  UNIQUE (scope, owner_id)
)"#;
/// 上下文压缩摘要表。合并前对两侧各补一次 DDL,与 SCOPE_VARIABLES_DDL 同理,
/// 避免旧库与新库合并时因「基线缺少源表」停止(§4.3)。
pub(super) const SESSION_COMPACTIONS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS session_compactions (
  session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  upto_message_id INTEGER NOT NULL,
  summary         TEXT NOT NULL DEFAULT '',
  model           TEXT NOT NULL DEFAULT '',
  created_at      TEXT NOT NULL,
  PRIMARY KEY (session_id, upto_message_id)
)"#;
/// LLM 请求快照表(第四点·主题 A):记录每次真正下发给模型的完整消息数组。
/// 合并前对两侧各补一次 DDL,避免旧库与新库合并时因「基线缺少源表」停止(§4.3)。
pub(super) const LLM_REQUESTS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS llm_requests (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  run_id      TEXT NOT NULL,
  seq         INTEGER NOT NULL,
  payload     TEXT NOT NULL,
  model       TEXT NOT NULL DEFAULT '',
  created_at  TEXT NOT NULL
)"#;
/// llm_requests 的 usage 缓存列(2026-08 缓存感知管线):(列名, DDL 片段)。
/// 旧库经 ALTER ADD COLUMN 补齐;列追加在表尾,与新版 CREATE_TABLES 建出的
/// schema normalize 后一致,保证跨库合并的 schema 一致性比对不冲突。
const LLM_REQUESTS_USAGE_COLUMNS: [(&str, &str); 4] = [
    (
        "prompt_cache_hit_tokens",
        "prompt_cache_hit_tokens INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "prompt_cache_miss_tokens",
        "prompt_cache_miss_tokens INTEGER NOT NULL DEFAULT 0",
    ),
    ("prompt_tokens", "prompt_tokens INTEGER NOT NULL DEFAULT 0"),
    (
        "completion_tokens",
        "completion_tokens INTEGER NOT NULL DEFAULT 0",
    ),
];

/// 幂等 schema 升级:为 llm_requests 补 usage 缓存列(缺失才 ALTER,已存在跳过)。
/// 启动时(Db::open)与跨库合并前(merge_databases 两侧)各执行一次。
pub fn ensure_llm_requests_usage_columns(conn: &Connection) -> Result<(), String> {
    let mut existing: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("PRAGMA table_info(llm_requests)")
            .map_err(|e| format!("读取 llm_requests 列失败: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("遍历 llm_requests 列失败: {e}"))?;
        for name in rows.flatten() {
            existing.push(name.to_lowercase());
        }
    }
    for (name, ddl) in LLM_REQUESTS_USAGE_COLUMNS {
        if existing.iter().any(|c| c == name) {
            continue;
        }
        conn.execute(&format!("ALTER TABLE llm_requests ADD COLUMN {ddl}"), [])
            .map_err(|e| format!("为 llm_requests 补列 {name} 失败: {e}"))?;
    }
    Ok(())
}

/// skills 渐进披露列(落地项 3):(列名, DDL 片段)。旧库经 ALTER ADD COLUMN 补齐;
/// 列追加在表尾,与新版 CREATE_TABLES 建出的 schema normalize 后一致。
const SKILLS_PROGRESSIVE_COLUMNS: [(&str, &str); 3] = [
    ("allowed_tools", "allowed_tools TEXT NOT NULL DEFAULT '[]'"),
    (
        "run_as_subagent",
        "run_as_subagent INTEGER NOT NULL DEFAULT 0",
    ),
    ("model", "model TEXT NOT NULL DEFAULT ''"),
];

/// 幂等 schema 升级:为 skills 补渐进披露列(缺失才 ALTER,已存在跳过)。
/// 启动时(Db::open)与跨库合并前(merge_databases 两侧)各执行一次。
/// skills 表不存在(极旧库/测试手工建库)时零列返回,直接跳过——
/// 建表由 create_tables_sql 或合并期 schema 比对负责,此处不能 ALTER 报错。
pub fn ensure_skills_progressive_columns(conn: &Connection) -> Result<(), String> {
    let mut existing: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("PRAGMA table_info(skills)")
            .map_err(|e| format!("读取 skills 列失败: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("遍历 skills 列失败: {e}"))?;
        for name in rows.flatten() {
            existing.push(name.to_lowercase());
        }
    }
    if existing.is_empty() {
        return Ok(());
    }
    for (name, ddl) in SKILLS_PROGRESSIVE_COLUMNS {
        if existing.iter().any(|c| c == name) {
            continue;
        }
        conn.execute(&format!("ALTER TABLE skills ADD COLUMN {ddl}"), [])
            .map_err(|e| format!("为 skills 补列 {name} 失败: {e}"))?;
    }
    Ok(())
}
/// 幂等 schema 升级(批次 4 六模式):为 tasks 补 task_mode 列(缺失才 ALTER,
/// 已存在跳过)。启动时(Db::open)执行;旧行经 DEFAULT 'legacy' 零迁移成本。
/// 列追加在表尾,与新版 CREATE_TABLES 建出的 schema normalize 后一致。
pub fn ensure_tasks_task_mode_column(conn: &Connection) -> Result<(), String> {
    let mut existing: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("PRAGMA table_info(tasks)")
            .map_err(|e| format!("读取 tasks 列失败: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("遍历 tasks 列失败: {e}"))?;
        for name in rows.flatten() {
            existing.push(name.to_lowercase());
        }
    }
    // tasks 表不存在(极旧快照/手工建库)时零列返回,直接跳过——
    // 建表由 CREATE_TABLES 或合并期 schema 比对负责,此处不能 ALTER 报错
    if existing.is_empty() {
        return Ok(());
    }
    if existing.iter().any(|c| c == "task_mode") {
        return Ok(());
    }
    conn.execute(
        "ALTER TABLE tasks ADD COLUMN task_mode TEXT NOT NULL DEFAULT 'legacy'",
        [],
    )
    .map_err(|e| format!("为 tasks 补 task_mode 列失败: {e}"))?;
    Ok(())
}

/// 幂等 schema 升级(可观测性问题①):为 task_llm_calls 补 finish_reason 列
///(缺失才 ALTER,已存在跳过)。启动时(Db::open)与跨库合并前(merge_databases
/// 两侧)各执行一次;旧行经 DEFAULT '' 零迁移成本('' = 未知/未下发,
/// 不等于 stop,避免旧数据被误读为正常收尾)。
/// 列追加在表尾,与新版 CREATE_TABLES 建出的 schema normalize 后一致。
/// task_llm_calls 表不存在(极旧快照/手工建库)时零列返回,直接跳过——
/// 建表由 create_tables_sql 或合并期 schema 比对负责,此处不能 ALTER 报错。
pub fn ensure_task_llm_calls_finish_reason_column(conn: &Connection) -> Result<(), String> {
    let mut existing: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("PRAGMA table_info(task_llm_calls)")
            .map_err(|e| format!("读取 task_llm_calls 列失败: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("遍历 task_llm_calls 列失败: {e}"))?;
        for name in rows.flatten() {
            existing.push(name.to_lowercase());
        }
    }
    if existing.is_empty() {
        return Ok(());
    }
    if existing.iter().any(|c| c == "finish_reason") {
        return Ok(());
    }
    conn.execute(
        "ALTER TABLE task_llm_calls ADD COLUMN finish_reason TEXT NOT NULL DEFAULT ''",
        [],
    )
    .map_err(|e| format!("为 task_llm_calls 补 finish_reason 列失败: {e}"))?;
    Ok(())
}

/// 契约变更历史表(阶段 C):append-only 审计 + 回滚源。合并前对两侧各补一次 DDL,
/// 与 SCOPE_VARIABLES_DDL 同理,避免旧库与新库合并时因「基线缺少源表」停止(§4.3)。
/// 索引不参与 schema 一致性比对(schema_map 仅读 type='table'),无需在此重复。
/// 注意:seq 仅库内局部有效——跨库合并(merge_databases)时主键冲突会被重映射,
/// rationale 中「回滚自 seq N」仅作提示,合并后时间线以 created_at 为准。
pub(super) const CONTRACT_CHANGELOG_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS contract_changelog (
  seq          INTEGER PRIMARY KEY AUTOINCREMENT,
  character_id TEXT NOT NULL,
  source       TEXT NOT NULL,
  op_kind      TEXT NOT NULL,
  path         TEXT NOT NULL DEFAULT 'contract',
  before_json  TEXT,
  after_json   TEXT,
  rationale    TEXT,
  created_at   TEXT NOT NULL
)"#;
/// 契约运行态表(P5):kaleido_state(每会话一行)+ kaleido_changelog(逐 op append)。
/// 合并前对两侧各补一次 DDL,与 SCOPE_VARIABLES_DDL 同理(§4.3);索引不参与
/// schema 一致性比对(schema_map 仅读 type='table')。kaleido_state 按业务键
/// session_id 单列主键,合并冲突走 INSERT 重映射路径(与其他 TEXT 主键表一致)。
pub(super) const KALEIDO_STATE_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS kaleido_state (
  session_id       TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL,
  stat_data        TEXT NOT NULL,
  meta_json        TEXT NOT NULL,
  revision_seq     INTEGER NOT NULL DEFAULT 0,
  revision_hash    TEXT NOT NULL DEFAULT '',
  updated_at       TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS kaleido_changelog (
  seq        INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL,
  turn_id    INTEGER NOT NULL,
  entry_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_kaleido_changelog_session ON kaleido_changelog(session_id, seq);
"#;
/// 跨会话记忆蒸馏表(落地项 2):memory_entries。合并前对两侧各补一次 DDL,
/// 与 SCOPE_VARIABLES_DDL 同理(§4.3);索引不参与 schema 一致性比对。
/// 正文只有 content 一列(蒸馏产物即短记忆行,无 raw/summary 拆分——若未来需要
/// 保留原文,拆两条或 ALTER 补列,normalize_sql 已兼容表尾追加列)。
pub(super) const MEMORY_ENTRIES_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS memory_entries (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  character_id      TEXT NOT NULL,
  source_session_id TEXT,
  kind              TEXT NOT NULL CHECK (kind IN ('distilled','tool','manual')),
  content           TEXT NOT NULL,
  usage_count       INTEGER NOT NULL DEFAULT 0,
  last_usage        TEXT,
  selected          INTEGER NOT NULL DEFAULT 1,
  created_at        TEXT NOT NULL,
  updated_at        TEXT NOT NULL,
  pinned            INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_memory_entries_character ON memory_entries(character_id, selected);
"#;

/// 记忆全文检索(升级工作流 B1):FTS5 外部内容表 + 3 个同步 trigger。
/// 与 models/db/schema.rs CREATE_TABLES 内的文本保持一致(normalize 比对依赖);
/// FTS 虚拟表与其影子表不参与合并比对(见 merge.rs is_fts_table)。
pub(super) const MEMORY_ENTRIES_FTS_DDL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS memory_entries_fts USING fts5(
  content,
  content='memory_entries',
  content_rowid='id',
  tokenize='trigram'
);
CREATE TRIGGER IF NOT EXISTS memory_entries_ai AFTER INSERT ON memory_entries BEGIN
  INSERT INTO memory_entries_fts(rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER IF NOT EXISTS memory_entries_ad AFTER DELETE ON memory_entries BEGIN
  INSERT INTO memory_entries_fts(memory_entries_fts, rowid, content)
    VALUES('delete', old.id, old.content);
END;
CREATE TRIGGER IF NOT EXISTS memory_entries_au AFTER UPDATE ON memory_entries BEGIN
  INSERT INTO memory_entries_fts(memory_entries_fts, rowid, content)
    VALUES('delete', old.id, old.content);
  INSERT INTO memory_entries_fts(rowid, content) VALUES (new.id, new.content);
END;
"#;

/// 幂等 schema 升级(B2 分层注入):为 memory_entries 补 pinned 列(缺失才 ALTER,
/// 已存在跳过)。列追加在表尾,与新版 CREATE_TABLES 建出的 schema normalize 后一致。
/// memory_entries 表不存在时零列返回直接跳过(建表由 CREATE_TABLES 负责)。
pub fn ensure_memory_entries_pinned_column(conn: &Connection) -> Result<(), String> {
    let mut existing: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("PRAGMA table_info(memory_entries)")
            .map_err(|e| format!("读取 memory_entries 列失败: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("遍历 memory_entries 列失败: {e}"))?;
        for name in rows.flatten() {
            existing.push(name.to_lowercase());
        }
    }
    if existing.is_empty() || existing.iter().any(|c| c == "pinned") {
        return Ok(());
    }
    conn.execute(
        "ALTER TABLE memory_entries ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0",
        [],
    )
    .map_err(|e| format!("为 memory_entries 补 pinned 列失败: {e}"))?;
    Ok(())
}

/// 幂等回填(B1 检索底座):FTS5 外部内容表建好后执行一次 rebuild,把旧库已有记忆
/// 灌进索引;backfill_meta 记标记避免每次启动重跑(rebuild 是 O(全表),不能进热启动路径)。
/// 新建库(空表)rebuild 代价可忽略,同样只做一次。
pub fn ensure_memory_entries_fts_backfill(conn: &Connection) -> Result<(), String> {
    const KEY: &str = "memory_fts_rebuilt";
    let done: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM backfill_meta WHERE key = ?1)",
            [KEY],
            |row| row.get::<_, i64>(0),
        )
        .map(|v| v == 1)
        .unwrap_or(false);
    if done {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO memory_entries_fts(memory_entries_fts) VALUES('rebuild')",
        [],
    )
    .map_err(|e| format!("记忆全文索引回填失败: {e}"))?;
    conn.execute(
        "INSERT OR REPLACE INTO backfill_meta (key, value) VALUES (?1, ?2)",
        rusqlite::params![KEY, "1"],
    )
    .map_err(|e| format!("写入记忆索引回填标记失败: {e}"))?;
    Ok(())
}
/// 任务消息表(批次 R2 多轮用户输入):任务全程的用户输入(followup 追加指令 /
/// plan_chat 批准环节对话)与助手产出按行落库,任务删除随外键级联清除。
/// 合并前对两侧各补一次 DDL,与 SCOPE_VARIABLES_DDL 同理,
/// 避免旧库与新库合并时因「基线缺少源表」停止(§4.3);索引不参与 schema 一致性比对。
/// 文本与 models/db/schema.rs CREATE_TABLES 内的建表语句保持一致(normalize 比对依赖)。
const TASK_MESSAGES_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS task_messages (
  id         TEXT PRIMARY KEY,
  task_id    TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  role       TEXT NOT NULL CHECK (role IN ('user','assistant')),
  kind       TEXT NOT NULL DEFAULT 'normal',
  content    TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_task_messages_task ON task_messages(task_id, created_at);
"#;

/// 幂等 schema 升级(批次 R2):task_messages 表缺失才建(PRAGMA table_info 探测,
/// 已存在跳过),与 ensure_*_column 同款探测模式,只是对象从列升级为整表。
/// 启动时(Db::open)与跨库合并前(merge_databases 两侧)各执行一次。
pub fn ensure_task_messages_table(conn: &Connection) -> Result<(), String> {
    let mut existing: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("PRAGMA table_info(task_messages)")
            .map_err(|e| format!("读取 task_messages 列失败: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("遍历 task_messages 列失败: {e}"))?;
        for name in rows.flatten() {
            existing.push(name);
        }
    }
    // 表不存在时 PRAGMA 返回零行(与 ensure_skills_progressive_columns 同口径)
    if !existing.is_empty() {
        return Ok(());
    }
    conn.execute_batch(TASK_MESSAGES_DDL)
        .map_err(|e| format!("创建 task_messages 表失败: {e}"))?;
    Ok(())
}
