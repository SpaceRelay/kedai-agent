// 契约变更历史服务(阶段 C):append-only changelog + 回滚源。
//
// 只复用 contracts::changelog::ChangelogSource 的来源语义(§7-ChangelogEntry.source),
// 不复用 ChangelogEntry 结构:本表按「契约整体替换/移除」的粒度落账,与领域 changelog
// 的 op 补丁粒度无关。seq 为表内自增主键;同一角色按 seq DESC 读取即最新在前。
use crate::contracts::changelog::ChangelogSource;
use crate::models::db::{now_iso, Db};
use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

/// 单条契约变更记录(内存表示;before/after 为解析后的 JSON)。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractChangeRecord {
    pub seq: i64,
    /// 序列化跳过:历史条目不回传 characterId(按角色隔离查询)
    #[serde(skip)]
    pub character_id: String,
    /// snake_case 序列化(agent / manual / contract_init / rollback / import / repair)
    pub source: ChangelogSource,
    /// 操作类型:replace(整体替换)| remove(移除)
    #[serde(rename = "op")]
    pub op_kind: String,
    pub path: String,
    /// 变更前契约(null 或 JSON 对象)
    pub before: Option<Value>,
    /// 变更后契约(null 或 JSON 对象)
    pub after: Option<Value>,
    pub rationale: Option<String>,
    pub created_at: String,
}

pub struct ContractChangelogService {
    db: Arc<Db>,
}

impl ContractChangelogService {
    pub fn new(db: Arc<Db>) -> Self {
        ContractChangelogService { db }
    }

    /// 追加一条变更记录,返回自增 seq。
    pub fn append(
        &self,
        character_id: &str,
        source: ChangelogSource,
        op_kind: &str,
        before: Option<&Value>,
        after: Option<&Value>,
        rationale: Option<&str>,
    ) -> Result<i64, String> {
        let source = source_to_str(source);
        let before_json = before.map(Value::to_string);
        let after_json = after.map(Value::to_string);
        let conn = self.db.write();
        conn.execute(
            "INSERT INTO contract_changelog (character_id, source, op_kind, path, before_json, after_json, rationale, created_at) \
             VALUES (?1, ?2, ?3, 'contract', ?4, ?5, ?6, ?7)",
            rusqlite::params![character_id, source, op_kind, before_json, after_json, rationale, now_iso()],
        )
        .map_err(|e| format!("写入契约变更记录失败: {e}"))?;
        Ok(conn.last_insert_rowid())
    }

    /// 该角色全部变更记录,最新在前;limit 夹取到 [1, 500]。
    pub fn list(
        &self,
        character_id: &str,
        limit: usize,
    ) -> Result<Vec<ContractChangeRecord>, String> {
        let limit = limit.clamp(1, 500) as i64;
        let conn = self.db.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT seq, character_id, source, op_kind, path, before_json, after_json, rationale, created_at \
                 FROM contract_changelog WHERE character_id = ?1 ORDER BY seq DESC LIMIT ?2",
            )
            .map_err(|e| format!("准备查询契约变更记录失败: {e}"))?;
        let rows = stmt
            .query_map(rusqlite::params![character_id, limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
            .map_err(|e| format!("查询契约变更记录失败: {e}"))?;
        let mut records = Vec::new();
        for row in rows {
            let (
                seq,
                character_id,
                source,
                op_kind,
                path,
                before_json,
                after_json,
                rationale,
                created_at,
            ) = row.map_err(|e| format!("读取契约变更记录失败: {e}"))?;
            records.push(build_record(
                seq,
                character_id,
                source,
                op_kind,
                path,
                before_json,
                after_json,
                rationale,
                created_at,
            )?);
        }
        Ok(records)
    }

    /// 单条记录(角色 + seq;不存在返回 None)。
    pub fn get(
        &self,
        character_id: &str,
        seq: i64,
    ) -> Result<Option<ContractChangeRecord>, String> {
        let conn = self.db.read()?;
        let row = conn
            .query_row(
                "SELECT seq, character_id, source, op_kind, path, before_json, after_json, rationale, created_at \
                 FROM contract_changelog WHERE character_id = ?1 AND seq = ?2",
                rusqlite::params![character_id, seq],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("查询契约变更记录失败: {e}"))?;
        match row {
            Some((
                seq,
                character_id,
                source,
                op_kind,
                path,
                before_json,
                after_json,
                rationale,
                created_at,
            )) => build_record(
                seq,
                character_id,
                source,
                op_kind,
                path,
                before_json,
                after_json,
                rationale,
                created_at,
            )
            .map(Some),
            None => Ok(None),
        }
    }
}

/// ChangelogSource → snake_case 字符串(枚举为 Copy,序列化不会失败)。
fn source_to_str(source: ChangelogSource) -> String {
    serde_json::to_string(&source)
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// snake_case 字符串 → ChangelogSource(读库行用)。
fn source_from_str(raw: &str) -> Result<ChangelogSource, String> {
    serde_json::from_value(Value::String(raw.to_string()))
        .map_err(|e| format!("未知 changelog 来源 {raw:?}: {e}"))
}

/// 把查询到的原始行字段组装为记录(before/after JSON 解析失败按数据损坏报错)。
#[allow(clippy::too_many_arguments)]
fn build_record(
    seq: i64,
    character_id: String,
    source_raw: String,
    op_kind: String,
    path: String,
    before_json: Option<String>,
    after_json: Option<String>,
    rationale: Option<String>,
    created_at: String,
) -> Result<ContractChangeRecord, String> {
    let before = match before_json {
        Some(raw) => {
            Some(serde_json::from_str(&raw).map_err(|e| format!("解析 before_json 失败: {e}"))?)
        }
        None => None,
    };
    let after = match after_json {
        Some(raw) => {
            Some(serde_json::from_str(&raw).map_err(|e| format!("解析 after_json 失败: {e}"))?)
        }
        None => None,
    };
    Ok(ContractChangeRecord {
        seq,
        character_id,
        source: source_from_str(&source_raw)?,
        op_kind,
        path,
        before,
        after,
        rationale,
        created_at,
    })
}
