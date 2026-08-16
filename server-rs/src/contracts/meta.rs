// 运行时状态元数据(§7-KaleidoState.meta 子集)。
//
// P1 只需 meta(供 due_fields 读 lastTurnId、observe 读 pending);
// 完整的 KaleidoState(含 changelog/checkpoints)在 P3 changelog 落地时补齐。
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::multi_step::op_hash;
use super::op::PatchOp;
use super::Confidence;

/// pending 队列长度上限(超限丢最旧,防低置信 op 反复入队膨胀)。
pub const MAX_PENDING: usize = 32;

/// 待复核 op(§7-PendingOp):低置信或校验失败入队,等待 Agent 自纠或作者确认。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingOp {
    pub id: String,
    pub op: PatchOp,
    /// 产生该 pending 的轮次
    pub created_at_turn: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// 运行时元数据(§7-KaleidoState.meta)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KaleidoMeta {
    /// 最近完成的正文轮次(dueFields 本地确定性调度的锚点)
    pub last_turn_id: u64,
    /// 当前周目号(§16.1,默认 0)
    #[serde(default)]
    pub run_id: u64,
    /// 各字段最近一次写入的置信度(path → confidence)
    #[serde(default)]
    pub confidence: BTreeMap<String, Confidence>,
    /// 待复核队列
    #[serde(default)]
    pub pending: Vec<PendingOp>,
    /// 最近一次生效的契约版本(调和用)
    #[serde(default)]
    pub last_contract_version: u32,
}

/// 合并本轮 pending 入队(P6 自纠前置):消费 → 去重 → 上限裁剪。
///
/// - 消费:既有 pending 中 path 已被本轮成功应用的条目移除(低置信提议被后续
///   高置信写覆盖,不再等待复核);
/// - 去重:incoming 与队列中 op_hash 相同(op+path+value 指纹)的跳过,防同一
///   低置信 op 每轮重复入队;
/// - 上限:超过 MAX_PENDING 丢最旧。turn_id 是消息数推算的近似值,clear/regreet
///   清空消息后会回退,故按 created_at_turn 显式稳定排序而非依赖队头即最旧。
pub fn merge_pending(
    existing: &[PendingOp],
    incoming: &[PatchOp],
    applied_paths: &[&str],
    turn_id: u64,
) -> Vec<PendingOp> {
    let mut out: Vec<PendingOp> = existing
        .iter()
        .filter(|p| !applied_paths.contains(&p.op.path.as_str()))
        .cloned()
        .collect();
    for op in incoming {
        let h = op_hash(op);
        if out.iter().any(|p| op_hash(&p.op) == h) {
            continue;
        }
        out.push(PendingOp {
            id: uuid::Uuid::new_v4().to_string(),
            op: op.clone(),
            created_at_turn: turn_id,
            rationale: op.rationale.clone(),
        });
    }
    if out.len() > MAX_PENDING {
        out.sort_by_key(|p| p.created_at_turn);
        let drop_n = out.len() - MAX_PENDING;
        out.drain(0..drop_n);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// meta 序列化字段名对齐 camelCase;空可选字段省略。
    #[test]
    fn meta_serializes_camel_case() {
        let m = KaleidoMeta::default();
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["lastTurnId"], 0);
        assert_eq!(v["runId"], 0);
        assert!(v.get("pending").is_some_and(|p| p.as_array().is_some_and(|a| a.is_empty())));
    }

    /// 反序列化缺字段用默认填充(向后兼容)。
    #[test]
    fn meta_deserializes_with_defaults() {
        let m: KaleidoMeta = serde_json::from_value(json!({ "lastTurnId": 42 })).unwrap();
        assert_eq!(m.last_turn_id, 42);
        assert_eq!(m.run_id, 0);
        assert!(m.pending.is_empty());
    }

    fn op(path: &str, value: i64) -> PatchOp {
        PatchOp {
            op: super::super::OpKind::Replace,
            path: path.to_string(),
            from: None,
            value: Some(json!(value)),
            confidence: None,
            rationale: None,
        }
    }

    fn pending_at(path: &str, value: i64, turn: u64) -> PendingOp {
        PendingOp {
            id: format!("p-{path}-{turn}"),
            op: op(path, value),
            created_at_turn: turn,
            rationale: None,
        }
    }

    /// merge_pending:同 path 成功应用 → 旧 pending 被消费;同指纹 incoming 去重。
    #[test]
    fn merge_pending_consumes_and_dedupes() {
        let existing = vec![pending_at("好感度", 3, 1), pending_at("情绪", 1, 1)];
        // 本轮成功应用 好感度;incoming 含同指纹 情绪 与新 op 体力
        let incoming = vec![op("情绪", 1), op("体力", 10)];
        let out = merge_pending(&existing, &incoming, &["好感度"], 5);
        let paths: Vec<&str> = out.iter().map(|p| p.op.path.as_str()).collect();
        assert!(!paths.contains(&"好感度"), "已应用的 path 消费: {paths:?}");
        assert_eq!(out.iter().filter(|p| p.op.path == "情绪").count(), 1, "去重");
        assert!(paths.contains(&"体力"), "新 op 入队: {paths:?}");
        assert!(out.iter().all(|p| p.created_at_turn > 0));
    }

    /// merge_pending:超过 MAX_PENDING 丢最旧。
    #[test]
    fn merge_pending_caps_queue_dropping_oldest() {
        let existing: Vec<PendingOp> = (0..MAX_PENDING as i64)
            .map(|i| pending_at(&format!("f{i}"), i, i as u64 + 1))
            .collect();
        let incoming = vec![op("新字段", 1)];
        let out = merge_pending(&existing, &incoming, &[], 99);
        assert_eq!(out.len(), MAX_PENDING);
        assert_eq!(out.last().unwrap().op.path, "新字段", "最新的在队尾");
        assert_eq!(out.first().unwrap().op.path, "f1", "最旧的 f0 被挤出");
    }
}
