// SQLite 访问层:rusqlite,建表与 Node 版(node:sqlite)完全一致,兼容现有 kedai.db
// 并发模型(2026-08 DB 并发改造):单写连接(Mutex 串行) + 只读连接池(r2d2),
// WAL 模式下读不阻塞写、写不阻塞读;写锁中毒恢复沿用 unwrap_or_else(into_inner)。
// 建表 SQL 在 schema.rs;启动幂等回填(scope_variables 镜像 + 游标)在 backfill.rs。
mod backfill;
mod schema;

pub use schema::{create_tables_sql, SCHEMA_VERSION};

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
        let init: ExtInit =
            std::mem::transmute::<*const (), ExtInit>(sqlite_vec::sqlite3_vec_init as *const ());
        rusqlite::ffi::sqlite3_auto_extension(Some(init));
    });
}

pub struct Db {
    /// 唯一写连接:所有写 SQL 经此串行执行(沿用既有单连接语义)
    writer: Mutex<Connection>,
    /// 只读连接池:WAL 模式下读与写互不阻塞;池大小 = CPU 核数
    readers: ReadPool,
}

/// 判断本次 `Db::open` 是否属于「对既有库做 schema 升级」,从而需要在升级前备份。
///
/// 三种情形:
///   · `db_version >= SCHEMA_VERSION` → 已是当前版本(或更高,已被上方拒绝分支拦截),
///     无需升级 → 不备份。**这条同时保证「每次启动都备份」不会发生**。
///   · `db_version == 0` → 全新库与未标记旧库共用此值,必须再看库里有没有业务表:
///     没有任何非 `sqlite_%` 的表 = 全新库(不备份);有表 = 0.3.0-beta 及更早的旧库(备份)。
///   · `0 < db_version < SCHEMA_VERSION` → 明确的老版本库 → 备份。
///
/// 读取失败按「不备份」处理:此处只影响是否留档,不能反过来阻断启动;
/// 真有问题会在随后的建表/迁移阶段以更明确的错误暴露。
fn needs_preupgrade_backup(conn: &Connection, db_version: i32) -> bool {
    if db_version >= schema::SCHEMA_VERSION {
        return false;
    }
    if db_version > 0 {
        return true;
    }
    // version == 0:看是否已有业务表(新库此刻还没有任何表)
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' \
         AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
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
        // ===== 写路径调优(2026-09-16 性能批次 P-1)=====
        // 此前上述三项之外全走 SQLite 默认值,其中两项对本应用明显不合适:
        //
        // ① synchronous:WAL 下默认仍是 FULL,即每次 commit 都 fsync。但 WAL 的
        //    FULL 与 DELETE 模式的 FULL 语义不同——WAL 里数据页在 commit 时已写入
        //    WAL 文件,**断电最坏只丢最近若干次已提交事务,数据库不会损坏**(完整性
        //    由 WAL 校验与恢复保证)。本应用写点极密(每条消息、任务每步状态、
        //    每次 usage 落库),FULL 的额外 fsync 直接压在生成与任务循环关键路径上,
        //    换来的只是「最后一条消息不丢」这一档收益,不值。
        //    **取舍显式记录**:掉电可能丢最近若干次提交(最坏丢最后一条消息/状态),
        //    但不损坏库;本机单用户应用接受此权衡。若需最强持久性,改回 FULL 即可
        //    (单行变更,无其他连带)。
        // ② temp_store/cache_size:默认 temp_store=FILE(排序/临时表落盘)与
        //    cache_size=-2000(约 2MiB 页缓存)。列表查询带 ORDER BY、压缩管线做
        //    多表连接,临时表与缓存命中率都吃紧,故提到 MEMORY + 16MiB。
        // ③ wal_autocheckpoint 保持默认(1000 页):checkpoint 是后台摊还成本,
        //    调大只会把 WAL 撑得更久,调小增加写停顿,默认值已合适。
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| format!("设置 synchronous 失败: {e}"))?;
        conn.pragma_update(None, "temp_store", "MEMORY")
            .map_err(|e| format!("设置 temp_store 失败: {e}"))?;
        conn.pragma_update(None, "cache_size", -16000_i64)
            .map_err(|e| format!("设置 cache_size 失败: {e}"))?;
        // 建表批之前先读库内 schema 版本(PRAGMA user_version,0 = 未标记的旧库)。
        // 库比代码新时**拒绝启动**:旧代码写新库会毁数据,宁可明确报错也不静默打开
        // (docs/契约-架构与数据.md §4.2 策略 1)。
        let db_version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|e| format!("读取数据库 schema 版本失败: {e}"))?;
        if db_version > schema::SCHEMA_VERSION {
            return Err(format!(
                "数据库由更新版本的 Kedai 写入(库 v{db_version} 高于应用支持的 v{});\
                 请升级应用后再打开,或从升级前的备份恢复(旧版本继续写入可能损坏数据)",
                schema::SCHEMA_VERSION
            ));
        }
        // 升级链事务化(known-limitations L21):建表批 + 11 个 ensure_* 包在同一个
        // 事务里。SQLite 的 DDL 是事务性的,任一步失败整链回滚,不留「部分升级态」。
        // 回滚必须落在 `?`/早退之前:否则连接停在打开的事务里,后续 Db::write 撞锁。
        //
        // 升级前自动备份(DB-2 / 计划批次):在动任何 schema 之前先留一份快照。判定
        // 「需要升级」必须区分两种 user_version=0 的库(**这点极易写错**):
        //   · 全新库:文件刚建、库里**没有任何业务表** → 无需备份;
        //   · 未标记的旧库:version=0 但有业务表(0.3.0-beta 及更早)→ **必须备份**。
        // 只判 `db_version > 0` 会漏掉后者,而那恰是真实用户升级的主路径。
        if needs_preupgrade_backup(&conn, db_version) {
            match crate::migration::snapshot_before_upgrade(db_path, data_dir, db_version) {
                Ok(p) => tracing::info!(path = %p.display(), "已生成升级前数据库快照"),
                // 失败绝不阻断启动:备份是「多一份保险」,不能因为它失败反而让库打不开
                // (与下方 PRAGMA optimize 的处置口径一致)。
                Err(e) => tracing::warn!("升级前数据库快照失败(不阻断启动): {e}"),
            }
        }
        let upgrade: Result<(), String> = (|| {
            conn.execute_batch("BEGIN IMMEDIATE;")
                .map_err(|e| format!("开启 schema 升级事务失败: {e}"))?;
            let body = (|| -> Result<(), String> {
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
                // 幂等 schema 升级(批次 4 子任务终态语义):两张子任务表补 finished_at 列
                crate::migration::ensure_agent_subtasks_finished_at_column(&conn)
                    .map_err(|e| format!("升级 agent_subtasks finished_at 列失败: {e}"))?;
                crate::migration::ensure_task_subtasks_finished_at_column(&conn)
                    .map_err(|e| format!("升级 task_subtasks finished_at 列失败: {e}"))?;
                // 幂等 schema 升级(批次 R2 多轮用户输入):旧库补建 task_messages 表
                crate::migration::ensure_task_messages_table(&conn)
                    .map_err(|e| format!("升级 task_messages 表失败: {e}"))?;
                // 幂等 schema 升级(升级工作流 B2):旧库 memory_entries 补 pinned 列
                crate::migration::ensure_memory_entries_pinned_column(&conn)
                    .map_err(|e| format!("升级 memory_entries pinned 列失败: {e}"))?;
                // 幂等 schema 升级(阶段 B/C):旧库补建 exec_audit 审计表
                crate::migration::ensure_exec_audit_table(&conn)
                    .map_err(|e| format!("升级 exec_audit 表失败: {e}"))?;
                // 幂等 schema 升级(批次 3 性能):旧库补建 sessions/tasks 列表查询索引
                crate::migration::ensure_perf_indexes(&conn)
                    .map_err(|e| format!("升级性能索引失败: {e}"))?;
                Ok(())
            })();
            let failure: String = match body {
                Ok(()) => match conn.execute_batch("COMMIT;") {
                    Ok(()) => return Ok(()),
                    Err(e) => format!("提交 schema 升级事务失败: {e}"),
                },
                Err(e) => e,
            };
            // 失败路径:回滚到升级前状态。回滚失败只留痕(原错误信息优先),
            // 但这是不变量被破坏的情形,日志必须显式可见。
            if let Err(rollback) = conn.execute_batch("ROLLBACK;") {
                tracing::error!("schema 升级失败后 ROLLBACK 也失败,连接可能停在事务中: {rollback}");
            }
            Err(failure)
        })();
        upgrade?;
        // 重量级幂等回填留在事务之外:全表 FTS rebuild 会把 WAL 撑大(长写事务代价高),
        // 且两者各靠 backfill_meta 标记幂等(见 migration/ddl.rs 与 backfill.rs)。
        // 幂等回填(升级工作流 B1):记忆全文索引首次建表后 rebuild 一次
        crate::migration::ensure_memory_entries_fts_backfill(&conn)
            .map_err(|e| format!("回填 memory_entries FTS 索引失败: {e}"))?;
        backfill::backfill_scope_variables(&conn)?;
        // 版本号最后写:过早抬高会让下次启动误判「已最新」而跳过补迁(L18 补齐路径)。
        conn.pragma_update(None, "user_version", schema::SCHEMA_VERSION)
            .map_err(|e| format!("写入数据库 schema 版本失败: {e}"))?;
        // 启动收尾跑一次 PRAGMA optimize(2026-09-16 性能批次 P-1):SQLite 官方推荐
        // 的「长期连接定期执行」动作——按需 ANALYZE 缺失统计并释放临时索引,让查询
        // 规划器拿到准确的基数估计。放在版本号写入之后:升级/建表已完成,统计才可信。
        // 幂等且通常极快(统计已新鲜时为 no-op);失败不阻断启动(仅留痕)。
        if let Err(e) = conn.execute_batch("PRAGMA optimize;") {
            tracing::warn!("启动 PRAGMA optimize 失败(不阻断启动): {e}");
        }

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
                // 每个池连接各有一份独立页缓存,故需逐连接设置(2026-09-16 性能批次 P-1)。
                // synchronous 对只读连接无意义(不产生提交),不设;WAL 模式是库级属性,
                // 由写连接在 open 时持久化,只读连接打开时自动沿用。
                c.pragma_update(None, "temp_store", "MEMORY")?;
                c.pragma_update(None, "cache_size", -16000_i64)?;
                Ok(())
            });
        let readers = ReadPool::builder()
            .max_size(pool_size)
            // 池借还超时(2026-09-16 性能批次 P-3):r2d2 默认 30s,池耗尽时
            // 一次取连接会静默阻塞半分钟——在 tokio worker 上就是事件循环停摆。
            // 收到 5s 与 busy_timeout 对齐:超时即返回错误,由 db_err 转 500,
            // 好过让整个请求挂到 30s。
            .connection_timeout(std::time::Duration::from_secs(5))
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

    /// 取写连接(互斥):写 SQL 与「读+写同事务」混合场景使用。
    ///
    /// 锁中毒恢复(2026-09-13 批次 2 加强):中毒说明上一次持锁者在 rusqlite 调用中途
    /// panic,连接可能停在未完成的隐式/显式事务里;此前直接 `into_inner()` 续用,语义未定义。
    /// 现在先留痕,再检查 `is_autocommit()`——非 autocommit 说明事务未收尾,显式 ROLLBACK
    /// 回退到干净状态后交还。仍选择「可用性优先」:本地单用户应用里,让整个应用崩掉
    /// 比带一处不一致继续跑更糟(与全仓锁中毒纪律一致)。
    pub fn write(&self) -> MutexGuard<'_, Connection> {
        match self.writer.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let guard = poisoned.into_inner();
                tracing::error!(
                    "db writer 互斥锁中毒(上一次持锁期间 panic);已恢复,如出现数据异常请重启应用"
                );
                if !guard.is_autocommit() {
                    tracing::error!("db writer 停在未完成事务中,执行 ROLLBACK 回退");
                    let _ = guard.execute_batch("ROLLBACK;");
                }
                guard
            }
        }
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
    use crate::utils::test_support::TempDataDir;

    /// 读写分离:写连接已提交的写入,只读池连接立即可见(WAL)
    #[test]
    fn read_pool_sees_committed_writes() {
        let dir = TempDataDir::new("db-rw");
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
    }

    /// 写连接调优参数:此前只设 WAL/外键/busy_timeout,其余走 SQLite 默认值,
    /// 其中 synchronous=FULL 对 WAL 库属过度保守——WAL 已保证「断电不损坏库」,
    /// FULL 只是让每次 commit 多一次 fsync,而本应用写点极密(每条消息/任务状态/usage),
    /// 这笔开销直接压在生成与任务循环的关键路径上。
    /// (2026-09-16 性能批次 P-1)
    #[test]
    fn writer_pragmas_configured_for_write_throughput() {
        let dir = TempDataDir::new("db-pragma-writer");
        let db = Db::open(&dir.join("kedai.db"), &dir).unwrap();
        let conn = db.write();
        let synchronous: i64 = conn
            .pragma_query_value(None, "synchronous", |r| r.get(0))
            .unwrap();
        assert_eq!(
            synchronous, 1,
            "synchronous 应为 NORMAL(1),而非默认 FULL(2)"
        );
        let temp_store: i64 = conn
            .pragma_query_value(None, "temp_store", |r| r.get(0))
            .unwrap();
        assert_eq!(temp_store, 2, "temp_store 应为 MEMORY(2)");
        let cache_size: i64 = conn
            .pragma_query_value(None, "cache_size", |r| r.get(0))
            .unwrap();
        assert_eq!(cache_size, -16000, "cache_size 应为 16MiB(负数为 KiB)");
        // WAL 仍是库级持久设置(不受本次调整影响)
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");
        drop(conn);
        drop(db);
    }

    /// 只读池连接共用写连接的页缓存/临时表调优(每个池连接各有一份缓存,
    /// 缺省 2MiB 对「全量列表 + 排序」类查询偏小);synchronous 对只读连接无意义,不设。
    /// (2026-09-16 性能批次 P-1)
    #[test]
    fn read_pool_connections_share_tuning_pragmas() {
        let dir = TempDataDir::new("db-pragma-reader");
        let db = Db::open(&dir.join("kedai.db"), &dir).unwrap();
        let conn = db.read().unwrap();
        let cache_size: i64 = conn
            .pragma_query_value(None, "cache_size", |r| r.get(0))
            .unwrap();
        assert_eq!(cache_size, -16000, "只读连接也应拿到 16MiB 页缓存");
        let temp_store: i64 = conn
            .pragma_query_value(None, "temp_store", |r| r.get(0))
            .unwrap();
        assert_eq!(temp_store, 2, "只读连接也应 temp_store=MEMORY");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(
            version, SCHEMA_VERSION as i64,
            "版本号写入不受本次 pragma 调整影响"
        );
        drop(conn);
        drop(db);
    }

    /// 写连接锁中毒可恢复:未完成事务被 ROLLBACK,连接仍可继续使用
    /// (2026-09-13 批次 2:此前直接 into_inner 续用,事务残留语义未定义)
    #[test]
    fn poisoned_writer_lock_recovers_and_rolls_back_open_transaction() {
        let dir = TempDataDir::new("db-poison");
        let db = Db::open(&dir.join("kedai.db"), &dir).unwrap();

        // 持写锁开事务后 panic:锁中毒,且连接停在未完成事务里
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let conn = db.write();
            conn.execute_batch("BEGIN; INSERT INTO characters (id, name, chara_name, description, file_path, data_raw, created_at) VALUES ('c-poison','c','c','','','{}','');")
                .unwrap();
            panic!("模拟持锁 panic");
        }));
        assert!(result.is_err(), "应捕获到 panic");
        assert!(db.writer.is_poisoned(), "锁应已中毒");

        // 恢复路径:自动 ROLLBACK,连接回到 autocommit 且可继续写
        let conn = db.write();
        assert!(conn.is_autocommit(), "未完成事务应已被 ROLLBACK");
        conn.execute(
            "INSERT INTO characters (id, name, chara_name, description, file_path, data_raw, created_at)
             VALUES ('c-after', 'c', 'c', '', '', '{}', '')",
            [],
        )
        .expect("恢复后的连接应仍可写");
        // 被回滚的插入不应存在(证明事务确实未提交)
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM characters WHERE id = 'c-poison'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "panic 前未提交的插入必须被回滚");
        drop(conn);
        drop(db);
    }
}
