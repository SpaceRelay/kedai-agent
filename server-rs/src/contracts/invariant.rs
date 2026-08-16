// 契约不变量(§7-Invariant):跨字段约束,提交前由 checkInvariants 校验。
use serde::{Deserialize, Serialize};

/// 不变量种类(§7-Invariant.kind)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvariantKind {
    /// 条件成立时要求字段存在/满足(condition 为白名单谓词)
    RequireIf,
    /// 互斥:paths 内字段不可同时为非空/非默认
    Mutex,
    /// 数值范围约束
    Range,
}

/// 跨字段不变量(§7-Invariant)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invariant {
    pub id: String,
    pub kind: InvariantKind,
    /// 涉及字段的点分路径
    pub paths: Vec<String>,
    /// 白名单谓词(禁止任意 eval,见 §17.8 条件条目系统)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// 违反时向作者/面板展示的信息
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invariant_serializes_kind_snake_case() {
        let inv = Invariant {
            id: "hp_range".into(),
            kind: InvariantKind::Range,
            paths: vec!["角色.hp".into()],
            condition: None,
            message: "hp 超出 0..=100".into(),
        };
        let v = serde_json::to_value(&inv).unwrap();
        assert_eq!(v["kind"], "range");
        assert_eq!(v["id"], "hp_range");
        assert!(v.get("condition").is_none());
    }
}
