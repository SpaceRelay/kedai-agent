// 契约层 op 方言 + 校验/置信度门控(纯函数,可单测)。
//
// 与 parsing/assistant 的 PatchOp(解析层,无 confidence/rationale)区分:
// 本模块的 PatchOp 是「契约引擎层的 op」,带置信度与理由,是 validate_ops 的输入/输出。
// 执行(apply)仍复用 AssistantVars::apply_patches(方案 B「复用现有应用地基」,P2 接入)。
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::field::MergeRule;
use super::Confidence;

/// op 种类(§7-PatchOp 方言:replace/delta/add/remove/move)。
/// add 语义与 replace 相同(写入目标值),此处保留独立变体以对齐文档 DSL。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Replace,
    Delta,
    Add,
    Remove,
    Move,
}

/// 契约层 op(§7-PatchOp):带可选 confidence 与 rationale。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchOp {
    pub op: OpKind,
    /// 点分路径(与 stat_data 树一致)
    pub path: String,
    /// move 的源路径
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// replace/delta/add 的值(delta 为增量数字)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// 置信度(AI 声明;缺省视为确信)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    /// 更新理由(进 changelog)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

impl PatchOp {
    /// 构造一个 replace op(测试与运行时最常用)。
    pub fn replace(path: impl Into<String>, value: Value) -> Self {
        PatchOp {
            op: OpKind::Replace,
            path: path.into(),
            from: None,
            value: Some(value),
            confidence: None,
            rationale: None,
        }
    }
}

/// 置信度门控判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// 通过门控,可写入
    Apply,
    /// 低置信,不写、入 pending 待复核
    Pending,
}

/// 被拒绝的 op(硬错误:未声明字段 / 越权)。
#[derive(Debug, Clone, PartialEq)]
pub struct RejectedOp {
    pub op: PatchOp,
    pub reason: String,
}

/// 校验结果:合法 op 分为 applied 与 pending 两组。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValidationOutcome {
    pub applied: Vec<PatchOp>,
    pub pending: Vec<PatchOp>,
    pub rejected: Vec<RejectedOp>,
}

/// 置信度门控(§4.2/§7-F4)。
///
/// 语义:guardrails.minConfidence 是「最低可写置信度」——Low 最宽松(默认,开箱即用),
/// Medium 只写 medium/high,High 只写 high。op 未声明 confidence 视为 High(兼容老协议,
/// 文本协议/工具路径未产出置信度时按确信通过)。
pub fn gate_confidence(contract: &super::Contract, op: &PatchOp) -> GateDecision {
    let effective = op.confidence.unwrap_or(Confidence::High);
    if effective >= contract.guardrails.min_confidence {
        GateDecision::Apply
    } else {
        GateDecision::Pending
    }
}

/// 校验一批 op(§4.7 所有权裁决 + 冲突合并 + 置信度门控)。
///
/// `writer` 为写者子系统 id(主路径为 "agent";M15/M16/M13 传入 plot/dice/memory)。
/// 流程:
///   1. 前置所有权检查:字段未在契约声明 → unknown_field;写者不在 writers 白名单 → not_owner。
///   2. 同 path 多条 op 冲突合并:按 FieldDef.ownership.merge 规则(§4.7)。
///   3. 置信度门控:低置信 → pending,否则 applied。
pub fn validate_ops(
    contract: &super::Contract,
    ops: &[PatchOp],
    writer: &str,
) -> ValidationOutcome {
    let mut outcome = ValidationOutcome::default();

    // 1. 所有权前置校验
    let mut accepted: Vec<PatchOp> = Vec::new();
    for op in ops {
        match check_ownership(contract, op, writer) {
            Ok(()) => accepted.push(op.clone()),
            Err(reason) => outcome.rejected.push(RejectedOp {
                op: op.clone(),
                reason,
            }),
        }
    }

    // 2. 同 path 冲突合并
    let mut grouped: std::collections::BTreeMap<&str, Vec<PatchOp>> =
        std::collections::BTreeMap::new();
    for op in &accepted {
        grouped
            .entry(op.path.as_str())
            .or_default()
            .push(op.clone());
    }
    let mut merged: Vec<PatchOp> = Vec::new();
    for (path, group) in grouped {
        let rule = contract
            .update_rules
            .get(path)
            .map(|f| f.merge())
            .unwrap_or(MergeRule::LastWrite);
        merged.push(merge_ops(path, &group, &rule));
    }

    // 3. 置信度门控
    for op in merged {
        match gate_confidence(contract, &op) {
            GateDecision::Apply => outcome.applied.push(op),
            GateDecision::Pending => outcome.pending.push(op),
        }
    }

    outcome
}

/// 检查单条 op 的所有权:字段必须已声明,且 writer 在 writers 白名单(或 "*")。
fn check_ownership(contract: &super::Contract, op: &PatchOp, writer: &str) -> Result<(), String> {
    let Some(field) = contract.update_rules.get(&op.path) else {
        return Err("unknown_field".into());
    };
    let writers = field.writers();
    if writers.iter().any(|w| w == "*" || w == writer) {
        Ok(())
    } else {
        Err("not_owner".into())
    }
}

/// 同 path 多条 op 按 merge 规则合并为一条。
///
/// - last_write:后者覆盖(默认)。
/// - sum/max/min:对数值 op 聚合(纯数值才合并,混入非数值回退 last_write)。
/// - custom_fn_id:需作者注册的纯函数(§18),P1 回退 last_write。
fn merge_ops(path: &str, ops: &[PatchOp], rule: &MergeRule) -> PatchOp {
    match rule {
        MergeRule::Sum | MergeRule::Max | MergeRule::Min => merge_numeric(path, ops, rule),
        MergeRule::LastWrite | MergeRule::CustomFnId => ops.last().expect("group 非空").clone(),
    }
}

/// 数值合并(sum/max/min);所有 op 的 value 必须是数字,否则回退 last_write。
fn merge_numeric(path: &str, ops: &[PatchOp], rule: &MergeRule) -> PatchOp {
    let mut values = Vec::with_capacity(ops.len());
    for op in ops {
        match op.value.as_ref().and_then(Value::as_f64) {
            Some(v) => values.push(v),
            None => return ops.last().expect("group 非空").clone(),
        }
    }
    let result = match rule {
        MergeRule::Sum => values.iter().sum::<f64>(),
        MergeRule::Max => values.iter().fold(f64::MIN, |a, b| a.max(*b)),
        MergeRule::Min => values.iter().fold(f64::MAX, |a, b| a.min(*b)),
        _ => unreachable!(),
    };
    // 聚合置信度取组内最低:任一 op 声明 low → 合并产物也 low(gate_confidence
    // 对 None 视为 High,若在此丢弃,同 path 两条低置信 delta 合并后即绕过
    // minConfidence 门控,且 pending 队列拿不到提议——自纠闭环同时失效)。
    let confidence = ops.iter().filter_map(|o| o.confidence).min();
    PatchOp {
        op: OpKind::Replace,
        path: path.to_string(),
        from: None,
        value: Some(serde_json::json!(result)),
        confidence,
        rationale: None,
    }
}

/// 跨字段不变量检查(§7-Invariant):返回违反信息列表(空 = 满足)。
///
/// P1 落地 mutex(互斥)类型;range/require_if 依赖白名单谓词引擎(§17.8 CES),
/// 后续里程碑实现,当前恒判定为满足。
pub fn check_invariants(contract: &super::Contract, stat_data: &Value) -> Vec<String> {
    let mut violations = Vec::new();
    for inv in &contract.invariants {
        match inv.kind {
            super::invariant::InvariantKind::Mutex => {
                let occupied = inv
                    .paths
                    .iter()
                    .filter(|p| read_value(stat_data, p).is_some_and(is_occupied))
                    .count();
                if occupied >= 2 {
                    violations.push(format!("{}: {}", inv.id, inv.message));
                }
            }
            // range/require_if 留待谓词引擎(§17.8),P1 不判定
            super::invariant::InvariantKind::Range | super::invariant::InvariantKind::RequireIf => {
            }
        }
    }
    violations
}

/// 将契约层 op 应用到 stat_data 树(复用 AssistantVars::apply_patches 的路径/校验语义)。
///
/// 这是「契约层 op → 解析层 op」的映射桥:P2/P5 契约驱动的变量应用路径,以及
/// changelog 回放,都经此复用同一套已测试的路径应用逻辑,避免两套寻址语义分叉。
pub fn apply_ops(tree: &mut Value, ops: &[PatchOp]) -> Result<(), String> {
    let converted: Vec<crate::parsing::assistant::PatchOp> = ops
        .iter()
        .map(convert_to_assistant_patch)
        .collect::<Result<_, _>>()?;
    let mut vars = crate::parsing::assistant::AssistantVars::from_value(tree.clone());
    vars.apply_patches(&converted)?;
    *tree = vars.tree().clone();
    Ok(())
}

/// 解析层 op → 契约层 op(带 confidence None = 视为确信,reason 进 rationale)。
/// 供契约门控把模型工具/文本协议产出的补丁送入 validate_ops。
pub fn from_assistant_patch(p: &crate::parsing::assistant::PatchOp) -> PatchOp {
    // 契约 updateRules 键与所有权校验用点分路径;解析层协议(JSON Patch 工具调用/
    // <JSONPatch> 文本)允许斜杠路径,在此统一归一化,避免 /a/b 全部落 unknown_field。
    let norm = |path: &str| crate::parsing::assistant::split_path(path).join(".");
    use crate::parsing::assistant::PatchOp as AP;
    match p {
        AP::Replace {
            path,
            value,
            reason,
        } => PatchOp {
            op: OpKind::Replace,
            path: norm(path),
            from: None,
            value: Some(value.clone()),
            confidence: None,
            rationale: reason.clone(),
        },
        AP::Delta {
            path,
            value,
            reason,
        } => PatchOp {
            op: OpKind::Delta,
            path: norm(path),
            from: None,
            value: Some(serde_json::json!(value)),
            confidence: None,
            rationale: reason.clone(),
        },
        AP::Remove { path, reason } => PatchOp {
            op: OpKind::Remove,
            path: norm(path),
            from: None,
            value: None,
            confidence: None,
            rationale: reason.clone(),
        },
        AP::Move { from, to, reason } => PatchOp {
            op: OpKind::Move,
            path: norm(to),
            from: Some(norm(from)),
            value: None,
            confidence: None,
            rationale: reason.clone(),
        },
    }
}

/// 契约层 op → 解析层 op(apply_mvu_patches 等既有应用路径只收解析层)。
pub fn to_assistant_patch(op: &PatchOp) -> crate::parsing::assistant::PatchOp {
    convert_to_assistant_patch(op).unwrap_or_else(|e| crate::parsing::assistant::PatchOp::Remove {
        path: op.path.clone(),
        reason: Some(format!("转换失败({e})")),
    })
}

/// 契约门控详细结果(P5):applied 携带解析层与契约层两种形态(同源同序),
/// rejected(带原因,供工具反馈与熔断指纹)与 pending 保留契约层 op。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GatedPatches {
    /// 通过门控可应用(解析层,供 apply_mvu_patches 应用)
    pub applied: Vec<crate::parsing::assistant::PatchOp>,
    /// 同批生效的契约层 op(带 confidence/rationale,供 entries_from_applied 留痕)
    pub applied_ops: Vec<PatchOp>,
    /// 契约层视角的被拒 op(未应用,含 unknown_field/not_owner 原因)
    pub rejected: Vec<RejectedOp>,
    pub pending: Vec<PatchOp>,
}

/// 契约门控过滤(纯函数,详细版):解析层 patches → 契约校验 → 四列表。
///
/// 无契约 → applied 原样放行且 applied_ops/rejected/pending 为空(与
/// gate_assistant_patches 语义一致);有契约 → validate_ops(writer) 后 applied
/// 转回解析层、applied_ops 保留契约层原样, rejected/pending 供运行态接线取
/// confidence/rationale 生成 changelog 与 PendingOp。
pub fn gate_assistant_patches_detailed(
    contract: Option<&super::Contract>,
    patches: &[crate::parsing::assistant::PatchOp],
    writer: &str,
) -> GatedPatches {
    gate_assistant_patches_overrides(contract, patches, &Default::default(), writer)
}

/// 带置信度覆盖的门控:解析层协议(JSON Patch)本身无 confidence 字段,
/// AI 工具调用可在 patch 对象里声明 confidence("low"/"medium"/"high"),
/// 由调用方提取为「点分路径 → Confidence」映射经本参数注入;未覆盖的 op
/// 保持缺省(视为 High)。语义与 detailed 完全一致。
pub fn gate_assistant_patches_overrides(
    contract: Option<&super::Contract>,
    patches: &[crate::parsing::assistant::PatchOp],
    confidence_overrides: &std::collections::BTreeMap<String, super::Confidence>,
    writer: &str,
) -> GatedPatches {
    let Some(c) = contract else {
        return GatedPatches {
            applied: patches.to_vec(),
            applied_ops: Vec::new(),
            rejected: Vec::new(),
            pending: Vec::new(),
        };
    };
    let mut contract_ops: Vec<PatchOp> = patches.iter().map(from_assistant_patch).collect();
    for op in &mut contract_ops {
        if let Some(cf) = confidence_overrides.get(&op.path) {
            op.confidence = Some(*cf);
        }
    }
    let outcome = validate_ops(c, &contract_ops, writer);
    GatedPatches {
        applied: outcome.applied.iter().map(to_assistant_patch).collect(),
        applied_ops: outcome.applied,
        rejected: outcome.rejected,
        pending: outcome.pending,
    }
}

/// 契约门控过滤(纯函数):解析层 patches → 契约层校验 → 只返回可应用的解析层补丁。
///
/// 无契约 → 原样放行(兼容层);有契约 → validate_ops(writer) 后:
/// applied 转回解析层返回,rejected/pending 计数丢弃。返回 (可应用补丁, rejected 数, pending 数)。
pub fn gate_assistant_patches(
    contract: Option<&super::Contract>,
    patches: &[crate::parsing::assistant::PatchOp],
    writer: &str,
) -> (Vec<crate::parsing::assistant::PatchOp>, usize, usize) {
    let gated = gate_assistant_patches_detailed(contract, patches, writer);
    (gated.applied, gated.rejected.len(), gated.pending.len())
}

/// 契约层 op → 解析层 PatchOp(add 语义与 replace 相同,均写入目标值)。
fn convert_to_assistant_patch(op: &PatchOp) -> Result<crate::parsing::assistant::PatchOp, String> {
    use crate::parsing::assistant::PatchOp as AP;
    let reason = op.rationale.clone();
    Ok(match op.op {
        OpKind::Replace | OpKind::Add => AP::Replace {
            path: op.path.clone(),
            value: op.value.clone().unwrap_or(Value::Null),
            reason,
        },
        OpKind::Delta => AP::Delta {
            path: op.path.clone(),
            value: op
                .value
                .as_ref()
                .and_then(Value::as_f64)
                .ok_or_else(|| format!("delta 值非数字: {}", op.path))?,
            reason,
        },
        OpKind::Remove => AP::Remove {
            path: op.path.clone(),
            reason,
        },
        OpKind::Move => AP::Move {
            from: op
                .from
                .clone()
                .ok_or_else(|| format!("move 缺 from: {}", op.path))?,
            to: op.path.clone(),
            reason,
        },
    })
}

/// 值是否「占用」(mutex 判定):非 null、非空字符串、非空数组/对象。
fn is_occupied(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
        _ => true,
    }
}

/// 按点路径读 stat_data 值(复用 assistant 路径工具,保证路径语义一致)。
fn read_value<'a>(stat_data: &'a Value, path: &str) -> Option<&'a Value> {
    let segs = crate::parsing::assistant::split_path(path);
    crate::parsing::assistant::path_get(stat_data, &segs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::field::{FieldDef, FieldType, UpdateMode};
    use serde_json::json;

    fn num_field(path: &str) -> FieldDef {
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

    /// 默认门槛(低)下:任何置信度(含 low)都 apply;未声明置信度视为确信也 apply。
    #[test]
    fn gate_confidence_default_low_threshold() {
        let c = contract_with(vec![num_field("a")]);
        let op = PatchOp {
            op: OpKind::Replace,
            path: "a".into(),
            from: None,
            value: Some(json!(1)),
            confidence: Some(Confidence::Low),
            rationale: None,
        };
        assert_eq!(gate_confidence(&c, &op), GateDecision::Apply);

        // 未声明置信度 → 视为 High
        let no_conf = PatchOp {
            confidence: None,
            ..op.clone()
        };
        assert_eq!(gate_confidence(&c, &no_conf), GateDecision::Apply);
    }

    /// 作者抬高门槛到 high 后:low/medium → pending。
    #[test]
    fn gate_confidence_respects_raised_threshold() {
        let mut c = contract_with(vec![num_field("a")]);
        c.guardrails.min_confidence = Confidence::High;
        let low = PatchOp {
            op: OpKind::Replace,
            path: "a".into(),
            from: None,
            value: Some(json!(1)),
            confidence: Some(Confidence::Low),
            rationale: None,
        };
        assert_eq!(gate_confidence(&c, &low), GateDecision::Pending);
    }

    /// 未声明字段的 op → unknown_field 拒绝。
    #[test]
    fn validate_ops_rejects_unknown_field() {
        let c = contract_with(vec![num_field("a")]);
        let out = validate_ops(&c, &[PatchOp::replace("不存在", json!(1))], "agent");
        assert!(out.applied.is_empty());
        assert_eq!(out.rejected.len(), 1);
        assert_eq!(out.rejected[0].reason, "unknown_field");
    }

    /// 写者不在白名单 → not_owner 拒绝(默认 ownership: writers=[agent,manual])。
    #[test]
    fn validate_ops_rejects_not_owner() {
        let c = contract_with(vec![num_field("a")]);
        let out = validate_ops(&c, &[PatchOp::replace("a", json!(1))], "dice");
        assert!(out.applied.is_empty());
        assert_eq!(out.rejected[0].reason, "not_owner");
    }

    /// 同 path 多条 op 按 last_write 合并(后者覆盖)。
    #[test]
    fn validate_ops_last_write_merges() {
        let c = contract_with(vec![num_field("a")]);
        let ops = vec![
            PatchOp::replace("a", json!(1)),
            PatchOp::replace("a", json!(2)),
        ];
        let out = validate_ops(&c, &ops, "agent");
        assert_eq!(out.applied.len(), 1);
        assert_eq!(out.applied[0].value, Some(json!(2)));
    }

    /// mutex 不变量:两个字段都非空 → 违反。
    #[test]
    fn check_invariants_mutex_violation() {
        let mut c = contract_with(vec![num_field("a"), num_field("b")]);
        c.invariants.push(crate::contracts::invariant::Invariant {
            id: "ab_mutex".into(),
            kind: crate::contracts::invariant::InvariantKind::Mutex,
            paths: vec!["a".into(), "b".into()],
            condition: None,
            message: "a 与 b 互斥".into(),
        });
        let stat = json!({ "a": 1, "b": 2 });
        let v = check_invariants(&c, &stat);
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("ab_mutex"));

        // 只有一个非空 → 不违反
        let ok = check_invariants(&c, &json!({ "a": 1 }));
        assert!(ok.is_empty());
    }

    /// apply_ops:契约层 op 应用到树(replace/delta/remove/move/add 全覆盖)。
    #[test]
    fn apply_ops_applies_contract_patch_to_tree() {
        let mut tree = json!({ "a": 1, "b": 2, "list": [0, 1] });
        let ops = vec![
            PatchOp {
                op: OpKind::Replace,
                path: "a".into(),
                from: None,
                value: Some(json!(10)),
                confidence: None,
                rationale: None,
            },
            PatchOp {
                op: OpKind::Delta,
                path: "b".into(),
                from: None,
                value: Some(json!(3)),
                confidence: None,
                rationale: None,
            },
            PatchOp {
                op: OpKind::Remove,
                path: "list.0".into(),
                from: None,
                value: None,
                confidence: None,
                rationale: None,
            },
            PatchOp {
                op: OpKind::Add,
                path: "c".into(),
                from: None,
                value: Some(json!("new")),
                confidence: None,
                rationale: None,
            },
        ];
        apply_ops(&mut tree, &ops).unwrap();
        assert_eq!(tree["a"], 10);
        assert_eq!(tree["b"], json!(5.0));
        assert_eq!(tree["list"], json!([1]));
        assert_eq!(tree["c"], "new");
    }

    /// gate_assistant_patches:无契约原样放行;有契约过滤掉未声明字段/越权 op。
    #[test]
    fn gate_assistant_patches_filters_against_contract() {
        use crate::parsing::assistant::PatchOp as AP;
        let c = contract_with(vec![num_field("a")]);
        let patches = vec![
            AP::Replace {
                path: "a".into(),
                value: json!(1),
                reason: None,
            },
            // 未声明字段 → rejected
            AP::Replace {
                path: "不存在".into(),
                value: json!(2),
                reason: None,
            },
        ];
        let (applied, rejected, pending) = gate_assistant_patches(Some(&c), &patches, "agent");
        assert_eq!(applied.len(), 1);
        match &applied[0] {
            AP::Replace { path, .. } => assert_eq!(path, "a"),
            other => panic!("应为 Replace,实际: {other:?}"),
        }
        assert_eq!(rejected, 1);
        assert_eq!(pending, 0);

        // 无契约 → 原样放行
        let (all, rej, pend) = gate_assistant_patches(None, &patches, "agent");
        assert_eq!(all.len(), 2);
        assert_eq!(rej, 0);
        assert_eq!(pend, 0);
    }

    /// detailed 版:三列表携带完整 op;契约层 rejected 保留原始 patch(供留痕);
    /// 同 path 多条 op 按 last_write 合并语义保留。
    /// 注:解析层协议不携带置信度(from_assistant_patch 恒置 None = High),
    /// 从解析层入口 pending 恒空——pending 分支供后续直接产契约层 op 的路径使用。
    #[test]
    fn gate_assistant_patches_detailed_returns_three_lists() {
        use crate::parsing::assistant::PatchOp as AP;
        let mut c = contract_with(vec![num_field("a")]);
        c.guardrails.min_confidence = Confidence::High;
        let patches = vec![
            AP::Replace {
                path: "a".into(),
                value: json!(1),
                reason: Some("第一次".into()),
            },
            AP::Replace {
                path: "a".into(),
                value: json!(2),
                reason: None,
            },
            // 未声明字段 → rejected
            AP::Replace {
                path: "未声明".into(),
                value: json!(3),
                reason: None,
            },
        ];
        let gated = gate_assistant_patches_detailed(Some(&c), &patches, "agent");
        // 未声明置信度视为 High → 通过;同 path 两条按 last_write 合并为一条
        assert_eq!(gated.applied.len(), 1, "同 path 合并后应只剩一条");
        assert_eq!(gated.rejected.len(), 1);
        assert_eq!(
            gated.rejected[0].op.path, "未声明",
            "rejected 保留契约层 op 原样"
        );
        assert_eq!(gated.pending.len(), 0);

        // 无契约 → applied 原样放行(不合并),两列表空
        let gated3 = gate_assistant_patches_detailed(None, &patches, "agent");
        assert_eq!(gated3.applied.len(), 3);
        assert!(gated3.rejected.is_empty() && gated3.pending.is_empty());
    }

    /// overrides 版:注入 low 置信 → pending;未覆盖的 op 保持缺省 High 通过。
    /// 覆盖键为归一化点分路径(斜杠/点分一致)。
    #[test]
    fn gate_assistant_patches_overrides_routes_low_confidence() {
        use crate::parsing::assistant::PatchOp as AP;
        use std::collections::BTreeMap;
        let mut c = contract_with(vec![num_field("a"), num_field("b")]);
        c.guardrails.min_confidence = Confidence::Medium;
        let patches = vec![
            AP::Replace {
                path: "a".into(),
                value: json!(1),
                reason: None,
            },
            AP::Replace {
                path: "b".into(),
                value: json!(2),
                reason: None,
            },
        ];
        let mut overrides = BTreeMap::new();
        overrides.insert("a".to_string(), Confidence::Low);
        let gated = gate_assistant_patches_overrides(Some(&c), &patches, &overrides, "agent");
        assert_eq!(gated.applied.len(), 1, "未覆盖的 b 正常通过");
        assert_eq!(gated.applied_ops[0].path, "b");
        assert_eq!(gated.pending.len(), 1, "覆盖 low 的 a 进 pending");
        assert_eq!(gated.pending[0].path, "a");
        assert_eq!(gated.pending[0].confidence, Some(Confidence::Low));
    }

    /// sum 合并的置信度取组内最低:同 path 两条 low delta 合并后不得因
    /// confidence 丢失而按 High 直接 Apply(否则外部调用方可稳定绕过
    /// minConfidence 门控,且 pending 队列拿不到提议)。
    #[test]
    fn merge_numeric_keeps_lowest_confidence() {
        use crate::parsing::assistant::PatchOp as AP;
        use std::collections::BTreeMap;
        let mut field = num_field("a");
        field.ownership = Some(super::super::field::Ownership {
            owner: "agent".into(),
            writers: Some(vec!["agent".into()]),
            priority: 0,
            merge: MergeRule::Sum,
            audit: true,
        });
        let mut c = contract_with(vec![field]);
        c.guardrails.min_confidence = Confidence::Medium;
        let patches = vec![
            AP::Delta {
                path: "a".into(),
                value: 1.0,
                reason: None,
            },
            AP::Delta {
                path: "a".into(),
                value: 2.0,
                reason: None,
            },
        ];
        let mut overrides = BTreeMap::new();
        overrides.insert("a".to_string(), Confidence::Low);
        let gated = gate_assistant_patches_overrides(Some(&c), &patches, &overrides, "agent");
        assert!(
            gated.applied.is_empty(),
            "两条 low delta 合并后不得直接 Apply: {:?}",
            gated.applied_ops
        );
        assert_eq!(gated.pending.len(), 1, "合并产物应携带最低置信度进 pending");
        assert_eq!(gated.pending[0].confidence, Some(Confidence::Low));
        // 值仍正确聚合
        assert_eq!(gated.pending[0].value, Some(json!(3.0)));
    }
}
