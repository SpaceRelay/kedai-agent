// SQLite 访问层:rusqlite,建表与 Node 版(node:sqlite)完全一致,兼容现有 kedai.db
// 并发模型(2026-08 DB 并发改造):单写连接(Mutex 串行) + 只读连接池(r2d2),
// WAL 模式下读不阻塞写、写不阻塞读;写锁中毒恢复沿用 unwrap_or_else(into_inner)。
// 建表 SQL 在 schema.rs;启动幂等回填(scope_variables 镜像 + 游标)在 backfill.rs。
mod backfill;
mod schema;

pub use schema::create_tables_sql;

use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, Once};

/// 只读连接池 / 池化只读连接类型别名(简化调用点签名)
pub type ReadPool = r2d2::Pool<SqliteConnectionManager>;
pub type PooledRead = r2d2::PooledConnection<SqliteConnectionManager>;

/// 注册 sqlite-vec 扩展(静态链接)。
/// `sqlite3_auto_extension` 是进程级全局注册:注册一次后,之后打开的**所有**连接
/// 自动加载 vec0 虚拟表模块,无需逐连接调用。必须在本进程打开任何连接之前完成,
/// 故用 Once 幂等保护,并在 Db::open 最开头调用。
///
/// 安全性:sqlite_vec_init 是 sqlite-vec crate 导出的标准扩展入口,签名与
/// SQLite 要求的 `sqlite3_extension_init` 一致;transmute 是官方文档给的用法。
pub fn register_sqlite_vec() {
    /// SQLite 扩展初始化函数指针类型(sqlite3_auto_extension 要求的签名)
    type ExtInit = unsafe extern "C" fn(
        *mut rusqlite::ffi::sqlite3,
        *mut *mut std::os::raw::c_char,
        *const rusqlite::ffi::sqlite3_api_routines,
    ) -> std::os::raw::c_int;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let init: ExtInit = std::mem::transmute::<*const (), ExtInit>(
            sqlite_vec::sqlite3_vec_init as *const (),
        );
        rusqlite::ffi::sqlite3_auto_extension(Some(init));
    });
}

pub struct Db {
    /// 唯一写连接:所有写 SQL 经此串行执行(沿用既有单连接语义)
    writer: Mutex<Connection>,
    /// 只读连接池:WAL 模式下读与写互不阻塞;池大小 = CPU 核数
    readers: ReadPool,
}

impl Db {
    pub fn open(db_path: &Path, data_dir: &Path) -> Result<Self, String> {
        // 向量扩展必须最先注册:之后所有连接(写连接 + 池中只读连接)才能用 vec0
        register_sqlite_vec();
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
        conn.execute_batch(schema::CREATE_TABLES)
            .map_err(|e| format!("建表失败: {e}"))?;
        // 幂等 schema 升级(2026-08 缓存感知管线):旧库 llm_requests 补 usage 缓存列
        crate::migration::ensure_llm_requests_usage_columns(&conn)
            .map_err(|e| format!("升级 llm_requests 缓存列失败: {e}"))?;
        // 幂等 schema 升级(落地项 3 技能渐进披露):旧库 skills 补 allowed_tools 等列
        crate::migration::ensure_skills_progressive_columns(&conn)
            .map_err(|e| format!("升级 skills 渐进披露列失败: {e}"))?;
        // 幂等 schema 升级(批次 4 六模式):旧库 tasks 补 task_mode 列
        crate::migration::ensure_tasks_task_mode_column(&conn)
            .map_err(|e| format!("升级 tasks task_mode 列失败: {e}"))?;
        // 幂等 schema 升级(可观测性问题①):旧库 task_llm_calls 补 finish_reason 列
        crate::migration::ensure_task_llm_calls_finish_reason_column(&conn)
            .map_err(|e| format!("升级 task_llm_calls finish_reason 列失败: {e}"))?;
        // 幂等 schema 升级(批次 R2 多轮用户输入):旧库补建 task_messages 表
        crate::migration::ensure_task_messages_table(&conn)
            .map_err(|e| format!("升级 task_messages 表失败: {e}"))?;
        // 幂等 schema 升级(升级工作流 B2):旧库 memory_entries 补 pinned 列
        crate::migration::ensure_memory_entries_pinned_column(&conn)
            .map_err(|e| format!("升级 memory_entries pinned 列失败: {e}"))?;
        // 幂等回填(升级工作流 B1):记忆全文索引首次建表后 rebuild 一次
        crate::migration::ensure_memory_entries_fts_backfill(&conn)
            .map_err(|e| format!("回填 memory_entries FTS 索引失败: {e}"))?;
        backfill::backfill_scope_variables(&conn)?;

        // 只读连接池:READ_ONLY 标志防止读路径误写;busy_timeout/foreign_keys 与写连接对齐。
        // 写连接全程持有(WAL/-shm 存在),只读连 WAL 库在 SQLite ≥3.22 下安全。
        // 池大小取 CPU 核数(读多为短查询,更多连接只会争 IO)。
        let pool_size = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(4);
        let manager = SqliteConnectionManager::file(db_path)
            .with_flags(OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_init(|c| {
                c.busy_timeout(std::time::Duration::from_secs(5))?;
                c.pragma_update(None, "foreign_keys", "ON")?;
                Ok(())
            });
        let readers = ReadPool::builder()
            .max_size(pool_size)
            .build(manager)
            .map_err(|e| format!("创建只读连接池失败: {e}"))?;
        Ok(Db {
            writer: Mutex::new(conn),
            readers,
        })
    }

    /// 取只读连接(池化):SELECT 专用;连接归还由 Drop 完成
    pub fn read(&self) -> Result<PooledRead, String> {
        self.readers
            .get()
            .map_err(|e| format!("获取只读连接失败: {e}"))
    }

    /// 取写连接(互斥):写 SQL 与「读+写同事务」混合场景使用;锁中毒按既有纪律恢复
    pub fn write(&self) -> MutexGuard<'_, Connection> {
        self.writer.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// ISO 时间戳(与 Node 版 new Date().toISOString() 对齐,UTC)
pub fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读写分离:写连接已提交的写入,只读池连接立即可见(WAL)
    #[test]
    fn read_pool_sees_committed_writes() {
        let dir = std::env::temp_dir().join(format!("kedai-db-rw-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("kedai.db"), &dir).unwrap();
        let id = {
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
            conn.execute(
                "INSERT INTO messages (session_id, role, content, extra, created_at) VALUES ('s1', 'user', 'x', '{}', '')",
                [],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let conn = db.read().unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE id = ?1 AND session_id = ?2",
                rusqlite::params![id, "s1"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        drop(conn);
        drop(db);
        std::fs::remove_dir_all(dir).ok();
    }
}
