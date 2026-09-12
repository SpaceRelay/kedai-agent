// 用户脚本(ScriptTree)服务:阶段三数据层。
// 两种存储:
//   - 全局(scope=global):SQLite user_scripts 表(owner_id=''),全量 JSON 保存
//   - 角色(scope=character):角色卡 data_raw.extensions.tavern_helper(规范存储,随卡导出),
//     V2 顶层 / V3 data.extensions 双位置定位;兼容旧字段 TavernHelper_scripts 首次读取迁移。
// 数据结构对齐 JS-Slash-Runner「酒馆助手」ScriptTree:顶层为数组,元素为
// script(含 content/data/button,可执行体)或 folder(含 scripts 子数组,可嵌套一层)。
use crate::models::db::{now_iso, Db};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct UserScriptService {
    db: Arc<Db>,
}

impl UserScriptService {
    pub fn new(db: Arc<Db>) -> Self {
        UserScriptService { db }
    }

    /// 读取脚本树:scope=global|character;角色级自动迁移旧字段并写回。
    pub fn get_tree(&self, scope: &str, owner_id: &str) -> Result<Value, String> {
        match scope {
            "global" => self.get_global(owner_id),
            "character" => self.get_character(owner_id),
            other => Err(format!("未知脚本作用域: {other}(仅支持 global/character)")),
        }
    }

    /// 保存脚本树(全量覆盖;先校验再补默认字段)
    pub fn save_tree(&self, scope: &str, owner_id: &str, trees: &Value) -> Result<(), String> {
        validate_trees(trees)?;
        let trees = ensure_defaults(trees);
        match scope {
            "global" => self.save_global(owner_id, &trees),
            "character" => self.save_character(owner_id, &trees),
            other => Err(format!("未知脚本作用域: {other}(仅支持 global/character)")),
        }
    }

    fn get_global(&self, owner_id: &str) -> Result<Value, String> {
        let conn = self.db.read()?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT data_json FROM user_scripts WHERE scope = 'global' AND owner_id = ?1",
                params![owner_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("读取全局脚本失败: {e}"))?;
        match raw {
            Some(raw) => serde_json::from_str(&raw).map_err(|e| format!("解析全局脚本失败: {e}")),
            None => Ok(Value::Array(Vec::new())),
        }
    }

    fn save_global(&self, owner_id: &str, trees: &Value) -> Result<(), String> {
        let raw = serde_json::to_string(trees).map_err(|e| format!("序列化脚本失败: {e}"))?;
        let now = now_iso();
        let conn = self.db.write();
        conn.execute(
            "INSERT INTO user_scripts (id, scope, owner_id, data_json, updated_at)
             VALUES (?1, 'global', ?2, ?3, ?4)
             ON CONFLICT(scope, owner_id) DO UPDATE SET data_json = ?3, updated_at = ?4",
            params![uuid::Uuid::new_v4().to_string(), owner_id, raw, now],
        )
        .map_err(|e| format!("写入全局脚本失败: {e}"))?;
        Ok(())
    }

    fn read_character_raw(&self, character_id: &str) -> Result<Value, String> {
        let conn = self.db.read()?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT data_raw FROM characters WHERE id = ?1",
                params![character_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("读取角色卡失败: {e}"))?;
        let raw = raw.ok_or_else(|| "角色不存在".to_string())?;
        serde_json::from_str(&raw).map_err(|e| format!("解析角色卡失败: {e}"))
    }

    fn write_character_raw(&self, character_id: &str, raw: &Value) -> Result<(), String> {
        let raw_str = serde_json::to_string(raw).map_err(|e| format!("序列化角色卡失败: {e}"))?;
        // conn 作用域收窄:UPDATE 语句执行后立即释放锁(与 quick_reply_service 一致)
        {
            let conn = self.db.write();
            conn.execute(
                "UPDATE characters SET data_raw = ?1 WHERE id = ?2",
                params![raw_str, character_id],
            )
            .map_err(|e| format!("回写角色卡失败: {e}"))?;
        }
        Ok(())
    }

    /// 读取角色卡脚本树;首次读取自动迁移旧字段并落库(引擎 3b-3 也经此读取)。
    pub fn get_character(&self, character_id: &str) -> Result<Value, String> {
        let mut raw = self.read_character_raw(character_id)?;
        // 首次读取时迁移旧字段并落库,之后读取不再重复迁移
        if migrate_legacy_scripts(&mut raw) {
            self.write_character_raw(character_id, &raw)?;
        }
        Ok(read_tavern_helper(&raw))
    }

    fn save_character(&self, character_id: &str, trees: &Value) -> Result<(), String> {
        let mut raw = self.read_character_raw(character_id)?;
        // 先迁移旧字段再写新值,避免新值落库后旧字段残留
        migrate_legacy_scripts(&mut raw);
        write_tavern_helper(&mut raw, trees);
        self.write_character_raw(character_id, &raw)
    }
}

/// 读取 tavern_helper:优先 V2 顶层 extensions.tavern_helper,再 V3 data.extensions.tavern_helper;
/// 均无(或非数组) → 空数组。
fn read_tavern_helper(raw: &Value) -> Value {
    if let Some(v) = raw
        .get("extensions")
        .and_then(|e| e.get("tavern_helper"))
        .filter(|v| v.is_array())
    {
        return v.clone();
    }
    if let Some(v) = raw
        .get("data")
        .and_then(|d| d.get("extensions"))
        .and_then(|e| e.get("tavern_helper"))
        .filter(|v| v.is_array())
    {
        return v.clone();
    }
    Value::Array(Vec::new())
}

/// 写回 tavern_helper:顶层 extensions 存在(含已提升的 V3)→ 写顶层;
/// 否则定位 V3 data.extensions(不存在则创建 data → extensions)。
fn write_tavern_helper(raw: &mut Value, trees: &Value) {
    if raw.get("extensions").is_some() {
        if let Some(ext) = raw.get_mut("extensions").and_then(|e| e.as_object_mut()) {
            ext.insert("tavern_helper".into(), trees.clone());
        }
        return;
    }
    let Some(data_obj) = raw
        .as_object_mut()
        .map(|o| o.entry("data").or_insert_with(|| json!({})))
    else {
        return;
    };
    if let Some(obj) = data_obj.as_object_mut() {
        let ext = obj.entry("extensions").or_insert_with(|| json!({}));
        if let Some(ext_obj) = ext.as_object_mut() {
            ext_obj.insert("tavern_helper".into(), trees.clone());
        }
    }
}

/// 迁移旧字段 extensions.TavernHelper_scripts → tavern_helper(数组)。
/// 旧脚本无 type 字段时按「含 scripts 子字段视为 folder,否则视为 script」补判;
/// 已有 tavern_helper 则并入。返回是否发生迁移。
/// TavernHelper_characterScriptVariables 是角色级变量对象,ScriptTree 数组内无安放点,
/// 保留原字段供阶段三执行层(3b)变量桥接读取,不在本层删除。
fn migrate_legacy_scripts(raw: &mut Value) -> bool {
    let Some(legacy) = raw
        .get("extensions")
        .and_then(|e| e.get("TavernHelper_scripts"))
        .and_then(|v| v.as_array())
    else {
        return false;
    };
    let mut merged = read_tavern_helper(raw);
    if !merged.is_array() {
        merged = Value::Array(Vec::new());
    }
    let arr = merged.as_array_mut().expect("刚设为数组");
    for node in legacy {
        let Some(obj) = node.as_object() else {
            continue;
        };
        let mut n = obj.clone();
        if n.get("type").is_none() {
            n.insert(
                "type".into(),
                if n.contains_key("scripts") {
                    json!("folder")
                } else {
                    json!("script")
                },
            );
        }
        arr.push(Value::Object(n));
    }
    write_tavern_helper(raw, &merged);
    if let Some(ext) = raw.get_mut("extensions").and_then(|e| e.as_object_mut()) {
        ext.remove("TavernHelper_scripts");
    }
    true
}

/// 校验脚本树:顶层须为数组,元素须为对象且 type ∈ {script, folder}。
fn validate_trees(trees: &Value) -> Result<(), String> {
    let arr = trees.as_array().ok_or("脚本树顶层须为数组")?;
    for node in arr {
        let obj = node.as_object().ok_or("脚本节点须为对象")?;
        match obj.get("type").and_then(|t| t.as_str()) {
            Some("script") | Some("folder") => {}
            Some(other) => return Err(format!("未知脚本节点类型: {other}")),
            None => return Err("脚本节点缺少 type 字段".into()),
        }
    }
    Ok(())
}

/// 补默认字段,保证存储结构稳定(缺失字段不影响运行,但编辑/导出需可预期)。
fn ensure_defaults(trees: &Value) -> Value {
    let mut out = trees.clone();
    if let Some(arr) = out.as_array_mut() {
        for node in arr.iter_mut() {
            let Some(obj) = node.as_object_mut() else {
                continue;
            };
            match obj.get("type").and_then(|t| t.as_str()) {
                Some("script") => {
                    obj.entry("enabled").or_insert(json!(true));
                    obj.entry("name").or_insert(json!(""));
                    obj.entry("content").or_insert(json!(""));
                    obj.entry("info").or_insert(json!(""));
                    obj.entry("data").or_insert(json!({}));
                    obj.entry("button")
                        .or_insert(json!({ "enabled": true, "buttons": [] }));
                    obj.entry("export_with")
                        .or_insert(json!({ "data": true, "button": true }));
                }
                Some("folder") => {
                    obj.entry("enabled").or_insert(json!(true));
                    obj.entry("name").or_insert(json!(""));
                    obj.entry("icon").or_insert(json!("fa-solid fa-folder"));
                    obj.entry("color").or_insert(json!(""));
                    obj.entry("scripts").or_insert(json!([]));
                }
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2_card_with_trees(trees: Value) -> Value {
        json!({
            "spec": "chara_card_v2", "spec_version": "1.0", "name": "测试",
            "description": "测试", "first_mes": "你好",
            "extensions": { "tavern_helper": trees }
        })
    }

    #[test]
    fn read_v2_top_level_extensions() {
        let card = v2_card_with_trees(json!([
            { "type": "script", "enabled": true, "name": "a", "content": "console.log(1)" }
        ]));
        assert_eq!(read_tavern_helper(&card).as_array().unwrap().len(), 1);
    }

    #[test]
    fn read_v3_data_subobject() {
        let card = json!({
            "spec": "chara_card_v3",
            "data": {
                "name": "测试",
                "extensions": { "tavern_helper": [ { "type": "script", "name": "b" } ] }
            }
        });
        assert_eq!(read_tavern_helper(&card).as_array().unwrap().len(), 1);
    }

    #[test]
    fn read_missing_returns_empty() {
        let card = json!({ "spec": "chara_card_v2", "name": "x" });
        assert_eq!(read_tavern_helper(&card), json!([]));
    }

    #[test]
    fn write_v2_top_level() {
        let mut card = json!({ "extensions": {} });
        write_tavern_helper(&mut card, &json!([{ "type": "script", "name": "s" }]));
        assert_eq!(
            card["extensions"]["tavern_helper"],
            json!([{ "type": "script", "name": "s" }])
        );
    }

    #[test]
    fn write_v3_creates_data_extensions() {
        let mut card = json!({ "name": "x" });
        write_tavern_helper(&mut card, &json!([{ "type": "script" }]));
        assert_eq!(
            card["data"]["extensions"]["tavern_helper"],
            json!([{ "type": "script" }])
        );
    }

    #[test]
    fn migrate_legacy_without_type() {
        let mut card = json!({
            "extensions": {
                "TavernHelper_scripts": [
                    { "name": "old", "content": "x" },
                    { "name": "dir", "scripts": [{ "name": "inner" }] }
                ]
            }
        });
        assert!(migrate_legacy_scripts(&mut card));
        // 旧字段删除
        assert!(card["extensions"].get("TavernHelper_scripts").is_none());
        // 无 type 的按 scripts 子字段判 folder,否则 script
        let trees = &card["extensions"]["tavern_helper"];
        assert_eq!(trees[0]["type"], json!("script"));
        assert_eq!(trees[1]["type"], json!("folder"));
        assert_eq!(trees[1]["scripts"][0]["name"], json!("inner"));
        // 幂等:再次迁移无事发生
        assert!(!migrate_legacy_scripts(&mut card));
    }

    #[test]
    fn migrate_legacy_merges_existing() {
        let mut card = json!({
            "extensions": {
                "tavern_helper": [ { "type": "script", "name": "new" } ],
                "TavernHelper_scripts": [ { "type": "script", "name": "old" } ]
            }
        });
        assert!(migrate_legacy_scripts(&mut card));
        let trees = &card["extensions"]["tavern_helper"];
        assert_eq!(trees.as_array().unwrap().len(), 2, "新旧脚本应合并");
    }

    #[test]
    fn validate_rejects_bad_input() {
        assert!(validate_trees(&json!({ "not": "array" })).is_err());
        assert!(validate_trees(&json!([{ "type": "macro" }])).is_err());
        assert!(validate_trees(&json!([{ "name": "no-type" }])).is_err());
        assert!(validate_trees(&json!([{ "type": "script" }])).is_ok());
    }

    #[test]
    fn defaults_fill_script_fields() {
        let out = ensure_defaults(&json!([{ "type": "script" }]));
        let s = &out[0];
        assert_eq!(s["enabled"], json!(true));
        assert_eq!(s["content"], json!(""));
        assert_eq!(s["data"], json!({}));
        assert_eq!(s["button"]["enabled"], json!(true));
    }
}
