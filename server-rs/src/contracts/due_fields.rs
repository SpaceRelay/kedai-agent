// 字段调度(§0.2 决策项「本轮哪些字段进入候选」+ §10.1 dueFields/classifyField)。
//
// due_fields 是「本地确定性调度」:到期判断一律本地计算,读 meta.last_turn_id。
// 它是缓存分池(L1/L2/L3)的决定性来源——任何策略改动必须 contractVersion+1(§4.5)。
use super::field::{FieldDef, UpdateMode};

/// 字段池分类(§10.1 classifyField):决定字段进哪层缓存。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldPool {
    /// 固定字段 → L1 前缀(STABLE_BATCH 引用 token)
    Static,
    /// 低频字段 → L2 前缀(未到期沿用值)
    LowFreq,
    /// 动态字段 → L3 尾部(每轮真实值)
    Dynamic,
}

/// 按 updateMode 分类字段池(与稳定性 stability 正交,§7-StabilityClass 注)。
pub fn classify_field(f: &FieldDef) -> FieldPool {
    match f.update_mode {
        UpdateMode::Fixed => FieldPool::Static,
        UpdateMode::EveryNTurns => FieldPool::LowFreq,
        // trigger 触发时机不定,归入动态尾部(默认不到期,由作者 Scheduler 覆盖)
        UpdateMode::EveryTurn | UpdateMode::Trigger => FieldPool::Dynamic,
    }
}

/// 本轮到期需刷新的字段路径(本地确定性,升序稳定输出)。
///
/// 到期规则:
///   - every_turn:每轮到期
///   - fixed:永不到期(由作者自定义规则维护)
///   - every_n_turns:turn_id % N == 0(第 N、2N、3N 轮触发)
///   - trigger:默认不到期,触发条件由外部 Scheduler(§18)决定
///
/// MVP 采用 `turn_id % N == 0` 的确定性语义;更精确的「距上次更新满 N 轮」需要
/// per-field lastUpdated 元数据,随 P3 changelog 落地。
pub fn due_fields(contract: &super::Contract, turn_id: u64) -> Vec<String> {
    contract
        .update_rules
        .iter()
        .filter(|(_, f)| is_due(f, turn_id))
        .map(|(path, _)| path.clone())
        .collect()
}

fn is_due(f: &FieldDef, turn_id: u64) -> bool {
    match f.update_mode {
        UpdateMode::EveryTurn => true,
        UpdateMode::Fixed => false,
        UpdateMode::EveryNTurns => {
            let n = f.every_n.unwrap_or(1) as u64;
            if n == 0 {
                false
            } else {
                turn_id.is_multiple_of(n)
            }
        }
        UpdateMode::Trigger => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::field::{FieldType, PersistScope, StabilityClass};
    use serde_json::json;

    fn field(path: &str, mode: UpdateMode, every_n: Option<u32>) -> FieldDef {
        FieldDef {
            path: path.to_string(),
            kind: FieldType::Number,
            default: None,
            update_mode: mode,
            every_n,
            dynamic: false,
            change_rule: None,
            cap: None,
            scope: None,
            ttl: None,
            persist: PersistScope::Chat,
            stability: StabilityClass::Volatile,
            display: true,
            dependencies: None,
            ownership: None,
        }
    }

    fn contract_with(fields: Vec<FieldDef>) -> super::super::Contract {
        let mut c: super::super::Contract = serde_json::from_value(json!({
            "version": 1, "id": "t", "schema": { "properties": {} },
            "updateRules": {}, "guardrails": {}
        }))
        .unwrap();
        c.update_rules = fields.into_iter().map(|f| (f.path.clone(), f)).collect();
        c
    }

    /// classifyField:fixed→static,every_n_turns→lowfreq,every_turn/trigger→dynamic。
    #[test]
    fn classify_field_maps_pools() {
        assert_eq!(
            classify_field(&field("a", UpdateMode::Fixed, None)),
            FieldPool::Static
        );
        assert_eq!(
            classify_field(&field("b", UpdateMode::EveryNTurns, Some(3))),
            FieldPool::LowFreq
        );
        assert_eq!(
            classify_field(&field("c", UpdateMode::EveryTurn, None)),
            FieldPool::Dynamic
        );
        assert_eq!(
            classify_field(&field("d", UpdateMode::Trigger, None)),
            FieldPool::Dynamic
        );
    }

    /// 到期判定:every_turn 每轮,fixed 永不到期,every_n 按 turn_id % N。
    #[test]
    fn due_fields_returns_due_paths() {
        let c = contract_with(vec![
            field("每轮", UpdateMode::EveryTurn, None),
            field("固定", UpdateMode::Fixed, None),
            field("低频", UpdateMode::EveryNTurns, Some(3)),
            field("触发", UpdateMode::Trigger, None),
        ]);

        // turn 3:every_turn 到期,every_n(3) 到期,fixed/trigger 不到期
        // (BTreeMap 按 path 码点升序:低 4F4E < 每 6BCF)
        let due = due_fields(&c, 3);
        assert_eq!(due, vec!["低频", "每轮"], "实际: {due:?}");

        // turn 2:every_n(3) 未到期
        let due2 = due_fields(&c, 2);
        assert_eq!(due2, vec!["每轮"], "实际: {due2:?}");
    }

    /// every_n 缺失按 1 处理(每轮),但为 0 时永不到期(防御)。
    #[test]
    fn due_fields_every_n_edge() {
        let c = contract_with(vec![field("无N", UpdateMode::EveryNTurns, None)]);
        assert_eq!(due_fields(&c, 1), vec!["无N"]);
        assert_eq!(due_fields(&c, 5), vec!["无N"]);
    }
}
