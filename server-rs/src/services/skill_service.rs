// Skill 库服务:提示词技能库(name/description/content),read 工具按名/关键词读取
use crate::models::db::{now_iso, Db};
use crate::models::types::SkillRecord;
use rusqlite::{params, OptionalExtension};
use std::sync::Arc;
use uuid::Uuid;

/// 导入用条目(API 层把单对象/数组/包壳统一解析为 Vec)
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SkillImport {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub content: String,
}

fn row_to_skill(row: &rusqlite::Row) -> rusqlite::Result<SkillRecord> {
    Ok(SkillRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        content: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        created_at: row.get(5)?,
    })
}

pub struct SkillService {
    db: Arc<Db>,
}

impl SkillService {
    pub fn new(db: Arc<Db>) -> Self {
        SkillService { db }
    }

    pub fn list(&self, only_enabled: bool) -> Vec<SkillRecord> {
        let conn = self.db.conn();
        let sql = if only_enabled {
            "SELECT id, name, description, content, enabled, created_at FROM skills WHERE enabled = 1 ORDER BY name ASC"
        } else {
            "SELECT id, name, description, content, enabled, created_at FROM skills ORDER BY name ASC"
        };
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map([], row_to_skill)
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    /// 按名称查找(大小写不敏感,启用优先)
    pub fn get_by_name(&self, name: &str) -> Option<SkillRecord> {
        let conn = self.db.conn();
        conn.query_row(
            "SELECT id, name, description, content, enabled, created_at FROM skills WHERE lower(name) = lower(?1) LIMIT 1",
            params![name],
            row_to_skill,
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn get(&self, id: &str) -> Option<SkillRecord> {
        let conn = self.db.conn();
        conn.query_row(
            "SELECT id, name, description, content, enabled, created_at FROM skills WHERE id = ?1",
            params![id],
            row_to_skill,
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 导入(同名覆盖):返回导入/更新条数
    pub fn import(&self, items: Vec<SkillImport>) -> Result<usize, String> {
        let conn = self.db.conn();
        let mut count = 0usize;
        for it in items {
            let name = it.name.trim().to_string();
            if name.is_empty() {
                return Err("skill 的 name 不能为空".into());
            }
            let now = now_iso();
            conn.execute(
                "INSERT INTO skills (id, name, description, content, enabled, created_at) VALUES (?1, ?2, ?3, ?4, 1, ?5) \
                 ON CONFLICT(name) DO UPDATE SET description = excluded.description, content = excluded.content",
                params![Uuid::new_v4().to_string(), name, it.description, it.content, now],
            )
            .map_err(|e| format!("导入 skill 失败: {e}"))?;
            count += 1;
        }
        Ok(count)
    }

    /// 更新:enabled / name / description / content(仅提供者生效)
    pub fn update(
        &self,
        id: &str,
        enabled: Option<bool>,
        name: Option<&str>,
        description: Option<&str>,
        content: Option<&str>,
    ) -> Option<SkillRecord> {
        let existing = self.get(id)?;
        let new_enabled = enabled.unwrap_or(existing.enabled);
        let new_name = name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(&existing.name)
            .to_string();
        let new_desc = description
            .map(|s| s.to_string())
            .unwrap_or(existing.description);
        let new_content = content.map(|s| s.to_string()).unwrap_or(existing.content);
        let conn = self.db.conn();
        let n = conn
            .execute(
                "UPDATE skills SET name = ?1, description = ?2, content = ?3, enabled = ?4 WHERE id = ?5",
                params![new_name, new_desc, new_content, new_enabled as i64, id],
            )
            .ok()?;
        if n == 0 {
            return None;
        }
        self.get(id)
    }

    pub fn delete(&self, id: &str) -> bool {
        self.db
            .conn()
            .execute("DELETE FROM skills WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false)
    }
}
