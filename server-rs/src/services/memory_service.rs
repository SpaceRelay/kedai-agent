// 跨会话记忆蒸馏服务(落地项 2,借鉴 Codex memories stage1_outputs 与 Reasonix 记忆修订):
// 记忆按 character_id 维度跨会话共享,解决长对话人设漂移。
//   - 蒸馏(distill):把会话历史交给 LLM 提取多条短记忆,每行一条落库(kind='distilled');
//   - 精选(select_for_injection):selected=1 的按 (pinned DESC, usage_count DESC,
//     last_usage DESC, id DESC) 排序取前 limit 条注入;排序键确定性(tie-break 用 id),
//     保证记忆集合未变时槽内容逐字节稳定;
//   - 召回(select_recall,升级工作流 B2 通道 2):按当前用户输入做 token 相关性召回;
//   - 衰减(touch):注入完成后批量回写 usage_count+1 与 last_usage,只动计数不改已注入内容;
//   - 淘汰(evict_over_capacity,升级工作流 B3):每角色 selected 条目超上限时把最低分
//     条目置 selected=0(只归档不删除,硬删走 prune);
//   - 去重(升级工作流 B4):insert 精确去重(content 相同则计数 +1),蒸馏时 Jaccard 近似去重。
// 工具写入(tools/memory.rs)与手动添加(kind='manual')共用同一张 memory_entries 表。
use crate::models::db::{now_iso, Db};
use crate::models::types::{LlmMessage, MessageRecord};
use crate::services::session_service::SessionService;
use crate::services::settings_service::RuntimeSettings;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex};

/// 单次蒸馏最多落库的记忆条数(防 LLM 长输出灌爆记忆库)
pub const MAX_DISTILLED_LINES: usize = 64;

/// 蒸馏近似去重的 Jaccard 相似度阈值(≥ 视为重复跳过;调此常量即可收紧/放宽)
pub const DISTILL_DEDUP_JACCARD_THRESHOLD: f64 = 0.8;

/// 通道 2 检索召回条数上限(升级工作流 B2)
pub const RECALL_LIMIT: usize = 3;

/// memory_read 工具读取上限(精选排序后截断,防无界返回)
pub const MEMORY_READ_LIMIT: usize = 50;

/// 每角色记忆容量上限默认值(升级工作流 B3;0 = 不限制淘汰)
pub const DEFAULT_MEMORY_MAX_ENTRIES: u32 = 200;

/// 记忆槽字符预算默认值(升级工作流 B2;0 = 不限制)
pub const DEFAULT_MEMORY_INJECT_CHAR_BUDGET: u32 = 2000;

/// 记忆条目(memory_entries 表行)
#[derive(Debug, Clone, Serialize)]
pub struct MemoryEntry {
    pub id: i64,
    pub character_id: String,
    pub source_session_id: Option<String>,
    /// distilled | tool | manual
    pub kind: String,
    pub content: String,
    pub usage_count: i64,
    pub last_usage: Option<String>,
    pub selected: bool,
    pub created_at: String,
    pub updated_at: String,
    /// 分层注入最高优先级(1 = 常驻置顶)
    pub pinned: bool,
}

fn row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<MemoryEntry> {
    Ok(MemoryEntry {
        id: row.get(0)?,
        character_id: row.get(1)?,
        source_session_id: row.get(2)?,
        kind: row.get(3)?,
        content: row.get(4)?,
        usage_count: row.get(5)?,
        last_usage: row.get(6)?,
        selected: row.get::<_, i64>(7)? != 0,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        pinned: row.get::<_, i64>(10)? != 0,
    })
}

const ENTRY_COLUMNS: &str =
    "id, character_id, source_session_id, kind, content, usage_count, last_usage, selected, created_at, updated_at, pinned";

/// 蒸馏结果摘要
#[derive(Debug, Serialize)]
pub struct DistillOutcome {
    pub character_id: String,
    pub inserted: usize,
    /// 近似去重跳过的条数(与已有记忆或本批已接受行重复)
    pub skipped: usize,
    /// 本次新增记忆的 id(供调用方按需补向量,免去重扫全表)
    pub new_ids: Vec<i64>,
}

/// 向量索引状态(Phase 3;供设置界面展示)
#[derive(Debug, Default, Serialize)]
pub struct VectorStatus {
    /// 记忆总条数
    pub total: i64,
    /// 已生成向量的条数
    pub embedded: i64,
    /// 当前向量表维度(None = 表未建)
    pub dim: Option<u32>,
    /// 维度漂移记录(如 "1536->1024"),非 None 表示需重建索引
    pub dim_mismatch: Option<String>,
}

/// vec0 向量列以 BLOB 返回:小端 float32 紧密排列,每 4 字节一个分量。
/// 长度不是 4 的倍数时返回 None(防御脏数据)。
fn blob_to_vec(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.is_empty() || !blob.len().is_multiple_of(4) {
        return None;
    }
    Some(
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// 解析 sqlite-vec 返回的向量文本格式 `[1.0,2.0,...]`(容错:空白跳过,非法项返回 None)。
/// 注:vec0 的向量列在查询时以 BLOB 返回,此函数仅用于少数以 TEXT 形态取数的兼容路径。
#[allow(dead_code)]
fn parse_vec_json(text: &str) -> Option<Vec<f32>> {
    let t = text.trim();
    let inner = t.strip_prefix('[')?.strip_suffix(']')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for part in inner.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        match p.parse::<f32>() {
            Ok(v) => out.push(v),
            Err(_) => return None,
        }
    }
    Some(out)
}

pub struct MemoryService {
    db: Arc<Db>,
    /// 运行期设置句柄(淘汰/预算等阈值来源)。由 AppState 在设置就绪后 attach;
    /// 未 attach(单测/工具单测)时按默认值工作。
    settings: std::sync::OnceLock<Arc<Mutex<RuntimeSettings>>>,
}

/// 蒸馏系统指令:取向沿用 compaction(人物关系与立场、关键事件与因果、未回收伏笔),
/// 输出改为「每行一条短记忆」以支持服务端按行拆分。
pub fn distill_system_prompt() -> &'static str {
    "你是对话记忆蒸馏助手。从下面的角色扮演对话中提取值得跨会话长期记住的记忆条目,\
     每条一行、独立成句、简洁平实(中文)。必须覆盖:人物关系与立场、已发生的关键事件\
     与因果、未回收的伏笔/承诺/约定。不要输出一次性的场景描写、寒暄或与长期记忆无关的\
     细节;不要编号、不要项目符号、不要评论或「记忆如下」等元文本,只输出记忆条目本身。"
}

/// 蒸馏输入文本:按「角色: 内容」逐条拼接(与 compaction_user_text 同格式)。
pub fn distill_user_text(history: &[MessageRecord]) -> String {
    history
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .map(|m| {
            let speaker = if m.role == "user" { "用户" } else { "角色" };
            format!("{speaker}: {}", m.content)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 蒸馏输出按行拆分:去空白、跳过空行、剥一次行首项目符号(- / *),上限 MAX_DISTILLED_LINES。
pub fn parse_distilled_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw
            .trim()
            .trim_start_matches('-')
            .trim_start_matches('*')
            .trim();
        if line.is_empty() {
            continue;
        }
        if out.len() >= MAX_DISTILLED_LINES {
            break;
        }
        out.push(line.to_string());
    }
    out
}

/// 注入候选精选(纯函数):selected=1 的条目按
/// (pinned DESC, usage_count DESC, last_usage DESC, id DESC) 排序取前 limit 条。
/// id 作最终 tie-break 保证确定性——记忆集合未变时输出顺序逐字节稳定(前缀缓存前提)。
pub fn select_for_injection(entries: &[MemoryEntry], limit: usize) -> Vec<&MemoryEntry> {
    let mut picked: Vec<&MemoryEntry> = entries.iter().filter(|e| e.selected).collect();
    picked.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(b.usage_count.cmp(&a.usage_count))
            .then(b.last_usage.cmp(&a.last_usage))
            .then(b.id.cmp(&a.id))
    });
    picked.truncate(limit);
    picked
}

/// 混合召回的打分权重:向量(语义)0.7 + Jaccard(词面)0.3。
/// 向量对同义改写敏感、Jaccard 对精确术语敏感,两路互补;
/// 任一不可用时退化为单路(纯 Jaccard 或纯向量)。
pub const RECALL_VEC_WEIGHT: f64 = 0.7;
pub const RECALL_JACCARD_WEIGHT: f64 = 0.3;

/// 检索召回(纯函数,升级工作流 B2 通道 2 + Phase 3 向量混合):
/// 按当前用户输入与记忆条目的相似度降序取前 limit 条,只返回 selected=1 且不在
/// exclude 内的条目。排序键 (相似度 DESC, pinned DESC, usage_count DESC, id DESC)
/// 确定性;相似度为 0 的条目不入召回(无相关就不注入)。exclude 为通道 1 已注入的 id。
///
/// `query_vec` 为查询向量(None = 向量化未启用/调用失败,退化为纯 Jaccard);
/// `entry_vecs` 为「记忆 id → 向量」映射,缺失该条目的向量时其向量分按 0 计。
pub fn select_recall<'a>(
    entries: &'a [MemoryEntry],
    query: &str,
    limit: usize,
    exclude: &[i64],
) -> Vec<&'a MemoryEntry> {
    select_recall_hybrid(
        entries,
        query,
        limit,
        exclude,
        None,
        &std::collections::HashMap::new(),
    )
}

/// 带向量的混合召回(纯函数,便于单测):
/// 有向量时 score = 向量余弦 × 0.7 + Jaccard × 0.3;无向量时 score = Jaccard。
pub fn select_recall_hybrid<'a>(
    entries: &'a [MemoryEntry],
    query: &str,
    limit: usize,
    exclude: &[i64],
    query_vec: Option<&[f32]>,
    entry_vecs: &std::collections::HashMap<i64, Vec<f32>>,
) -> Vec<&'a MemoryEntry> {
    let query_tokens = tokenize_for_similarity(query);
    if limit == 0 {
        return Vec::new();
    }
    // 查询既无词面 token 又无向量 → 无法判断相关性
    let vec_ready = query_vec.map(|v| !v.is_empty()).unwrap_or(false);
    if query_tokens.is_empty() && !vec_ready {
        return Vec::new();
    }
    let excluded: HashSet<i64> = exclude.iter().copied().collect();
    let mut scored: Vec<(f64, &MemoryEntry)> = entries
        .iter()
        .filter(|e| e.selected && !excluded.contains(&e.id))
        .filter_map(|e| {
            let jac = if query_tokens.is_empty() {
                0.0
            } else {
                jaccard_similarity(&query_tokens, &tokenize_for_similarity(&e.content))
            };
            let score = match (query_vec, entry_vecs.get(&e.id)) {
                (Some(qv), Some(ev)) if !qv.is_empty() && !ev.is_empty() => {
                    let cos = crate::services::embedding_service::cosine_similarity(qv, ev) as f64;
                    // 余弦可能为负(方向相反);钳到 0 避免负分压过词面匹配
                    let cos = cos.max(0.0);
                    cos * RECALL_VEC_WEIGHT + jac * RECALL_JACCARD_WEIGHT
                }
                _ => jac,
            };
            (score > 0.0).then_some((score, e))
        })
        .collect();
    scored.sort_by(|(sa, a), (sb, b)| {
        sb.partial_cmp(sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.pinned.cmp(&a.pinned))
            .then(b.usage_count.cmp(&a.usage_count))
            .then(b.id.cmp(&a.id))
    });
    scored.truncate(limit);
    scored.into_iter().map(|(_, e)| e).collect()
}

/// 相似度用 token 切分:按非字母数字/非 CJK 字符切段,再对 CJK 段做二元组(bigram)
/// 展开(中文无空格,单字集合区分度低),ASCII 词保留整词并小写化。
pub fn tokenize_for_similarity(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();
    let flush_ascii = |ascii: &mut String, out: &mut HashSet<String>| {
        if !ascii.is_empty() {
            out.insert(ascii.to_lowercase());
            ascii.clear();
        }
    };
    let flush_cjk = |cjk: &mut Vec<char>, out: &mut HashSet<String>| {
        match cjk.len() {
            0 => {}
            1 => {
                out.insert(cjk[0].to_string());
            }
            _ => {
                for pair in cjk.windows(2) {
                    out.insert(pair.iter().collect::<String>());
                }
            }
        }
        cjk.clear();
    };
    for ch in text.chars() {
        let is_cjk = matches!(ch as u32,
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x3040..=0x30FF);
        if is_cjk {
            flush_ascii(&mut ascii, &mut out);
            cjk.push(ch);
        } else if ch.is_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            ascii.push(ch);
        } else {
            flush_ascii(&mut ascii, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_ascii(&mut ascii, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out
}

/// token 集合 Jaccard 相似度(|交| / |并|);任一侧为空返回 0
pub fn jaccard_similarity(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

impl MemoryService {
    pub fn new(db: Arc<Db>) -> Self {
        MemoryService {
            db,
            settings: std::sync::OnceLock::new(),
        }
    }

    /// 注入运行期设置句柄(由 AppState 在设置就绪后调用;幂等,已注入则忽略)。
    /// 未注入时淘汰/预算阈值取默认常量(单测与工具单测路径)。
    pub fn attach_settings(&self, settings: Arc<Mutex<RuntimeSettings>>) {
        let _ = self.settings.set(settings);
    }

    /// 运行期设置快照(未注入返回 None);供写入链路判断向量化是否启用
    pub fn settings_snapshot(&self) -> Option<RuntimeSettings> {
        self.settings
            .get()
            .map(|s| s.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    /// 每角色记忆容量上限(0 = 不淘汰)
    fn max_entries(&self) -> usize {
        self.settings
            .get()
            .map(|s| {
                s.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .memory_max_entries
            })
            .unwrap_or(DEFAULT_MEMORY_MAX_ENTRIES) as usize
    }

    /// 角色全部记忆(最新在前),供列表 API
    pub fn list(&self, character_id: &str) -> Vec<MemoryEntry> {
        let Ok(conn) = self.db.read() else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(&format!(
            "SELECT {ENTRY_COLUMNS} FROM memory_entries WHERE character_id = ?1 ORDER BY id DESC"
        )) else {
            return Vec::new();
        };
        stmt.query_map(params![character_id], row_to_entry)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn get(&self, id: i64) -> Option<MemoryEntry> {
        let conn = self.db.read().ok()?;
        conn.query_row(
            &format!("SELECT {ENTRY_COLUMNS} FROM memory_entries WHERE id = ?1"),
            params![id],
            row_to_entry,
        )
        .optional()
        .ok()
        .flatten()
    }

    // ===== 向量索引(Phase 3) =====

    /// 当前配置的 embedding 维度(0 = 未探测);settings 未注入时返回 0
    pub fn embedding_dim(&self) -> u32 {
        self.settings
            .get()
            .map(|s| s.lock().unwrap_or_else(|e| e.into_inner()).embedding_dim)
            .unwrap_or(0)
    }

    /// 确保 vec0 虚拟表存在且维度匹配。维度变化时**不自动删除**已有向量
    /// (数据安全优先),仅返回 false 由调用方提示「需重建索引」。
    /// 返回 Ok(true) = 表可用;Ok(false) = 维度不匹配需重建。
    pub fn ensure_vec_table(&self, dim: u32) -> Result<bool, String> {
        if dim == 0 {
            return Ok(false);
        }
        let conn = self.db.write();
        // 读现有元信息
        let current_dim: Option<u32> = conn
            .query_row(
                "SELECT value FROM memory_vec_meta WHERE key = 'dim'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok());
        let table_exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_vectors'",
                [],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some();

        match (table_exists, current_dim) {
            (true, Some(d)) if d == dim => Ok(true),
            (true, Some(d)) if d != dim => {
                // 维度不匹配:记录待重建状态,不动旧表(用户点「重建向量索引」再 drop)
                conn.execute(
                    "INSERT INTO memory_vec_meta(key, value) VALUES('dim_mismatch', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![format!("{d}->{dim}")],
                )
                .map_err(|e| format!("记录维度不匹配失败: {e}"))?;
                Ok(false)
            }
            _ => {
                conn.execute_batch(&format!(
                    "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vectors USING vec0(
                        memory_id INTEGER PRIMARY KEY,
                        embedding float[{dim}]
                     );"
                ))
                .map_err(|e| format!("创建向量表失败(维度 {dim}): {e}"))?;
                conn.execute(
                    "INSERT INTO memory_vec_meta(key, value) VALUES('dim', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![dim.to_string()],
                )
                .map_err(|e| format!("写入向量维度元信息失败: {e}"))?;
                conn.execute("DELETE FROM memory_vec_meta WHERE key = 'dim_mismatch'", [])
                    .ok();
                Ok(true)
            }
        }
    }

    /// 强制重建向量表(丢弃全部已存向量;调用方随后应触发回填)
    pub fn drop_vec_table(&self) -> Result<(), String> {
        let conn = self.db.write();
        conn.execute_batch("DROP TABLE IF EXISTS memory_vectors;")
            .map_err(|e| format!("删除向量表失败: {e}"))?;
        conn.execute(
            "DELETE FROM memory_vec_meta WHERE key IN ('dim','dim_mismatch')",
            [],
        )
        .ok();
        Ok(())
    }

    /// 写入/覆盖单条记忆的向量(先确保表存在)
    pub fn upsert_vector(&self, memory_id: i64, vec: &[f32]) -> Result<(), String> {
        if vec.is_empty() {
            return Ok(());
        }
        if !self.ensure_vec_table(vec.len() as u32)? {
            return Err("向量维度与索引表不匹配,请先重建向量索引".into());
        }
        let json = crate::services::embedding_service::vec_to_json(vec);
        let conn = self.db.write();
        // vec0 不支持 UPSERT 语法,先删后插
        conn.execute(
            "DELETE FROM memory_vectors WHERE memory_id = ?1",
            params![memory_id],
        )
        .map_err(|e| format!("删除旧向量失败: {e}"))?;
        conn.execute(
            "INSERT INTO memory_vectors(memory_id, embedding) VALUES (?1, ?2)",
            params![memory_id, json],
        )
        .map_err(|e| format!("写入向量失败: {e}"))?;
        Ok(())
    }

    /// 批量写入向量(同一事务,减少写锁往返)
    pub fn upsert_vectors(&self, items: &[(i64, Vec<f32>)]) -> Result<usize, String> {
        if items.is_empty() {
            return Ok(0);
        }
        let dim = items[0].1.len() as u32;
        if !self.ensure_vec_table(dim)? {
            return Err("向量维度与索引表不匹配,请先重建向量索引".into());
        }
        let mut conn = self.db.write();
        let tx = conn
            .transaction()
            .map_err(|e| format!("开启事务失败: {e}"))?;
        let mut n = 0usize;
        for (id, vec) in items {
            if vec.is_empty() {
                continue;
            }
            let json = crate::services::embedding_service::vec_to_json(vec);
            tx.execute(
                "DELETE FROM memory_vectors WHERE memory_id = ?1",
                params![*id],
            )
            .map_err(|e| format!("删除旧向量失败: {e}"))?;
            tx.execute(
                "INSERT INTO memory_vectors(memory_id, embedding) VALUES (?1, ?2)",
                params![*id, json],
            )
            .map_err(|e| format!("写入向量失败: {e}"))?;
            n += 1;
        }
        tx.commit().map_err(|e| format!("提交向量事务失败: {e}"))?;
        Ok(n)
    }

    /// 读取指定记忆的向量(供混合召回;缺失返回 None)
    pub fn get_vectors(&self, ids: &[i64]) -> std::collections::HashMap<i64, Vec<f32>> {
        let mut out = std::collections::HashMap::new();
        if ids.is_empty() {
            return out;
        }
        let Ok(conn) = self.db.read() else {
            return out;
        };
        let table_exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_vectors'",
                [],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some();
        if !table_exists {
            return out;
        }
        let placeholders = vec!["?"; ids.len()].join(", ");
        let sql = format!(
            "SELECT memory_id, embedding FROM memory_vectors WHERE memory_id IN ({placeholders})"
        );
        let Ok(mut stmt) = conn.prepare(&sql) else {
            return out;
        };
        let params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = ids
            .iter()
            .map(|i| Box::new(*i) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params_vec.iter().map(|v| v.as_ref())),
            |r| {
                let id: i64 = r.get(0)?;
                // vec0 的向量列以 BLOB 返回(float32 小端紧密排列),非 TEXT
                let blob: Vec<u8> = r.get(1)?;
                Ok((id, blob))
            },
        );
        if let Ok(rows) = rows {
            for (id, blob) in rows.flatten() {
                if let Some(v) = blob_to_vec(&blob) {
                    out.insert(id, v);
                }
            }
        }
        out
    }

    /// 向量索引状态:总数 / 已嵌入数 / 维度 / 是否维度漂移
    pub fn vector_status(&self) -> VectorStatus {
        let conn = match self.db.read() {
            Ok(c) => c,
            Err(_) => return VectorStatus::default(),
        };
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_entries", [], |r| r.get(0))
            .unwrap_or(0);
        let table_exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_vectors'",
                [],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some();
        let embedded: i64 = if table_exists {
            conn.query_row("SELECT COUNT(*) FROM memory_vectors", [], |r| r.get(0))
                .unwrap_or(0)
        } else {
            0
        };
        let dim: Option<u32> = conn
            .query_row(
                "SELECT value FROM memory_vec_meta WHERE key = 'dim'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok());
        let mismatch: Option<String> = conn
            .query_row(
                "SELECT value FROM memory_vec_meta WHERE key = 'dim_mismatch'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten();
        VectorStatus {
            total,
            embedded,
            dim,
            dim_mismatch: mismatch,
        }
    }

    /// 取出所有缺失向量的记忆(供手动回填);limit 控制单批大小
    pub fn entries_missing_vectors(&self, limit: usize) -> Vec<MemoryEntry> {
        let conn = match self.db.read() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let table_exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_vectors'",
                [],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some();
        let sql = if table_exists {
            format!(
                "SELECT {ENTRY_COLUMNS} FROM memory_entries m \
                 WHERE NOT EXISTS (SELECT 1 FROM memory_vectors v WHERE v.memory_id = m.id) \
                 ORDER BY m.id LIMIT ?1"
            )
        } else {
            format!("SELECT {ENTRY_COLUMNS} FROM memory_entries m ORDER BY m.id LIMIT ?1")
        };
        let Ok(mut stmt) = conn.prepare(&sql) else {
            return Vec::new();
        };
        stmt.query_map(params![limit as i64], row_to_entry)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// 全文检索(B1):FTS5 MATCH + bm25 升序;查询 trim 后 < 3 字符时回退 LIKE
    /// (trigram 分词器要求至少 3 字符,否则无命中)。
    pub fn search(&self, character_id: &str, q: &str, limit: usize) -> Vec<MemoryEntry> {
        let q = q.trim();
        if q.is_empty() || character_id.trim().is_empty() || limit == 0 {
            return Vec::new();
        }
        let Ok(conn) = self.db.read() else {
            return Vec::new();
        };
        if q.chars().count() < 3 {
            // 短查询回退 LIKE(转义 % 与 _ 防通配注入)
            let escaped = q
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            let pattern = format!("%{escaped}%");
            let Ok(mut stmt) = conn.prepare(&format!(
                "SELECT {ENTRY_COLUMNS} FROM memory_entries \
                 WHERE character_id = ?1 AND content LIKE ?2 ESCAPE '\\' ORDER BY id DESC LIMIT ?3"
            )) else {
                return Vec::new();
            };
            return stmt
                .query_map(params![character_id, pattern, limit as i64], row_to_entry)
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();
        }
        // FTS5 查询串:整体加双引号作短语匹配(trigram 下等价子串匹配),
        // 双引号内部再转义 " → "" 防语法错误
        let phrase = format!("\"{}\"", q.replace('"', "\"\""));
        let Ok(mut stmt) = conn.prepare(&format!(
            "SELECT m.{cols} FROM memory_entries_fts f \
             JOIN memory_entries m ON m.id = f.rowid \
             WHERE f.content MATCH ?1 AND m.character_id = ?2 \
             ORDER BY bm25(memory_entries_fts), m.id DESC LIMIT ?3",
            cols = ENTRY_COLUMNS.replace(", ", ", m.")
        )) else {
            return Vec::new();
        };
        stmt.query_map(params![phrase, character_id, limit as i64], row_to_entry)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// 落库一条记忆(kind: distilled|tool|manual);content 空白视为非法。
    /// B4 精确去重:同角色 + 相同 content(trim 后)已存在 → 不新建,
    /// usage_count+1 并返回已有条目。
    /// B3 淘汰:写入后按角色容量上限归档最低分条目(只置 selected=0)。
    pub fn insert(
        &self,
        character_id: &str,
        source_session_id: Option<&str>,
        kind: &str,
        content: &str,
    ) -> Result<MemoryEntry, String> {
        let content = content.trim();
        if content.is_empty() {
            return Err("记忆内容不能为空".into());
        }
        if character_id.trim().is_empty() {
            return Err("缺少 character_id".into());
        }
        if !matches!(kind, "distilled" | "tool" | "manual") {
            return Err(format!("非法记忆类型: {kind}"));
        }
        if let Some(existing) = self.find_exact(character_id, content) {
            self.touch(&[existing.id])?;
            return self
                .get(existing.id)
                .ok_or_else(|| "记忆去重后读取失败".into());
        }
        let now = now_iso();
        let id = {
            let conn = self.db.write();
            conn.execute(
                "INSERT INTO memory_entries
                   (character_id, source_session_id, kind, content, usage_count, last_usage, selected, created_at, updated_at, pinned)
                 VALUES (?1, ?2, ?3, ?4, 0, NULL, 1, ?5, ?5, 0)",
                params![character_id, source_session_id, kind, content, now],
            )
            .map_err(|e| format!("写入记忆失败: {e}"))?;
            conn.last_insert_rowid()
        };
        // 锁已释放再淘汰(evict 内部会取写锁;std::sync::Mutex 不可重入)
        self.evict_over_capacity(character_id)?;
        self.get(id).ok_or_else(|| "写入记忆后读取失败".into())
    }

    /// 精确去重查找:同角色 + content 完全相同(trim 后)的最早一条
    fn find_exact(&self, character_id: &str, content: &str) -> Option<MemoryEntry> {
        let conn = self.db.read().ok()?;
        conn.query_row(
            &format!(
                "SELECT {ENTRY_COLUMNS} FROM memory_entries \
                 WHERE character_id = ?1 AND content = ?2 ORDER BY id ASC LIMIT 1"
            ),
            params![character_id, content],
            row_to_entry,
        )
        .optional()
        .ok()
        .flatten()
    }

    /// B3 容量淘汰:每角色 selected 条目数超过 memory_max_entries 时,把最低分
    /// (usage_count ASC, last_usage ASC, id ASC)条目置 selected=0(只归档不删除)。
    /// 返回归档条数。pinned 条目最后才被归档(排序键把 pinned 排在最前保留)。
    pub fn evict_over_capacity(&self, character_id: &str) -> Result<usize, String> {
        let cap = self.max_entries();
        if cap == 0 {
            return Ok(0);
        }
        let conn = self.db.write();
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_entries WHERE character_id = ?1 AND selected = 1",
                params![character_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("统计记忆条数失败: {e}"))?;
        let excess = total - cap as i64;
        if excess <= 0 {
            return Ok(0);
        }
        // 最低分在前;pinned 条目排到最后才淘汰(保留优先级)
        let archived = conn
            .execute(
                "UPDATE memory_entries SET selected = 0, updated_at = ?2 WHERE id IN ( \
                   SELECT id FROM memory_entries WHERE character_id = ?1 AND selected = 1 \
                   ORDER BY pinned ASC, usage_count ASC, last_usage ASC, id ASC LIMIT ?3 \
                 )",
                params![character_id, now_iso(), excess],
            )
            .map_err(|e| format!("归档超限记忆失败: {e}"))?;
        Ok(archived)
    }

    /// B3 硬删除:清空该角色 selected=0 的归档条目,返回删除条数
    pub fn prune(&self, character_id: &str) -> Result<usize, String> {
        let conn = self.db.write();
        conn.execute(
            "DELETE FROM memory_entries WHERE character_id = ?1 AND selected = 0",
            params![character_id],
        )
        .map_err(|e| format!("清理归档记忆失败: {e}"))
    }

    /// 手动添加(kind='manual')
    pub fn create_manual(&self, character_id: &str, content: &str) -> Result<MemoryEntry, String> {
        self.insert(character_id, None, "manual", content)
    }

    /// 编辑(content / selected / pinned 任选,可同时)
    pub fn update(
        &self,
        id: i64,
        content: Option<&str>,
        selected: Option<bool>,
        pinned: Option<bool>,
    ) -> Option<MemoryEntry> {
        let mut sets: Vec<String> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(c) = content {
            let c = c.trim();
            if c.is_empty() {
                return None;
            }
            sets.push("content = ?".into());
            values.push(Box::new(c.to_string()));
        }
        if let Some(s) = selected {
            sets.push("selected = ?".into());
            values.push(Box::new(if s { 1i64 } else { 0i64 }));
        }
        if let Some(p) = pinned {
            sets.push("pinned = ?".into());
            values.push(Box::new(if p { 1i64 } else { 0i64 }));
        }
        if sets.is_empty() {
            return self.get(id);
        }
        sets.push("updated_at = ?".into());
        values.push(Box::new(now_iso()));
        let sql = format!(
            "UPDATE memory_entries SET {} WHERE id = {}",
            sets.join(", "),
            id
        );
        {
            let conn = self.db.write();
            let n = conn
                .execute(
                    sql.as_str(),
                    rusqlite::params_from_iter(values.iter().map(|v| v.as_ref())),
                )
                .ok()?;
            if n == 0 {
                return None;
            }
        }
        self.get(id)
    }

    pub fn delete(&self, id: i64) -> bool {
        let conn = self.db.write();
        conn.execute("DELETE FROM memory_entries WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    /// 注入后衰减回写:批量 usage_count+1 与 last_usage,不改 content 与 selected,
    /// 也不触碰已注入消息数组(本轮内容在构建时已冻结)。
    pub fn touch(&self, ids: &[i64]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = now_iso();
        let placeholders = vec!["?"; ids.len()].join(", ");
        let sql = format!(
            "UPDATE memory_entries SET usage_count = usage_count + 1, last_usage = ?1, updated_at = ?1 \
             WHERE id IN ({placeholders})"
        );
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(now)];
        for id in ids {
            params_vec.push(Box::new(*id));
        }
        let conn = self.db.write();
        conn.execute(
            sql.as_str(),
            rusqlite::params_from_iter(params_vec.iter().map(|v| v.as_ref())),
        )
        .map_err(|e| format!("记忆使用计数回写失败: {e}"))?;
        Ok(())
    }

    /// 蒸馏一个会话:取历史(复用 SessionService 读法)→ 调 LLM → 按行落库。
    /// LLM 调用抽象为函数参数(mock/真连接器均可注入);空历史直接跳过(不调 LLM)。
    pub async fn distill_session<L, F>(
        &self,
        sessions: &SessionService,
        session_id: &str,
        llm: L,
    ) -> Result<DistillOutcome, String>
    where
        L: FnOnce(Vec<LlmMessage>) -> F,
        F: Future<Output = Result<String, String>>,
    {
        let Some(session) = sessions.get(session_id) else {
            return Err(format!("会话不存在: {session_id}"));
        };
        let history = sessions.get_messages(session_id);
        let user_text = distill_user_text(&history);
        if user_text.trim().is_empty() {
            // 空历史(无 user/assistant 消息)不蒸馏,不消耗 LLM 调用
            return Ok(DistillOutcome {
                character_id: session.character_id,
                inserted: 0,
                skipped: 0,
                new_ids: Vec::new(),
            });
        }
        let messages = vec![
            LlmMessage::plain("system", distill_system_prompt()),
            LlmMessage::plain("user", &user_text),
        ];
        let output = llm(messages).await?;
        let lines = parse_distilled_lines(&output);
        if lines.is_empty() {
            return Err("蒸馏结果为空,未写入记忆".into());
        }
        // B4 近似去重:与已有记忆 token Jaccard ≥ 阈值视为重复跳过;
        // 本批内也已接受的行同样参与比较(防同一输出内近义重复)
        let mut accepted: Vec<HashSet<String>> = self
            .list(&session.character_id)
            .iter()
            .map(|e| tokenize_for_similarity(&e.content))
            .collect();
        let mut inserted = 0usize;
        let mut skipped = 0usize;
        let mut new_ids: Vec<i64> = Vec::new();
        for line in lines {
            let tokens = tokenize_for_similarity(&line);
            let duplicate = accepted
                .iter()
                .any(|t| jaccard_similarity(t, &tokens) >= DISTILL_DEDUP_JACCARD_THRESHOLD);
            if duplicate {
                skipped += 1;
                continue;
            }
            let entry = self.insert(&session.character_id, Some(session_id), "distilled", &line)?;
            new_ids.push(entry.id);
            accepted.push(tokens);
            inserted += 1;
        }
        Ok(DistillOutcome {
            character_id: session.character_id,
            inserted,
            skipped,
            new_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: i64, usage: i64, last_usage: Option<&str>, selected: bool) -> MemoryEntry {
        entry_pinned(id, usage, last_usage, selected, false)
    }

    fn entry_pinned(
        id: i64,
        usage: i64,
        last_usage: Option<&str>,
        selected: bool,
        pinned: bool,
    ) -> MemoryEntry {
        MemoryEntry {
            id,
            character_id: "c1".into(),
            source_session_id: None,
            kind: "distilled".into(),
            content: format!("记忆{id}"),
            usage_count: usage,
            last_usage: last_usage.map(String::from),
            selected,
            created_at: String::new(),
            updated_at: String::new(),
            pinned,
        }
    }

    fn service() -> (MemoryService, SessionService, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("kedai-memory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("kedai.db"), &dir).unwrap());
        let memory = MemoryService::new(db.clone());
        let sessions = SessionService::new(db);
        (memory, sessions, dir)
    }

    fn seed_session(
        _sessions: &SessionService,
        dir: &std::path::Path,
        character_id: &str,
        sid: &str,
    ) {
        // SessionService.db 为私有,测试以独立连接播种(与 compaction.rs 测试同模式)
        let conn = rusqlite::Connection::open(dir.join("kedai.db")).unwrap();
        conn.execute(
            "INSERT INTO characters (id, name, chara_name, description, file_path, data_raw, created_at)
             VALUES (?1, 'c', 'c', '', '', '{}', '')",
            params![character_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, character_id, title, created_at, updated_at)
             VALUES (?1, ?2, 't', '', '')",
            params![sid, character_id],
        )
        .unwrap();
    }

    fn msg(id: i64, role: &str, content: &str) -> MessageRecord {
        MessageRecord {
            id,
            session_id: "s1".into(),
            role: role.into(),
            content: content.into(),
            extra: json!({}),
            created_at: String::new(),
        }
    }

    /// memory_entries 表随 Db::open 自动建立(旧库启动幂等升级),列与索引齐全
    #[test]
    fn memory_table_created_on_open() {
        let (memory, _, dir) = service();
        let conn = memory.db.read().unwrap();
        let mut columns: Vec<String> = Vec::new();
        {
            let mut stmt = conn.prepare("PRAGMA table_info(memory_entries)").unwrap();
            let rows = stmt.query_map([], |row| row.get::<_, String>(1)).unwrap();
            for c in rows.flatten() {
                columns.push(c);
            }
        }
        for expected in [
            "id",
            "character_id",
            "source_session_id",
            "kind",
            "content",
            "usage_count",
            "last_usage",
            "selected",
            "created_at",
            "updated_at",
            "pinned",
        ] {
            assert!(
                columns.iter().any(|c| c == expected),
                "memory_entries 应含列 {expected},实际: {columns:?}"
            );
        }
        // 索引:character_id + selected
        let mut idx_count = 0;
        {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_schema WHERE type='index' AND tbl_name='memory_entries'")
                .unwrap();
            let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
            for _ in rows.flatten() {
                idx_count += 1;
            }
        }
        assert!(
            idx_count >= 1,
            "memory_entries 应建 character_id+selected 索引"
        );
        // kind CHECK 约束:非法 kind 拒绝
        assert!(memory.insert("c1", None, "bogus", "x").is_err());
        drop(conn);
        std::fs::remove_dir_all(dir).ok();
    }

    /// 精选排序:pinned 优先 → usage_count DESC → last_usage DESC(None 最后)
    /// → id DESC(tie-break 确定性);selected=0 过滤;limit 截断
    #[test]
    fn select_orders_by_usage_recency_and_limit() {
        let entries = vec![
            entry(1, 0, None, true), // 从未使用,排最后
            entry(2, 5, Some("2026-08-01T00:00:00Z"), true),
            entry(3, 5, Some("2026-08-02T00:00:00Z"), true), // 同计数,更近使用在前
            entry(4, 9, Some("2026-07-01T00:00:00Z"), true), // 计数最高
            entry(5, 99, Some("2026-08-03T00:00:00Z"), false), // 未选中,不参与
            entry(6, 5, Some("2026-08-02T00:00:00Z"), true), // 与 3 完全同键,id 大在前
            entry_pinned(7, 0, None, true, true),            // pinned:即使零使用也置顶
        ];
        let picked = select_for_injection(&entries, 10);
        let ids: Vec<i64> = picked.iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            vec![7, 4, 6, 3, 2, 1],
            "排序应为 pinned→计数→最近使用→id(新在前): {ids:?}"
        );
        // limit 截断
        let top2 = select_for_injection(&entries, 2);
        assert_eq!(top2.iter().map(|e| e.id).collect::<Vec<_>>(), vec![7, 4]);
        // 确定性:同集合两次调用输出一致(逐字节稳定前提)
        let again = select_for_injection(&entries, 10);
        assert_eq!(picked.len(), again.len());
        for (a, b) in picked.iter().zip(again.iter()) {
            assert_eq!(a.id, b.id);
        }
    }

    /// 蒸馏输出拆分:去空行、剥行首 -/*、上限截断
    #[test]
    fn parse_distilled_lines_splits_and_normalizes() {
        let text = "用户与角色在图书馆初识\n\n- 角色承诺周末带用户看画展\n* 用户透露自己害怕打雷\n  \n第4条\n";
        let lines = parse_distilled_lines(text);
        assert_eq!(
            lines,
            vec![
                "用户与角色在图书馆初识",
                "角色承诺周末带用户看画展",
                "用户透露自己害怕打雷",
                "第4条",
            ]
        );
        // 全空白输出 → 空(调用方应报错)
        assert!(parse_distilled_lines("  \n \n").is_empty());
        // 超上限截断
        let long = (0..100)
            .map(|i| format!("记忆{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse_distilled_lines(&long).len(), MAX_DISTILLED_LINES);
    }

    /// 蒸馏落库:fake LLM 返回多行 → 每行一条 kind='distilled',
    /// character_id 按会话归属,source_session_id 记来源
    #[tokio::test]
    async fn distill_session_inserts_lines() {
        let (memory, sessions, dir) = service();
        seed_session(&sessions, &dir, "charA", "s1");
        sessions
            .add_message("s1", "user", "我们好像在哪见过?", json!({}))
            .unwrap();
        sessions
            .add_message("s1", "assistant", "图书馆,上周三的雨夜。", json!({}))
            .unwrap();

        let outcome = memory
            .distill_session(&sessions, "s1", |messages| async move {
                assert_eq!(messages[0].role, "system");
                assert!(
                    messages[1].content.contains("图书馆"),
                    "蒸馏输入应含历史: {}",
                    messages[1].content
                );
                Ok("用户与角色在图书馆初识\n角色承诺周末看画展\n\n- 用户害怕打雷".into())
            })
            .await
            .unwrap();
        assert_eq!(outcome.inserted, 3);
        assert_eq!(outcome.skipped, 0);
        assert_eq!(outcome.character_id, "charA");

        let list = memory.list("charA");
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|e| e.kind == "distilled"));
        assert!(list
            .iter()
            .all(|e| e.source_session_id.as_deref() == Some("s1")));
        assert!(
            list.iter().any(|e| e.content == "用户害怕打雷"),
            "- 前缀应被剥除"
        );
        assert!(list.iter().all(|e| e.usage_count == 0 && e.selected));
        std::fs::remove_dir_all(dir).ok();
    }

    /// 空历史不蒸馏:不调 LLM(fake 计数为 0)、inserted=0
    #[tokio::test]
    async fn distill_session_empty_history_skips() {
        let (memory, sessions, dir) = service();
        seed_session(&sessions, &dir, "charB", "s2");
        let outcome = memory
            .distill_session(&sessions, "s2", |_| async {
                panic!("空历史不应调用 LLM");
            })
            .await
            .unwrap();
        assert_eq!(outcome.inserted, 0);
        assert!(memory.list("charB").is_empty());
        // 会话不存在 → 报错
        assert!(memory
            .distill_session(&sessions, "missing", |_| async { Ok("x".into()) })
            .await
            .is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    /// LLM 失败上抛;蒸馏输出全空白 → 报错不落库
    #[tokio::test]
    async fn distill_session_llm_failure_propagates() {
        let (memory, sessions, dir) = service();
        seed_session(&sessions, &dir, "charC", "s3");
        sessions
            .add_message("s3", "user", "你好", json!({}))
            .unwrap();
        assert!(memory
            .distill_session(&sessions, "s3", |_| async { Err("LLM 不可用".into()) })
            .await
            .is_err());
        assert!(memory
            .distill_session(&sessions, "s3", |_| async { Ok("  \n ".into()) })
            .await
            .is_err());
        assert!(memory.list("charC").is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    /// CRUD:手动添加/编辑/删除;touch 只动计数与时间戳
    #[test]
    fn crud_and_touch_semantics() {
        let (memory, _, dir) = service();
        let e = memory.create_manual("c1", "  用户喜欢薄荷茶  ").unwrap();
        assert_eq!(e.content, "用户喜欢薄荷茶", "content 应 trim");
        assert_eq!(e.kind, "manual");
        assert_eq!(e.source_session_id, None);

        // 编辑:content + selected + pinned
        let updated = memory
            .update(e.id, Some("用户喜欢洋甘菊茶"), Some(false), Some(true))
            .unwrap();
        assert_eq!(updated.content, "用户喜欢洋甘菊茶");
        assert!(!updated.selected);
        assert!(updated.pinned);
        // 空白 content → 404 语义(拒绝)
        assert!(memory.update(e.id, Some("   "), None, None).is_none());
        // 不存在的 id
        assert!(memory.update(9999, Some("x"), None, None).is_none());

        // touch:计数 +1、last_usage 落时间戳,content/selected 不动
        memory.touch(&[e.id]).unwrap();
        let touched = memory.get(e.id).unwrap();
        assert_eq!(touched.usage_count, 1);
        assert!(touched.last_usage.is_some());
        assert_eq!(touched.content, "用户喜欢洋甘菊茶");
        assert!(!touched.selected);
        // 空 ids 幂等
        memory.touch(&[]).unwrap();
        assert_eq!(memory.get(e.id).unwrap().usage_count, 1);

        // 删除
        assert!(memory.delete(e.id));
        assert!(!memory.delete(e.id));
        assert!(memory.get(e.id).is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    /// Phase 3 向量索引端到端(不调外部 API):
    /// vec0 表懒建 → 写入向量 → 读取回环 → 混合召回按向量相似度排序。
    #[test]
    fn vector_index_roundtrip_and_hybrid_recall() {
        let (memory, _, dir) = service();
        let a = memory.create_manual("c1", "用户喜欢薄荷茶").unwrap();
        let b = memory.create_manual("c1", "角色承诺周末带用户看画展").unwrap();

        // 维度非法(0)时拒绝
        assert!(memory.ensure_vec_table(0).is_ok_and(|ok| !ok));

        // 建表 + 写入(4 维便于手工构造语义关系)
        let va = vec![1.0, 0.0, 0.0, 0.0];
        let vb = vec![0.0, 1.0, 0.0, 0.0];
        memory.upsert_vector(a.id, &va).unwrap();
        memory.upsert_vector(b.id, &vb).unwrap();

        // 读取回环:向量数值与维度一致
        let got = memory.get_vectors(&[a.id, b.id]);
        assert_eq!(got.len(), 2, "两条向量都应读到");
        assert_eq!(got[&a.id].len(), 4);
        assert!((got[&a.id][0] - 1.0).abs() < 1e-6, "向量值应往返一致");

        // 覆盖写:同一 id 再写不应产生重复行
        memory.upsert_vector(a.id, &va).unwrap();
        assert_eq!(memory.get_vectors(&[a.id]).len(), 1, "覆盖写后仍只有一行");
        assert_eq!(memory.vector_status().embedded, 2, "向量总数应为 2");

        // 混合召回:查询向量贴近 a → a 排前(即使词面无交集)
        let entries = memory.list("c1");
        let qv = vec![1.0, 0.0, 0.0, 0.0];
        let recalled = select_recall_hybrid(&entries, "完全无关的查询", 2, &[], Some(&qv), &got);
        assert_eq!(
            recalled.first().map(|e| e.id),
            Some(a.id),
            "向量贴近 a 时应召回 a 在前: {:?}",
            recalled.iter().map(|e| e.id).collect::<Vec<_>>()
        );

        // 无向量时退化为纯 Jaccard:词面命中 b 的内容
        let recalled2 = select_recall(&entries, "画展", 2, &[]);
        assert_eq!(
            recalled2.first().map(|e| e.id),
            Some(b.id),
            "无向量时按词面召回"
        );

        // 维度不匹配:换维度后 ensure 返回 false(保留旧数据,不自动删)
        assert!(memory.ensure_vec_table(8).is_ok_and(|ok| !ok));
        let st = memory.vector_status();
        assert!(st.dim_mismatch.is_some(), "应记录维度漂移");
        assert_eq!(st.embedded, 2, "漂移时旧向量保留");

        // 显式重建:清表后可重新写入新维度
        memory.drop_vec_table().unwrap();
        assert_eq!(memory.vector_status().embedded, 0, "重建后向量清空");
        let v8 = vec![0.5f32; 8];
        memory.upsert_vector(a.id, &v8).unwrap();
        assert_eq!(memory.vector_status().dim, Some(8), "新维度生效");

        std::fs::remove_dir_all(dir).ok();
    }

    /// B1 检索:中文 FTS5 命中(trigram);<3 字符回退 LIKE;
    /// 更新/删除经 trigger 同步索引;查询短语中的双引号不炸语法
    #[test]
    fn search_hits_chinese_via_fts_and_falls_back_to_like() {
        let (memory, _, dir) = service();
        memory
            .insert("c1", None, "manual", "用户与角色在图书馆初识")
            .unwrap();
        memory.insert("c1", None, "manual", "角色害怕打雷").unwrap();
        memory
            .insert("c2", None, "manual", "另一个角色的图书馆")
            .unwrap();

        // FTS5 中文命中(≥3 字符)
        let hits = memory.search("c1", "图书馆", 10);
        assert_eq!(hits.len(), 1, "中文应经 FTS5 命中: {hits:?}");
        assert_eq!(hits[0].content, "用户与角色在图书馆初识");
        // 角色隔离:c2 的记忆不出现在 c1 检索结果
        assert!(memory
            .search("c2", "图书馆", 10)
            .iter()
            .all(|e| e.character_id == "c2"));

        // <3 字符回退 LIKE(trigram 无法匹配 2 字)
        let short = memory.search("c1", "打雷", 10);
        assert_eq!(short.len(), 1, "2 字符查询应回退 LIKE 命中: {short:?}");
        assert_eq!(short[0].content, "角色害怕打雷");
        // LIKE 通配符转义:查询 "%" 不应匹配全部
        assert!(
            memory.search("c1", "%%", 10).is_empty(),
            "% 应被转义而非通配"
        );

        // 更新后索引同步(trigger AFTER UPDATE)
        memory
            .update(hits[0].id, Some("用户与角色在美术馆初识"), None, None)
            .unwrap();
        assert!(
            memory.search("c1", "图书馆", 10).is_empty(),
            "旧内容应已出索引"
        );
        assert_eq!(memory.search("c1", "美术馆", 10).len(), 1, "新内容应入索引");

        // 删除后索引同步(trigger AFTER DELETE)
        memory.delete(short[0].id);
        assert!(
            memory.search("c1", "打雷", 10).is_empty(),
            "删除后不应再命中"
        );

        // limit 生效
        for i in 0..5 {
            memory
                .insert("c3", None, "manual", &format!("共同关键词记忆{i}"))
                .unwrap();
        }
        assert_eq!(memory.search("c3", "共同关键词", 2).len(), 2);
        std::fs::remove_dir_all(dir).ok();
    }

    /// B3 淘汰:超过 memory_max_entries 时把最低分条目置 selected=0(只归档不删除),
    /// prune 硬删除归档条目;容量 0 = 不淘汰
    #[test]
    fn evict_archives_lowest_score_and_prune_removes() {
        let (memory, _, dir) = service();
        // 先落 5 条(未接设置 → 默认容量 200,不淘汰),再压低容量触发淘汰
        let ids: Vec<i64> = (0..5)
            .map(|i| {
                memory
                    .insert("cap", None, "manual", &format!("容量测试记忆{i}"))
                    .unwrap()
                    .id
            })
            .collect();
        // 前两条 usage 更高(第 3..5 条最低分被归档)
        memory.touch(&ids[0..2]).unwrap();
        let settings = Arc::new(Mutex::new(RuntimeSettings::from_config(
            &crate::config::AppConfig::from_env(),
        )));
        settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .memory_max_entries = 3;
        memory.attach_settings(settings);
        let archived = memory.evict_over_capacity("cap").unwrap();
        assert_eq!(archived, 2, "超容量 2 条应被归档");

        let all = memory.list("cap");
        assert_eq!(all.len(), 5, "淘汰只归档不删除: {all:?}");
        let selected: Vec<i64> = all.iter().filter(|e| e.selected).map(|e| e.id).collect();
        assert_eq!(selected.len(), 3, "选中条目应压到容量上限");
        assert!(
            selected.contains(&ids[0]) && selected.contains(&ids[1]),
            "高使用条目应保留: selected={selected:?} ids={ids:?}"
        );

        // 幂等:再次淘汰无变化
        assert_eq!(memory.evict_over_capacity("cap").unwrap(), 0);

        // prune:硬删除归档条目,返回条数;再次 prune 幂等为 0
        assert_eq!(memory.prune("cap").unwrap(), 2);
        assert_eq!(memory.list("cap").len(), 3);
        assert_eq!(memory.prune("cap").unwrap(), 0);

        // 容量 0 = 不淘汰
        let settings0 = Arc::new(Mutex::new(RuntimeSettings::from_config(
            &crate::config::AppConfig::from_env(),
        )));
        settings0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .memory_max_entries = 0;
        let (memory0, _, dir0) = service();
        memory0.attach_settings(settings0);
        for i in 0..6 {
            memory0
                .insert("nocap", None, "manual", &format!("不淘汰记忆{i}"))
                .unwrap();
        }
        assert_eq!(
            memory0.list("nocap").iter().filter(|e| e.selected).count(),
            6,
            "容量 0 应不淘汰"
        );
        std::fs::remove_dir_all(dir).ok();
        std::fs::remove_dir_all(dir0).ok();
    }

    /// B4 精确去重:同角色相同 content(trim 后)不新建,usage_count+1 并返回已有条目;
    /// 不同角色 / 不同内容照常新建
    #[test]
    fn insert_dedups_exact_content() {
        let (memory, _, dir) = service();
        let first = memory
            .insert("d1", None, "manual", "  用户喜欢薄荷茶  ")
            .unwrap();
        assert_eq!(first.usage_count, 0);
        let again = memory
            .insert("d1", Some("s9"), "tool", "用户喜欢薄荷茶")
            .unwrap();
        assert_eq!(again.id, first.id, "相同 content 应复用已有条目");
        assert_eq!(again.usage_count, 1, "去重应 usage_count+1");
        assert_eq!(memory.list("d1").len(), 1, "不应新建第二条");

        // 不同角色同内容:各建一条
        memory
            .insert("d2", None, "manual", "用户喜欢薄荷茶")
            .unwrap();
        assert_eq!(memory.list("d2").len(), 1);

        // 不同内容:新建
        memory.insert("d1", None, "manual", "用户喜欢咖啡").unwrap();
        assert_eq!(memory.list("d1").len(), 2);
        std::fs::remove_dir_all(dir).ok();
    }

    /// B4 近似去重:蒸馏输出与已有记忆 Jaccard ≥ 0.8 跳过;
    /// 同一批内近义行也只落一条;不相似行照常落库
    #[tokio::test]
    async fn distill_skips_near_duplicates_by_jaccard() {
        let (memory, sessions, dir) = service();
        seed_session(&sessions, &dir, "charJ", "sj");
        sessions
            .add_message("sj", "user", "聊聊记忆", json!({}))
            .unwrap();
        // 已有记忆:与第一条蒸馏行高度相似
        memory
            .insert("charJ", None, "manual", "用户与角色在图书馆初识")
            .unwrap();

        let outcome = memory
            .distill_session(&sessions, "sj", |_| async {
                // 第 1 行与已有记忆仅差尾字(Jaccard ≈ 0.91 ≥ 0.8)→ 跳过
                Ok("用户与角色在图书馆初识了\n角色害怕打雷".into())
            })
            .await
            .unwrap();
        assert_eq!(outcome.inserted, 1, "近似重复应跳过,仅落 1 条");
        assert_eq!(outcome.skipped, 1);
        let contents: Vec<String> = memory
            .list("charJ")
            .iter()
            .map(|e| e.content.clone())
            .collect();
        assert!(contents.iter().any(|c| c == "角色害怕打雷"));
        assert!(
            !contents.iter().any(|c| c.contains("初识了")),
            "近似重复不应落库: {contents:?}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// 相似度工具:中文 bigram + ASCII 整词;Jaccard 对称、空集为 0
    #[test]
    fn similarity_tokens_and_jaccard() {
        let a = tokenize_for_similarity("用户喜欢薄荷茶");
        let b = tokenize_for_similarity("用户喜欢薄荷茶");
        assert_eq!(jaccard_similarity(&a, &b), 1.0, "同串相似度应为 1");
        let c = tokenize_for_similarity("角色害怕打雷");
        assert_eq!(jaccard_similarity(&a, &c), 0.0, "无关串相似度应为 0");
        // ASCII 整词 + 大小写归一
        let d = tokenize_for_similarity("Hello World");
        assert!(d.contains("hello") && d.contains("world"));
        assert_eq!(
            jaccard_similarity(&d, &tokenize_for_similarity("hello world")),
            1.0
        );
        // 空集
        assert_eq!(jaccard_similarity(&HashSet::new(), &d), 0.0);
        assert!(tokenize_for_similarity("   ").is_empty());
    }

    /// B2 召回(纯函数):按相关性降序、selected 过滤、exclude 排除通道 1、
    /// 上限截断、零相似不召回、排序确定性
    #[test]
    fn select_recall_ranks_by_relevance_and_excludes() {
        let mut e1 = entry(1, 0, None, true);
        e1.content = "用户与角色在图书馆初识".into();
        let mut e2 = entry(2, 0, None, true);
        e2.content = "用户喜欢薄荷茶".into();
        let mut e3 = entry(3, 0, None, true);
        e3.content = "角色害怕打雷".into();
        let mut e4 = entry(4, 0, None, false);
        e4.content = "用户与角色在图书馆初识".into(); // 未选中,不参与
        let entries = vec![e1, e2, e3, e4];

        let recalled = select_recall(&entries, "还记得图书馆的事吗", 3, &[]);
        assert_eq!(recalled.len(), 1, "只有 e1 相关: {recalled:?}");
        assert_eq!(recalled[0].id, 1);
        // selected=0 不召回
        assert!(
            select_recall(&entries, "图书馆", 3, &[1]).is_empty(),
            "exclude 后应无命中"
        );
        // limit=0 与空查询
        assert!(select_recall(&entries, "图书馆", 0, &[]).is_empty());
        assert!(select_recall(&entries, "   ", 3, &[]).is_empty());
        // 确定性:两次调用顺序一致
        let a = select_recall(&entries, "图书馆 薄荷茶 打雷", 3, &[]);
        let b = select_recall(&entries, "图书馆 薄荷茶 打雷", 3, &[]);
        assert_eq!(
            a.iter().map(|e| e.id).collect::<Vec<_>>(),
            b.iter().map(|e| e.id).collect::<Vec<_>>()
        );
    }

    /// distill_user_text:只取 user/assistant,role 映射可读称呼
    #[test]
    fn distill_user_text_filters_roles() {
        let history = vec![
            msg(1, "user", "你好"),
            msg(2, "assistant", "你好呀"),
            msg(3, "system", "工具记忆条目"),
        ];
        let text = distill_user_text(&history);
        assert!(text.contains("用户: 你好"));
        assert!(text.contains("角色: 你好呀"));
        assert!(!text.contains("工具记忆条目"), "system 消息不进蒸馏输入");
    }
}
