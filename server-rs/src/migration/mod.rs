// 数据迁移模块(目录化拆分,纯代码移动,逻辑不变):
//   backup.rs   数据库快照备份(SQLite Online Backup API,含未 checkpoint 的 WAL)
//   merge.rs    双库合并:schema 一致性比对、按外键依赖序逐表合并、文件树/JSON 配置合并、合并后校验
//   ddl.rs      幂等 DDL 升级:新增表 DDL 常量与 ensure_* 补列/补表函数
//   conflict.rs 冲突处理:主键重映射、外键改写、等价行探测、文件冲突改名与值工具
// 本文件只做公共入口与再导出,保证 `kedai_server::migration::*` 对外路径不变。
mod backup;
mod conflict;
mod ddl;
mod merge;

pub use backup::snapshot_database;
pub use ddl::{
    ensure_llm_requests_usage_columns, ensure_memory_entries_fts_backfill,
    ensure_memory_entries_pinned_column, ensure_skills_progressive_columns,
    ensure_task_llm_calls_finish_reason_column, ensure_task_messages_table,
    ensure_tasks_task_mode_column,
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
use std::fs;
#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("kedai-migration-{tag}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
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
        fs::remove_dir_all(dir).ok();
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
        fs::remove_dir_all(dir).ok();
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
        fs::remove_dir_all(dir).ok();
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
        fs::remove_dir_all(dir).ok();
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
        fs::remove_dir_all(dir).ok();
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
        fs::remove_dir_all(dir).ok();
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
        fs::remove_dir_all(dir).ok();
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
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn merge_remaps_conflicting_keys_and_is_idempotent() {
        let baseline = temp_dir("baseline");
        let source = temp_dir("source");
        let first = temp_dir("first").join("work");
        let second = temp_dir("second").join("work");
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

        fs::remove_dir_all(baseline).ok();
        fs::remove_dir_all(source).ok();
    }
}
