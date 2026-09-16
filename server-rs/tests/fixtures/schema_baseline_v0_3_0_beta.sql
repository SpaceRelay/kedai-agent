-- 冻结基线(迁移元测试用):0.3.0-beta 发布时的建表 SQL —— 即 schema.rs 的 CREATE_TABLES 原文。
-- 由 tests/schema_migration_meta.rs 读取:在临时目录建「老库」→ 跑 Db::open(执行全部 ensure_* 迁移)
-- → 与全新库的结构指纹比对。**本文件是历史快照,不得随代码演进更新**;
-- 新增列/表/索引必须写 ensure_* 迁移,否则该测试会失败(防「加列漏迁移」的旧库 no such column)。

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
  created_at  TEXT NOT NULL,
  allowed_tools TEXT NOT NULL DEFAULT '[]',
  run_as_subagent INTEGER NOT NULL DEFAULT 0,
  model       TEXT NOT NULL DEFAULT ''
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
-- 任务模式(task 工作台):任务主表 + 子任务表。
-- 子任务独立于 agent_subtasks(后者的 session_id 外键指向 sessions 表,任务 id 不在其中)。
CREATE TABLE IF NOT EXISTS tasks (
  id           TEXT PRIMARY KEY,
  title        TEXT NOT NULL,
  status       TEXT NOT NULL DEFAULT 'pending',
  plan         TEXT NOT NULL DEFAULT '[]',
  result       TEXT NOT NULL DEFAULT '',
  error        TEXT NOT NULL DEFAULT '',
  character_id TEXT,
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL,
  -- 执行模式(批次 4 六模式):legacy|solo|multi|plan|team|custom;旧库经
  -- migration::ensure_tasks_task_mode_column 幂等补列,旧行默认 'legacy'
  task_mode    TEXT NOT NULL DEFAULT 'legacy'
);
CREATE INDEX IF NOT EXISTS idx_tasks_created ON tasks(created_at);
CREATE TABLE IF NOT EXISTS task_subtasks (
  id           TEXT PRIMARY KEY,
  task_id      TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  name         TEXT NOT NULL DEFAULT '',
  instruction  TEXT NOT NULL DEFAULT '',
  status       TEXT NOT NULL DEFAULT 'pending',
  result       TEXT NOT NULL DEFAULT '',
  error        TEXT NOT NULL DEFAULT '',
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_task_subtasks_task ON task_subtasks(task_id);
-- 任务模式 token 用量:每次 LLM 调用(规划/步骤/汇总)一行;任务删除随外键级联清除。
-- 不复用 llm_requests 表:其 session_id 外键指向 sessions(id),任务 id 不在其中。
CREATE TABLE IF NOT EXISTS task_usage (
  id                TEXT PRIMARY KEY,
  task_id           TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  phase             TEXT NOT NULL,
  step_index        INTEGER,
  model             TEXT NOT NULL,
  prompt_tokens     INTEGER NOT NULL DEFAULT 0,
  completion_tokens INTEGER NOT NULL DEFAULT 0,
  reasoning_tokens  INTEGER NOT NULL DEFAULT 0,
  created_at        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_task_usage_task ON task_usage(task_id);
-- 任务模式 LLM 调用追踪(批次 3「调用情况」面板):每次任务侧 LLM 调用一行
--(planner/step/summarize/agent/subagent/audit),含提示词/响应摘要与耗时;
-- 任务删除随外键级联清除。与 task_usage 并存:usage 管 token 口径,本表管可观测性。
-- finish_reason(可观测性问题①):上游 finish_reason(stop/length/content_filter 等),
-- '' = 未知/未下发(旧行默认值,避免旧数据被误读为正常 stop 收尾);
-- length 即 max_tokens 截断的直接证据——修复前截断与正常完成在面板上无从区分。
-- 旧库由 migration::ensure_task_llm_calls_finish_reason_column 幂等补列;
-- 注意:列定义处不得写行内注释,否则 sqlite_schema 存储文本与 ALTER 补列的
-- 旧库 normalize 后不一致,跨库合并 schema 比对会误报冲突。
CREATE TABLE IF NOT EXISTS task_llm_calls (
  id                TEXT PRIMARY KEY,
  task_id           TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  phase             TEXT NOT NULL,
  step_index        INTEGER,
  model             TEXT NOT NULL,
  prompt_summary    TEXT NOT NULL DEFAULT '',
  response_summary  TEXT NOT NULL DEFAULT '',
  prompt_tokens     INTEGER NOT NULL DEFAULT 0,
  completion_tokens INTEGER NOT NULL DEFAULT 0,
  reasoning_tokens  INTEGER NOT NULL DEFAULT 0,
  elapsed_ms        INTEGER NOT NULL DEFAULT 0,
  status            TEXT NOT NULL DEFAULT 'ok',
  created_at        TEXT NOT NULL,
  finish_reason     TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_task_llm_calls_task ON task_llm_calls(task_id, created_at);
-- 任务消息(批次 R2 多轮用户输入):任务全程的用户输入与助手产出按行落库。
-- role: user | assistant;kind: normal | followup(终态追加指令)| plan_chat(批准环节对话);
-- 任务删除随外键级联清除。旧库由 migration::ensure_task_messages_table 幂等建表;
-- 注意:列定义处不得写行内注释(同 task_llm_calls 教训,跨库合并 schema 比对会误报)。
CREATE TABLE IF NOT EXISTS task_messages (
  id         TEXT PRIMARY KEY,
  task_id    TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  role       TEXT NOT NULL CHECK (role IN ('user','assistant')),
  kind       TEXT NOT NULL DEFAULT 'normal',
  content    TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_task_messages_task ON task_messages(task_id, created_at);

-- 命令执行审计(阶段 B/C):每次尝试执行(含被拒绝的)落一行,root/ADB 级命令
-- 不可逆,这是唯一回溯依据。旧库由 migration::ensure_exec_audit_table 幂等建表;
-- 注意:列定义处不得写行内注释(同 task_llm_calls 教训,跨库合并 schema 比对会误报)。
CREATE TABLE IF NOT EXISTS exec_audit (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  ts             TEXT NOT NULL,
  source         TEXT NOT NULL DEFAULT 'chat',
  task_id        TEXT,
  session_id     TEXT,
  command        TEXT NOT NULL,
  shell          TEXT NOT NULL DEFAULT '',
  tier           TEXT NOT NULL DEFAULT 'sandbox',
  risk           TEXT NOT NULL DEFAULT 'sensitive',
  decision       TEXT NOT NULL DEFAULT 'allowed',
  exit_code      INTEGER,
  stdout_summary TEXT NOT NULL DEFAULT '',
  stderr_summary TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_exec_audit_ts ON exec_audit(ts DESC);
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
  created_at  TEXT NOT NULL,
  prompt_cache_hit_tokens   INTEGER NOT NULL DEFAULT 0,
  prompt_cache_miss_tokens  INTEGER NOT NULL DEFAULT 0,
  prompt_tokens             INTEGER NOT NULL DEFAULT 0,
  completion_tokens         INTEGER NOT NULL DEFAULT 0
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
-- 跨会话记忆蒸馏(2026-08 落地项 2):按角色维度共享的长期记忆条目。
-- kind: distilled(LLM 蒸馏)/ tool(agent memory_write 工具写入)/ manual(手动添加);
-- selected: 是否参与注入候选(0/1);usage_count/last_usage 为衰减精选排序键。
-- character_id 不设外键:记忆须活过会话生命周期由用户显式管理(删角色不级联清记忆),
-- 与 agent_subtasks.character_id 同策略;source_session_id 记录来源会话(可空)。
-- pinned: 分层注入的最高优先级(1 = 常驻置顶,排序键首位);旧库经 ALTER 补列。
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
-- 记忆全文检索(升级工作流 B1):FTS5 外部内容表(content='memory_entries'),
-- trigram 分词器支持中文子串匹配(<3 字符查询由服务层回退 LIKE);
-- 3 个同步 trigger 保持索引与主表一致;旧库首次建表后由 ensure_memory_entries_fts
-- 执行一次 rebuild 回填(backfill_meta 记标记,避免每次启动重跑)。
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
-- 记忆向量索引元信息(升级工作流 Phase 3):记录当前 vec0 表的维度与来源模型。
-- vec0 虚拟表的维度必须建表时确定,故不写进静态 DDL,由 memory_service 在首次
-- 写入/检索时按配置懒建;本表用于检测「换模型/换维度」并提示需要重建索引。
CREATE TABLE IF NOT EXISTS memory_vec_meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
-- 回填游标元表(2026-08 DB 并发改造):记录启动幂等回填的增量进度。
-- 当前唯一 key:message_scope_last_id(messages.id 已回填边界,含 extra='{}' 的跳过行)。
CREATE TABLE IF NOT EXISTS backfill_meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
-- 回退快照(批次 6.1「undo」):写工具(write/replace/create/update_variables/memory_write)
-- 执行前取逆操作负载落本表,供「回退到此处」恢复。payload 为 JSON 逆操作集
-- (文件原内容内嵌,单文件超 256KB 截断并标 truncated=true,restore 拒绝);
-- anchor_message_id = 快照时该会话 max(messages.id)(无消息为 NULL)。
-- 不设外键:快照生命周期独立于消息清理(truncate/删除),恢复语义由 undo_service 校验。
CREATE TABLE IF NOT EXISTS undo_snapshots (
  id                TEXT PRIMARY KEY,
  session_id        TEXT NOT NULL,
  anchor_message_id INTEGER,
  tool_name         TEXT NOT NULL,
  label             TEXT NOT NULL DEFAULT '',
  payload           TEXT NOT NULL,
  created_at        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_undo_snapshots_session ON undo_snapshots(session_id, created_at);
