// Skill 库服务:提示词技能库(name/description/content),read 工具按名/关键词读取。
// 渐进披露(落地项 3):system 只注入 name+description 紧凑清单(skill_manifest),
// 正文按需经 read(type=skill) 读取;allowed_tools/run_as_subagent/model 为
// 子智能体调度增强预留元数据(旧库缺列由迁移补默认)。
use super::log_query_failure;
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
    /// 工具白名单(可空 = 不限制)
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// 是否可作为子智能体技能派发(默认 false)
    #[serde(default)]
    pub run_as_subagent: bool,
    /// 可选模型名覆盖(空 = 用当前模型)
    #[serde(default)]
    pub model: String,
}

/// 渐进披露技能清单(落地项 3):只列 name + description,每技能一行
/// 「- name:description」。顺序按 name 字节序稳定排序,同输入两次构建
/// 逐字节一致(前缀缓存要求);正文不进清单,按需 read(type=skill) 加载。
pub fn skill_manifest(skills: &[SkillRecord]) -> String {
    let mut sorted: Vec<&SkillRecord> = skills.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    sorted
        .iter()
        .map(|s| format!("- {}:{}", s.name, s.description.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 解析 allowed_tools 列(JSON 数组字符串 → Vec);空串/非法 JSON/非数组回退空表。
pub fn parse_allowed_tools(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect()
}

fn row_to_skill(row: &rusqlite::Row) -> rusqlite::Result<SkillRecord> {
    Ok(SkillRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        content: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        created_at: row.get(5)?,
        allowed_tools: row.get(6)?,
        run_as_subagent: row.get::<_, i64>(7)? != 0,
        model: row.get(8)?,
    })
}

const SKILL_COLS: &str =
    "id, name, description, content, enabled, created_at, allowed_tools, run_as_subagent, model";

pub struct SkillService {
    db: Arc<Db>,
}

impl SkillService {
    pub fn new(db: Arc<Db>) -> Self {
        SkillService { db }
    }

    pub fn list(&self, only_enabled: bool) -> Vec<SkillRecord> {
        let conn = self.db.read().expect("获取只读连接失败");
        let sql = if only_enabled {
            "SELECT id, name, description, content, enabled, created_at, allowed_tools, run_as_subagent, model FROM skills WHERE enabled = 1 ORDER BY name ASC"
        } else {
            "SELECT id, name, description, content, enabled, created_at, allowed_tools, run_as_subagent, model FROM skills ORDER BY name ASC"
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(e) => return log_query_failure("技能列表 prepare", e),
        };
        let query = stmt.query_map([], row_to_skill);
        match query {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => log_query_failure("技能列表 query_map", e),
        }
    }

    /// 按名称查找(大小写不敏感,启用优先)
    pub fn get_by_name(&self, name: &str) -> Option<SkillRecord> {
        let conn = self.db.read().ok()?;
        conn.query_row(
            "SELECT id, name, description, content, enabled, created_at, allowed_tools, run_as_subagent, model FROM skills WHERE lower(name) = lower(?1) LIMIT 1",
            params![name],
            row_to_skill,
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn get(&self, id: &str) -> Option<SkillRecord> {
        let conn = self.db.read().ok()?;
        conn.query_row(
            &format!("SELECT {SKILL_COLS} FROM skills WHERE id = ?1"),
            params![id],
            row_to_skill,
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 导入(同名覆盖):返回导入/更新条数
    pub fn import(&self, items: Vec<SkillImport>) -> Result<usize, String> {
        let conn = self.db.write();
        let mut count = 0usize;
        for it in items {
            let name = it.name.trim().to_string();
            if name.is_empty() {
                return Err("skill 的 name 不能为空".into());
            }
            let allowed_tools =
                serde_json::to_string(&it.allowed_tools).unwrap_or_else(|_| "[]".into());
            let now = now_iso();
            conn.execute(
                "INSERT INTO skills (id, name, description, content, enabled, created_at, allowed_tools, run_as_subagent, model) \
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(name) DO UPDATE SET description = excluded.description, content = excluded.content, \
                 allowed_tools = excluded.allowed_tools, run_as_subagent = excluded.run_as_subagent, model = excluded.model",
                params![
                    Uuid::new_v4().to_string(),
                    name,
                    it.description,
                    it.content,
                    now,
                    allowed_tools,
                    it.run_as_subagent as i64,
                    it.model.trim()
                ],
            )
            .map_err(|e| format!("导入 skill 失败: {e}"))?;
            count += 1;
        }
        Ok(count)
    }

    /// 更新:enabled / name / description / content 及渐进披露元数据(仅提供者生效)
    #[allow(clippy::too_many_arguments)] // 编排函数参数即上下文,拆 struct 收益低
    pub fn update(
        &self,
        id: &str,
        enabled: Option<bool>,
        name: Option<&str>,
        description: Option<&str>,
        content: Option<&str>,
        allowed_tools: Option<&str>,
        run_as_subagent: Option<bool>,
        model: Option<&str>,
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
        let new_tools = allowed_tools
            .map(|s| s.to_string())
            .unwrap_or(existing.allowed_tools);
        let new_sub = run_as_subagent.unwrap_or(existing.run_as_subagent);
        let new_model = model
            .map(|s| s.trim().to_string())
            .unwrap_or(existing.model);
        let conn = self.db.write();
        let n = conn
            .execute(
                "UPDATE skills SET name = ?1, description = ?2, content = ?3, enabled = ?4, \
                 allowed_tools = ?5, run_as_subagent = ?6, model = ?7 WHERE id = ?8",
                params![
                    new_name,
                    new_desc,
                    new_content,
                    new_enabled as i64,
                    new_tools,
                    new_sub as i64,
                    new_model,
                    id
                ],
            )
            .ok()?;
        // 必须先释放连接锁再 re-get:self.get 会再次加锁同一 Mutex<Connection>,
        // std Mutex 不可重入,持锁调用将死锁(挂死无任何输出即此形态)。
        drop(conn);
        if n == 0 {
            return None;
        }
        self.get(id)
    }

    pub fn delete(&self, id: &str) -> bool {
        self.db
            .write()
            .execute("DELETE FROM skills WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> SkillService {
        let dir = std::env::temp_dir().join(format!(
            "kedai-skill-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let db = Arc::new(Db::open(&dir.join("t.db"), &dir).unwrap());
        SkillService::new(db)
    }

    /// 渐进披露清单:格式固定「- name:description」,按 name 稳定排序,
    /// 同输入两次构建逐字节一致;正文不出现在清单里。
    #[test]
    fn skill_manifest_format_sorted_and_stable() {
        let skills = [
            SkillRecord {
                id: "1".into(),
                name: "zeta 技能".into(),
                description: "排在后面".into(),
                content: "zeta 正文".into(),
                enabled: true,
                created_at: "t".into(),
                allowed_tools: "[]".into(),
                run_as_subagent: false,
                model: String::new(),
            },
            SkillRecord {
                id: "2".into(),
                name: "alpha 技能".into(),
                description: "排在前面".into(),
                content: "alpha 正文".into(),
                enabled: true,
                created_at: "t".into(),
                allowed_tools: "[]".into(),
                run_as_subagent: false,
                model: String::new(),
            },
        ];
        let m = skill_manifest(&skills);
        assert_eq!(m, "- alpha 技能:排在前面\n- zeta 技能:排在后面");
        // 逐字节稳定:两次构建一致(前缀缓存要求)
        assert_eq!(m, skill_manifest(&skills));
        assert!(m.as_bytes().eq(skill_manifest(&skills).as_bytes()));
        // 正文不进清单
        assert!(!m.contains("alpha 正文"));
    }

    /// 兼容旧技能:无 description(description 空串)时清单行仍列 name。
    #[test]
    fn skill_manifest_lists_legacy_skill_without_description() {
        let skills = [SkillRecord {
            id: "1".into(),
            name: "旧技能".into(),
            description: String::new(),
            content: "旧正文".into(),
            enabled: true,
            created_at: "t".into(),
            allowed_tools: "[]".into(),
            run_as_subagent: false,
            model: String::new(),
        }];
        assert_eq!(skill_manifest(&skills), "- 旧技能:");
    }

    /// 禁用技能不进清单(调用方传 list(true) 只取启用);description 尾空白裁剪。
    #[test]
    fn skill_manifest_trims_description_tail() {
        let skills = [SkillRecord {
            id: "1".into(),
            name: "技能".into(),
            description: "  描述  ".into(),
            content: "x".into(),
            enabled: false,
            created_at: "t".into(),
            allowed_tools: "[]".into(),
            run_as_subagent: false,
            model: String::new(),
        }];
        assert_eq!(skill_manifest(&skills), "- 技能:描述");
    }

    /// 渐进披露元数据:导入带 allowed_tools/run_as_subagent/model 的技能,
    /// 旧格式(缺字段)导入回退默认;更新接口可改 enabled 与新字段;往返一致。
    #[test]
    fn import_and_update_progressive_metadata() {
        let svc = service();
        // 旧格式导入:缺三个新字段 → 默认 [] / false / ''
        svc.import(vec![SkillImport {
            name: "旧技能".into(),
            description: "旧".into(),
            content: "c".into(),
            allowed_tools: Vec::new(),
            run_as_subagent: false,
            model: String::new(),
        }])
        .unwrap();
        let legacy = svc.get_by_name("旧技能").unwrap();
        assert_eq!(legacy.allowed_tools, "[]");
        assert!(!legacy.run_as_subagent);
        assert_eq!(legacy.model, "");

        // 新格式导入:同名覆盖,元数据生效
        svc.import(vec![SkillImport {
            name: "新技能".into(),
            description: "新".into(),
            content: "c2".into(),
            allowed_tools: vec!["read".into(), "write".into()],
            run_as_subagent: true,
            model: "deepseek-chat".into(),
        }])
        .unwrap();
        let fresh = svc.get_by_name("新技能").unwrap();
        assert_eq!(fresh.allowed_tools, r#"["read","write"]"#);
        assert!(fresh.run_as_subagent);
        assert_eq!(fresh.model, "deepseek-chat");
        assert_eq!(
            parse_allowed_tools(&fresh.allowed_tools),
            vec!["read", "write"]
        );

        // 更新:停用 + 改 model;未提供字段保持
        let updated = svc
            .update(
                &fresh.id,
                Some(false),
                None,
                None,
                None,
                None,
                None,
                Some("other-model"),
            )
            .unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.model, "other-model");
        assert_eq!(updated.allowed_tools, r#"["read","write"]"#);

        // 清单只列启用技能:list(true) 不含已停用的「新技能」
        let manifest = skill_manifest(&svc.list(true));
        assert!(manifest.contains("旧技能"));
        assert!(!manifest.contains("新技能"));
    }

    /// parse_allowed_tools 容错:非法 JSON / 非数组 / 空白项回退空表。
    #[test]
    fn parse_allowed_tools_tolerates_invalid_input() {
        assert!(parse_allowed_tools("[]").is_empty());
        assert!(parse_allowed_tools("").is_empty());
        assert!(parse_allowed_tools("not-json").is_empty());
        assert!(parse_allowed_tools(r#"{"a":1}"#).is_empty());
        assert_eq!(
            parse_allowed_tools(r#"["read", " ", "write"]"#),
            vec!["read", "write"]
        );
    }
}
