// 契约 DSL 模块(万花筒机制移植进 Kedai Rust 后端 · 方案 B 的 M1 前置)。
//
// 代际: L1(老层·稳 / Anchored Core)——兼容契约层。
// 判据: 契约是变量系统的**唯一事实源**(字段/更新策略/护栏/不变量),结构变化即
//       影响既有会话数据;仅依赖同层 parsing,不依赖上层。
// 纪律: contractVersion+1 并触发调和,不得静默修改结构;禁止依赖 services/agents/api/tools。
// 详见 docs/契约-架构与数据.md §2.2。
//
pub mod changelog;
pub mod due_fields;
pub mod extract;
pub mod field;
pub mod invariant;
pub mod meta;
pub mod multi_step;
pub mod observe;
pub mod op;
pub mod registry;
pub mod render;
pub mod state;
pub mod validation;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use changelog::{
    entries_from_applied, recover, should_checkpoint, ChangelogConfig, ChangelogEntry,
    ChangelogSource, CheckpointReason, StateCheckpoint,
};
pub use due_fields::{classify_field, due_fields, FieldPool};
pub use extract::{
    extract_from_character_card, extract_from_world_entries, raw_contract_from_character_card,
};
pub use field::{FieldDef, MergeRule, Ownership, PersistScope, StabilityClass, UpdateMode};
pub use invariant::{Invariant, InvariantKind};
pub use meta::{merge_pending, KaleidoMeta, PendingOp, MAX_PENDING};
pub use multi_step::{
    failure_hash, op_hash, record_and_check, rejection_hashes, would_break, BREAK_THRESHOLD,
    BREAK_WINDOW,
};
pub use observe::{observe, Observation, ObservationOpts, ObservedField};
pub use op::{
    apply_ops, check_invariants, from_assistant_patch, gate_assistant_patches,
    gate_assistant_patches_detailed, gate_assistant_patches_overrides, gate_confidence,
    to_assistant_patch, validate_ops, GateDecision, GatedPatches, OpKind, PatchOp, RejectedOp,
    ValidationOutcome,
};
pub use registry::ContractRegistry;
pub use render::build_state_prompt;
pub use state::{apply_contract_defaults, KaleidoState, StateRevision};

/// 嵌套结构声明(§7-Contract.schema):类型/枚举/范围/loose/strict。
///
/// schema 描述 stat_data 的形态;用 serde 开放式结构 + `extra` 保留未知字段无损
/// (呼应「未知字段无损保留」原则)。核心字段取最小集,避免在 P0 过度锁死形态。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaNode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 节点类型(与 FieldDef.type 同源);容器节点可省略
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<field::FieldType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// 允许的枚举值
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#enum: Option<Vec<Value>>,
    /// 数值范围 [min, max](仅 number)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
    /// 宽松模式:多余字段不报错
    #[serde(default)]
    pub loose: bool,
    /// 严格模式:超 schema 字段按 strict 处理(§7.4)
    #[serde(default)]
    pub strict: bool,
    /// 嵌套子节点
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, SchemaNode>,
    /// 未知字段无损保留
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// 展示规则(§7-Contract.displayRules):状态表渲染方式。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayRule {
    pub path: String,
    pub render: DisplayRender,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayRender {
    Value,
    Hidden,
    Summary,
}

/// 护栏默认值(§7-Contract.guardrails):保守默认,作者可配。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Guardrails {
    /// L3 尾部 token 预算
    #[serde(default = "default_max_status_tokens")]
    pub max_status_tokens: u32,
    /// 单轮最大 op 数
    #[serde(default = "default_max_ops_per_turn")]
    pub max_ops_per_turn: u32,
    /// 置信度门控阈值
    #[serde(default = "default_min_confidence")]
    pub min_confidence: Confidence,
    /// 自纠重试次数(默认 1)
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// 多步模式护栏(默认 0=禁用多步)
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    /// 多步每步 token 预算上限(默认 2048)
    #[serde(default = "default_max_tokens_per_step")]
    pub max_tokens_per_step: u32,
    /// dependencies 传递深度上限(默认 3,运行时截断)
    #[serde(default = "default_max_dependency_depth")]
    pub max_dependency_depth: u32,
}

fn default_max_status_tokens() -> u32 {
    2048
}
fn default_max_ops_per_turn() -> u32 {
    16
}
fn default_min_confidence() -> Confidence {
    Confidence::Low
}
fn default_max_retries() -> u32 {
    1
}
fn default_max_steps() -> u32 {
    0
}
fn default_max_tokens_per_step() -> u32 {
    2048
}
fn default_max_dependency_depth() -> u32 {
    3
}

impl Default for Guardrails {
    fn default() -> Self {
        Guardrails {
            max_status_tokens: 2048,
            max_ops_per_turn: 16,
            min_confidence: Confidence::Low,
            max_retries: 1,
            max_steps: 0,
            max_tokens_per_step: 2048,
            max_dependency_depth: 3,
        }
    }
}

/// 置信度等级(§7-guardrails.minConfidence)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// 契约(§7-Contract,完整结构)。
///
/// MVP 必含字段(P0 冻结):version/id/schema/updateRules/displayRules/guardrails/invariants。
/// 其余 @since 字段以 Option 预留(默认 None)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contract {
    /// 作者改任何内容 → +1
    pub version: u32,
    /// 契约 id(cardFingerprint 的一部分)
    pub id: String,
    pub schema: SchemaNode,
    /// path → 字段定义
    pub update_rules: BTreeMap<String, FieldDef>,
    #[serde(default)]
    pub display_rules: Vec<DisplayRule>,
    #[serde(default)]
    pub guardrails: Guardrails,
    #[serde(default)]
    pub invariants: Vec<Invariant>,
    // ---- @since M10+ 可选能力(P0 预留,默认 None) ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub achievements: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ejs: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_boundary: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_effects: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub middleware: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mixins: Option<Vec<String>>,
}

/// 解析契约原始 JSON(角色卡 extensions.nlkaleido / 世界书 [nlkaleido_contract] / 导出包)。
///
/// 反序列化失败抛错;成功后再做结构校验(validate_contract),失败返回错误列表。
/// 返回的契约即为运行时唯一事实源。
pub fn parse_contract(raw: &Value) -> Result<Contract, String> {
    let contract: Contract =
        serde_json::from_value(raw.clone()).map_err(|e| format!("契约解析失败: {e}"))?;
    let errors = validation::validate_contract(&contract);
    if !errors.is_empty() {
        return Err(format!("契约校验失败: {}", errors.join("; ")));
    }
    Ok(contract)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn minimal_contract() -> Value {
        json!({
            "version": 1,
            "id": "test-card",
            "schema": { "properties": {} },
            "updateRules": {
                "角色.好感度": {
                    "path": "角色.好感度",
                    "type": "number",
                    "default": 0,
                    "updateMode": "every_n_turns",
                    "everyN": 3,
                    "display": true
                }
            },
            "displayRules": [{ "path": "角色.好感度", "render": "value" }],
            "guardrails": { "maxStatusTokens": 1024 }
        })
    }

    /// 契约序列化字段名对齐文档 DSL camelCase;guardrails 未给全时用默认值填充。
    #[test]
    fn parse_contract_fills_guardrail_defaults() {
        let c = parse_contract(&minimal_contract()).expect("应解析成功");
        assert_eq!(c.version, 1);
        assert_eq!(c.id, "test-card");
        assert_eq!(c.guardrails.max_ops_per_turn, 16, "未给字段用默认");
        assert_eq!(c.guardrails.min_confidence, Confidence::Low);
        assert_eq!(c.guardrails.max_dependency_depth, 3);
        let f = c.update_rules.get("角色.好感度").expect("应有字段");
        assert_eq!(f.kind, field::FieldType::Number);
        assert_eq!(f.update_mode, UpdateMode::EveryNTurns);
    }

    /// 契约含依赖环时 parse_contract 直接报错(拓扑序拒绝)。
    #[test]
    fn parse_contract_rejects_dependency_cycle() {
        let mut raw = minimal_contract();
        raw["updateRules"]["A"] = json!({
            "path": "A", "type": "number", "updateMode": "every_turn",
            "display": true, "dependencies": ["B"]
        });
        raw["updateRules"]["B"] = json!({
            "path": "B", "type": "number", "updateMode": "every_turn",
            "display": true, "dependencies": ["A"]
        });
        let err = parse_contract(&raw).unwrap_err();
        assert!(err.contains("dependencies cycle"), "应报依赖环: {err}");
    }
}
