// 角色卡服务(与 Node 版 character.service.ts 对齐):上传/CRUD/文件落盘
use crate::models::db::{now_iso, Db};
use crate::models::types::CharacterRecord;
use crate::parsing::character_card::{parse_character_card, safe_file_name};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

/// 内置「系统助手」角色 id(稳定,幂等;无提示词,作为默认助手使用)
pub const BUILTIN_SYSTEM_ID: &str = "builtin-system";

/// 从 data_raw 提取备用开场列表(alternate_greetings):取非空字符串,空数组返回 None
fn extract_alternate_greetings(data_raw: &Value) -> Option<Vec<String>> {
    let arr = data_raw.get("alternate_greetings")?.as_array()?;
    let list: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .collect();
    if list.is_empty() {
        None
    } else {
        Some(list)
    }
}

pub struct CharacterService {
    db: Arc<Db>,
    data_dir: PathBuf,
}

fn row_to_character(row: &rusqlite::Row, with_data_raw: bool) -> rusqlite::Result<CharacterRecord> {
    // SQL 恒查 data_raw 列(第 7 列);regex_scripts 为前端渲染必需,列表也始终提取;
    // data_raw 本体仅在详情接口返回(体积大)。
    let raw_str: Option<String> = row.get(6)?;
    let data_raw_value: Option<Value> = raw_str
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let first_mes = data_raw_value
        .as_ref()
        .and_then(|d| d.get("first_mes"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let alternate_greetings = data_raw_value
        .as_ref()
        .and_then(extract_alternate_greetings);
    let regex_scripts = data_raw_value
        .as_ref()
        .map(crate::parsing::regex_script::extract_regex_scripts)
        .filter(|v| !v.is_empty());
    Ok(CharacterRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        chara_name: row.get(2)?,
        description: row.get(3)?,
        file_path: row.get(4)?,
        avatar_path: row.get(5)?,
        data_raw: if with_data_raw { data_raw_value } else { None },
        first_mes,
        alternate_greetings,
        regex_scripts,
        card_plugins: None,
        created_at: row.get(7)?,
    })
}

impl CharacterService {
    pub fn new(db: Arc<Db>, data_dir: PathBuf) -> Self {
        std::fs::create_dir_all(data_dir.join("characters")).ok();
        std::fs::create_dir_all(data_dir.join("avatars")).ok();
        CharacterService { db, data_dir }
    }

    /// 列表不含 data_raw,按 created_at DESC
    pub fn list(&self) -> Vec<CharacterRecord> {
        let conn = self.db.conn();
        let mut stmt = conn
            .prepare("SELECT id, name, chara_name, description, file_path, avatar_path, data_raw, created_at FROM characters ORDER BY created_at DESC")
            .unwrap();
        stmt.query_map([], |row| row_to_character(row, false))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<CharacterRecord> {
        let conn = self.db.conn();
        conn.query_row(
            "SELECT id, name, chara_name, description, file_path, avatar_path, data_raw, created_at FROM characters WHERE id = ?1",
            params![id],
            |row| row_to_character(row, true),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 启动时注入默认「系统助手」角色:无提示词(description/first_mes 为空),作为通用助手。
    /// 幂等:内置 id 已存在则跳过;删除后下次启动会重新补回(属默认角色)。
    pub fn seed_default_character(&self) {
        if self.get(BUILTIN_SYSTEM_ID).is_some() {
            // 幂等重刷存量 V3 卡:把 data 子对象中的空顶层字段补到顶层
            self.reflatten_v3_cards();
            return;
        }
        let data_raw = serde_json::json!({
            "spec": "chara_card_v2",
            "spec_version": "1.0",
            "name": "系统助手",
            "description": "",
            "first_mes": "",
        });
        let created_at = now_iso();
        let file_path = self.data_dir.join("characters").join("builtin-system.json");
        let _ = std::fs::write(
            &file_path,
            serde_json::to_vec_pretty(&data_raw).unwrap_or_default(),
        );
        let data_raw_str = serde_json::to_string(&data_raw).unwrap_or_else(|_| "{}".into());
        let _ = self.db.conn().execute(
            "INSERT INTO characters (id, name, chara_name, description, file_path, avatar_path, data_raw, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                BUILTIN_SYSTEM_ID,
                "system",
                "系统助手",
                "",
                file_path.to_string_lossy().to_string(),
                Option::<String>::None,
                data_raw_str,
                created_at
            ],
        );
    }

    /// 幂等迁移存量 V3 角色卡:V3 卡正文位于 data 子对象,旧版本导入时顶层 V2 字段为空,
    /// 导致人设(description/personality/scenario 等)未注入提示词。此处把空顶层字段
    /// 从 data 子对象补全并同步 description/chara_name 列。二次运行:顶层已补全 → 无变更。
    pub fn reflatten_v3_cards(&self) {
        use crate::parsing::character_card::flatten_v3_data;
        let conn = self.db.conn();
        let mut ids = Vec::new();
        if let Ok(mut stmt) = conn.prepare("SELECT id FROM characters") {
            if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
                for r in rows {
                    if let Ok(id) = r {
                        ids.push(id);
                    }
                }
            }
        }
        for id in ids {
            let raw_str: Option<String> = conn
                .query_row(
                    "SELECT data_raw FROM characters WHERE id = ?1",
                    params![&id],
                    |row| row.get(0),
                )
                .optional()
                .ok()
                .flatten();
            let Some(raw_str) = raw_str else { continue };
            let Some(raw) = serde_json::from_str::<Value>(&raw_str).ok() else {
                continue;
            };
            // 非 V3 或已补全 → 无需写库
            let is_v3 = matches!(
                raw.get("spec").and_then(|s| s.as_str()),
                Some("chara_card_v3")
            );
            if !is_v3 {
                continue;
            }
            let fixed = flatten_v3_data(raw.clone());
            if fixed == raw {
                continue;
            }
            let new_raw_str = serde_json::to_string(&fixed).unwrap_or_else(|_| "{}".into());
            // description 列:补全后顶层优先(与 upload 行为一致)
            let description = fixed
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let chara_name = fixed
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let _ = conn.execute(
                "UPDATE characters SET data_raw = ?1, description = ?2, chara_name = ?3 WHERE id = ?4",
                params![new_raw_str, description, chara_name, id],
            );
        }
    }

    pub fn upload(&self, buffer: &[u8], original_name: &str) -> Result<CharacterRecord, String> {
        let mut parsed = parse_character_card(buffer, original_name).map_err(|e| e.to_string())?;
        // 上传时自动规范化角色卡内嵌世界书(character_book.entries):
        // 修正关键词字段变体/逗号拆分、常态激发自动判定、属性默认自动,其余字段不动
        crate::parsing::world_book_convert::convert_world_book(&mut parsed.data);
        // 扩展名:优先原文件扩展名小写;无则按魔数推断
        let ext = detect_ext(original_name, buffer);
        let id = Uuid::new_v4().to_string();
        let safe = safe_file_name(original_name);
        let name = strip_ext(&safe);
        let file_name = format!("{id}{ext}");
        let chara_dir = self.data_dir.join("characters");
        let file_path = chara_dir.join(&file_name);
        let avatar_path = if let Some(avatar) = &parsed.avatar {
            let av_file = format!("{id}.png");
            let av_path = self.data_dir.join("avatars").join(&av_file);
            std::fs::write(&av_path, avatar).map_err(|e| format!("写头像失败: {e}"))?;
            Some(av_path.to_string_lossy().to_string())
        } else {
            None
        };
        std::fs::write(&file_path, buffer).map_err(|e| format!("写角色文件失败: {e}"))?;

        let chara_name = parsed
            .data
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| parsed.name.clone());
        let description = parsed
            .data
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let data_raw = serde_json::to_string(&parsed.data).unwrap_or_else(|_| "{}".into());
        let created_at = now_iso();

        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO characters (id, name, chara_name, description, file_path, avatar_path, data_raw, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                name,
                chara_name,
                description,
                file_path.to_string_lossy(),
                avatar_path.as_deref(),
                data_raw,
                created_at
            ],
        )
        .map_err(|e| format!("写数据库失败: {e}"))?;

        let regex_scripts = crate::parsing::regex_script::extract_regex_scripts(&parsed.data);
        let regex_scripts = if regex_scripts.is_empty() {
            None
        } else {
            Some(regex_scripts)
        };
        let first_mes = parsed
            .data
            .get("first_mes")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let alternate_greetings = extract_alternate_greetings(&parsed.data);
        Ok(CharacterRecord {
            id,
            name,
            chara_name,
            description,
            file_path: file_path.to_string_lossy().to_string(),
            avatar_path,
            data_raw: Some(parsed.data),
            first_mes,
            alternate_greetings,
            regex_scripts,
            card_plugins: None,
            created_at,
        })
    }

    /// 更新 chara_name / description / first_mes / alternate_greetings,同步写回 data_raw,保留其余
    pub fn update(
        &self,
        id: &str,
        chara_name: Option<&str>,
        description: Option<&str>,
        first_mes: Option<&str>,
        alternate_greetings: Option<&[String]>,
    ) -> Option<CharacterRecord> {
        let existing = self.get(id)?;
        let mut data_raw: Value = existing
            .data_raw
            .unwrap_or_else(|| Value::Object(Default::default()));
        if let Some(obj) = data_raw.as_object_mut() {
            if let Some(n) = chara_name {
                obj.insert("name".into(), Value::String(n.to_string()));
            }
            if let Some(d) = description {
                obj.insert("description".into(), Value::String(d.to_string()));
            }
            if let Some(f) = first_mes {
                // 空串表示清空开场白
                obj.insert("first_mes".into(), Value::String(f.to_string()));
            }
            if let Some(list) = alternate_greetings {
                // 空数组表示清空备用开场
                let arr: Vec<Value> = list
                    .iter()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| Value::String(s.to_string()))
                    .collect();
                if arr.is_empty() {
                    obj.remove("alternate_greetings");
                } else {
                    obj.insert("alternate_greetings".into(), Value::Array(arr));
                }
            }
        }
        let new_name = chara_name.unwrap_or(&existing.chara_name).to_string();
        let new_desc = description.unwrap_or(&existing.description).to_string();
        // 返回记录的开场白:有更新则用新值,否则沿用旧的
        let new_first_mes = match first_mes {
            Some(f) if !f.is_empty() => Some(f.to_string()),
            Some(_) => None,
            None => existing.first_mes.clone(),
        };
        // 返回记录的备用开场:有更新则用新值(去空后),否则沿用旧的
        let new_alternate_greetings = match alternate_greetings {
            Some(list) => {
                let v: Vec<String> = list
                    .iter()
                    .filter(|s| !s.trim().is_empty())
                    .cloned()
                    .collect();
                if v.is_empty() {
                    None
                } else {
                    Some(v)
                }
            }
            None => existing.alternate_greetings.clone(),
        };
        let data_raw_str = serde_json::to_string(&data_raw).unwrap_or_else(|_| "{}".into());
        // 注意:conn(MutexGuard)必须在再次调用 self.get 之前释放,避免 Mutex 重入死锁
        {
            let conn = self.db.conn();
            let n = conn
                .execute(
                    "UPDATE characters SET chara_name = ?1, description = ?2, data_raw = ?3 WHERE id = ?4",
                    params![new_name, new_desc, data_raw_str, id],
                )
                .ok()?;
            if n == 0 {
                return None;
            }
        }
        self.get(id).map(|mut c| {
            // get 返回的 first_mes 从新 data_raw 提取,已一致;仅兜底
            if let Some(f) = new_first_mes {
                if c.first_mes.is_none() {
                    c.first_mes = Some(f);
                }
            }
            if let Some(g) = new_alternate_greetings {
                if c.alternate_greetings.is_none() {
                    c.alternate_greetings = Some(g);
                }
            }
            c
        })
    }

    /// 写入/移除角色卡内嵌契约(extensions.nlkaleido),其余 data_raw 字段保留。
    /// 契约按调用方给定的 JSON 原样存储(保留作者自定义字段);
    /// 结构校验由调用方(契约 API 的 parse_contract)前置完成,此处只管落库。
    pub fn set_embedded_contract(&self, id: &str, contract: Option<&Value>) -> Option<()> {
        let existing = self.get(id)?;
        let mut data_raw: Value = existing
            .data_raw
            .unwrap_or_else(|| Value::Object(Default::default()));
        let obj = data_raw.as_object_mut()?;
        match contract {
            Some(c) => {
                let ext = obj
                    .entry("extensions")
                    .or_insert_with(|| Value::Object(Default::default()));
                ext.as_object_mut()?.insert("nlkaleido".into(), c.clone());
            }
            None => {
                if let Some(ext) = obj.get_mut("extensions") {
                    if let Some(ext_obj) = ext.as_object_mut() {
                        ext_obj.remove("nlkaleido");
                    }
                }
            }
        }
        let data_raw_str = serde_json::to_string(&data_raw).unwrap_or_else(|_| "{}".into());
        let conn = self.db.conn();
        let n = conn
            .execute(
                "UPDATE characters SET data_raw = ?1 WHERE id = ?2",
                params![data_raw_str, id],
            )
            .ok()?;
        if n == 0 {
            None
        } else {
            Some(())
        }
    }

    /// 删除:删库记录 + 删原文件与头像文件,级联删会话/消息
    pub fn delete(&self, id: &str) -> bool {
        let Some(rec) = self.get(id) else {
            return false;
        };
        let conn = self.db.conn();
        if conn
            .execute("DELETE FROM characters WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(Path::new(&rec.file_path));
            if let Some(av) = &rec.avatar_path {
                let _ = std::fs::remove_file(Path::new(av));
            }
            true
        } else {
            false
        }
    }
}

/// 扩展名:原文件扩展名小写;空则按魔数(0x89 → .png,否则 .json)
fn detect_ext(original_name: &str, buffer: &[u8]) -> String {
    if let Some(pos) = original_name.rfind('.') {
        let ext = original_name[pos..].to_lowercase();
        if ext.len() > 1 && ext.len() <= 8 && !ext.contains(' ') {
            return ext;
        }
    }
    if buffer.first() == Some(&0x89) {
        ".png".to_string()
    } else {
        ".json".to_string()
    }
}

fn strip_ext(name: &str) -> String {
    if let Some(pos) = name.rfind('.') {
        name[..pos].to_string()
    } else {
        name.to_string()
    }
}
