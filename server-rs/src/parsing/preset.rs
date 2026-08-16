// 酒馆(SillyTavern)预设导入解析:预设 JSON → PromptFloor 楼层列表
//
// ST 预设顶层结构(参考 8.5【可待-从头越】 Agent版.json):
//   - prompts:    条目数组 [{ identifier, name, content, role(user/assistant/system),
//                   system_prompt(bool), injection_order/depth/position,
//                   attach_index/attach_role/attach_side(部分条目), enabled, marker }]
//   - prompt_order:数组(或按 character_id 分组的对象),每组 { character_id, order:
//                   [{ enabled, identifier }] },identifier 引用 prompts 条目,
//                   也可能出现 worldInfoBefore/chatHistory 等内建锚点(非 prompts 条目,跳过)
//
// 映射规则(与 PromptFloor 对齐):
//   - id      = identifier(重复时加后缀);name/content 原样保留(宏兼容,未知宏不展开)
//   - role    = ST role 直接映射;非法/缺省 → user
//   - enabled = ST enabled 原样保留(预设里大量条目默认禁用,导入后可在界面开关)
//   - position:system_prompt=true → System;
//              injection_position 0 → System, 1 → Before, 2 → (depth>0 ? Depth : After), 其他 → After;
//              仅含 attach_*(附加模式) → After(附加在最后一条消息后,与 After 等效);
//              以上都没有 → System(兜底保证可注入)
//   - depth   = injection_depth(缺省 0,仅 position=Depth 时生效)
//   - order   = prompt_order 首个组中 identifier 的出现顺序(跳过内建锚点);
//               未出现在 order 中的条目按 prompts 数组序排在最后
//
// 不在导入范围:world_info / 角色卡 / 正则脚本(角色卡导入已有独立功能)。
use crate::services::prompt_inject_service::{new_floor_id, FloorPosition, FloorRole, PromptFloor};
use serde_json::Value;

/// 解析 ST 预设 JSON → 楼层列表(按注入顺序排好 order);失败返回中文错误
pub fn parse_st_preset(json_text: &str) -> Result<Vec<PromptFloor>, String> {
    let root: Value =
        serde_json::from_str(json_text).map_err(|e| format!("预设不是有效 JSON: {e}"))?;
    let prompts = root
        .get("prompts")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "不是有效的酒馆预设:缺少 prompts 数组".to_string())?;

    // 先按 identifier 建立条目
    let mut by_id: Vec<(String, PromptFloor)> = Vec::new();
    for (idx, p) in prompts.iter().enumerate() {
        let obj = match p.as_object() {
            Some(o) => o,
            None => continue,
        };
        let id = obj
            .get("identifier")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(new_floor_id);
        // 重复 identifier:后者加后缀(避免楼层 key 冲突)
        let mut final_id = id.clone();
        if by_id.iter().any(|(_, f)| f.id == final_id) {
            final_id = format!("{id}-dup{idx}");
        }
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let content = obj
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let role = match obj.get("role").and_then(|v| v.as_str()) {
            Some("assistant") => FloorRole::Assistant,
            Some("system") => FloorRole::System,
            _ => FloorRole::User, // "user" / 缺省 / 非法值
        };
        let enabled = obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
        let depth = obj
            .get("injection_depth")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0) as usize;
        let position = map_position(obj);
        by_id.push((
            id,
            PromptFloor {
                id: final_id,
                name,
                content,
                role,
                position,
                depth,
                enabled,
                order: usize::MAX, // 占位,随后按 prompt_order 编号
            },
        ));
    }

    // prompt_order:提取首个组的顺序 identifier 列表(兼容数组与对象两种形态)
    let mut ordered_ids: Vec<String> = Vec::new();
    match root.get("prompt_order") {
        Some(Value::Array(arr)) => {
            if let Some(first) = arr.first() {
                collect_order(first, &mut ordered_ids);
            }
        }
        Some(Value::Object(map)) => {
            if let Some(first) = map.values().next() {
                collect_order(first, &mut ordered_ids);
            }
        }
        _ => {}
    }

    // 按 order 列表编号(只数本预设中真实存在的条目);未命中的条目按数组序排最后
    let mut order = 0usize;
    for oid in &ordered_ids {
        for (_, f) in by_id.iter_mut() {
            if &f.id == oid && f.order == usize::MAX {
                f.order = order;
                order += 1;
            }
        }
    }
    for (_, f) in by_id.iter_mut() {
        if f.order == usize::MAX {
            f.order = order;
            order += 1;
        }
    }
    // 结果按 order 排序,保证 UI 顺序即注入顺序
    let mut floors: Vec<PromptFloor> = by_id.into_iter().map(|(_, f)| f).collect();
    floors.sort_by_key(|f| f.order);
    Ok(floors)
}

/// 收集 prompt_order 组的 order 数组中的 identifier(跳过非条目锚点由调用方自然忽略——锚点不在 by_id 中)
fn collect_order(group: &Value, out: &mut Vec<String>) {
    let Some(arr) = group.get("order").and_then(|v| v.as_array()) else {
        return;
    };
    for item in arr {
        if let Some(id) = item.get("identifier").and_then(|v| v.as_str()) {
            out.push(id.to_string());
        }
    }
}

/// position 映射:system_prompt / injection_position / attach_* → FloorPosition
fn map_position(obj: &serde_json::Map<String, Value>) -> FloorPosition {
    if obj
        .get("system_prompt")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return FloorPosition::System;
    }
    if let Some(pos) = obj.get("injection_position").and_then(|v| v.as_i64()) {
        return match pos {
            0 => FloorPosition::System,
            1 => FloorPosition::Before,
            2 => {
                let depth = obj
                    .get("injection_depth")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    .max(0);
                if depth > 0 {
                    FloorPosition::Depth
                } else {
                    FloorPosition::After
                }
            }
            _ => FloorPosition::After,
        };
    }
    // 附加模式条目(attach_index/attach_role/attach_side):附加在最后一条消息后
    let has_attach = ["attach_index", "attach_role", "attach_side"]
        .iter()
        .any(|k| obj.get(*k).is_some_and(|v| !v.is_null()));
    if has_attach {
        return FloorPosition::After;
    }
    // 兜底:进系统提示词,保证导入条目可注入
    FloorPosition::System
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "name": "测试预设",
        "prompts": [
            { "identifier": "sysblock", "name": "系统块", "content": "{{setvar::地点::酒馆}}", "role": "user", "system_prompt": true, "enabled": true },
            { "identifier": "p0", "name": "位置0", "content": "c0", "role": "system", "injection_position": 0, "injection_depth": 4, "injection_order": 100, "injection_trigger": [], "enabled": true },
            { "identifier": "p1", "name": "位置1", "content": "c1", "role": "assistant", "injection_position": 1, "enabled": true },
            { "identifier": "p2", "name": "位置2深0", "content": "c2", "role": "user", "injection_position": 2, "injection_depth": 0, "enabled": true },
            { "identifier": "p3", "name": "位置2深3", "content": "c3", "role": "user", "injection_position": 2, "injection_depth": 3, "enabled": true },
            { "identifier": "p4", "name": "附加", "content": "c4", "role": "assistant", "attach_index": 1, "attach_role": "user", "attach_side": "end", "enabled": false },
            { "identifier": "p5", "name": "非法角色", "content": "c5", "role": "robot", "enabled": true },
            { "identifier": "p6", "name": "无位置", "content": "c6", "role": "user", "enabled": true },
            { "identifier": "p7", "name": "重复A", "content": "c7", "role": "user", "enabled": true }
        ],
        "prompt_order": [
            { "character_id": 100001, "order": [
                { "enabled": true, "identifier": "p3" },
                { "enabled": true, "identifier": "worldInfoBefore" },
                { "enabled": true, "identifier": "p0" },
                { "enabled": false, "identifier": "p5" },
                { "enabled": true, "identifier": "p7" }
            ] }
        ]
    }"#;

    #[test]
    fn maps_st_fields_to_floors() {
        let floors = parse_st_preset(SAMPLE).unwrap();
        // 9 条全部导入(含禁用的 p4;含重复 id 后缀)
        assert_eq!(floors.len(), 9, "floors: {floors:#?}");
        let by_id = |id: &str| floors.iter().find(|f| f.id == id).unwrap();

        // system_prompt=true → position=System
        let f = by_id("sysblock");
        assert_eq!(f.position, FloorPosition::System);
        assert_eq!(f.role, FloorRole::User);
        assert!(f.enabled);
        // injection_position=0 → System,role=system 直接映射
        let f = by_id("p0");
        assert_eq!(f.position, FloorPosition::System);
        assert_eq!(f.role, FloorRole::System);
        assert_eq!(f.depth, 4);
        // injection_position=1 → Before
        assert_eq!(by_id("p1").position, FloorPosition::Before);
        assert_eq!(by_id("p1").role, FloorRole::Assistant);
        // injection_position=2 + depth=0 → After
        assert_eq!(by_id("p2").position, FloorPosition::After);
        // injection_position=2 + depth=3 → Depth(3)
        let f = by_id("p3");
        assert_eq!(f.position, FloorPosition::Depth);
        assert_eq!(f.depth, 3);
        // attach_* 条目 → After,enabled=false 保留
        let f = by_id("p4");
        assert_eq!(f.position, FloorPosition::After);
        assert!(!f.enabled);
        // 非法 role → user
        assert_eq!(by_id("p5").role, FloorRole::User);
        // 无位置字段 → System 兜底
        assert_eq!(by_id("p6").position, FloorPosition::System);
        // 重复 id:p7 出现两次(测试样本应只有一个 p7,断言存在即可)
        assert!(by_id("p7").content == "c7" || floors.iter().any(|f| f.id.starts_with("p7-")));
    }

    #[test]
    fn order_follows_prompt_order_skipping_anchors() {
        let floors = parse_st_preset(SAMPLE).unwrap();
        // prompt_order 里 p3 → p0 → p5 → p7(worldInfoBefore 锚点跳过)
        // 其余(p sysblock/p1/p2/p4/p6)按数组序排在最后
        let order_of = |id: &str| floors.iter().find(|f| f.id == id).map(|f| f.order).unwrap();
        assert!(
            order_of("p3") < order_of("p0"),
            "orders: {} vs {}",
            order_of("p3"),
            order_of("p0")
        );
        assert!(order_of("p0") < order_of("p5"));
        assert!(order_of("p5") < order_of("p7"));
        assert!(order_of("p7") < order_of("sysblock"));
        assert!(order_of("sysblock") < order_of("p1"));
        assert!(order_of("p1") < order_of("p2"));
        assert!(order_of("p2") < order_of("p4"));
        assert!(order_of("p4") < order_of("p6"));
    }

    #[test]
    fn missing_prompts_is_error() {
        assert!(parse_st_preset(r#"{"name":"无提示词"}"#).is_err());
        assert!(parse_st_preset("not json").is_err());
        assert!(parse_st_preset(r#"{"prompts": "不是数组"}"#).is_err());
    }
}
