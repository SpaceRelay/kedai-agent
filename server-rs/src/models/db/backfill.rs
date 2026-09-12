// 启动幂等回填:把既有存储镜像进 scope_variables 表(自 db.rs 迁入)
use rusqlite::Connection;

/// 启动幂等回填(计划二 · 7 作用域变量;2026-08 起 message 部分改增量游标):
/// 把既有存储镜像进 scope_variables 表。
/// 全部使用 INSERT OR IGNORE(主键 (scope, scope_id) 已存在即跳过),可重复执行不产生重复行。
///
/// message 回填游标语义:backfill_meta['message_scope_last_id'] = 已扫描到的 messages.id
/// 上界(含 extra='{}' 无需回填的行)。启动只扫 id > 游标 的增量,扫完把游标推进到
/// MAX(messages.id)。无游标的旧库升级:若 scope_variables 已有 'message' 行,说明旧版本
/// 已做过全量回填,游标直接初始化为 MAX(messages.id),避免重复全表扫描。
pub(super) fn backfill_scope_variables(conn: &Connection) -> Result<(), String> {
    // 1) chat 镜像:session_assistant_vars 整树 → scope_variables('chat', session_id)
    //    (单条 INSERT..SELECT,代价低,保留全量)
    conn.execute(
        "INSERT OR IGNORE INTO scope_variables (scope, scope_id, data_raw, updated_at)
         SELECT 'chat', session_id, data_raw, updated_at FROM session_assistant_vars",
        [],
    )
    .map_err(|e| format!("回填 chat 作用域镜像失败: {e}"))?;

    // 2) message 回填(增量):messages.extra.mvu.stat_data 子树 → scope_variables('message', id)
    const CURSOR_KEY: &str = "message_scope_last_id";
    let cursor: Option<i64> = conn
        .query_row(
            "SELECT value FROM backfill_meta WHERE key = ?1",
            [CURSOR_KEY],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|s| s.parse::<i64>().ok());
    let max_message_id: i64 = conn
        .query_row("SELECT COALESCE(MAX(id), 0) FROM messages", [], |row| {
            row.get(0)
        })
        .map_err(|e| format!("读取消息最大 id 失败: {e}"))?;
    let last_id = match cursor {
        Some(v) => v,
        None => {
            // 旧库兜底:已有 message 作用域回填行 → 视为旧版全量已完成,游标跳到当前上界
            let already_backfilled: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM scope_variables WHERE scope = 'message')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|v| v == 1)
                .unwrap_or(false);
            if already_backfilled {
                conn.execute(
                    "INSERT OR REPLACE INTO backfill_meta (key, value) VALUES (?1, ?2)",
                    rusqlite::params![CURSOR_KEY, max_message_id.to_string()],
                )
                .map_err(|e| format!("初始化 message 回填游标失败: {e}"))?;
                max_message_id
            } else {
                0 // 全新库或从未回填:全量扫描一次
            }
        }
    };
    if last_id >= max_message_id {
        return Ok(()); // 无增量
    }
    let mut stmt = conn
        .prepare("SELECT id, extra FROM messages WHERE id > ?1 AND extra != '{}'")
        .map_err(|e| format!("准备消息回填语句失败: {e}"))?;
    let rows = stmt
        .query_map([last_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
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
            rusqlite::params![id.to_string(), stat_data.to_string(), super::now_iso()],
        )
        .map_err(|e| format!("回填 message 作用域失败: {e}"))?;
    }
    // 游标推进到消息表上界(不是回填行最大 id):extra='{}' 的行也已确认无需回填
    conn.execute(
        "INSERT OR REPLACE INTO backfill_meta (key, value) VALUES (?1, ?2)",
        rusqlite::params![CURSOR_KEY, max_message_id.to_string()],
    )
    .map_err(|e| format!("更新 message 回填游标失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::models::db::Db;

    /// 造一个带 character+session 的临时库,返回 (db, 目录, session_id)
    fn fixture(tag: &str) -> (Db, std::path::PathBuf, &'static str) {
        let dir = std::env::temp_dir().join(format!("kedai-db-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("kedai.db"), &dir).unwrap();
        let conn = db.write();
        conn.execute(
            "INSERT INTO characters (id, name, chara_name, description, file_path, data_raw, created_at)
             VALUES ('c1', 'c', 'c', '', '', '{}', '')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, character_id, title, created_at, updated_at)
             VALUES ('s1', 'c1', 't', '', '')",
            [],
        )
        .unwrap();
        drop(conn);
        (db, dir, "s1")
    }

    fn add_msg(db: &Db, session_id: &str, extra: &str) -> i64 {
        let conn = db.write();
        conn.execute(
            "INSERT INTO messages (session_id, role, content, extra, created_at) VALUES (?1, 'user', 'x', ?2, '')",
            rusqlite::params![session_id, extra],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn cursor_of(db: &Db) -> Option<i64> {
        let conn = db.read().unwrap();
        conn.query_row(
            "SELECT value FROM backfill_meta WHERE key = 'message_scope_last_id'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|s| s.parse().ok())
    }

    fn scope_row(db: &Db, scope_id: &str) -> Option<String> {
        let conn = db.read().unwrap();
        conn.query_row(
            "SELECT data_raw FROM scope_variables WHERE scope = 'message' AND scope_id = ?1",
            [scope_id],
            |row| row.get(0),
        )
        .ok()
    }

    /// 增量回填:首批消息回填后,追加新消息再 open 只扫增量(游标推进,新行入库)
    #[test]
    fn backfill_message_scope_is_incremental() {
        let (db, dir, sid) = fixture("incr");
        let extra = r#"{"mvu":{"stat_data":{"hp":10}}}"#;
        let id1 = add_msg(&db, sid, extra);
        let id_empty = add_msg(&db, sid, "{}"); // 无需回填的行也要被游标覆盖
        drop(db);

        // 重新 open:首次回填(无游标、无存量 message 行 → 全量)
        let db = Db::open(&dir.join("kedai.db"), &dir).unwrap();
        assert_eq!(
            scope_row(&db, &id1.to_string()).as_deref(),
            Some(r#"{"hp":10}"#)
        );
        assert!(scope_row(&db, &id_empty.to_string()).is_none());
        assert_eq!(cursor_of(&db), Some(id_empty), "游标应推进到消息表上界");

        // 追加新消息后再次 open:只扫增量
        let id2 = add_msg(&db, sid, extra);
        drop(db);
        let db = Db::open(&dir.join("kedai.db"), &dir).unwrap();
        assert_eq!(
            scope_row(&db, &id2.to_string()).as_deref(),
            Some(r#"{"hp":10}"#),
            "新增消息应被增量回填"
        );
        assert_eq!(cursor_of(&db), Some(id2));
        drop(db);
        std::fs::remove_dir_all(dir).ok();
    }

    /// 旧库升级兜底:无游标但 scope_variables 已有 message 行(旧版全量已跑过)
    /// → 游标直接初始化为 MAX(messages.id),不做重复全扫
    #[test]
    fn backfill_cursor_initialized_from_existing_rows() {
        let (db, dir, sid) = fixture("legacy");
        let extra = r#"{"mvu":{"stat_data":{"hp":1}}}"#;
        let id1 = add_msg(&db, sid, extra);
        // 模拟旧版回填产物:scope_variables 已有 message 行,但无 backfill_meta 游标
        {
            let conn = db.write();
            conn.execute(
                "INSERT OR REPLACE INTO scope_variables (scope, scope_id, data_raw, updated_at)
                 VALUES ('message', ?1, '{\"hp\":1}', '')",
                [id1.to_string()],
            )
            .unwrap();
            conn.execute(
                "DELETE FROM backfill_meta WHERE key = 'message_scope_last_id'",
                [],
            )
            .unwrap();
        }
        let id2 = add_msg(&db, sid, extra); // 游标缺失后追加的消息
        drop(db);

        let db = Db::open(&dir.join("kedai.db"), &dir).unwrap();
        // 游标应直接初始化到 MAX(id),id2 不补扫(与旧版全量语义一致:不重复回填)
        assert_eq!(cursor_of(&db), Some(id2));
        // 既有行不被覆盖(INSERT OR IGNORE 语义保持)
        assert_eq!(
            scope_row(&db, &id1.to_string()).as_deref(),
            Some(r#"{"hp":1}"#)
        );
        drop(db);
        std::fs::remove_dir_all(dir).ok();
    }
}
