// SQLite 访问层:rusqlite,建表与 Node 版(node:sqlite)完全一致,兼容现有 kedai.db
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

const CREATE_TABLES: &str = r#"
CREATE TABLE IF NOT EXISTS characters (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  chara_name  TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  file_path   TEXT NOT NULL,
  avatar_path TEXT,
  data_raw    TEXT NOT NULL,
  created_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
  id           TEXT PRIMARY KEY,
  character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
  title        TEXT NOT NULL DEFAULT '新会话',
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS messages (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  role       TEXT NOT NULL CHECK (role IN ('user','assistant','system')),
  content    TEXT NOT NULL,
  extra      TEXT NOT NULL DEFAULT '{}',
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, id);
CREATE TABLE IF NOT EXISTS agent_sessions (
  id         TEXT PRIMARY KEY,
  session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
  state      TEXT NOT NULL,
  plan       TEXT NOT NULL DEFAULT '[]',
  steps      TEXT NOT NULL DEFAULT '[]',
  step_index INTEGER NOT NULL DEFAULT 0,
  agent_mode TEXT NOT NULL DEFAULT 'fast',
  started_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS tool_calls (
  id               TEXT PRIMARY KEY,
  agent_session_id TEXT NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
  name             TEXT NOT NULL,
  input            TEXT NOT NULL,
  output           TEXT NOT NULL,
  duration_ms      INTEGER NOT NULL,
  created_at       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tool_calls_agent ON tool_calls(agent_session_id);
CREATE TABLE IF NOT EXISTS world_books (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  character_id TEXT REFERENCES characters(id) ON DELETE CASCADE,
  enabled      INTEGER NOT NULL DEFAULT 1,
  source       TEXT NOT NULL DEFAULT 'upload',
  entry_count  INTEGER NOT NULL DEFAULT 0,
  data_raw     TEXT NOT NULL,
  created_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_world_books_character ON world_books(character_id);
CREATE TABLE IF NOT EXISTS skills (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL UNIQUE,
  description TEXT NOT NULL DEFAULT '',
  content     TEXT NOT NULL DEFAULT '',
  enabled     INTEGER NOT NULL DEFAULT 1,
  created_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_subtasks (
  id           TEXT PRIMARY KEY,
  session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  character_id TEXT NOT NULL DEFAULT '',
  name         TEXT NOT NULL DEFAULT '',
  instruction  TEXT NOT NULL DEFAULT '',
  status       TEXT NOT NULL DEFAULT 'pending',
  result       TEXT NOT NULL DEFAULT '',
  error        TEXT NOT NULL DEFAULT '',
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_subtasks_session ON agent_subtasks(session_id);
CREATE TABLE IF NOT EXISTS session_vars (
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  key        TEXT NOT NULL,
  value      TEXT NOT NULL DEFAULT '',
  updated_at TEXT NOT NULL,
  PRIMARY KEY (session_id, key)
);
CREATE TABLE IF NOT EXISTS session_assistant_vars (
  session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
  data_raw   TEXT NOT NULL DEFAULT '{}',
  updated_at TEXT NOT NULL
);
-- 7 作用域变量通用表(计划二):scope=global|chat|character|preset|message|script|extension。
-- chat 作用域以 session_assistant_vars 为规范存储,本表为兼容镜像(启动幂等回填);
-- 其余作用域以本表为规范存储。message 作用域 scope_id = messages.id(字符串)。
CREATE TABLE IF NOT EXISTS scope_variables (
  scope      TEXT NOT NULL,
  scope_id   TEXT NOT NULL DEFAULT '',
  data_raw   TEXT NOT NULL DEFAULT '{}',
  updated_at TEXT NOT NULL,
  PRIMARY KEY (scope, scope_id)
);
CREATE TABLE IF NOT EXISTS quick_replies (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT NOT NULL,
  label      TEXT NOT NULL DEFAULT '',
  content    TEXT NOT NULL DEFAULT '',
  enabled    INTEGER NOT NULL DEFAULT 1,
  position   INTEGER NOT NULL DEFAULT 0,
  sort_order INTEGER NOT NULL DEFAULT 100,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_quick_replies_name ON quick_replies(name);
-- 用户脚本(ScriptTree,阶段三):scope=global 或 character。
-- 角色级脚本亦可存于角色卡 data_raw.extensions.tavern_helper(兼容 ST 卡导出),
-- 本表仅承载「非随卡导出」的全局脚本;角色级以角色卡为规范存储(见 user_script_service)。
CREATE TABLE IF NOT EXISTS user_scripts (
  id         TEXT PRIMARY KEY,
  scope      TEXT NOT NULL,
  owner_id   TEXT NOT NULL DEFAULT '',
  data_json  TEXT NOT NULL DEFAULT '[]',
  updated_at TEXT NOT NULL,
  UNIQUE (scope, owner_id)
);
CREATE INDEX IF NOT EXISTS idx_user_scripts_owner ON user_scripts(scope, owner_id);
CREATE TABLE IF NOT EXISTS session_usage (
  session_id        TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
  total_prompt      INTEGER NOT NULL DEFAULT 0,
  total_completion  INTEGER NOT NULL DEFAULT 0,
  total_tokens      INTEGER NOT NULL DEFAULT 0,
  updated_at        TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS global_usage (
  id                INTEGER PRIMARY KEY CHECK (id = 1),
  total_prompt      INTEGER NOT NULL DEFAULT 0,
  total_completion  INTEGER NOT NULL DEFAULT 0,
  total_tokens      INTEGER NOT NULL DEFAULT 0,
  updated_at        TEXT NOT NULL
);
INSERT OR IGNORE INTO global_usage (id, total_prompt, total_completion, total_tokens, updated_at)
VALUES (1, 0, 0, 0, '');
CREATE TABLE IF NOT EXISTS session_compactions (
  session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  upto_message_id INTEGER NOT NULL,
  summary         TEXT NOT NULL DEFAULT '',
  model           TEXT NOT NULL DEFAULT '',
  created_at      TEXT NOT NULL,
  PRIMARY KEY (session_id, upto_message_id)
);
CREATE INDEX IF NOT EXISTS idx_session_compactions ON session_compactions(session_id);
CREATE TABLE IF NOT EXISTS llm_requests (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  run_id      TEXT NOT NULL,
  seq         INTEGER NOT NULL,
  payload     TEXT NOT NULL,
  model       TEXT NOT NULL DEFAULT '',
  created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_llm_requests_session ON llm_requests(session_id, seq);
-- 契约变更历史(阶段 C):append-only 审计 + 回滚源。
-- op_kind: replace(整体替换)| remove(移除);before/after 为契约 JSON 文本或 NULL。
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
);
CREATE INDEX IF NOT EXISTS idx_contract_changelog_char ON contract_changelog(character_id, seq);
-- 契约运行态(P5):KaleidoState 落库,每会话一行(会话级唯一事实源)。
-- meta_json 为 KaleidoMeta(pending/confidence 等);revision_hash 预留(sha2 未引入,先空串)。
CREATE TABLE IF NOT EXISTS kaleido_state (
  session_id       TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL,
  stat_data        TEXT NOT NULL,
  meta_json        TEXT NOT NULL,
  revision_seq     INTEGER NOT NULL DEFAULT 0,
  revision_hash    TEXT NOT NULL DEFAULT '',
  updated_at       TEXT NOT NULL
);
-- 契约运行态变更日志(P5):append-only 领域 changelog(§7-ChangelogEntry 逐 op 落账)。
-- entry_json 为 ChangelogEntry(回填真实 seq 后序列化);revision_seq 取本会话最大 seq。
CREATE TABLE IF NOT EXISTS kaleido_changelog (
  seq        INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL,
  turn_id    INTEGER NOT NULL,
  entry_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_kaleido_changelog_session ON kaleido_changelog(session_id, seq);
"#;

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(db_path: &Path, data_dir: &Path) -> Result<Self, String> {
        if let Err(e) = std::fs::create_dir_all(data_dir) {
            return Err(format!("创建数据目录失败: {e}"));
        }
        let conn = Connection::open(db_path).map_err(|e| format!("打开数据库失败: {e}"))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| format!("设置 WAL 失败: {e}"))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| format!("设置外键失败: {e}"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| format!("设置 busy_timeout 失败: {e}"))?;
        conn.execute_batch(CREATE_TABLES)
            .map_err(|e| format!("建表失败: {e}"))?;
        backfill_scope_variables(&conn)?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 启动幂等回填(计划二 · 7 作用域变量):把既有存储镜像进 scope_variables 表。
/// 全部使用 INSERT OR IGNORE(主键 (scope, scope_id) 已存在即跳过),可重复执行不产生重复行。
fn backfill_scope_variables(conn: &Connection) -> Result<(), String> {
    // 1) chat 镜像:session_assistant_vars 整树 → scope_variables('chat', session_id)
    conn.execute(
        "INSERT OR IGNORE INTO scope_variables (scope, scope_id, data_raw, updated_at)
         SELECT 'chat', session_id, data_raw, updated_at FROM session_assistant_vars",
        [],
    )
    .map_err(|e| format!("回填 chat 作用域镜像失败: {e}"))?;
    // 2) message 回填:messages.extra.mvu.stat_data 子树 → scope_variables('message', id)
    let mut stmt = conn
        .prepare("SELECT id, extra FROM messages WHERE extra != '{}'")
        .map_err(|e| format!("准备消息回填语句失败: {e}"))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| format!("遍历消息回填失败: {e}"))?;
    for row in rows.flatten() {
        let (id, extra) = row;
        let stat_data = serde_json::from_str::<serde_json::Value>(&extra)
            .ok()
            .and_then(|v| v.get("mvu").and_then(|m| m.get("stat_data")).cloned());
        let Some(stat_data) = stat_data else { continue };
        if stat_data.is_null() {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO scope_variables (scope, scope_id, data_raw, updated_at)
             VALUES ('message', ?1, ?2, ?3)",
            rusqlite::params![id.to_string(), stat_data.to_string(), now_iso()],
        )
        .map_err(|e| format!("回填 message 作用域失败: {e}"))?;
    }
    Ok(())
}

/// ISO 时间戳(与 Node 版 new Date().toISOString() 对齐,UTC)
pub fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}
