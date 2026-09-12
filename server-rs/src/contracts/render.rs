// 状态提示文本构建(契约驱动裁剪 vs 整树兼容层,纯函数可单测)。
//
// 这是方案 B「把 generate_mvu_status 升级为契约驱动」的提示词组装层:
//   - 有契约 → dueFields 裁剪 + observe 观察层投影(只暴露 due + dependencies 字段);
//   - 无契约 → 整树 to_json()(兼容层,等价现状,存量卡不受影响)。
// 观察层变化只影响 L3 尾部(§3.6),不破坏 L0-L2 前缀。
use serde_json::Value;

use super::{due_fields, observe, Contract, KaleidoMeta};

/// 构建「当前状态」提示文本。
///
/// 返回 (状态文本, due 字段列表)。due 列表供调用方做后续 op 门控/裁剪复用;
/// 无契约时 due 为空(整树兼容层不区分到期字段)。
pub fn build_state_prompt(
    contract: Option<&Contract>,
    stat_data: &Value,
    turn_id: u64,
) -> (String, Vec<String>) {
    let Some(c) = contract else {
        return (stat_data.to_string(), Vec::new());
    };

    let due = due_fields(c, turn_id);
    let meta = KaleidoMeta {
        last_turn_id: turn_id,
        ..KaleidoMeta::default()
    };
    let obs = observe(
        c,
        stat_data,
        &due,
        &meta.pending,
        "agent",
        &super::ObservationOpts::default(),
    );
    let mut lines: Vec<String> = Vec::new();
    for f in &obs.fields {
        let mut line = format!("{}: {}", f.path, f.value);
        if f.masked {
            line.push_str(" [已脱敏]");
        }
        lines.push(line);
    }
    if let Some(summary) = &obs.folded_summary {
        lines.push(summary.clone());
    }
    if lines.is_empty() {
        (String::new(), due)
    } else {
        (lines.join("\n"), due)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::field::{FieldType, UpdateMode};
    use serde_json::json;

    fn field(path: &str, mode: UpdateMode) -> super::super::FieldDef {
        super::super::FieldDef {
            path: path.to_string(),
            kind: FieldType::Number,
            default: None,
            update_mode: mode,
            every_n: None,
            dynamic: false,
            change_rule: None,
            cap: None,
            scope: None,
            ttl: None,
            persist: Default::default(),
            stability: Default::default(),
            display: true,
            dependencies: None,
            ownership: None,
        }
    }

    fn contract_with(fields: Vec<super::super::FieldDef>) -> Contract {
        let mut c: Contract = serde_json::from_value(json!({
            "version": 1, "id": "t", "schema": { "properties": {} },
            "updateRules": {}, "guardrails": {}
        }))
        .unwrap();
        c.update_rules = fields.into_iter().map(|f| (f.path.clone(), f)).collect();
        c
    }

    /// 无契约 → 整树 JSON(兼容层,等价现状)。
    #[test]
    fn build_state_prompt_no_contract_full_tree() {
        let stat = json!({ "a": 1, "b": 2 });
        let (text, due) = build_state_prompt(None, &stat, 0);
        assert_eq!(text, stat.to_string());
        assert!(due.is_empty());
    }

    /// 有契约 → 只投影 due 字段,非 due 字段不进提示词(G4 增量裁剪)。
    #[test]
    fn build_state_prompt_contract_prunes() {
        let c = contract_with(vec![
            field("每轮", UpdateMode::EveryTurn),
            field("固定", UpdateMode::Fixed),
        ]);
        let stat = json!({ "每轮": 1, "固定": 2 });
        let (text, due) = build_state_prompt(Some(&c), &stat, 5);
        assert_eq!(due, vec!["每轮"], "fixed 字段不应到期");
        assert!(text.contains("每轮: 1"), "text: {text}");
        assert!(!text.contains("固定"), "固定字段不应进提示词: {text}");
    }

    /// 有契约但无到期字段 → 空文本。
    #[test]
    fn build_state_prompt_contract_no_due_empty() {
        let c = contract_with(vec![field("固定", UpdateMode::Fixed)]);
        let stat = json!({ "固定": 2 });
        let (text, due) = build_state_prompt(Some(&c), &stat, 3);
        assert!(due.is_empty());
        assert!(text.is_empty());
    }
}
