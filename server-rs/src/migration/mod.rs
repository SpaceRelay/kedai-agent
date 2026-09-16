// 数据迁移模块(目录化拆分,纯代码移动,逻辑不变):
//
// 代际: L1(老层·稳 / Anchored Core)——兼容契约的**载体内**。
// 判据: 决定「老层经验」能否跨版本延续(幂等 DDL 升级、双库合并、快照备份);
//       零 `crate::` 出边,是纯基石模块。
// 纪律: 所有升级必须**幂等**(探测后补列/补表),失败返回 Err 不 panic。
// 详见 docs/契约-架构与数据.md §2.2。
//
//   backup.rs   数据库快照备份(SQLite Online Backup API,含未 checkpoint 的 WAL)
//   merge.rs    双库合并:schema 一致性比对、按外键依赖序逐表合并、文件树/JSON 配置合并、合并后校验
//   ddl.rs      幂等 DDL 升级:新增表 DDL 常量与 ensure_* 补列/补表函数
mod backup;
mod conflict;
mod ddl;
mod merge;

pub use backup::{snapshot_before_upgrade, snapshot_database};
pub use ddl::{
    ensure_agent_subtasks_finished_at_column, ensure_exec_audit_table,
    ensure_llm_requests_usage_columns, ensure_memory_entries_fts_backfill,
    ensure_memory_entries_pinned_column, ensure_perf_indexes, ensure_skills_progressive_columns,
    ensure_task_llm_calls_finish_reason_column, ensure_task_messages_table,
    ensure_task_subtasks_finished_at_column, ensure_tasks_task_mode_column,
};
pub use merge::{merge_data_dirs, MergeReport, TableReport};

const DATABASE_FILE: &str = "kedai.db";
const SKIPPED_SIDECARS: [&str; 2] = ["kedai.db-wal", "kedai.db-shm"];

// 供测试经 `use super::*` 取名的内部符号与原文件顶层 import,集中在此保持测试零改动。
#[cfg(test)]
use merge::{normalize_sql, table_columns};
#[cfg(test)]
use rusqlite::Connection;
#[cfg(test)]
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_support::TempDataDir;

    /// 隔离临时数据目录(uuid 唯一 + 作用域结束自动清理)
    fn temp_dir(tag: &str) -> TempDataDir {
        TempDataDir::new(&format!("migration-{tag}"))
    }

    /// usage 缓存列迁移:旧版 llm_requests(无 usage 列)补列成功,且幂等可重复执行。
    #[test]
    fn ensure_llm_requests_usage_columns_adds_and_is_idempotent() {
        let dir = temp_dir("usage-columns");
        let db = dir.join(DATABASE_FILE);
        let conn = Connection::open(&db).unwrap();
        // 旧版表结构(2026-08 缓存感知管线之前):无 prompt_cache_hit_tokens 等列
        conn.execute_batch(
            "CREATE TABLE llm_requests (
              id          INTEGER PRIMARY KEY AUTOINCREMENT,
              session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
              run_id      TEXT NOT NULL,
              seq         INTEGER NOT NULL,
              payload     TEXT NOT NULL,
              model       TEXT NOT NULL DEFAULT '',
              created_at  TEXT NOT NULL
            );",
        )
        .unwrap();

        ensure_llm_requests_usage_columns(&conn).unwrap();
        let columns = table_columns(&conn, "main", "llm_requests")
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect::<Vec<_>>();
        for expected in [
            "prompt_cache_hit_tokens",
            "prompt_cache_miss_tokens",
            "prompt_tokens",
            "completion_tokens",
        ] {
            assert!(
                columns.iter().any(|c| c == expected),
                "迁移后应包含列 {expected},实际: {columns:?}"
            );
        }
        // 新列默认值 0(NOT NULL DEFAULT 0)
        let hit: i64 = conn
            .query_row(
                "SELECT prompt_cache_hit_tokens FROM llm_requests LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        assert_eq!(hit, 0);

        // 幂等:重复执行不报错、不产生重复列
        ensure_llm_requests_usage_columns(&conn).unwrap();
        let columns2 = table_columns(&conn, "main", "llm_requests")
            .unwrap()
            .into_iter()
            .filter(|c| c.name == "prompt_cache_hit_tokens")
            .count();
        assert_eq!(columns2, 1, "重复迁移不应产生重复列");
        drop(conn);
    }

    /// task_mode 列迁移(批次 4 六模式):旧版 tasks(无 task_mode 列)补列成功、
    /// 旧行默认 'legacy'、幂等可重复执行。
    #[test]
    fn ensure_tasks_task_mode_column_adds_and_is_idempotent() {
        let dir = temp_dir("tasks-task-mode");
        let db = dir.join(DATABASE_FILE);
        let conn = Connection::open(&db).unwrap();
        // 旧版表结构(批次 4 之前):无 task_mode 列
        conn.execute_batch(
            "CREATE TABLE tasks (
              id           TEXT PRIMARY KEY,
              title        TEXT NOT NULL,
              status       TEXT NOT NULL DEFAULT 'pending',
              plan         TEXT NOT NULL DEFAULT '[]',
              result       TEXT NOT NULL DEFAULT '',
              error        TEXT NOT NULL DEFAULT '',
              character_id TEXT,
              created_at   TEXT NOT NULL,
              updated_at   TEXT NOT NULL
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, created_at, updated_at) VALUES ('t1', '旧行', 'c', 'u')",
            [],
        )
        .unwrap();

        ensure_tasks_task_mode_column(&conn).unwrap();
        let columns = table_columns(&conn, "main", "tasks")
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect::<Vec<_>>();
        assert!(
            columns.iter().any(|c| c == "task_mode"),
            "迁移后应包含 task_mode 列,实际: {columns:?}"
        );
        // 旧行零迁移成本:默认 'legacy'
        let mode: String = conn
            .query_row("SELECT task_mode FROM tasks WHERE id = 't1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(mode, "legacy", "旧行 task_mode 应默认 legacy");

        // 幂等:重复执行不报错、不产生重复列
        ensure_tasks_task_mode_column(&conn).unwrap();
        let dup = table_columns(&conn, "main", "tasks")
            .unwrap()
            .into_iter()
            .filter(|c| c.name == "task_mode")
            .count();
        assert_eq!(dup, 1, "重复迁移不应产生重复列");
        drop(conn);
    }

    /// 迁移后旧表 schema 应与新版 CREATE_TABLES 建出的表 normalize 后一致
    /// (跨库合并 schema 一致性比对依赖这一点)。
    /// skills 渐进披露列迁移(落地项 3):旧版 skills(无 allowed_tools 等列)补列成功、
    /// 默认值生效、幂等可重复执行,且迁移后 schema 与新版 CREATE_TABLES 一致。
    #[test]
    fn ensure_skills_progressive_columns_adds_defaults_and_matches_fresh_schema() {
        let dir = temp_dir("skills-columns");
        let db = dir.join(DATABASE_FILE);
        let conn = Connection::open(&db).unwrap();
        // 旧版表结构(落地项 3 之前):无 allowed_tools / run_as_subagent / model
        conn.execute_batch(
            "CREATE TABLE skills (
              id          TEXT PRIMARY KEY,
              name        TEXT NOT NULL UNIQUE,
              description TEXT NOT NULL DEFAULT '',
              content     TEXT NOT NULL DEFAULT '',
              enabled     INTEGER NOT NULL DEFAULT 1,
              created_at  TEXT NOT NULL
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO skills (id, name, description, content, enabled, created_at) \
             VALUES ('s1', '旧技能', '旧描述', '正文', 1, '2026-01-01')",
            [],
        )
        .unwrap();

        ensure_skills_progressive_columns(&conn).unwrap();
        // 新列默认值:'[]' / 0 / ''(旧行不受影响)
        let (tools, as_sub, model): (String, i64, String) = conn
            .query_row(
                "SELECT allowed_tools, run_as_subagent, model FROM skills WHERE id = 's1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(tools, "[]");
        assert_eq!(as_sub, 0);
        assert_eq!(model, "");

        // 幂等:重复执行不报错、不产生重复列
        ensure_skills_progressive_columns(&conn).unwrap();
        let dup = table_columns(&conn, "main", "skills")
            .unwrap()
            .into_iter()
            .filter(|c| c.name == "allowed_tools")
            .count();
        assert_eq!(dup, 1, "重复迁移不应产生重复列");

        // 迁移后 schema 与新版 CREATE_TABLES 建出的表 normalize 后一致(合并比对依赖)
        let migrated = normalize_sql(
            conn.query_row(
                "SELECT sql FROM sqlite_schema WHERE type='table' AND name='skills'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
            .as_str(),
        );
        let fresh_conn = Connection::open_in_memory().unwrap();
        fresh_conn
            .execute_batch(crate::models::db::create_tables_sql())
            .unwrap();
        let fresh = normalize_sql(
            fresh_conn
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type='table' AND name='skills'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .unwrap()
                .as_str(),
        );
        assert_eq!(migrated, fresh, "迁移后 skills schema 应与新建库一致");
        drop(conn);
    }

    /// task_messages 表迁移(批次 R2 多轮用户输入):旧库(无此表)建表成功、
    /// 幂等可重复执行、schema 与新版 CREATE_TABLES 建出的表 normalize 后一致
    /// (合并比对依赖)、外键 ON DELETE CASCADE 生效(删任务级联清消息)。
    #[test]
    fn ensure_task_messages_table_creates_and_is_idempotent() {
        let dir = temp_dir("task-messages");
        let db = dir.join(DATABASE_FILE);
        let conn = Connection::open(&db).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        // 旧版库(批次 R2 之前):只有 tasks 表,无 task_messages
        conn.execute_batch(
            "CREATE TABLE tasks (
              id TEXT PRIMARY KEY, title TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
              plan TEXT NOT NULL DEFAULT '[]', result TEXT NOT NULL DEFAULT '',
              error TEXT NOT NULL DEFAULT '', character_id TEXT,
              created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );",
        )
        .unwrap();
        conn.execute("INSERT INTO tasks (id, title, created_at, updated_at) VALUES ('t1', '旧任务', 'c', 'u')", [])
            .unwrap();

        ensure_task_messages_table(&conn).unwrap();
        // 建表后可插入/查询;列齐全(id/task_id/role/kind/content/created_at)
        conn.execute(
            "INSERT INTO task_messages (id, task_id, role, kind, content, created_at) \
             VALUES ('m1', 't1', 'user', 'followup', '再补充一点', 'c')",
            [],
        )
        .unwrap();
        let kind: String = conn
            .query_row("SELECT kind FROM task_messages WHERE id = 'm1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kind, "followup");

        // 幂等:重复执行不报错、数据保留
        ensure_task_messages_table(&conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "重复迁移不应丢数据");

        // 迁移后 schema 与新版 CREATE_TABLES 建出的表 normalize 后一致(合并比对依赖)
        let migrated = normalize_sql(
            conn.query_row(
                "SELECT sql FROM sqlite_schema WHERE type='table' AND name='task_messages'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
            .as_str(),
        );
        let fresh_conn = Connection::open_in_memory().unwrap();
        fresh_conn
            .execute_batch(crate::models::db::create_tables_sql())
            .unwrap();
        let fresh = normalize_sql(
            fresh_conn
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type='table' AND name='task_messages'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .unwrap()
                .as_str(),
        );
        assert_eq!(
            migrated, fresh,
            "迁移后 task_messages schema 应与新建库一致"
        );

        // 外键级联:删任务 → 消息一并删除(ON DELETE CASCADE)
        conn.execute("DELETE FROM tasks WHERE id = 't1'", [])
            .unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "删除任务应级联清消息");
        drop(conn);
    }

    /// finish_reason 列迁移(可观测性问题①):旧版 task_llm_calls(无 finish_reason 列)
    /// 补列成功、旧行默认 ''(未知/未下发,而非误判 stop)、幂等可重复执行,
    /// 且迁移后 schema 与新版 CREATE_TABLES 建出的表 normalize 后一致(合并比对依赖)。
    #[test]
    fn ensure_task_llm_calls_finish_reason_column_adds_and_is_idempotent() {
        let dir = temp_dir("llm-calls-finish-reason");
        let db = dir.join(DATABASE_FILE);
        let conn = Connection::open(&db).unwrap();
        // 旧版表结构(可观测性修复之前):无 finish_reason 列
        conn.execute_batch(
            "CREATE TABLE tasks (
              id TEXT PRIMARY KEY, title TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
              plan TEXT NOT NULL DEFAULT '[]', result TEXT NOT NULL DEFAULT '',
              error TEXT NOT NULL DEFAULT '', character_id TEXT,
              created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE task_llm_calls (
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
              created_at        TEXT NOT NULL
            );",
        )
        .unwrap();
        conn.execute("INSERT INTO tasks (id, title, created_at, updated_at) VALUES ('t1', '旧任务', 'c', 'u')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO task_llm_calls (id, task_id, phase, model, created_at) VALUES ('c1', 't1', 'step', 'm', 'c')",
            [],
        )
        .unwrap();

        ensure_task_llm_calls_finish_reason_column(&conn).unwrap();
        // 旧行零迁移成本:finish_reason 默认 ''(未知,不等于 stop,避免旧数据被误读为正常收尾)
        let reason: String = conn
            .query_row(
                "SELECT finish_reason FROM task_llm_calls WHERE id = 'c1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reason, "", "旧行 finish_reason 应默认空串(未知)");

        // 幂等:重复执行不报错、不产生重复列
        ensure_task_llm_calls_finish_reason_column(&conn).unwrap();
        let dup = table_columns(&conn, "main", "task_llm_calls")
            .unwrap()
            .into_iter()
            .filter(|c| c.name == "finish_reason")
            .count();
        assert_eq!(dup, 1, "重复迁移不应产生重复列");

        // 迁移后 schema 与新版 CREATE_TABLES 建出的表 normalize 后一致(合并比对依赖)
        let migrated = normalize_sql(
            conn.query_row(
                "SELECT sql FROM sqlite_schema WHERE type='table' AND name='task_llm_calls'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
            .as_str(),
        );
        let fresh_conn = Connection::open_in_memory().unwrap();
        fresh_conn
            .execute_batch(crate::models::db::create_tables_sql())
            .unwrap();
        let fresh = normalize_sql(
            fresh_conn
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type='table' AND name='task_llm_calls'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .unwrap()
                .as_str(),
        );
        assert_eq!(
            migrated, fresh,
            "迁移后 task_llm_calls schema 应与新建库一致"
        );
        drop(conn);
    }

    /// finished_at 列迁移(批次 4 子任务终态语义):旧版 agent_subtasks/task_subtasks
    /// (无 finished_at 列)补列成功、旧行默认 ''(未知/该列引入前完成,不反推语义)、
    /// 幂等可重复执行,且迁移后 schema 与新版 CREATE_TABLES normalize 后一致(合并比对依赖)。
    /// 两表都要覆盖:只补一张会让跨库合并报「基线缺少列」。
    #[test]
    fn ensure_subtasks_finished_at_column_adds_and_is_idempotent() {
        let dir = temp_dir("subtasks-finished-at");
        let db = dir.join(DATABASE_FILE);
        let conn = Connection::open(&db).unwrap();
        // 旧版表结构(finished_at 引入之前):两张子任务表均无该列。
        // sessions/tasks 一并建出以满足外键(此处只需插入成功)。
        conn.execute_batch(
            "CREATE TABLE sessions (
              id TEXT PRIMARY KEY, character_id TEXT NOT NULL, title TEXT NOT NULL DEFAULT '',
              created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE tasks (
              id TEXT PRIMARY KEY, title TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
              plan TEXT NOT NULL DEFAULT '[]', result TEXT NOT NULL DEFAULT '',
              error TEXT NOT NULL DEFAULT '', character_id TEXT,
              created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE agent_subtasks (
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
            CREATE TABLE task_subtasks (
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
            INSERT INTO sessions (id, character_id, created_at, updated_at) VALUES ('s1', 'c1', 'c', 'u');
            INSERT INTO tasks (id, title, created_at, updated_at) VALUES ('t1', '旧任务', 'c', 'u');
            INSERT INTO agent_subtasks (id, session_id, name, created_at, updated_at) VALUES ('a1', 's1', '旧子任务', 'c', 'u');
            INSERT INTO task_subtasks (id, task_id, name, created_at, updated_at) VALUES ('b1', 't1', '旧任务子任务', 'c', 'u');",
        )
        .unwrap();

        ensure_agent_subtasks_finished_at_column(&conn).unwrap();
        ensure_task_subtasks_finished_at_column(&conn).unwrap();
        // 旧行零迁移成本:finished_at 默认 ''(未知),不得被误读为「已完成,时刻为 epoch」
        let agent_finished: String = conn
            .query_row(
                "SELECT finished_at FROM agent_subtasks WHERE id = 'a1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            agent_finished, "",
            "旧 agent_subtasks 行 finished_at 应默认空串"
        );
        let task_finished: String = conn
            .query_row(
                "SELECT finished_at FROM task_subtasks WHERE id = 'b1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            task_finished, "",
            "旧 task_subtasks 行 finished_at 应默认空串"
        );

        // 幂等:重复执行不报错、不产生重复列
        ensure_agent_subtasks_finished_at_column(&conn).unwrap();
        ensure_task_subtasks_finished_at_column(&conn).unwrap();
        for table in ["agent_subtasks", "task_subtasks"] {
            let dup = table_columns(&conn, "main", table)
                .unwrap()
                .into_iter()
                .filter(|c| c.name == "finished_at")
                .count();
            assert_eq!(dup, 1, "{table} 重复迁移不应产生重复列");
        }

        // 迁移后列**名字与顺序**与新版 CREATE_TABLES 建出的表逐位一致。
        // 不用 normalize_sql 比 SQL 原文:表定义内带说明注释(注释只存在于新建库的
        // sqlite_schema 文本里,ALTER 追加的列没有),原文比对必然不等;
        // 结构一致性由这里的列序比对 + tests/schema_migration_meta.rs 的逐表指纹共同把关。
        let fresh_conn = Connection::open_in_memory().unwrap();
        fresh_conn
            .execute_batch(crate::models::db::create_tables_sql())
            .unwrap();
        for table in ["agent_subtasks", "task_subtasks"] {
            let migrated: Vec<String> = table_columns(&conn, "main", table)
                .unwrap()
                .into_iter()
                .map(|c| c.name)
                .collect();
            let fresh: Vec<String> = table_columns(&fresh_conn, "main", table)
                .unwrap()
                .into_iter()
                .map(|c| c.name)
                .collect();
            assert_eq!(
                migrated, fresh,
                "{table} 迁移后的列名与顺序应与新建库逐位一致(ALTER 只能追加到表尾)"
            );
            assert_eq!(
                migrated.last().map(String::as_str),
                Some("finished_at"),
                "{table} 的 finished_at 必须是最后一列(与建表语句一致)"
            );
        }
        drop(conn);
    }

    /// 表缺失时两张子任务表的 ensure 都应零列返回、不报错(交建表批负责)。
    /// 极旧快照/手工建库会命中这条路径;此处若 ALTER 报错将让整个 Db::open 升级链回滚。
    #[test]
    fn ensure_subtasks_finished_at_column_skips_missing_tables() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_agent_subtasks_finished_at_column(&conn)
            .expect("表不存在应跳过而非报错(建表由 CREATE_TABLES 负责)");
        ensure_task_subtasks_finished_at_column(&conn)
            .expect("表不存在应跳过而非报错(建表由 CREATE_TABLES 负责)");
    }

    /// memory_entries 升级(升级工作流 B1+B2):旧库(无 pinned 列 / 无 FTS)补列建索引成功、
    /// pinned 默认 0、FTS 索引经 rebuild 回填旧行、幂等可重复执行,且迁移后 schema 与
    /// 新版 CREATE_TABLES 一致(合并比对依赖)。
    #[test]
    fn ensure_memory_entries_pinned_and_fts_upgrade_and_is_idempotent() {
        let dir = temp_dir("memory-entries-upgrade");
        let db = dir.join(DATABASE_FILE);
        let conn = Connection::open(&db).unwrap();
        // 旧版表结构(升级工作流之前):无 pinned 列、无 FTS 索引
        conn.execute_batch(
            "CREATE TABLE memory_entries (
              id                INTEGER PRIMARY KEY AUTOINCREMENT,
              character_id      TEXT NOT NULL,
              source_session_id TEXT,
              kind              TEXT NOT NULL CHECK (kind IN ('distilled','tool','manual')),
              content           TEXT NOT NULL,
              usage_count       INTEGER NOT NULL DEFAULT 0,
              last_usage        TEXT,
              selected          INTEGER NOT NULL DEFAULT 1,
              created_at        TEXT NOT NULL,
              updated_at        TEXT NOT NULL
            );
            CREATE TABLE backfill_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_entries (character_id, kind, content, created_at, updated_at) \
             VALUES ('c1', 'manual', '旧库记忆:图书馆初识', 'c', 'u')",
            [],
        )
        .unwrap();

        ensure_memory_entries_pinned_column(&conn).unwrap();
        conn.execute_batch(super::ddl::MEMORY_ENTRIES_FTS_DDL)
            .unwrap();
        ensure_memory_entries_fts_backfill(&conn).unwrap();

        // pinned 列默认 0
        let pinned: i64 = conn
            .query_row("SELECT pinned FROM memory_entries LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(pinned, 0, "旧行 pinned 应默认 0");
        // FTS rebuild 已回填旧行
        let hit: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_entries_fts WHERE content MATCH '\"图书馆\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, 1, "rebuild 应把旧行灌进 FTS 索引");
        // trigger 同步:插入新行后 FTS 可见
        conn.execute(
            "INSERT INTO memory_entries (character_id, kind, content, created_at, updated_at) \
             VALUES ('c1', 'manual', '新行:美术馆', 'c', 'u')",
            [],
        )
        .unwrap();
        let hit2: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_entries_fts WHERE content MATCH '\"美术馆\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit2, 1, "AFTER INSERT trigger 应同步索引");

        // 幂等:重复执行不报错、不重复 rebuild(backfill_meta 标记)
        ensure_memory_entries_pinned_column(&conn).unwrap();
        ensure_memory_entries_fts_backfill(&conn).unwrap();
        let dup = table_columns(&conn, "main", "memory_entries")
            .unwrap()
            .into_iter()
            .filter(|c| c.name == "pinned")
            .count();
        assert_eq!(dup, 1, "重复迁移不应产生重复列");

        // 迁移后 schema 与新版 CREATE_TABLES 建出的表 normalize 后一致
        let migrated = normalize_sql(
            conn.query_row(
                "SELECT sql FROM sqlite_schema WHERE type='table' AND name='memory_entries'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
            .as_str(),
        );
        let fresh_conn = Connection::open_in_memory().unwrap();
        fresh_conn
            .execute_batch(crate::models::db::create_tables_sql())
            .unwrap();
        let fresh = normalize_sql(
            fresh_conn
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type='table' AND name='memory_entries'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .unwrap()
                .as_str(),
        );
        assert_eq!(
            migrated, fresh,
            "迁移后 memory_entries schema 应与新建库一致"
        );
        drop(conn);
    }

    #[test]
    fn ensure_llm_requests_usage_columns_matches_fresh_schema() {
        let dir = temp_dir("usage-schema");
        let db = dir.join(DATABASE_FILE);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE llm_requests (
              id          INTEGER PRIMARY KEY AUTOINCREMENT,
              session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
              run_id      TEXT NOT NULL,
              seq         INTEGER NOT NULL,
              payload     TEXT NOT NULL,
              model       TEXT NOT NULL DEFAULT '',
              created_at  TEXT NOT NULL
            );",
        )
        .unwrap();
        ensure_llm_requests_usage_columns(&conn).unwrap();
        let migrated = normalize_sql(
            conn.query_row(
                "SELECT sql FROM sqlite_schema WHERE type='table' AND name='llm_requests'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
            .as_str(),
        );

        let fresh_conn = Connection::open_in_memory().unwrap();
        fresh_conn
            .execute_batch(crate::models::db::create_tables_sql())
            .unwrap();
        let fresh = normalize_sql(
            fresh_conn
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type='table' AND name='llm_requests'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .unwrap()
                .as_str(),
        );
        assert_eq!(
            migrated, fresh,
            "迁移后 schema 应与新建表 normalize 后一致(迁移: {migrated} / 新建: {fresh})"
        );
        drop(conn);
    }

    fn create_database(path: &Path, marker: &str) {
        let conn = Connection::open(path.join(DATABASE_FILE)).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE characters(id TEXT PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE sessions(id TEXT PRIMARY KEY, character_id TEXT NOT NULL REFERENCES characters(id));
             CREATE TABLE messages(id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL REFERENCES sessions(id), content TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute("INSERT INTO characters VALUES('same', ?1)", [marker])
            .unwrap();
        conn.execute("INSERT INTO sessions VALUES('session', 'same')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO messages(id, session_id, content) VALUES(1, 'session', ?1)",
            [marker],
        )
        .unwrap();
    }

    #[test]
    fn snapshot_includes_uncheckpointed_wal() {
        let dir = temp_dir("snapshot");
        let db = dir.join(DATABASE_FILE);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE t(id INTEGER PRIMARY KEY, value TEXT);",
        )
        .unwrap();
        conn.execute("INSERT INTO t(value) VALUES('wal-data')", [])
            .unwrap();
        let snapshot = dir.join("snapshot.db");
        snapshot_database(&db, &snapshot).unwrap();
        let snapshot_conn = Connection::open(snapshot).unwrap();
        assert_eq!(
            snapshot_conn
                .query_row("SELECT COUNT(*) FROM t", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(conn);
    }

    #[test]
    fn merge_remaps_conflicting_keys_and_is_idempotent() {
        let baseline = temp_dir("baseline");
        let source = temp_dir("source");
        // 守卫绑定到局部变量:写成 `temp_dir("first").join("work")` 会让守卫语句末即析构,
        // 随后 merge_data_dirs 重建 <root>/work → 目录复活成残留(见 test_support 模块头)
        let first_root = temp_dir("first");
        let second_root = temp_dir("second");
        let first = first_root.join("work");
        let second = second_root.join("work");
        create_database(&baseline, "baseline");
        create_database(&source, "source");

        let report = merge_data_dirs(&baseline, &source, &first).unwrap();
        assert_eq!(report.foreign_key_errors, 0);
        let conn = Connection::open(first.join(DATABASE_FILE)).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM characters", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        drop(conn);

        merge_data_dirs(&first, &source, &second).unwrap();
        let conn = Connection::open(second.join(DATABASE_FILE)).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM characters", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    /// exec_audit 表迁移(阶段 B/C):旧库(无此表)建表成功、幂等可重复执行、
    /// 索引存在、且可正常插入/查询(审计写入路径依赖这些列)。
    #[test]
    fn ensure_exec_audit_table_creates_and_is_idempotent() {
        let dir = temp_dir("exec-audit");
        let db = dir.join(DATABASE_FILE);
        let conn = Connection::open(&db).unwrap();

        // 旧库:无 exec_audit 表
        ensure_exec_audit_table(&conn).unwrap();
        // 幂等:重复执行不报错
        ensure_exec_audit_table(&conn).unwrap();

        // 插入一行(列齐全)后能读回
        conn.execute(
            "INSERT INTO exec_audit (ts, source, command, shell, tier, risk, decision, exit_code) \
             VALUES ('2026-09-13T00:00:00Z', 'chat', 'ls -la', 'sh', 'sandbox', 'safe', 'allowed', 0)",
            [],
        )
        .unwrap();
        let (cmd, decision): (String, String) = conn
            .query_row(
                "SELECT command, decision FROM exec_audit ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(cmd, "ls -la");
        assert_eq!(decision, "allowed");

        // 索引已建(按 ts 查询可用)
        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_exec_audit_ts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 1, "审计时间索引应存在");
    }
}
