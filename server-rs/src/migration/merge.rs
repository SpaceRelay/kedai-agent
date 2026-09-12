// 双库合并:schema 一致性比对、按外键依赖序逐表合并、非库文件树与 JSON 配置合并、合并后校验。
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, Transaction};
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use super::backup::snapshot_database;
use super::conflict::{
    collision_path, find_equivalent_key, fresh_primary_key, quote_ident, remap_foreign_keys,
    same_file, scalar_text, select_by_key, value_key,
};
use super::ddl::{
    ensure_llm_requests_usage_columns, ensure_memory_entries_pinned_column,
    ensure_skills_progressive_columns, ensure_task_llm_calls_finish_reason_column,
    ensure_task_messages_table, ensure_tasks_task_mode_column, CONTRACT_CHANGELOG_DDL,
    KALEIDO_STATE_DDL, LLM_REQUESTS_DDL, MEMORY_ENTRIES_DDL, MEMORY_ENTRIES_FTS_DDL,
    SCOPE_VARIABLES_DDL, SESSION_COMPACTIONS_DDL, USER_SCRIPTS_DDL,
};
use super::{DATABASE_FILE, SKIPPED_SIDECARS};

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
pub(super) struct Column {
    pub(super) name: String,
    pub(super) pk_position: i64,
}

#[derive(Debug, Clone)]
pub(super) struct ForeignKey {
    pub(super) from: String,
    pub(super) parent_table: String,
    pub(super) to: String,
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
            serde_json::to_vec_pretty(&signatures)
                .map_err(|e| format!("序列化合并来源标记失败: {e}"))?,
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
        let names = columns
            .iter()
            .map(|column| quote_ident(&column.name))
            .collect::<Vec<_>>()
            .join(", ");
        let order = columns
            .iter()
            .filter(|column| column.pk_position > 0)
            .map(|column| quote_ident(&column.name))
            .collect::<Vec<_>>();
        let mut query = format!("SELECT {names} FROM {}", quote_ident(&table));
        if !order.is_empty() {
            query.push_str(&format!(" ORDER BY {}", order.join(", ")));
        }
        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("读取表 {table} 计算签名失败: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                (0..columns.len())
                    .map(|index| row.get::<_, Value>(index))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .map_err(|e| format!("遍历表 {table} 计算签名失败: {e}"))?;
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
    // llm_requests usage 缓存列:旧库 ALTER 补齐,保证两侧 schema 一致(比对在 DDL 补齐之后)
    ensure_llm_requests_usage_columns(&conn)
        .map_err(|e| format!("补齐基线库 llm_requests 缓存列失败: {e}"))?;
    // skills 渐进披露列:旧库 ALTER 补齐,保证两侧 schema 一致(比对在 DDL 补齐之后)
    ensure_skills_progressive_columns(&conn)
        .map_err(|e| format!("补齐基线库 skills 渐进披露列失败: {e}"))?;
    // tasks task_mode 列(批次 4 六模式):旧库 ALTER 补齐,保证两侧 schema 一致
    ensure_tasks_task_mode_column(&conn)
        .map_err(|e| format!("补齐基线库 tasks task_mode 列失败: {e}"))?;
    // task_llm_calls finish_reason 列(可观测性问题①):旧库 ALTER 补齐
    ensure_task_llm_calls_finish_reason_column(&conn)
        .map_err(|e| format!("补齐基线库 task_llm_calls finish_reason 列失败: {e}"))?;
    conn.execute_batch(CONTRACT_CHANGELOG_DDL)
        .map_err(|e| format!("补齐基线库 contract_changelog 表失败: {e}"))?;
    conn.execute_batch(KALEIDO_STATE_DDL)
        .map_err(|e| format!("补齐基线库 kaleido 契约运行态表失败: {e}"))?;
    conn.execute_batch(MEMORY_ENTRIES_DDL)
        .map_err(|e| format!("补齐基线库 memory_entries 表失败: {e}"))?;
    // 记忆 pinned 列(B2 分层注入)+ FTS 索引:旧库补齐,保证两侧 schema 一致
    // (FTS 虚拟表本身不参与比对,但补齐后主表 trigger 才能同步索引)
    ensure_memory_entries_pinned_column(&conn)
        .map_err(|e| format!("补齐基线库 memory_entries pinned 列失败: {e}"))?;
    conn.execute_batch(MEMORY_ENTRIES_FTS_DDL)
        .map_err(|e| format!("补齐基线库 memory_entries FTS 索引失败: {e}"))?;
    // 任务消息表(批次 R2):旧库缺失才建,保证两侧 schema 一致(比对在 DDL 补齐之后)
    ensure_task_messages_table(&conn)
        .map_err(|e| format!("补齐基线库 task_messages 表失败: {e}"))?;
    let source_conn =
        Connection::open(source).map_err(|e| format!("打开源快照补齐 schema 失败: {e}"))?;
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
    ensure_llm_requests_usage_columns(&source_conn)
        .map_err(|e| format!("补齐源快照 llm_requests 缓存列失败: {e}"))?;
    ensure_skills_progressive_columns(&source_conn)
        .map_err(|e| format!("补齐源快照 skills 渐进披露列失败: {e}"))?;
    ensure_tasks_task_mode_column(&source_conn)
        .map_err(|e| format!("补齐源快照 tasks task_mode 列失败: {e}"))?;
    ensure_task_llm_calls_finish_reason_column(&source_conn)
        .map_err(|e| format!("补齐源快照 task_llm_calls finish_reason 列失败: {e}"))?;
    source_conn
        .execute_batch(CONTRACT_CHANGELOG_DDL)
        .map_err(|e| format!("补齐源快照 contract_changelog 表失败: {e}"))?;
    source_conn
        .execute_batch(KALEIDO_STATE_DDL)
        .map_err(|e| format!("补齐源快照 kaleido 契约运行态表失败: {e}"))?;
    source_conn
        .execute_batch(MEMORY_ENTRIES_DDL)
        .map_err(|e| format!("补齐源快照 memory_entries 表失败: {e}"))?;
    ensure_memory_entries_pinned_column(&source_conn)
        .map_err(|e| format!("补齐源快照 memory_entries pinned 列失败: {e}"))?;
    source_conn
        .execute_batch(MEMORY_ENTRIES_FTS_DDL)
        .map_err(|e| format!("补齐源快照 memory_entries FTS 索引失败: {e}"))?;
    ensure_task_messages_table(&source_conn)
        .map_err(|e| format!("补齐源快照 task_messages 表失败: {e}"))?;
    drop(source_conn);
    conn.execute(
        "ATTACH DATABASE ?1 AS src",
        [source.to_string_lossy().as_ref()],
    )
    .map_err(|e| format!("附加源快照失败: {e}"))?;

    let baseline_schema = schema_map(&conn, "main")?;
    let source_schema = schema_map(&conn, "src")?;
    for (table, sql) in &source_schema {
        match baseline_schema.get(table) {
            Some(existing) if normalize_sql(existing) != normalize_sql(sql) => {
                return Err(format!(
                    "表 {table} 的 schema 冲突,已停止合并并保留两份原数据"
                ));
            }
            None => {
                return Err(format!("基线缺少源表 {table},已停止合并并保留两份原数据"));
            }
            _ => {}
        }
    }

    let order = dependency_order(&conn, source_schema.keys().cloned().collect())?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("开始合并事务失败: {e}"))?;
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
        .prepare(&format!(
            "SELECT {quoted_columns} FROM src.{}",
            quote_ident(table)
        ))
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
            if let Some(equivalent_key) = find_equivalent_key(tx, table, &columns, pk_index, &row)?
            {
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

fn schema_map(conn: &Connection, schema: &str) -> Result<BTreeMap<String, String>, String> {
    // FTS5 虚拟表与其影子表不参与合并:旧库无索引表不应报「基线缺少源表」,
    // 且逐行 INSERT 时由主表 trigger 自动同步索引,无需(也不应)按表搬运。
    let skipped = non_mergeable_tables(conn, schema)?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT name, sql FROM {schema}.sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        ))
        .map_err(|e| format!("读取 {schema} schema 失败: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("遍历 {schema} schema 失败: {e}"))?;
    let all = rows
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()
        .map_err(|e| format!("解析 {schema} schema 失败: {e}"))?;
    Ok(all
        .into_iter()
        .filter(|(name, _)| !skipped.contains(name))
        .collect())
}

/// 列出不应参与合并的表名:虚拟表(virtual)与其影子表(shadow)。
/// 依赖 `PRAGMA table_list`(SQLite 3.37+,bundled 版本满足),按 schema 列过滤;
/// 极旧引擎不支持该 pragma 时返回空集(退化为按建表 SQL 前缀跳过)。
fn non_mergeable_tables(conn: &Connection, schema: &str) -> Result<HashSet<String>, String> {
    let mut stmt = match conn.prepare("PRAGMA table_list") {
        Ok(stmt) => stmt,
        Err(_) => return Ok(HashSet::new()),
    };
    // 列序:schema, name, type, ncol, wr, strict
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| format!("遍历表清单失败: {e}"))?;
    let mut out = HashSet::new();
    for row in rows {
        let (row_schema, name, kind) = row.map_err(|e| format!("解析表清单失败: {e}"))?;
        if row_schema != schema {
            continue;
        }
        if kind.eq_ignore_ascii_case("virtual") || kind.eq_ignore_ascii_case("shadow") {
            out.insert(name);
        }
    }
    Ok(out)
}

/// schema 比对前的 SQL 归一:标点(逗号/括号)两侧空白归一 + 空白折叠 + 小写。
/// 标点归一是为 ALTER TABLE ADD COLUMN 补列后的旧库与新版建表的 sql 兼容——
/// SQLite 追加列时存储为「旧列定义 , 新列…新列定义)」,逗号/右括号前的空白
/// 与新建表「旧列定义,\n新列…新列\n)」不同;把标点独立成 token 后两者一致。
/// (schema sql 的字符串字面量仅见空串默认值 '',不含括号,替换无误伤。)
pub(super) fn normalize_sql(sql: &str) -> String {
    sql.replace(',', " , ")
        .replace('(', " ( ")
        .replace(')', " ) ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
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

pub(super) fn table_columns(
    conn: &Connection,
    schema: &str,
    table: &str,
) -> Result<Vec<Column>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "PRAGMA {schema}.table_info({})",
            quote_ident(table)
        ))
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
        .prepare(&format!(
            "PRAGMA {schema}.foreign_key_list({})",
            quote_ident(table)
        ))
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
        // rusqlite 0.34 起移除了 usize 的 FromSql 实现;COUNT(*) 恒为 INTEGER,读 i64 后转换
        |row| row.get::<_, i64>(0).map(|n| n as usize),
    )
    .map_err(|e| format!("统计表 {table} 失败: {e}"))
}

fn copy_non_database_tree(
    source: &Path,
    target: &Path,
    merge: bool,
    report: &mut MergeReport,
) -> Result<(), String> {
    for entry in
        fs::read_dir(source).map_err(|e| format!("读取目录 {} 失败: {e}", source.display()))?
    {
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
    for name in [
        "settings.json",
        "prompt_floors.json",
        "agent_flows.json",
        "tool_permissions.json",
    ] {
        let baseline_file = baseline.join(name);
        let source_file = source.join(name);
        if !source_file.is_file() {
            continue;
        }
        if !baseline_file.is_file() {
            fs::copy(&source_file, work.join(name))
                .map_err(|e| format!("复制配置 {name} 失败: {e}"))?;
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
            serde_json::to_vec_pretty(&merged)
                .map_err(|e| format!("序列化配置 {name} 失败: {e}"))?,
        )
        .map_err(|e| format!("写入配置 {name} 失败: {e}"))?;
    }
    Ok(())
}

fn merge_json_values(
    baseline: JsonValue,
    source: JsonValue,
    report: &mut MergeReport,
) -> JsonValue {
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

fn rewrite_known_paths(
    db: &Path,
    baseline: &Path,
    source: &Path,
    work: &Path,
) -> Result<(), String> {
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
        return Err(format!(
            "合并数据库存在 {} 个外键错误",
            report.foreign_key_errors
        ));
    }
    Ok(())
}

fn is_structured_config(name: &str) -> bool {
    matches!(
        name,
        "settings.json" | "prompt_floors.json" | "agent_flows.json" | "tool_permissions.json"
    )
}
