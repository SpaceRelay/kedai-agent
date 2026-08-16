// 观察层(§3.6):状态投影 + 可见性控制,位于 dueFields 与变量请求之间。
//
// observe 决定「Agent 本轮看到哪些字段、以何格式」,绝不把完整 stat_data 直接暴露给模型。
// 与缓存层正交:观察层变只影响 L3 尾部,不破坏 L0-L2 前缀(§3.6)。
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

use super::field::FieldDef;
use super::meta::PendingOp;

/// 观察层选项(§10.1 ObservationOpts)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationOpts {
    /// 字符串字段最大长度(超过截断,保留前缀 + 长度标记)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_field_len: Option<usize>,
    /// 最多暴露的字段数(超过则折叠为摘要行)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<usize>,
    /// 敏感字段脱敏(string 值替换为占位,标记 masked)
    #[serde(default)]
    pub sensitive_mask: bool,
}

impl Default for ObservationOpts {
    fn default() -> Self {
        ObservationOpts {
            max_field_len: None,
            top_k: None,
            sensitive_mask: false,
        }
    }
}

/// 观察层输出的单个字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedField {
    pub path: String,
    pub value: Value,
    /// 是否已脱敏
    #[serde(default)]
    pub masked: bool,
}

/// 观察层输出(非全量 StatePreview)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub fields: Vec<ObservedField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folded_summary: Option<String>,
}

/// 观察投影(§3.6):due + dependencies + pending 基础上叠加可见性、脱敏、聚合。
///
/// `viewer` 为观察者子系统 id(主路径 "agent");仅保留 viewer 可写(在 writers 白名单
/// 或 owner)且非 display:hidden 的字段。`pending` 里的字段也纳入(Agent 需看到待复核
/// 项以便自纠)。
pub fn observe(
    contract: &super::Contract,
    stat_data: &Value,
    due: &[String],
    pending: &[PendingOp],
    viewer: &str,
    opts: &ObservationOpts,
) -> Observation {
    // 1. 依赖扩展(静态引用 + 显式依赖的传递闭包,深度受 guardrails.maxDependencyDepth)
    let deps = super::validation::compute_dependencies(contract, due).unwrap_or_default();
    let mut field_paths: BTreeSet<String> = BTreeSet::new();
    for d in due {
        field_paths.insert(d.clone());
    }
    for ds in deps.values() {
        for d in ds {
            field_paths.insert(d.clone());
        }
    }

    // 2. pending 字段也纳入
    for p in pending {
        field_paths.insert(p.op.path.clone());
    }

    // 3. 逐字段投影:可见性过滤 → 读值 → 截断/脱敏
    let mut fields: Vec<ObservedField> = Vec::new();
    for path in field_paths {
        let Some(field) = contract.update_rules.get(&path) else {
            continue;
        };
        if !is_visible(contract, field, viewer) {
            continue;
        }
        let value = read_value(stat_data, &path).cloned().unwrap_or(Value::Null);
        let (value, masked) = project_value(&value, opts);
        fields.push(ObservedField {
            path,
            value,
            masked,
        });
    }

    // 4. top-K 聚合折叠(按 path 升序稳定,超出的折叠为摘要行)
    if let Some(k) = opts.top_k {
        if fields.len() > k {
            let rest = fields.len() - k;
            fields.truncate(k);
            return Observation {
                fields,
                folded_summary: Some(format!("另有 {rest} 个字段未展示")),
            };
        }
    }

    Observation {
        fields,
        folded_summary: None,
    }
}

/// 可见性:viewer 在 writers 白名单(或 "*")或为 owner,且字段非 display:hidden。
fn is_visible(contract: &super::Contract, field: &FieldDef, viewer: &str) -> bool {
    if is_hidden(contract, &field.path) {
        return false;
    }
    let writers = field.writers();
    writers.iter().any(|w| w == "*" || w == viewer) || field.owner() == viewer
}

/// 查 displayRules 是否把该 path 声明为 hidden。
fn is_hidden(contract: &super::Contract, path: &str) -> bool {
    contract
        .display_rules
        .iter()
        .any(|r| r.path == path && r.render == super::DisplayRender::Hidden)
}

/// 值投影:截断 + 脱敏。
fn project_value(value: &Value, opts: &ObservationOpts) -> (Value, bool) {
    match value {
        Value::String(s) => {
            if opts.sensitive_mask {
                return (Value::String("[masked]".into()), true);
            }
            if let Some(max_len) = opts.max_field_len {
                let chars: Vec<char> = s.chars().collect();
                if chars.len() > max_len {
                    let prefix: String = chars[..max_len].iter().collect();
                    return (
                        Value::String(format!("{prefix}…(共 {} 字符)", chars.len())),
                        false,
                    );
                }
            }
            (value.clone(), false)
        }
        other => (other.clone(), false),
    }
}

/// 按点路径读 stat_data 值(复用 assistant 路径工具)。
fn read_value<'a>(stat_data: &'a Value, path: &str) -> Option<&'a Value> {
    let segs = crate::parsing::assistant::split_path(path);
    crate::parsing::assistant::path_get(stat_data, &segs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::field::{FieldType, UpdateMode};
    use serde_json::json;

    fn field(path: &str) -> FieldDef {
        FieldDef {
            path: path.to_string(),
            kind: FieldType::Number,
            default: None,
            update_mode: UpdateMode::EveryTurn,
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

    fn contract_with(fields: Vec<FieldDef>) -> super::super::Contract {
        let mut c: super::super::Contract = serde_json::from_value(json!({
            "version": 1, "id": "t", "schema": { "properties": {} },
            "updateRules": {}, "guardrails": {}
        }))
        .unwrap();
        c.update_rules = fields.into_iter().map(|f| (f.path.clone(), f)).collect();
        c
    }

    /// 观察只投影 due + 依赖字段,且读实际值。
    #[test]
    fn observe_projects_due_and_dependencies() {
        let mut a = field("A");
        a.dependencies = Some(vec!["B".into()]);
        let c = contract_with(vec![a, field("B"), field("C")]);
        let stat = json!({ "A": 1, "B": 2, "C": 3 });
        let obs = observe(&c, &stat, &["A".to_string()], &[], "agent", &ObservationOpts::default());
        let paths: Vec<&str> = obs.fields.iter().map(|f| f.path.as_str()).collect();
        // A 是 due,B 是 A 的依赖,C 既非 due 也非依赖 → 不进
        assert_eq!(paths, vec!["A", "B"]);
    }

    /// pending 里的字段也纳入观察(Agent 需看到待复核项)。
    #[test]
    fn observe_includes_pending_paths() {
        let c = contract_with(vec![field("A"), field("B")]);
        let stat = json!({ "A": 1, "B": 2 });
        let pending = vec![PendingOp {
            id: "p1".into(),
            op: crate::contracts::op::PatchOp::replace("B", json!(9)),
            created_at_turn: 1,
            rationale: None,
        }];
        let obs = observe(&c, &stat, &["A".to_string()], &pending, "agent", &ObservationOpts::default());
        let paths: Vec<&str> = obs.fields.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["A", "B"]);
    }

    /// 观察者越权(非 writers 非 owner)的字段被过滤。
    #[test]
    fn observe_filters_unauthorized_viewer() {
        let c = contract_with(vec![field("A")]); // 默认 writers=[agent,manual]
        let stat = json!({ "A": 1 });
        let obs = observe(&c, &stat, &["A".to_string()], &[], "dice", &ObservationOpts::default());
        assert!(obs.fields.is_empty(), "dice 不是 A 的 writers");
    }

    /// 长字符串截断 + 敏感脱敏。
    #[test]
    fn observe_truncates_and_masks() {
        let mut a = field("A");
        a.kind = FieldType::String;
        let c = contract_with(vec![a]);
        let stat = json!({ "A": "一二三四五六七八九十" });

        let trunc = observe(
            &c,
            &stat,
            &["A".to_string()],
            &[],
            "agent",
            &ObservationOpts {
                max_field_len: Some(3),
                ..Default::default()
            },
        );
        assert_eq!(trunc.fields[0].masked, false);
        assert!(trunc.fields[0].value.as_str().unwrap().contains("…"));

        let masked = observe(
            &c,
            &stat,
            &["A".to_string()],
            &[],
            "agent",
            &ObservationOpts {
                sensitive_mask: true,
                ..Default::default()
            },
        );
        assert!(masked.fields[0].masked);
        assert_eq!(masked.fields[0].value, json!("[masked]"));
    }

    /// top-K 折叠:超过 K 的字段折叠为摘要行。
    #[test]
    fn observe_folds_over_top_k() {
        let c = contract_with(vec![field("A"), field("B"), field("C")]);
        let stat = json!({ "A": 1, "B": 2, "C": 3 });
        let due = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let obs = observe(
            &c,
            &stat,
            &due,
            &[],
            "agent",
            &ObservationOpts {
                top_k: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(obs.fields.len(), 2);
        assert_eq!(obs.folded_summary, Some("另有 1 个字段未展示".into()));
    }

    /// display:hidden 的字段不进观察。
    #[test]
    fn observe_excludes_hidden_display() {
        let mut c = contract_with(vec![field("A")]);
        c.display_rules.push(crate::contracts::DisplayRule {
            path: "A".into(),
            render: crate::contracts::DisplayRender::Hidden,
        });
        let stat = json!({ "A": 1 });
        let obs = observe(&c, &stat, &["A".to_string()], &[], "agent", &ObservationOpts::default());
        assert!(obs.fields.is_empty());
    }
}
