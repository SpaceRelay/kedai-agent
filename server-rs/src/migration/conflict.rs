// 冲突处理:主键冲突重映射、外键改写、等价行探测、文件冲突改名,及合并期值/标识符工具。
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Transaction};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use super::merge::{Column, ForeignKey};

pub(super) fn fresh_primary_key(
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

pub(super) fn remap_foreign_keys(
    row: &mut [Value],
    columns: &[Column],
    foreign_keys: &[ForeignKey],
    id_maps: &HashMap<(String, String), HashMap<String, String>>,
) {
    for foreign_key in foreign_keys {
        let Some(index) = columns
            .iter()
            .position(|column| column.name == foreign_key.from)
        else {
            continue;
        };
        let Some(mapping) =
            id_maps.get(&(foreign_key.parent_table.clone(), foreign_key.to.clone()))
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

pub(super) fn find_equivalent_key(
    tx: &Transaction<'_>,
    table: &str,
    columns: &[Column],
    pk_index: usize,
    values: &[Value],
) -> Result<Option<String>, String> {
    let non_key = (0..columns.len())
        .filter(|index| *index != pk_index)
        .collect::<Vec<_>>();
    if non_key.is_empty() {
        return Ok(None);
    }
    let conditions = non_key
        .iter()
        .enumerate()
        .map(|(position, index)| {
            format!(
                "{} IS ?{}",
                quote_ident(&columns[*index].name),
                position + 1
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let params = non_key.iter().map(|index| values[*index].clone());
    let sql = format!(
        "SELECT {} FROM {} WHERE {conditions} LIMIT 1",
        quote_ident(&columns[pk_index].name),
        quote_ident(table)
    );
    let mut stmt = tx
        .prepare(&sql)
        .map_err(|e| format!("检查表 {table} 等价行失败: {e}"))?;
    let mut rows = stmt
        .query(params_from_iter(params))
        .map_err(|e| format!("查询表 {table} 等价行失败: {e}"))?;
    match rows
        .next()
        .map_err(|e| format!("读取表 {table} 等价行失败: {e}"))?
    {
        Some(row) => scalar_text(
            &row.get::<_, Value>(0)
                .map_err(|e| format!("读取表 {table} 等价主键失败: {e}"))?,
        )
        .map(Some),
        None => Ok(None),
    }
}

pub(super) fn select_by_key(
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
        .map(|(position, index)| {
            format!("{} = ?{}", quote_ident(&columns[*index].name), position + 1)
        })
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
    match rows
        .next()
        .map_err(|e| format!("读取表 {table} 主键失败: {e}"))?
    {
        Some(row) => (0..columns.len())
            .map(|index| row.get::<_, Value>(index))
            .collect::<rusqlite::Result<Vec<_>>>()
            .map(Some)
            .map_err(|e| format!("读取表 {table} 既有行失败: {e}")),
        None => Ok(None),
    }
}

pub(super) fn same_file(left: &Path, right: &Path) -> Result<bool, String> {
    let left = fs::read(left).map_err(|e| format!("读取 {} 失败: {e}", left.display()))?;
    let right = fs::read(right).map_err(|e| format!("读取 {} 失败: {e}", right.display()))?;
    Ok(left == right)
}

pub(super) fn collision_path(original: &Path, source: &Path) -> Result<PathBuf, String> {
    let bytes = fs::read(source).map_err(|e| format!("读取冲突文件失败: {e}"))?;
    let hash = stable_hash(&bytes);
    let stem = original
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
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

pub(super) fn scalar_text(value: &Value) -> Result<String, String> {
    match value {
        Value::Integer(value) => Ok(value.to_string()),
        Value::Text(value) => Ok(value.clone()),
        _ => Err("主键类型无法转换为外键值".into()),
    }
}

pub(super) fn value_key(value: &Value) -> String {
    match value {
        Value::Null => "null:".into(),
        Value::Integer(value) => format!("i:{value}"),
        Value::Real(value) => format!("r:{value}"),
        Value::Text(value) => format!("t:{value}"),
        Value::Blob(value) => format!("b:{:016x}", stable_hash(value)),
    }
}

pub(super) fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
