// 迁移元测试(2026-09-13 批次 2,P0-2):防「schema.rs 加了列/表/索引但忘了写
// ensure_* 迁移」——症状是旧库升级后运行时 `no such column`,只在老用户机器上炸,
// 新装机与其它全量测试都发现不了。
//
// 做法:
//   ① 用冻结基线(fixtures/schema_baseline_v0_3_0_beta.sql = 0.3.0-beta 发布时的
//      CREATE_TABLES 原文)在临时目录建一个「老库」;
//   ② 对该库执行 Db::open(启动路径会跑全部 ensure_* 幂等迁移);
//   ③ 另建全新库(Db::open 直接建全量 schema);
//   ④ 逐表比对结构指纹:PRAGMA table_info(名称/类型/NOT NULL/默认值/主键)+ 索引清单。
// 两侧必须完全一致;新增结构而漏写迁移 → 不一致 → 本测试失败。
//
// 批次 3 扩测(2026-09-15,兑现 known-limitations L18「无 user_version」与
// L21「升级链无事务」):补 7 条验收断言,方案与断言清单见
// docs/契约-架构与数据.md §4.1-§4.3——
//   1. 全新库写入 SCHEMA_VERSION;
//   2. 老库升级后版本正确且结构指纹与全新库一致;
//   3. 库版本 > 代码版本 → Db::open 拒绝启动,文案含两个版本号(§4.2 策略 1);
//   4. 连续两次 open 幂等(版本/结构都不变);
//   5. 某个 ensure_* 失败 → 整链回滚:版本不抬、CREATE_TABLES 新建表与已跑补列都撤销;
//   6. 合并产物/工作库的 user_version 对齐;
//   7. 静态护栏:migration/ddl.rs 的 *_DDL 文本必须与 CREATE_TABLES normalize 后一致。
use kedai_server::models::db::{Db, SCHEMA_VERSION};
use kedai_server::utils::test_support::TempDataDir;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::Path;

/// 冻结基线建表 SQL(**历史快照,不得随代码演进更新**;见文件头注释)
const BASELINE_SQL: &str = include_str!("fixtures/schema_baseline_v0_3_0_beta.sql");

/// 隔离临时数据目录(uuid 唯一 + 作用域结束自动清理);
/// 用例内如需跨两次 open 复用同一路径,守卫绑定在测试函数作用域即可
fn temp_dir(tag: &str) -> TempDataDir {
    TempDataDir::new(&format!("schema-meta-{tag}"))
}

/// 建一个「0.3.0-beta 老库」数据目录(user_version 天然为 0,即「未标记」)
fn legacy_dir(tag: &str) -> TempDataDir {
    let dir = temp_dir(tag);
    let conn = Connection::open(dir.join("kedai.db")).unwrap();
    conn.execute_batch(BASELINE_SQL)
        .expect("基线 SQL 应可执行(文件被截断?)");
    drop(conn);
    dir
}

/// 直连(不经 Db,便于断言「升级失败后」的库内状态)读 user_version
fn user_version(path: &Path) -> i32 {
    let conn = Connection::open(path).unwrap();
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

/// 直连读表存在性(sqlite_master 口径,只认实体表)
fn table_exists(path: &Path, table: &str) -> bool {
    object_exists(path, "table", table)
}

/// 直连读视图存在性(注入手法用视图占名,见失败回滚用例)
fn view_exists(path: &Path, view: &str) -> bool {
    object_exists(path, "view", view)
}

fn object_exists(path: &Path, kind: &str, name: &str) -> bool {
    let conn = Connection::open(path).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
        [kind, name],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

/// 直连读某表的列名(小写);表不存在返回空清单
fn columns_of(path: &Path, table: &str) -> Vec<String> {
    let conn = Connection::open(path).unwrap();
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let rows = stmt.query_map([], |row| row.get::<_, String>(1)).unwrap();
    rows.map(|row| row.unwrap().to_lowercase()).collect()
}

/// 结构指纹:表名 → 有序的「列定义 + 索引定义」清单
fn fingerprint(db: &Db) -> BTreeMap<String, Vec<String>> {
    let conn = db.write();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let tables: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    for t in tables {
        let mut items: Vec<String> = Vec::new();
        {
            // 表名来自 sqlite_master(非用户输入),format! 拼接无注入面
            let mut stmt = conn.prepare(&format!("PRAGMA table_info({t})")).unwrap();
            let rows = stmt
                .query_map([], |r| {
                    let name: String = r.get(1)?;
                    let ctype: String = r.get(2)?;
                    let notnull: i64 = r.get(3)?;
                    let dflt: Option<String> = r.get(4)?;
                    let pk: i64 = r.get(5)?;
                    Ok(format!(
                        "col:{name}|{ctype}|nn={notnull}|dflt={dflt:?}|pk={pk}"
                    ))
                })
                .unwrap();
            items.extend(rows.map(|r| r.unwrap()));
        }
        // FTS5 虚拟表不支持 PRAGMA index_list(会报错):跳过索引项,仅比列
        if let Ok(mut stmt) = conn.prepare(&format!("PRAGMA index_list({t})")) {
            let rows = stmt
                .query_map([], |r| {
                    let name: String = r.get(1)?;
                    let origin: String = r.get(3)?;
                    Ok(format!("idx:{name}|{origin}"))
                })
                .unwrap();
            let mut idx: Vec<String> = rows.map(|r| r.unwrap()).collect();
            idx.sort();
            items.extend(idx);
        }
        out.insert(t, items);
    }
    out
}

/// 【断言 2】老库(0.3.0-beta 基线)经 Db::open 升级后:结构必须与全新库逐表一致,
/// 且版本号被标记为 SCHEMA_VERSION
#[test]
fn old_database_upgrades_to_current_schema() {
    // ① 老库:执行冻结基线建表 SQL
    let old_dir = legacy_dir("old");
    let old_path = old_dir.join("kedai.db");
    // 老库天然是「未标记版本」(0 与 SCHEMA_VERSION 语义不同,不可混同)
    assert_eq!(
        user_version(&old_path),
        0,
        "冻结基线老库不得自带版本标记(否则「老库升级」用例失去意义)"
    );
    assert_ne!(
        SCHEMA_VERSION, 0,
        "0 保留给未标记的旧库,当前版本号必须是正数"
    );

    // ② 升级:Db::open 跑 ensure_* 迁移(与真实启动路径一致)
    let upgraded = Db::open(&old_path, &old_dir).expect("老库应能被 Db::open 升级打开");
    assert_eq!(
        user_version(&old_path),
        SCHEMA_VERSION,
        "老库升级成功后应写入当前 schema 版本"
    );

    // ③ 全新库
    let fresh_dir = temp_dir("fresh");
    let fresh = Db::open(&fresh_dir.join("kedai.db"), &fresh_dir).expect("全新库应能建立");

    // ④ 比对表集合与逐表结构
    let a = fingerprint(&upgraded);
    let b = fingerprint(&fresh);
    assert!(
        b.len() >= 20,
        "全新库表数异常({} 张):基线文件可能被截断或 schema 解析异常",
        b.len()
    );

    let only_upgraded: Vec<&String> = a.keys().filter(|k| !b.contains_key(*k)).collect();
    let only_fresh: Vec<&String> = b.keys().filter(|k| !a.contains_key(*k)).collect();
    assert!(
        only_upgraded.is_empty(),
        "升级库存在全新库没有的表(迁移多建了表?):{only_upgraded:?}"
    );
    assert!(
        only_fresh.is_empty(),
        "全新库存在升级库没有的表(新增表漏写 ensure_* 迁移?):{only_fresh:?}"
    );

    for (table, fresh_defs) in &b {
        let upgraded_defs = &a[table];
        assert_eq!(
            upgraded_defs, fresh_defs,
            "表 {table} 升级后结构与全新库不一致(新增列/索引漏写 ensure_* 迁移?)"
        );
    }
}

/// 【断言 1】全新库:升级链全部成功后写入 user_version == SCHEMA_VERSION
#[test]
fn fresh_database_records_schema_version() {
    let dir = temp_dir("fresh-version");
    let path = dir.join("kedai.db");
    let db = Db::open(&path, &dir).expect("全新库应能建立");
    assert_ne!(
        SCHEMA_VERSION, 0,
        "0 保留给未标记的旧库,当前版本号必须是正数"
    );
    assert_eq!(
        user_version(&path),
        SCHEMA_VERSION,
        "全新库建好后就应带当前 schema 版本标记(否则降级检测无从判断)"
    );
    // 版本号不是靠建表 SQL 写进去的(PRAGMA 不进 CREATE_TABLES,静态文本保持不变)
    drop(db);
}

/// 【断言 3】库版本 > 代码版本 → 拒绝启动(§4.2 策略 1),文案须同时含两个版本号,
/// 且在建表批之前就返回(不得对更新的库做任何写入)
#[test]
fn newer_database_version_is_rejected_with_both_versions() {
    let newer = SCHEMA_VERSION + 1;
    let dir = legacy_dir("newer-version");
    let path = dir.join("kedai.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", newer).unwrap();
    }

    let error = Db::open(&path, &dir)
        .err()
        .expect("库版本高于代码版本时必须拒绝启动(旧代码写新库会毁数据)");
    assert!(
        error.contains(&format!("v{newer}")),
        "文案应含库内版本号 v{newer}(用户要知道是哪一端的版本):{error}"
    );
    assert!(
        error.contains(&format!("v{SCHEMA_VERSION}")),
        "文案应含应用支持版本号 v{SCHEMA_VERSION}(用户要知道该升到哪):{error}"
    );
    // 分流在建表批之前:更新的库不得被老代码写任何结构
    assert!(
        !table_exists(&path, "script_authorizations"),
        "拒绝启动必须发生在建表批之前(更新的库不能被老代码改动)"
    );
    assert_eq!(user_version(&path), newer, "拒绝启动不得改写版本号");
}

/// 【断言 4】幂等:连续两次 Db::open 不改版本、不改结构(升级链每轮照跑但结果收敛)
#[test]
fn reopen_keeps_version_and_schema_unchanged() {
    // 用老库起步:同时覆盖「升级 → 再升级」两跳
    let dir = legacy_dir("idempotent");
    let path = dir.join("kedai.db");

    let first = Db::open(&path, &dir).expect("老库首次升级应成功");
    let before = fingerprint(&first);
    assert_eq!(user_version(&path), SCHEMA_VERSION);
    drop(first);

    let second = Db::open(&path, &dir).expect("第二次打开应成功(幂等)");
    assert_eq!(
        user_version(&path),
        SCHEMA_VERSION,
        "第二次 open 不得改动版本号"
    );
    assert_eq!(
        fingerprint(&second),
        before,
        "第二次 open 不得改动结构(升级链必须幂等)"
    );
    drop(second);
}

/// 【断言 5,锁 L21】某个 ensure_* 失败 → Db::open 返回 Err,且整链事务回滚:
/// 版本号保持旧值,失败前**已执行成功的补列与建表**全部撤销
#[test]
fn failed_ensure_rolls_back_whole_upgrade_chain_and_keeps_version() {
    let dir = legacy_dir("rollback");
    let path = dir.join("kedai.db");

    // 失败注入(两处,均只改老库的**既有对象**,不碰建表批):
    //
    // ① 让升级链第 1 步真正干活:把 llm_requests 还原成「无 usage 缓存列」的旧形态
    //    (与 migration/ddl.rs 的 LLM_REQUESTS_DDL 原文同款),这样
    //    ensure_llm_requests_usage_columns 会在失败之前真执行 4 条 ALTER ADD COLUMN。
    //
    // ② 让升级链第 2 步失败:老库的 skills 换成**同名视图**。为什么能可靠失败(已实测):
    //    - CREATE_TABLES 里 skills 只有 CREATE TABLE IF NOT EXISTS,且 skills 无索引,
    //      视图占名时被静默跳过 → 建表批不报错(不像 memory_entries 有索引,视图会被拒绝);
    //    - ensure_skills_progressive_columns 用 PRAGMA table_info(skills) 探测列,
    //      视图同样返回列(非零行)→ 判为「表已存在」→ ALTER TABLE skills ADD COLUMN
    //      → SQLite 报 "Cannot add a column to a view"。
    //    不能改用同名表:IF NOT EXISTS 跳过建表,且 ensure_* 探测到列齐全也跳过,永不失败。
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            // ① 旧形态 llm_requests(7 列;列定义与 ddl.rs LLM_REQUESTS_DDL 一致)
            "DROP TABLE llm_requests;
             CREATE TABLE llm_requests (
               id          INTEGER PRIMARY KEY AUTOINCREMENT,
               session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
               run_id      TEXT NOT NULL,
               seq         INTEGER NOT NULL,
               payload     TEXT NOT NULL,
               model       TEXT NOT NULL DEFAULT '',
               created_at  TEXT NOT NULL
             );
             -- ② 同名视图占住 skills
             DROP TABLE skills;
             CREATE VIEW skills AS SELECT 1 AS id;",
        )
        .unwrap();
    }
    assert_eq!(user_version(&path), 0, "注入后老库仍应是未标记版本");
    assert!(
        !columns_of(&path, "llm_requests")
            .iter()
            .any(|c| c == "prompt_cache_hit_tokens"),
        "注入后 llm_requests 应为无 usage 缓存列的旧形态(否则第 1 步不会真执行 ALTER)"
    );

    let error = Db::open(&path, &dir)
        .err()
        .expect("ensure_* 失败必须让 Db::open 返回 Err(失败即退出,不静默降级)");
    assert!(
        error.contains("skills"),
        "错误文案应指向失败的升级步骤(skills 渐进披露列):{error}"
    );

    // ① 版本不抬:失败时写版本会让下次启动误判「已最新」而跳过补迁
    assert_eq!(
        user_version(&path),
        0,
        "升级失败后 user_version 必须保持旧值(否则部分升级态会被当成最新库)"
    );
    // ② 结构回滚:同一事务里 CREATE_TABLES 新建的表必须撤销
    assert!(
        !table_exists(&path, "script_authorizations"),
        "事务回滚应撤销 CREATE_TABLES 新建的表(script_authorizations 在冻结基线中不存在)"
    );
    // ③ 结构回滚:失败**之前**已执行成功的 ensure_* 补列必须撤销
    //    (ensure_llm_requests_usage_columns 是升级链第 1 步,先于 skills 失败)
    assert!(
        !columns_of(&path, "llm_requests")
            .iter()
            .any(|c| c == "prompt_cache_hit_tokens"),
        "事务回滚应撤销失败前已执行的 ensure_* 补列(否则仍是部分升级态)"
    );
    // ④ 注入的既有对象原样保留(回滚不等于把老库清空)
    assert!(
        view_exists(&path, "skills"),
        "回滚后老库原有的视图应原样保留"
    );
    // ⑤ 不把连接留在打开的事务里:失败的 Db::open 返回后,另起连接必须能立刻写。
    //    (若升级链早退时没 ROLLBACK 而连接又被保留,这里会因写锁未释放而 SQLITE_BUSY;
    //    当前实现即使漏写 ROLLBACK,连接也会随 Err 析构回滚——本断言守住的是
    //    「失败后库文件可写」这一外部可观测不变量,后续若把连接改为复用/池化即会失效报警。)
    {
        let probe = Connection::open(&path).unwrap();
        probe
            .busy_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        probe
            .execute_batch("BEGIN IMMEDIATE; COMMIT;")
            .expect("升级链失败后不应有残留写事务/写锁(否则后续 Db::write 会撞锁)");
    }

    // ⑥ 恢复路径:移除注入后同一个库能一次升级成功——失败态可收敛,且没有残留写事务锁
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP VIEW skills").unwrap();
    }
    let db = Db::open(&path, &dir).expect("移除冲突后应能正常升级");
    assert_eq!(user_version(&path), SCHEMA_VERSION);
    assert!(
        columns_of(&path, "skills")
            .iter()
            .any(|c| c == "allowed_tools"),
        "恢复升级后 skills 应补齐渐进披露列"
    );
    assert!(
        columns_of(&path, "llm_requests")
            .iter()
            .any(|c| c == "prompt_cache_hit_tokens"),
        "恢复升级后 llm_requests 应补齐 usage 缓存列"
    );
    drop(db);
}

/// 【断言 6】合并产物(工作库)的 user_version 对齐 SCHEMA_VERSION:
/// 输入是未标记老库时也要对齐(否则合并产物的降级检测形同虚设)
#[test]
fn merge_aligns_schema_version_on_output() {
    // ① 两侧都是老库(未标记):预补 DDL 后必须把工作库标到当前版本
    let baseline = legacy_dir("merge-legacy-baseline");
    let source = legacy_dir("merge-legacy-source");
    // 守卫绑定到测试函数作用域:work 子目录在同一用例内跨多次 open/merge 复用
    let work_root = temp_dir("merge-legacy-work");
    let work = work_root.join("work");
    kedai_server::migration::merge_data_dirs(&baseline, &source, &work)
        .expect("两个同版本老库应能合并");
    assert_eq!(
        user_version(&work.join("kedai.db")),
        SCHEMA_VERSION,
        "合并产物(工作库)应被标记为当前 schema 版本"
    );

    // ② 两侧都是全新库(Db::open 已写过版本):合并后仍为当前版本
    let fresh_a = temp_dir("merge-fresh-a");
    let fresh_b = temp_dir("merge-fresh-b");
    Db::open(&fresh_a.join("kedai.db"), &fresh_a).expect("全新库 A 应能建立");
    Db::open(&fresh_b.join("kedai.db"), &fresh_b).expect("全新库 B 应能建立");
    let work2_root = temp_dir("merge-fresh-work");
    let work2 = work2_root.join("work");
    kedai_server::migration::merge_data_dirs(&fresh_a, &fresh_b, &work2)
        .expect("两个全新库应能合并");
    assert_eq!(
        user_version(&work2.join("kedai.db")),
        SCHEMA_VERSION,
        "全新库合并产物版本号应保持当前版本"
    );
}

/// 【断言 7】静态护栏:migration/ddl.rs 每个 `*_DDL` 里的 CREATE 语句,
/// 必须能在 schema.rs 的 CREATE_TABLES 原文里找到(normalize_sql 后)——
/// 漏同步的后果是合并期 schema 比对把同一张表判成冲突 → 合并直接停止。
#[test]
fn ddl_texts_match_create_tables_after_normalize() {
    let schema_source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/models/db/schema.rs"
    ))
    .unwrap();
    let ddl_source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/migration/ddl.rs"))
            .unwrap();
    let schema_text = normalize_sql(&strip_line_comments(&create_tables_text(&schema_source)));

    let constants = ddl_constants(&ddl_source);
    assert!(
        constants.len() >= 12,
        "ddl.rs 的 DDL 常量抽取失败(应 ≥12 个,实际 {}):护栏已失效",
        constants.len()
    );

    let mut checked = 0usize;
    for (name, text) in &constants {
        for statement in sql_statements(text) {
            let normalized = normalize_sql(&strip_line_comments(&statement));
            if !normalized.starts_with("create ") {
                continue;
            }
            // 例外:llm_requests 在 CREATE_TABLES 里是「补齐 usage 缓存列」后的形态,
            // ddl.rs 保留旧版原文(由 ensure_llm_requests_usage_columns 补列对齐),
            // 两者 normalize 后必然不同——该表的一致性由「老库升级→结构指纹一致」把关。
            if normalized.starts_with("create table if not exists llm_requests") {
                continue;
            }
            assert!(
                schema_text.contains(&normalized),
                "{name} 的语句在 CREATE_TABLES 里找不到(漏同步?):{normalized}"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 20,
        "实际检查到的语句数偏少({checked}):抽取逻辑失效会让本护栏变成空转"
    );
}

// ===== 升级前自动备份(DB-2 / 计划批次)=====
//
// 登记口径:升级链已事务化(失败整体回滚),但此前**没有**任何前置快照,用户要退回旧版
// exe 只能靠自己事前的备份。本组用例锁定四件事:老库升级留档、新库不留、已最新不重复留、
// 备份失败不阻断启动。

/// 列出 `<dir>/backups/` 下的升级前备份文件名(排序);目录不存在返回空
fn backup_files(data_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(data_dir.join("backups")) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// 【备份 1】老库(user_version=0 且有业务表)升级前留档,且快照内容确是**升级前**状态
#[test]
fn legacy_database_gets_preupgrade_backup_with_old_version() {
    let dir = legacy_dir("backup-legacy");
    let path = dir.join("kedai.db");
    // 前置于此:legacy_dir 建的库 user_version 天然为 0(即「未标记的旧库」)。
    // 这正是判定最容易写错的一档——只判 `> 0` 会漏掉它,而它是真实用户的升级主路径。
    assert_eq!(user_version(&path), 0);
    assert!(
        table_exists(&path, "characters"),
        "老库必须带业务表,否则与「全新库」不可区分"
    );

    let _db = Db::open(&path, &dir).expect("老库应能升级");

    let backups = backup_files(&dir);
    assert_eq!(backups.len(), 1, "老库升级应留下恰好一份快照:{backups:?}");
    assert!(
        backups[0].starts_with("kedai-preupgrade-v0-") && backups[0].ends_with(".db"),
        "命名应含升级前版本号与时间戳(时间戳是唯一性所必需):{}",
        backups[0]
    );

    // 关键判别断言:快照里的版本号必须是**升级前**的 0。
    // 若实现把备份放在了升级之后,这里会读到 SCHEMA_VERSION 而非 0。
    let snapshot = dir.join("backups").join(&backups[0]);
    assert_eq!(
        user_version(&snapshot),
        0,
        "快照必须反映升级前状态(版本号 0),否则它在降级场景下无用"
    );
    assert!(
        table_exists(&snapshot, "characters"),
        "快照应含升级前的业务表(确为一份可用备份)"
    );
    // 原库确实已完成升级(证明备份发生在升级之前、且升级照常完成)
    assert_eq!(user_version(&path), SCHEMA_VERSION);
}

/// 【备份 2】全新库不留档(库里没有任何业务表 → 没有可备份的内容)
#[test]
fn fresh_database_creates_no_backup() {
    let dir = temp_dir("backup-fresh");
    let path = dir.join("kedai.db");
    let _db = Db::open(&path, &dir).expect("全新库应能建立");
    assert!(
        backup_files(&dir).is_empty(),
        "全新库不该产生升级前备份(其 user_version 同为 0,靠「有无业务表」区分)"
    );
}

/// 【备份 3】已是当前版本的库再打开不留档(否则每次启动都会多一份)
#[test]
fn already_current_database_creates_no_additional_backup() {
    let dir = legacy_dir("backup-idempotent");
    let path = dir.join("kedai.db");

    let first = Db::open(&path, &dir).expect("首次升级应成功");
    drop(first);
    let after_upgrade = backup_files(&dir);
    assert_eq!(after_upgrade.len(), 1, "首次升级应留一份");

    let second = Db::open(&path, &dir).expect("再次打开应成功");
    drop(second);
    assert_eq!(
        backup_files(&dir),
        after_upgrade,
        "库已是当前版本时不应再留档(否则每次启动都新增一份)"
    );
}

/// 【备份 4,锁「不阻断启动」不变量】备份失败时 Db::open 仍须成功
///
/// 手法:把 `<data_dir>/backups` 占成**同名文件**,使快照的
/// `create_dir_all(parent)` 失败。此时应用必须照常启动——备份只是「多一份保险」,
/// 不能因为它失败反而把「本来能正常升级」的库变成「应用打不开」。
#[test]
fn backup_failure_does_not_block_open() {
    let dir = legacy_dir("backup-failure");
    let path = dir.join("kedai.db");
    std::fs::write(dir.join("backups"), b"not a directory").unwrap();

    let db = Db::open(&path, &dir).expect("备份失败不得阻断启动(硬不变量)");

    // 升级确实照常完成
    assert_eq!(
        user_version(&path),
        SCHEMA_VERSION,
        "备份失败时升级仍应正常完成"
    );
    assert!(table_exists(&path, "script_authorizations"));
    drop(db);
    assert!(
        dir.join("backups").is_file(),
        "占位文件不应被删除(我们没理由动用户的路径)"
    );
}

/// 取出 schema.rs 中 CREATE_TABLES 的原文(`auto-comment` 留在串内的注释由
/// strip_line_comments 负责剥离)
fn create_tables_text(source: &str) -> String {
    const PREFIX: &str = "const CREATE_TABLES: &str = r#\"";
    let start = source
        .find(PREFIX)
        .expect("schema.rs 未找到 CREATE_TABLES 常量")
        + PREFIX.len();
    let end = source[start..]
        .find("\"#;")
        .expect("CREATE_TABLES 原始字符串未闭合");
    source[start..start + end].to_string()
}

/// 抽出 ddl.rs 中所有 `const <名字>_DDL: &str = "…";` / `r#"…"#` 的字符串正文。
/// 只认这两种书写形态(与本文件现状一致),不做通用 Rust 解析——护栏只需在写法改变时
/// 因「抽取数量不足」报错(见调用处的 len 断言)。
fn ddl_constants(source: &str) -> Vec<(String, String)> {
    const MARKER: &str = "_DDL: &str =";
    let mut out = Vec::new();
    let mut offset = 0usize;
    while let Some(found) = source[offset..].find(MARKER) {
        let marker = offset + found;
        let name_start = source[..marker]
            .rfind("const ")
            .map(|index| index + "const ".len())
            .unwrap_or(marker);
        let name = source[name_start..marker].trim().to_string();
        let tail = &source[marker + MARKER.len()..];
        let body = tail.trim_start();
        let (text, rest) = if let Some(raw) = body.strip_prefix("r#\"") {
            let end = raw.find("\"#").expect("_DDL 原始字符串字面量未闭合");
            (raw[..end].to_string(), &raw[end + 2..])
        } else {
            let plain = body
                .strip_prefix('"')
                .expect("_DDL 常量应为字符串字面量(原始串或普通串)");
            let end = plain.find("\";").expect("_DDL 字符串字面量未以 \"; 收尾");
            (plain[..end].to_string(), &plain[end + 2..])
        };
        out.push((name, text));
        offset = source.len() - rest.len();
    }
    out
}

/// 去 SQL 行注释(`--` 至行尾):CREATE_TABLES 的原始串里带 `--` 注释,
/// 而 ddl.rs 的 DDL 文本没有。两侧只有空串字面量 `''`,不含 `--`,按行截断无误伤。
fn strip_line_comments(sql: &str) -> String {
    sql.lines()
        .map(|line| match line.find("--") {
            Some(index) => &line[..index],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 与 migration/merge.rs 的 normalize_sql 同款口径:标点独立成 token + 空白折叠 + 小写。
/// (合并期的 schema 一致性比对就用这个口径,护栏必须同口径才有意义。)
fn normalize_sql(sql: &str) -> String {
    sql.replace(',', " , ")
        .replace('(', " ( ")
        .replace(')', " ) ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// 按 `;` 切分 SQL 语句:CREATE TRIGGER 的 BEGIN…END 体内也含分号,
/// 故以 trigger 开头的语句要一路拼到 `END` 收尾。
fn sql_statements(sql: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in sql.split(';') {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        let open_trigger = out
            .last()
            .map(|last| {
                let lowered = last.trim_end().to_lowercase();
                lowered.starts_with("create trigger") && !lowered.ends_with("end")
            })
            .unwrap_or(false);
        if open_trigger {
            let last = out.last_mut().unwrap();
            last.push_str(" ; ");
            last.push_str(part);
        } else {
            out.push(part.to_string());
        }
    }
    out
}
