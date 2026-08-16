// 快速回复(Quick Replies)服务:SQLite 表 quick_replies 的 CRUD。
// 条目可被世界书 EJS 模板经 getqr/getQuickReply(name[, label]) 读取并嵌套渲染
// (ST-Prompt-Template / 酒馆助手生态兼容);管理 UI 在后续阶段提供。
use crate::models::db::{now_iso, Db};
use crate::parsing::assistant::QuickReplyCtx;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// 快速回复视图(API 返回/写入)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuickReplyRecord {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub content: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 注入位置权重(0-4,对齐酒馆 injection position;当前仅用于排序)
    #[serde(default)]
    pub position: i64,
    /// 组内排序(升序)
    #[serde(default = "default_order")]
    pub sort_order: i64,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

fn default_true() -> bool {
    true
}
fn default_order() -> i64 {
    100
}

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<QuickReplyRecord> {
    Ok(QuickReplyRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        label: row.get(2)?,
        content: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        position: row.get(5)?,
        sort_order: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

const SELECT_COLS: &str =
    "id, name, label, content, enabled, position, sort_order, created_at, updated_at";

impl QuickReplyRecord {
    /// 校验并规范化写入口(名称非空;缺省字段补默认)
    pub fn normalize(&mut self) {
        self.name = self.name.trim().to_string();
        self.content = self.content.trim().to_string();
        if self.position < 0 {
            self.position = 0;
        }
        if self.sort_order <= 0 {
            self.sort_order = default_order();
        }
    }

    pub fn valid(&self) -> bool {
        !self.name.is_empty()
    }
}

pub struct QuickReplyService {
    db: Arc<Db>,
}

impl QuickReplyService {
    pub fn new(db: Arc<Db>) -> Self {
        QuickReplyService { db }
    }

    /// 列表(启用优先,按 name/position/sort_order 排序)
    pub fn list(&self, only_enabled: bool) -> Vec<QuickReplyRecord> {
        let conn = self.db.conn();
        let sql = if only_enabled {
            format!("SELECT {SELECT_COLS} FROM quick_replies WHERE enabled = 1 ORDER BY name, position, sort_order, id")
        } else {
            format!("SELECT {SELECT_COLS} FROM quick_replies ORDER BY name, position, sort_order, id")
        };
        let mut stmt = conn.prepare(&sql).unwrap();
        stmt.query_map([], |row| row_to_record(row))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    /// 引擎加载:启用条目 → 渲染上下文可读的 QuickReplyCtx 列表
    pub fn render_lib(&self) -> Vec<QuickReplyCtx> {
        self.list(true)
            .into_iter()
            .map(|r| QuickReplyCtx {
                name: r.name,
                label: r.label,
                content: r.content,
            })
            .collect()
    }

    pub fn get(&self, id: i64) -> Option<QuickReplyRecord> {
        self.db
            .conn()
            .query_row(
                &format!("SELECT {SELECT_COLS} FROM quick_replies WHERE id = ?1"),
                params![id],
                |row| row_to_record(row),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// 新建;返回新记录(含自增 id)
    pub fn create(&self, mut r: QuickReplyRecord) -> Result<QuickReplyRecord, String> {
        r.normalize();
        if !r.valid() {
            return Err("快速回复名称不能为空".into());
        }
        let now = now_iso();
        let id = self
            .db
            .conn()
            .execute(
                "INSERT INTO quick_replies (name, label, content, enabled, position, sort_order, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![r.name, r.label, r.content, r.enabled, r.position, r.sort_order, now, now],
            )
            .map_err(|e| format!("写入数据库失败: {e}"))?;
        Ok(QuickReplyRecord {
            id: id as i64,
            ..r
        })
    }

    /// 更新指定字段;None 表示不变
    pub fn update(&self, id: i64, mut r: QuickReplyRecord) -> Option<QuickReplyRecord> {
        r.normalize();
        let existing = self.get(id)?;
        let name = if r.name.is_empty() {
            existing.name
        } else {
            r.name.trim().to_string()
        };
        if name.is_empty() {
            return None;
        }
        let now = now_iso();
        // conn 作用域收窄:UPDATE 语句执行后立即释放锁,
        // 避免末尾 self.get(id) 再次 lock 同一 Mutex 造成自死锁
        let n = {
            let conn = self.db.conn();
            conn.execute(
                "UPDATE quick_replies SET name = ?1, label = ?2, content = ?3, enabled = ?4, position = ?5, sort_order = ?6, updated_at = ?7 WHERE id = ?8",
                params![name, r.label, r.content, r.enabled, r.position, r.sort_order, now, id],
            )
            .ok()?
        };
        if n == 0 {
            return None;
        }
        self.get(id)
    }

    pub fn delete(&self, id: i64) -> bool {
        self.db
            .conn()
            .execute("DELETE FROM quick_replies WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false)
    }
}
