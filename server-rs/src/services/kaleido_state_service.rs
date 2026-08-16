// 契约运行态服务(P5):KaleidoState 运行态接线到 SQLite。
//
// 两张表:
//   - kaleido_state:每会话一行(commit_turn 内 upsert),存 stat_data 快照 +
//     meta_json(KaleidoMeta:pending/confidence/lastTurnId 等)+ revision;
//   - kaleido_changelog:逐 op append(§7-ChangelogEntry),seq 为表内自增主键,
//     插入后回填到 entry 再序列化落库(读出的 entry 自带权威 seq,可回滚重放)。
//
// 死锁注意:Db 是单连接 Mutex;本服务每个方法独立短持锁,事务内不调用其他 service。
use crate::contracts::changelog::ChangelogEntry;
use crate::contracts::meta::KaleidoMeta;
use crate::models::db::{now_iso, Db};
use rusqlite::OptionalExtension;
use serde_json::Value;
use std::sync::Arc;

pub struct KaleidoStateService {
    db: Arc<Db>,
}

/// kaleido_state 行(P7 HTTP 出口只读投影);stat_data/meta_json 保持原始
/// JSON 文本,由 handler 侧解析与序列化(服务层不假设消费方形态)。
pub struct KaleidoStateRow {
    pub contract_version: u32,
    pub stat_data: String,
    pub meta_json: String,
    pub revision_seq: u64,
    pub revision_hash: String,
    pub updated_at: String,
}

impl KaleidoStateService {
    pub fn new(db: Arc<Db>) -> Self {
        KaleidoStateService { db }
    }

    /// 读取会话运行态的 meta 与契约版本;无行(尚未 commit 过)返回 None。
    pub fn load_meta(
        &self,
        session_id: &str,
    ) -> Result<Option<(u32, KaleidoMeta)>, String> {
        let conn = self.db.conn();
        let row = conn
            .query_row(
                "SELECT contract_version, meta_json FROM kaleido_state WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("查询契约运行态失败: {e}"))?;
        match row {
            Some((version, meta_json)) => {
                let meta: KaleidoMeta = serde_json::from_str(&meta_json)
                    .map_err(|e| format!("解析契约运行态 meta 失败: {e}"))?;
                Ok(Some((u32::try_from(version).map_err(|_| "契约版本越界".to_string())?, meta)))
            }
            None => Ok(None),
        }
    }

    /// 读取运行态整行(P7 HTTP 出口):stat_data 快照 + meta + revision。
    /// 尚未 commit 过返回 None。
    pub fn load_state(
        &self,
        session_id: &str,
    ) -> Result<Option<KaleidoStateRow>, String> {
        let conn = self.db.conn();
        let row = conn
            .query_row(
                "SELECT contract_version, stat_data, meta_json, revision_seq, revision_hash, updated_at \
                 FROM kaleido_state WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("查询契约运行态失败: {e}"))?;
        let Some((version, stat_data, meta_json, seq, hash, updated_at)) = row else {
            return Ok(None);
        };
        Ok(Some(KaleidoStateRow {
            contract_version: u32::try_from(version).map_err(|_| "契约版本越界".to_string())?,
            stat_data,
            meta_json,
            revision_seq: u64::try_from(seq).map_err(|_| "运行态序号越界".to_string())?,
            revision_hash: hash,
            updated_at,
        }))
    }

    /// 提交一轮运行态:changelog 逐条插入(回填真实 seq)后 upsert kaleido_state。
    ///
    /// entries 为本轮全部生效变更(可能跨正文/两步两条来源);空 entries 也会
    /// upsert(meta.lastTurnId 等仍需推进),revision_seq 保持旧值。
    /// contract_version 与表内旧值不一致时不调和,直接覆盖写入(由调用方决定)。
    pub fn commit_turn(
        &self,
        session_id: &str,
        contract_version: u32,
        stat_data: &Value,
        meta: &KaleidoMeta,
        entries: &mut [ChangelogEntry],
    ) -> Result<(), String> {
        let mut conn = self.db.conn();
        let tx = conn
            .transaction()
            .map_err(|e| format!("开启契约运行态事务失败: {e}"))?;
        let ts = now_iso();
        for entry in entries.iter_mut() {
            tx.execute(
                "INSERT INTO kaleido_changelog (session_id, turn_id, entry_json, created_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![session_id, entry.turn_id as i64, serde_json::to_string(&entry).map_err(|e| format!("序列化变更记录失败: {e}"))?, ts],
            )
            .map_err(|e| format!("写入契约运行态变更记录失败: {e}"))?;
            // 回填权威 seq(表内自增主键),供回滚/重放按 seq 定位
            let seq = tx
                .last_insert_rowid()
                .try_into()
                .map_err(|_| "变更序号越界".to_string())?;
            entry.seq = seq;
            // seq 已知后再序列化落库:读出的 entry_json 自带权威 seq
            tx.execute(
                "UPDATE kaleido_changelog SET entry_json = ?1 WHERE seq = ?2",
                rusqlite::params![
                    serde_json::to_string(&entry).map_err(|e| format!("序列化变更记录失败: {e}"))?,
                    seq as i64
                ],
            )
            .map_err(|e| format!("回填变更记录序号失败: {e}"))?;
        }
        // revision_hash:stat_data + contractVersion + revision_seq 的 FNV-1a 指纹
        // (sha2 未引入,64 位 FNV 占位;篡改对账用,P6 起写入)。
        let revision_hash = {
            let max_seq = entries.iter().map(|e| e.seq).max();
            let existing_seq: Option<u64> = tx
                .query_row(
                    "SELECT revision_seq FROM kaleido_state WHERE session_id = ?1",
                    rusqlite::params![session_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(|e| format!("读取运行态版本失败: {e}"))?
                .and_then(|v| u64::try_from(v).ok());
            let seq = max_seq.unwrap_or(0).max(existing_seq.unwrap_or(0));
            let mut h: u64 = 0xcbf29ce484222325;
            for b in stat_data.to_string().bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x100000001b3);
            }
            for b in contract_version.to_string().bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x100000001b3);
            }
            for b in seq.to_string().bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x100000001b3);
            }
            format!("{h:016x}")
        };
        // revision_seq:本轮有新条目取最大 seq;空则保持旧值(COALESCE 读现有行)。
        // INSERT 分支与 DO UPDATE 分支语义一致(同事务内本轮 changelog 行可见,
        // 首轮 commit 也能拿到正确 seq,避免 revision_seq 恒 0 丢第一条)。
        tx.execute(
            "INSERT INTO kaleido_state (session_id, contract_version, stat_data, meta_json, revision_seq, revision_hash, updated_at) \
             VALUES (?1, ?2, ?3, ?4, \
               COALESCE((SELECT MAX(seq) FROM kaleido_changelog WHERE session_id = ?1), 0), ?6, ?5) \
             ON CONFLICT(session_id) DO UPDATE SET \
               contract_version = excluded.contract_version, \
               stat_data = excluded.stat_data, \
               meta_json = excluded.meta_json, \
               revision_seq = MAX(kaleido_state.revision_seq, \
                 COALESCE((SELECT MAX(seq) FROM kaleido_changelog WHERE session_id = excluded.session_id), 0)), \
               revision_hash = excluded.revision_hash, \
               updated_at = excluded.updated_at",
            rusqlite::params![
                session_id,
                contract_version as i64,
                stat_data.to_string(),
                serde_json::to_string(meta).map_err(|e| format!("序列化运行态 meta 失败: {e}"))?,
                ts,
                revision_hash,
            ],
        )
        .map_err(|e| format!("写入契约运行态失败: {e}"))?;
        tx.commit()
            .map_err(|e| format!("提交契约运行态事务失败: {e}"))?;
        Ok(())
    }

    /// 删除会话的契约运行态(state 行 + changelog 全部);幂等,不存在也成功。
    /// 供会话删除 API 清理孤儿数据(kaleido 两表无 FK 级联,须显式清理)。
    /// 两 DELETE 同事务:半清理(日志空而 state 残留)会让 recover 重放语义失真。
    pub fn delete_for_session(&self, session_id: &str) -> Result<(), String> {
        let mut conn = self.db.conn();
        let tx = conn
            .transaction()
            .map_err(|e| format!("开启契约运行态清理事务失败: {e}"))?;
        tx.execute(
            "DELETE FROM kaleido_changelog WHERE session_id = ?1",
            rusqlite::params![session_id],
        )
        .map_err(|e| format!("清理契约运行态变更失败: {e}"))?;
        tx.execute(
            "DELETE FROM kaleido_state WHERE session_id = ?1",
            rusqlite::params![session_id],
        )
        .map_err(|e| format!("清理契约运行态失败: {e}"))?;
        tx.commit()
            .map_err(|e| format!("提交契约运行态清理事务失败: {e}"))?;
        Ok(())
    }

    /// 读取会话 changelog(最新在前);limit 夹取到 [1, 1000]。
    pub fn list_entries(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<ChangelogEntry>, String> {
        let limit = limit.clamp(1, 1000) as i64;
        let conn = self.db.conn();
        let mut stmt = conn
            .prepare(
                "SELECT entry_json FROM kaleido_changelog WHERE session_id = ?1 \
                 ORDER BY seq DESC LIMIT ?2",
            )
            .map_err(|e| format!("准备查询契约运行态变更失败: {e}"))?;
        let rows = stmt
            .query_map(rusqlite::params![session_id, limit], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| format!("查询契约运行态变更失败: {e}"))?;
        let mut entries = Vec::new();
        for raw in rows {
            let raw = raw.map_err(|e| format!("读取契约运行态变更失败: {e}"))?;
            entries.push(
                serde_json::from_str(&raw)
                    .map_err(|e| format!("解析契约运行态变更失败: {e}"))?,
            );
        }
        Ok(entries)
    }
}
