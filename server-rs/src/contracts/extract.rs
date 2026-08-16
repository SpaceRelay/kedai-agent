// 契约来源提取(§6.2-P5 契约内嵌解析):从角色卡 / 世界书提取契约。
//
// 纯函数,不依赖服务层;端到端接线在 engine 层(load_character_contract)组合两源。
// 提取失败(缺字段/解析失败/校验失败)一律返回 None,不 panic——契约是可选的
// 增强,存量卡无契约时走兼容层(文档 D-2)。
use serde_json::Value;

use super::Contract;

/// 从角色卡 data_raw 提取契约(优先 `extensions.nlkaleido`)。
///
/// 角色卡 V2/V3 解析后 extensions 已补到顶层(character_card.rs 归一化),
/// 兼容两种内嵌形态:
///   - `extensions.nlkaleido = <Contract 对象>`
///   - `extensions.nlkaleido = { "contract": <Contract 对象> }`(带包装)
pub fn extract_from_character_card(data_raw: &Value) -> Option<Contract> {
    raw_contract_from_character_card(data_raw).and_then(extract_contract_value)
}

/// 角色卡中内嵌契约的原始 JSON(包装形态已解包)。
/// 供契约编辑 API 读取作者原样存储(不做 parse/规范化),与提取逻辑共用解包规则。
pub fn raw_contract_from_character_card(data_raw: &Value) -> Option<&Value> {
    let raw = data_raw.get("extensions")?.get("nlkaleido")?;
    Some(unwrap_contract_value(raw))
}

/// 兼容包装形态解包:{ contract: {...} } 且自身无 version 字段 → 取内层。
fn unwrap_contract_value(raw: &Value) -> &Value {
    if raw.is_object()
        && raw.get("version").is_none()
        && raw.get("contract").is_some()
    {
        raw.get("contract").expect("已确认存在")
    } else {
        raw
    }
}

/// 从世界书条目提取契约:comment 含 `[nlkaleido_contract]` 的条目,内容为契约 JSON。
/// 大小写不敏感;取第一条命中(多条目时按条目顺序优先)。
pub fn extract_from_world_entries(entries: &[crate::parsing::world_book::WorldEntry]) -> Option<Contract> {
    entries
        .iter()
        .find(|e| e.comment.to_ascii_lowercase().contains("[nlkaleido_contract]"))
        .and_then(|e| serde_json::from_str::<Value>(&e.content).ok())
        .and_then(|v| extract_contract_value(&v))
}

/// 兼容包装形态的契约值提取。
fn extract_contract_value(raw: &Value) -> Option<Contract> {
    super::parse_contract(unwrap_contract_value(raw)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::field::{FieldType, UpdateMode};
    use serde_json::json;

    fn world_entry(comment: &str, content: &str) -> crate::parsing::world_book::WorldEntry {
        crate::parsing::world_book::WorldEntry {
            id: 0,
            comment: comment.to_string(),
            keys: vec![],
            keys_secondary: vec![],
            regex: None,
            use_regex: false,
            content: content.to_string(),
            constant: false,
            enabled: true,
            position: 0,
            depth: 4,
            order: 100,
            case_sensitive: false,
            sticky: 0,
            cooldown: 0,
            probability: 100,
            use_probability: false,
            role: None,
            decorators: Vec::new(),
        }
    }

    fn contract_json() -> Value {
        json!({
            "version": 1,
            "id": "card-contract",
            "schema": { "properties": {} },
            "updateRules": {
                "好感度": {
                    "path": "好感度", "type": "number", "updateMode": "every_turn",
                    "display": true
                }
            }
        })
    }

    /// 角色卡 extensions.nlkaleido 直接内嵌契约对象。
    #[test]
    fn extract_character_card_direct() {
        let card = json!({
            "name": "芽衣",
            "extensions": { "nlkaleido": contract_json() }
        });
        let c = extract_from_character_card(&card).expect("应提取");
        assert_eq!(c.id, "card-contract");
        assert!(c.update_rules.contains_key("好感度"));
        assert_eq!(c.update_rules["好感度"].kind, FieldType::Number);
        assert_eq!(c.update_rules["好感度"].update_mode, UpdateMode::EveryTurn);
    }

    /// 角色卡 extensions.nlkaleido 用 { contract: {...} } 包装。
    #[test]
    fn extract_character_card_wrapped() {
        let card = json!({
            "extensions": { "nlkaleido": { "contract": contract_json() } }
        });
        let c = extract_from_character_card(&card).expect("应提取包装形态");
        assert_eq!(c.id, "card-contract");
    }

    /// 角色卡无 extensions.nlkaleido → None(存量卡兼容)。
    #[test]
    fn extract_character_card_missing() {
        let card = json!({ "name": "普通卡", "extensions": { "tavern_helper": {} } });
        assert!(extract_from_character_card(&card).is_none());
        assert!(extract_from_character_card(&json!({})).is_none());
    }

    /// 世界书 [nlkaleido_contract] 条目(大小写不敏感)。
    #[test]
    fn extract_world_entries_tagged() {
        let entries = vec![
            world_entry("背景设定", "图书馆安静。"),
            world_entry("[NLKALEIDO_CONTRACT]", &contract_json().to_string()),
        ];
        let c = extract_from_world_entries(&entries).expect("应提取");
        assert_eq!(c.id, "card-contract");
    }

    /// 世界书无契约条目 → None。
    #[test]
    fn extract_world_entries_missing() {
        let entries = vec![world_entry("背景", "内容")];
        assert!(extract_from_world_entries(&entries).is_none());
        assert!(extract_from_world_entries(&[]).is_none());
    }

    /// 契约校验失败(缺必填字段)→ None,不 panic。
    #[test]
    fn extract_invalid_contract_returns_none() {
        let card = json!({ "extensions": { "nlkaleido": { "version": 1 } } });
        assert!(extract_from_character_card(&card).is_none());
    }
}
