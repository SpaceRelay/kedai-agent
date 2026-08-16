use rusqlite::backup::Backup;
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, OpenFlags, Transaction};
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const DATABASE_FILE: &str = "kedai.db";
const SKIPPED_SIDECARS: [&str; 2] = ["kedai.db-wal", "kedai.db-shm"];
/// 计划二新增的 7 作用域变量表。合并前对两侧各补一次 DDL,避免旧版本数据库
/// (无此表)与新版本数据库合并时因「基线缺少源表」停止(§4.3)。
const SCOPE_VARIABLES_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS scope_variables (
  scope      TEXT NOT NULL,
  scope_id   TEXT NOT NULL DEFAULT '',
  data_raw   TEXT NOT NULL DEFAULT '{}',
  updated_at TEXT NOT NULL,
  PRIMARY KEY (scope, scope_id)
)"#;
/// 阶段三新增的用户脚本表(ScriptTree 全局脚本)。合并前对两侧各补一次 DDL,
/// 与 SCOPE_VARIABLES_DDL 同理,避免旧库与新库合并时因「基线缺少源表」停止(§4.3)。
const USER_SCRIPTS_DDL: &str = r#"
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
const SESSION_COMPACTIONS_DDL: &str = r#"
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
const LLM_REQUESTS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS llm_requests (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  run_id      TEXT NOT NULL,
  seq         INTEGER NOT NULL,
  payload     TEXT NOT NULL,
  model       TEXT NOT NULL DEFAULT '',
  created_at  TEXT NOT NULL
)"#;
/// 契约变更历史表(阶段 C):append-only 审计 + 回滚源。合并前对两侧各补一次 DDL,
/// 与 SCOPE_VARIABLES_DDL 同理,避免旧库与新库合并时因「基线缺少源表」停止(§4.3)。
/// 索引不参与 schema 一致性比对(schema_map 仅读 type='table'),无需在此重复。
/// 注意:seq 仅库内局部有效——跨库合并(merge_databases)时主键冲突会被重映射,
/// rationale 中「回滚自 seq N」仅作提示,合并后时间线以 created_at 为准。
const CONTRACT_CHANGELOG_DDL: &str = r#"
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
const KALEIDO_STATE_DDL: &str = r#"
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

#[derive(Debug, Default, Clone, Serialize)]
pub struct MergeReport {
    pub tables: BTreeMap<String, TableReport>,
    pub copied_files: usize,
    pub deduplicated_files: usize,
    pub renamed_files: usize,
    pub settings_conflicts_kept_baseline: usize,
    pub integrity_check: String,
    pub foreign_key_errors: usize,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct TableReport {
    pub baseline: usize,
    pub source: usize,
    pub inserted: usize,
    pub deduplicated: usize,
    pub remapped: usize,
}

#[derive(Debug, Clone)]
struct Column {
    name: String,
    pk_position: i64,
}

#[derive(Debug, Clone)]
struct ForeignKey {
    from: String,
    parent_table: String,
    to: String,
}

pub fn snapshot_database(source: &Path, destination: &Path) -> Result<(), String> {
    if !source.is_file() {
        return Err(format!("数据库不存在: {}", source.display()));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建快照目录失败: {e}"))?;
    }
    if destination.exists() {
        fs::remove_file(destination).map_err(|e| format!("移除旧快照失败: {e}"))?;
    }
    let source = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打开源数据库失败: {e}"))?;
    source
        .busy_timeout(std::time::Duration::from_secs(30))
        .map_err(|e| format!("设置快照超时失败: {e}"))?;
    let mut destination = Connection::open(destination)
        .map_err(|e| format!("创建数据库快照失败: {e}"))?;
    let backup = Backup::new(&source, &mut destination)
        .map_err(|e| format!("初始化 SQLite backup 失败: {e}"))?;
    backup
        .run_to_completion(128, std::time::Duration::from_millis(10), None)
        .map_err(|e| format!("生成 SQLite 快照失败: {e}"))?;
    Ok(())
}

pub fn merge_data_dirs(
    baseline_dir: &Path,
    source_dir: &Path,
    work_dir: &Path,
) -> Result<MergeReport, String> {
    if work_dir.exists() {
        fs::remove_dir_all(work_dir).map_err(|e| format!("清理工作目录失败: {e}"))?;
    }
    fs::create_dir_all(work_dir).map_err(|e| format!("创建工作目录失败: {e}"))?;

    let mut initial_report = MergeReport::default();
    copy_non_database_tree(baseline_dir, work_dir, false, &mut initial_report)?;
    let work_db = work_dir.join(DATABASE_FILE);
    snapshot_database(&baseline_dir.join(DATABASE_FILE), &work_db)?;
    let source_snapshot = work_dir.join(".source-snapshot.db");
    snapshot_database(&source_dir.join(DATABASE_FILE), &source_snapshot)?;
    let source_signature = logical_database_signature(&source_snapshot)?;
    rewrite_known_paths(&work_db, baseline_dir, baseline_dir, work_dir)?;
    rewrite_known_paths(&source_snapshot, source_dir, source_dir, work_dir)?;

    let marker = work_dir.join(".kedai-merged-sources.json");
    let mut signatures = read_merge_signatures(&marker)?;
    let mut report = if signatures.contains(&source_signature) {
        report_existing_counts(&work_db)?
    } else {
        let report = merge_databases(&work_db, &source_snapshot)?;
        signatures.insert(source_signature);
        fs::write(
            &marker,
            serde_json::to_vec_pretty(&signatures).map_err(|e| format!("序列化合并来源标记失败: {e}"))?,
        )
        .map_err(|e| format!("写入合并来源标记失败: {e}"))?;
        report
    };
    fs::remove_file(&source_snapshot).map_err(|e| format!("移除临时源快照失败: {e}"))?;
    report.copied_files += initial_report.copied_files;
    copy_non_database_tree(source_dir, work_dir, true, &mut report)?;
    merge_json_configs(baseline_dir, source_dir, work_dir, &mut report)?;
    rewrite_known_paths(&work_db, baseline_dir, source_dir, work_dir)?;
    validate_database(&work_db, &mut report)?;
    Ok(report)
}

fn logical_database_signature(db: &Path) -> Result<String, String> {
    let conn = Connection::open(db).map_err(|e| format!("打开源快照计算签名失败: {e}"))?;
    let schema = schema_map(&conn, "main")?;
    let mut hash = 0xcbf29ce484222325u64;
    for (table, sql) in schema {
        hash_bytes(&mut hash, table.as_bytes());
        hash_bytes(&mut hash, sql.as_bytes());
        let columns = table_columns(&conn, "main", &table)?;
        let names = columns.iter().map(|column| quote_ident(&column.name)).collect::<Vec<_>>().join(", ");
        let order = columns.iter().filter(|column| column.pk_position > 0).map(|column| quote_ident(&column.name)).collect::<Vec<_>>();
        let mut query = format!("SELECT {names} FROM {}", quote_ident(&table));
        if !order.is_empty() {
            query.push_str(&format!(" ORDER BY {}", order.join(", ")));
        }
        let mut stmt = conn.prepare(&query).map_err(|e| format!("读取表 {table} 计算签名失败: {e}"))?;
        let rows = stmt.query_map([], |row| {
            (0..columns.len()).map(|index| row.get::<_, Value>(index)).collect::<rusqlite::Result<Vec<_>>>()
        }).map_err(|e| format!("遍历表 {table} 计算签名失败: {e}"))?;
        for row in rows {
            for value in row.map_err(|e| format!("读取表 {table} 签名行失败: {e}"))? {
                hash_bytes(&mut hash, value_key(&value).as_bytes());
            }
        }
    }
    Ok(format!("{hash:016x}"))
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

fn read_merge_signatures(path: &Path) -> Result<HashSet<String>, String> {
    if !path.is_file() {
        return Ok(HashSet::new());
    }
    serde_json::from_slice(&fs::read(path).map_err(|e| format!("读取合并来源标记失败: {e}"))?)
        .map_err(|e| format!("解析合并来源标记失败: {e}"))
}

fn report_existing_counts(db: &Path) -> Result<MergeReport, String> {
    let conn = Connection::open(db).map_err(|e| format!("打开幂等校验数据库失败: {e}"))?;
    let schema = schema_map(&conn, "main")?;
    let mut report = MergeReport::default();
    for table in schema.keys() {
        report.tables.insert(
            table.clone(),
            TableReport {
                baseline: count_rows(&conn, "main", table)?,
                ..TableReport::default()
            },
        );
    }
    Ok(report)
}

fn merge_databases(baseline: &Path, source: &Path) -> Result<MergeReport, String> {
    let mut conn = Connection::open(baseline).map_err(|e| format!("打开工作数据库失败: {e}"))?;
    conn.pragma_update(None, "foreign_keys", "OFF")
        .map_err(|e| format!("关闭合并期外键检查失败: {e}"))?;
    // 合并前对齐 schema:基线库与源快照各补一次 scope_variables DDL(同一文本,
    // 均不带 schema 前缀,保证两侧 sqlite_schema 存储形式一致),避免新旧版本
    // 数据库合并不因新增表报「基线缺少源表/表 schema 冲突」(§4.3)。
    conn.execute_batch(SCOPE_VARIABLES_DDL)
        .map_err(|e| format!("补齐基线库 scope_variables 表失败: {e}"))?;
    conn.execute_batch(USER_SCRIPTS_DDL)
        .map_err(|e| format!("补齐基线库 user_scripts 表失败: {e}"))?;
    conn.execute_batch(SESSION_COMPACTIONS_DDL)
        .map_err(|e| format!("补齐基线库 session_compactions 表失败: {e}"))?;
    conn.execute_batch(LLM_REQUESTS_DDL)
        .map_err(|e| format!("补齐基线库 llm_requests 表失败: {e}"))?;
    conn.execute_batch(CONTRACT_CHANGELOG_DDL)
        .map_err(|e| format!("补齐基线库 contract_changelog 表失败: {e}"))?;
    conn.execute_batch(KALEIDO_STATE_DDL)
        .map_err(|e| format!("补齐基线库 kaleido 契约运行态表失败: {e}"))?;
    let source_conn = Connection::open(source)
        .map_err(|e| format!("打开源快照补齐 schema 失败: {e}"))?;
    source_conn
        .execute_batch(SCOPE_VARIABLES_DDL)
        .map_err(|e| format!("补齐源快照 scope_variables 表失败: {e}"))?;
    source_conn
        .execute_batch(USER_SCRIPTS_DDL)
        .map_err(|e| format!("补齐源快照 user_scripts 表失败: {e}"))?;
    source_conn
        .execute_batch(SESSION_COMPACTIONS_DDL)
        .map_err(|e| format!("补齐源快照 session_compactions 表失败: {e}"))?;
    source_conn
        .execute_batch(LLM_REQUESTS_DDL)
        .map_err(|e| format!("补齐源快照 llm_requests 表失败: {e}"))?;
    source_conn
        .execute_batch(CONTRACT_CHANGELOG_DDL)
        .map_err(|e| format!("补齐源快照 contract_changelog 表失败: {e}"))?;
    source_conn
        .execute_batch(KALEIDO_STATE_DDL)
        .map_err(|e| format!("补齐源快照 kaleido 契约运行态表失败: {e}"))?;
    drop(source_conn);
    conn.execute("ATTACH DATABASE ?1 AS src", [source.to_string_lossy().as_ref()])
        .map_err(|e| format!("附加源快照失败: {e}"))?;

    let baseline_schema = schema_map(&conn, "main")?;
    let source_schema = schema_map(&conn, "src")?;
    for (table, sql) in &source_schema {
        match baseline_schema.get(table) {
            Some(existing) if normalize_sql(existing) != normalize_sql(sql) => {
                return Err(format!("表 {table} 的 schema 冲突,已停止合并并保留两份原数据"));
            }
            None => {
                return Err(format!("基线缺少源表 {table},已停止合并并保留两份原数据"));
            }
            _ => {}
        }
    }

    let order = dependency_order(&conn, source_schema.keys().cloned().collect())?;
    let tx = conn.transaction().map_err(|e| format!("开始合并事务失败: {e}"))?;
    let mut report = MergeReport::default();
    let mut id_maps: HashMap<(String, String), HashMap<String, String>> = HashMap::new();
    for table in order {
        merge_table(&tx, &table, &mut id_maps, &mut report)?;
    }
    tx.commit().map_err(|e| format!("提交合并事务失败: {e}"))?;
    conn.execute_batch("DETACH DATABASE src; PRAGMA foreign_keys=ON;")
        .map_err(|e| format!("结束数据库合并失败: {e}"))?;
    Ok(report)
}

fn merge_table(
    tx: &Transaction<'_>,
    table: &str,
    id_maps: &mut HashMap<(String, String), HashMap<String, String>>,
    report: &mut MergeReport,
) -> Result<(), String> {
    let columns = table_columns(tx, "main", table)?;
    if columns.is_empty() {
        return Ok(());
    }
    let foreign_keys = foreign_keys(tx, "main", table)?;
    let pk_indices: Vec<usize> = columns
        .iter()
        .enumerate()
        .filter_map(|(index, column)| (column.pk_position > 0).then_some(index))
        .collect();
    let quoted_columns = columns
        .iter()
        .map(|column| quote_ident(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let mut source_stmt = tx
        .prepare(&format!("SELECT {quoted_columns} FROM src.{}", quote_ident(table)))
        .map_err(|e| format!("读取源表 {table} 失败: {e}"))?;
    let source_rows = source_stmt
        .query_map([], |row| {
            (0..columns.len())
                .map(|index| row.get::<_, Value>(index))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|e| format!("遍历源表 {table} 失败: {e}"))?;

    let stats = report.tables.entry(table.to_string()).or_default();
    stats.baseline = count_rows(tx, "main", table)?;
    stats.source = count_rows(tx, "src", table)?;
    for source_row in source_rows {
        let original = source_row.map_err(|e| format!("读取源表 {table} 行失败: {e}"))?;
        let mut row = original.clone();
        remap_foreign_keys(&mut row, &columns, &foreign_keys, id_maps);

        let existing = if pk_indices.is_empty() {
            None
        } else {
            select_by_key(tx, table, &columns, &pk_indices, &row)?
        };
        if let Some(existing) = existing {
            if existing == row {
                stats.deduplicated += 1;
                continue;
            }
            if pk_indices.len() != 1 {
                if table == "scope_variables" {
                    // 作用域变量按 (scope, scope_id) 复合主键冲突:保守保留基线并跳过,
                    // 与 global_usage 同策略,保证合并幂等且不停止(§4.3)。
                    stats.deduplicated += 1;
                    continue;
                }
                return Err(format!("表 {table} 的复合主键发生内容冲突,已保守停止"));
            }
            let pk_index = pk_indices[0];
            let old_key = value_key(&original[pk_index]);
            if table == "global_usage" {
                // 全局统计无法判断两侧是否有重叠来源,保守保留基线可保证幂等。
                stats.deduplicated += 1;
                continue;
            }
            if let Some(equivalent_key) = find_equivalent_key(tx, table, &columns, pk_index, &row)? {
                id_maps
                    .entry((table.to_string(), columns[pk_index].name.clone()))
                    .or_default()
                    .insert(old_key, equivalent_key);
                stats.deduplicated += 1;
                continue;
            }
            row[pk_index] = fresh_primary_key(tx, table, &columns[pk_index], &row[pk_index])?;
            let new_key = scalar_text(&row[pk_index])?;
            id_maps
                .entry((table.to_string(), columns[pk_index].name.clone()))
                .or_default()
                .insert(old_key, new_key);
            stats.remapped += 1;
        }

        let placeholders = (1..=columns.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        tx.execute(
            &format!(
                "INSERT INTO {} ({quoted_columns}) VALUES ({placeholders})",
                quote_ident(table)
            ),
            params_from_iter(row),
        )
        .map_err(|e| format!("插入表 {table} 失败: {e}"))?;
        stats.inserted += 1;
    }
    Ok(())
}

fn fresh_primary_key(
    tx: &Transaction<'_>,
    table: &str,
    column: &Column,
    old: &Value,
) -> Result<Value, String> {
    match old {
        Value::Integer(_) => {
            let sql = format!(
                "SELECT COALESCE(MAX({}), 0) + 1 FROM {}",
                quote_ident(&column.name),
                quote_ident(table)
            );
            tx.query_row(&sql, [], |row| row.get(0))
                .map(Value::Integer)
                .map_err(|e| format!("为表 {table} 生成新自增主键失败: {e}"))
        }
        Value::Text(_) => Ok(Value::Text(Uuid::new_v4().to_string())),
        _ => Err(format!("表 {table} 使用不支持重映射的主键类型")),
    }
}

fn remap_foreign_keys(
    row: &mut [Value],
    columns: &[Column],
    foreign_keys: &[ForeignKey],
    id_maps: &HashMap<(String, String), HashMap<String, String>>,
) {
    for foreign_key in foreign_keys {
        let Some(index) = columns.iter().position(|column| column.name == foreign_key.from) else {
            continue;
        };
        let Some(mapping) = id_maps.get(&(foreign_key.parent_table.clone(), foreign_key.to.clone()))
        else {
            continue;
        };
        let key = value_key(&row[index]);
        if let Some(mapped) = mapping.get(&key) {
            row[index] = parse_mapped_value(&row[index], mapped);
        }
    }
}

fn parse_mapped_value(original: &Value, mapped: &str) -> Value {
    match original {
        Value::Integer(_) => mapped
            .parse::<i64>()
            .map(Value::Integer)
            .unwrap_or_else(|_| original.clone()),
        Value::Text(_) => Value::Text(mapped.to_string()),
        _ => original.clone(),
    }
}

fn find_equivalent_key(
    tx: &Transaction<'_>,
    table: &str,
    columns: &[Column],
    pk_index: usize,
    values: &[Value],
) -> Result<Option<String>, String> {
    let non_key = (0..columns.len()).filter(|index| *index != pk_index).collect::<Vec<_>>();
    if non_key.is_empty() {
        return Ok(None);
    }
    let conditions = non_key
        .iter()
        .enumerate()
        .map(|(position, index)| format!("{} IS ?{}", quote_ident(&columns[*index].name), position + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let params = non_key.iter().map(|index| values[*index].clone());
    let sql = format!(
        "SELECT {} FROM {} WHERE {conditions} LIMIT 1",
        quote_ident(&columns[pk_index].name),
        quote_ident(table)
    );
    let mut stmt = tx.prepare(&sql).map_err(|e| format!("检查表 {table} 等价行失败: {e}"))?;
    let mut rows = stmt.query(params_from_iter(params)).map_err(|e| format!("查询表 {table} 等价行失败: {e}"))?;
    match rows.next().map_err(|e| format!("读取表 {table} 等价行失败: {e}"))? {
        Some(row) => scalar_text(&row.get::<_, Value>(0).map_err(|e| format!("读取表 {table} 等价主键失败: {e}"))?).map(Some),
        None => Ok(None),
    }
}

fn select_by_key(
    tx: &Transaction<'_>,
    table: &str,
    columns: &[Column],
    pk_indices: &[usize],
    values: &[Value],
) -> Result<Option<Vec<Value>>, String> {
    let selected = columns
        .iter()
        .map(|column| quote_ident(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let conditions = pk_indices
        .iter()
        .enumerate()
        .map(|(position, index)| format!("{} = ?{}", quote_ident(&columns[*index].name), position + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let params = pk_indices.iter().map(|index| values[*index].clone());
    let mut stmt = tx
        .prepare(&format!(
            "SELECT {selected} FROM {} WHERE {conditions}",
            quote_ident(table)
        ))
        .map_err(|e| format!("检查表 {table} 主键失败: {e}"))?;
    let mut rows = stmt
        .query(params_from_iter(params))
        .map_err(|e| format!("查询表 {table} 主键失败: {e}"))?;
    match rows.next().map_err(|e| format!("读取表 {table} 主键失败: {e}"))? {
        Some(row) => (0..columns.len())
            .map(|index| row.get::<_, Value>(index))
            .collect::<rusqlite::Result<Vec<_>>>()
            .map(Some)
            .map_err(|e| format!("读取表 {table} 既有行失败: {e}")),
        None => Ok(None),
    }
}

fn schema_map(conn: &Connection, schema: &str) -> Result<BTreeMap<String, String>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT name, sql FROM {schema}.sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        ))
        .map_err(|e| format!("读取 {schema} schema 失败: {e}"))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| format!("遍历 {schema} schema 失败: {e}"))?;
    rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()
        .map_err(|e| format!("解析 {schema} schema 失败: {e}"))
}

fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn dependency_order(conn: &Connection, tables: Vec<String>) -> Result<Vec<String>, String> {
    let table_set: HashSet<_> = tables.iter().cloned().collect();
    let mut remaining: HashSet<_> = table_set.clone();
    let mut result = Vec::new();
    while !remaining.is_empty() {
        let mut ready = remaining
            .iter()
            .filter(|table| {
                foreign_keys(conn, "main", table)
                    .map(|keys| {
                        keys.iter().all(|key| {
                            !table_set.contains(&key.parent_table)
                                || key.parent_table == **table
                                || result.contains(&key.parent_table)
                        })
                    })
                    .unwrap_or(false)
            })
            .cloned()
            .collect::<Vec<_>>();
        ready.sort();
        if ready.is_empty() {
            let mut rest = remaining.iter().cloned().collect::<Vec<_>>();
            rest.sort();
            result.extend(rest);
            break;
        }
        for table in ready {
            remaining.remove(&table);
            result.push(table);
        }
    }
    Ok(result)
}

fn table_columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<Column>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA {schema}.table_info({})", quote_ident(table)))
        .map_err(|e| format!("读取表 {table} 列失败: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Column {
                name: row.get(1)?,
                pk_position: row.get(5)?,
            })
        })
        .map_err(|e| format!("遍历表 {table} 列失败: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("解析表 {table} 列失败: {e}"))
}

fn foreign_keys(conn: &Connection, schema: &str, table: &str) -> Result<Vec<ForeignKey>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA {schema}.foreign_key_list({})", quote_ident(table)))
        .map_err(|e| format!("读取表 {table} 外键失败: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ForeignKey {
                parent_table: row.get(2)?,
                from: row.get(3)?,
                to: row.get(4)?,
            })
        })
        .map_err(|e| format!("遍历表 {table} 外键失败: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("解析表 {table} 外键失败: {e}"))
}

fn count_rows(conn: &Connection, schema: &str, table: &str) -> Result<usize, String> {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {schema}.{}", quote_ident(table)),
        [],
        |row| row.get(0),
    )
    .map_err(|e| format!("统计表 {table} 失败: {e}"))
}

fn copy_non_database_tree(
    source: &Path,
    target: &Path,
    merge: bool,
    report: &mut MergeReport,
) -> Result<(), String> {
    for entry in fs::read_dir(source).map_err(|e| format!("读取目录 {} 失败: {e}", source.display()))? {
        let entry = entry.map_err(|e| format!("读取目录项失败: {e}"))?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if name_text == DATABASE_FILE || SKIPPED_SIDECARS.contains(&name_text.as_ref()) {
            continue;
        }
        let from = entry.path();
        let mut to = target.join(&name);
        if from.is_dir() {
            fs::create_dir_all(&to).map_err(|e| format!("创建目录 {} 失败: {e}", to.display()))?;
            copy_non_database_tree(&from, &to, merge, report)?;
            continue;
        }
        if to.exists() {
            if same_file(&from, &to)? {
                if merge {
                    report.deduplicated_files += 1;
                }
                continue;
            }
            if merge && is_structured_config(&name_text) {
                continue;
            }
            if merge {
                to = collision_path(&to, &from)?;
                report.renamed_files += 1;
            }
        }
        fs::copy(&from, &to).map_err(|e| format!("复制 {} 失败: {e}", from.display()))?;
        report.copied_files += 1;
    }
    Ok(())
}

fn merge_json_configs(
    baseline: &Path,
    source: &Path,
    work: &Path,
    report: &mut MergeReport,
) -> Result<(), String> {
    for name in ["settings.json", "prompt_floors.json", "agent_flows.json", "tool_permissions.json"] {
        let baseline_file = baseline.join(name);
        let source_file = source.join(name);
        if !source_file.is_file() {
            continue;
        }
        if !baseline_file.is_file() {
            fs::copy(&source_file, work.join(name)).map_err(|e| format!("复制配置 {name} 失败: {e}"))?;
            continue;
        }
        let baseline_value: JsonValue = serde_json::from_slice(
            &fs::read(&baseline_file).map_err(|e| format!("读取基线配置 {name} 失败: {e}"))?,
        )
        .map_err(|e| format!("解析基线配置 {name} 失败: {e}"))?;
        let source_value: JsonValue = serde_json::from_slice(
            &fs::read(&source_file).map_err(|e| format!("读取源配置 {name} 失败: {e}"))?,
        )
        .map_err(|e| format!("解析源配置 {name} 失败: {e}"))?;
        let merged = merge_json_values(baseline_value, source_value, report);
        fs::write(
            work.join(name),
            serde_json::to_vec_pretty(&merged).map_err(|e| format!("序列化配置 {name} 失败: {e}"))?,
        )
        .map_err(|e| format!("写入配置 {name} 失败: {e}"))?;
    }
    Ok(())
}

fn merge_json_values(baseline: JsonValue, source: JsonValue, report: &mut MergeReport) -> JsonValue {
    match (baseline, source) {
        (JsonValue::Object(mut baseline), JsonValue::Object(source)) => {
            for (key, value) in source {
                match baseline.remove(&key) {
                    Some(existing) => {
                        baseline.insert(key, merge_json_values(existing, value, report));
                    }
                    None => {
                        baseline.insert(key, value);
                    }
                }
            }
            JsonValue::Object(baseline)
        }
        (JsonValue::Array(mut baseline), JsonValue::Array(source)) => {
            for value in source {
                if !baseline.contains(&value) {
                    baseline.push(value);
                }
            }
            JsonValue::Array(baseline)
        }
        (baseline, source) if baseline == source => baseline,
        (baseline, _) => {
            report.settings_conflicts_kept_baseline += 1;
            baseline
        }
    }
}

fn rewrite_known_paths(db: &Path, baseline: &Path, source: &Path, work: &Path) -> Result<(), String> {
    let conn = Connection::open(db).map_err(|e| format!("打开工作数据库修正路径失败: {e}"))?;
    let columns = table_columns(&conn, "main", "characters")?;
    if !columns.iter().any(|column| column.name == "file_path")
        || !columns.iter().any(|column| column.name == "avatar_path")
    {
        return Ok(());
    }
    for old in [baseline, source] {
        let old = old.to_string_lossy();
        let new = work.to_string_lossy();
        conn.execute(
            "UPDATE characters SET file_path=replace(file_path, ?1, ?2), avatar_path=replace(avatar_path, ?1, ?2) WHERE file_path LIKE ?3 OR avatar_path LIKE ?3",
            rusqlite::params![old.as_ref(), new.as_ref(), format!("{}%", old)],
        )
        .map_err(|e| format!("修正数据库文件路径失败: {e}"))?;
    }
    Ok(())
}

fn validate_database(db: &Path, report: &mut MergeReport) -> Result<(), String> {
    let conn = Connection::open(db).map_err(|e| format!("打开合并数据库校验失败: {e}"))?;
    report.integrity_check = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| format!("integrity_check 失败: {e}"))?;
    if report.integrity_check != "ok" {
        return Err("合并数据库 integrity_check 未通过".into());
    }
    let mut stmt = conn
        .prepare("PRAGMA foreign_key_check")
        .map_err(|e| format!("foreign_key_check 失败: {e}"))?;
    report.foreign_key_errors = stmt
        .query_map([], |_| Ok(()))
        .map_err(|e| format!("读取 foreign_key_check 失败: {e}"))?
        .count();
    if report.foreign_key_errors != 0 {
        return Err(format!("合并数据库存在 {} 个外键错误", report.foreign_key_errors));
    }
    Ok(())
}

fn same_file(left: &Path, right: &Path) -> Result<bool, String> {
    let left = fs::read(left).map_err(|e| format!("读取 {} 失败: {e}", left.display()))?;
    let right = fs::read(right).map_err(|e| format!("读取 {} 失败: {e}", right.display()))?;
    Ok(left == right)
}

fn collision_path(original: &Path, source: &Path) -> Result<PathBuf, String> {
    let bytes = fs::read(source).map_err(|e| format!("读取冲突文件失败: {e}"))?;
    let hash = stable_hash(&bytes);
    let stem = original.file_stem().and_then(|value| value.to_str()).unwrap_or("file");
    let extension = original.extension().and_then(|value| value.to_str());
    let name = match extension {
        Some(extension) => format!("{stem}.merged-{hash:016x}.{extension}"),
        None => format!("{stem}.merged-{hash:016x}"),
    };
    Ok(original.with_file_name(name))
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn is_structured_config(name: &str) -> bool {
    matches!(name, "settings.json" | "prompt_floors.json" | "agent_flows.json" | "tool_permissions.json")
}

fn scalar_text(value: &Value) -> Result<String, String> {
    match value {
        Value::Integer(value) => Ok(value.to_string()),
        Value::Text(value) => Ok(value.clone()),
        _ => Err("主键类型无法转换为外键值".into()),
    }
}

fn value_key(value: &Value) -> String {
    match value {
        Value::Null => "null:".into(),
        Value::Integer(value) => format!("i:{value}"),
        Value::Real(value) => format!("r:{value}"),
        Value::Text(value) => format!("t:{value}"),
        Value::Blob(value) => format!("b:{:016x}", stable_hash(value)),
    }
}

fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("kedai-migration-{tag}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
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
        conn.execute("INSERT INTO characters VALUES('same', ?1)", [marker]).unwrap();
        conn.execute("INSERT INTO sessions VALUES('session', 'same')", []).unwrap();
        conn.execute("INSERT INTO messages(id, session_id, content) VALUES(1, 'session', ?1)", [marker]).unwrap();
    }

    #[test]
    fn snapshot_includes_uncheckpointed_wal() {
        let dir = temp_dir("snapshot");
        let db = dir.join(DATABASE_FILE);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(id INTEGER PRIMARY KEY, value TEXT);").unwrap();
        conn.execute("INSERT INTO t(value) VALUES('wal-data')", []).unwrap();
        let snapshot = dir.join("snapshot.db");
        snapshot_database(&db, &snapshot).unwrap();
        let snapshot_conn = Connection::open(snapshot).unwrap();
        assert_eq!(snapshot_conn.query_row("SELECT COUNT(*) FROM t", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
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
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM characters", [], |row| row.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get::<_, i64>(0)).unwrap(), 2);
        drop(conn);

        merge_data_dirs(&first, &source, &second).unwrap();
        let conn = Connection::open(second.join(DATABASE_FILE)).unwrap();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM characters", [], |row| row.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get::<_, i64>(0)).unwrap(), 2);

        fs::remove_dir_all(baseline).ok();
        fs::remove_dir_all(source).ok();
    }
}
