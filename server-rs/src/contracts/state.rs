// 运行时完整状态(§7-KaleidoState):stat_data + revision + meta + changelog + checkpoints。
//
// 这是契约驱动变量系统的「唯一运行时事实源」聚合类型,与 meta.rs 的 KaleidoMeta
// 是包含关系(meta 是 KaleidoState 的一个字段)。P3 落地结构 + 恢复纯函数;
// 持久化(序列化到 chat_metadata / SQLite)在 P5 契约应用路径接线时统一落。
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::changelog::{ChangelogEntry, StateCheckpoint};
use super::meta::KaleidoMeta;

/// 全局单调版本(§7-StateRevision):commit 串行器分配。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateRevision {
    /// commit 串行器分配的全局单调序号
    pub seq: u64,
    /// sha256(stat_data + contractVersion + seq) 指纹(可回滚对账)
    pub hash: String,
    /// 更新时间(Unix 毫秒)
    pub updated_at: u64,
}

/// 运行时完整状态(§7-KaleidoState)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KaleidoState {
    pub contract_version: u32,
    /// stat_data 是领域术语(与代码库 extra.mvu.stat_data 一致),序列化保持 snake_case
    #[serde(rename = "stat_data")]
    pub stat_data: Value,
    pub revision: StateRevision,
    pub meta: KaleidoMeta,
    #[serde(default)]
    pub changelog: Vec<ChangelogEntry>,
    #[serde(default)]
    pub checkpoints: Vec<StateCheckpoint>,
}

impl KaleidoState {
    /// 从 stat_data 构造初始状态(新会话 bootstrap):空 changelog、seq 0、默认 meta。
    pub fn initial(contract_version: u32, stat_data: Value) -> Self {
        KaleidoState {
            contract_version,
            stat_data,
            revision: StateRevision {
                seq: 0,
                hash: String::new(),
                updated_at: 0,
            },
            meta: KaleidoMeta::default(),
            changelog: Vec::new(),
            checkpoints: Vec::new(),
        }
    }
}

/// 契约 default 填充(P8 老卡兼容层):树中缺失的契约声明字段用
/// FieldDef.default 补齐——作者在 schema 里声明的初始值兜底, InitVar
/// 没写(或老卡根本没有 InitVar)时字段不缺位。
///
/// - 只填缺失字段(path 不可达或值为 null);已有值一律不动(作者显式
///   覆盖优先于契约默认,对齐「未知字段无损保留」原则);
/// - 无 default 的字段跳过(不造 0/空串——缺省语义留给字段池调度);
/// - 返回填充的字段数(0 = 树已完备或契约无 default)。
pub fn apply_contract_defaults(contract: &super::Contract, tree: &mut Value) -> usize {
    use crate::parsing::assistant::{path_get, path_set, split_path};
    let mut filled = 0;
    for field in contract.update_rules.values() {
        let Some(default) = field.default.as_ref() else {
            continue;
        };
        let segs = split_path(&field.path);
        if segs.is_empty() {
            continue;
        }
        let missing = match path_get(tree, &segs) {
            None => true,
            // null 视为未初始化(InitVar 显式 null / 部分写入的残留)
            Some(Value::Null) => true,
            Some(_) => false,
        };
        if missing {
            path_set(tree, &segs, default.clone());
            filled += 1;
        }
    }
    filled
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// initial 构造:空 changelog/checkpoint、seq 0、meta 默认。
    #[test]
    fn initial_state_shape() {
        let s = KaleidoState::initial(1, json!({ "a": 1 }));
        assert_eq!(s.contract_version, 1);
        assert_eq!(s.stat_data, json!({ "a": 1 }));
        assert_eq!(s.revision.seq, 0);
        assert!(s.changelog.is_empty());
        assert!(s.checkpoints.is_empty());
        assert_eq!(s.meta.last_turn_id, 0);
    }

    /// 序列化字段名对齐 §7:contractVersion 用 camelCase,stat_data 保持 snake_case。
    #[test]
    fn state_serializes_camel_case() {
        let s = KaleidoState::initial(1, json!({ "a": 1 }));
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["contractVersion"], 1);
        assert_eq!(v["stat_data"], json!({ "a": 1 }));
        assert_eq!(v["revision"]["seq"], 0);
    }

    /// apply_contract_defaults:缺失/null 字段补 default;已有值与未声明字段不动。
    #[test]
    fn defaults_fill_missing_without_overwrite() {
        use super::super::field::{FieldDef, FieldType, UpdateMode};
        let field = |path: &str, default: Option<Value>| FieldDef {
            path: path.to_string(),
            kind: FieldType::Number,
            default,
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
        };
        let mut contract: super::super::Contract = serde_json::from_value(json!({
            "version": 1, "id": "t", "schema": { "properties": {} },
            "updateRules": {}, "guardrails": {}
        }))
        .unwrap();
        contract.update_rules = [
            ("心之所向.好感度", field("心之所向.好感度", Some(json!(0)))),
            ("心之所向.信任度", field("心之所向.信任度", Some(json!(10)))),
            ("心之所向.心情", field("心之所向.心情", Some(json!("平静")))),
            ("无默认", field("无默认", None)),
        ]
        .into_iter()
        .map(|(k, f)| (k.to_string(), f))
        .collect();
        // 好感度已有值;信任度为 null;心情缺失;无默认字段缺失
        let mut tree = json!({ "心之所向": { "好感度": 5, "信任度": null } });
        let filled = apply_contract_defaults(&contract, &mut tree);
        assert_eq!(filled, 2, "只填信任度(null)与心情(缺失)");
        assert_eq!(tree["心之所向"]["好感度"], json!(5), "已有值不动");
        assert_eq!(tree["心之所向"]["信任度"], json!(10), "null 补默认");
        assert_eq!(tree["心之所向"]["心情"], json!("平静"), "缺失补默认");
        assert!(
            tree.get("无默认").is_none(),
            "无 default 的字段不造值"
        );
    }

    /// apply_contract_defaults:空树(老卡无 InitVar)也能按契约骨架建出嵌套层。
    #[test]
    fn defaults_build_nested_tree_from_empty() {
        use super::super::field::{FieldDef, FieldType, UpdateMode};
        let mut contract: super::super::Contract = serde_json::from_value(json!({
            "version": 1, "id": "t", "schema": { "properties": {} },
            "updateRules": {}, "guardrails": {}
        }))
        .unwrap();
        contract.update_rules = [(
            "a.b.c".to_string(),
            FieldDef {
                path: "a.b.c".into(),
                kind: FieldType::Number,
                default: Some(json!(1)),
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
            },
        )]
        .into_iter()
        .collect();
        let mut tree = json!({});
        let filled = apply_contract_defaults(&contract, &mut tree);
        assert_eq!(filled, 1);
        assert_eq!(tree["a"]["b"]["c"], json!(1));
    }
}
