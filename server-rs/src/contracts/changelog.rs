// changelog 两段式(§4.4.1):近期 N 条 append-only log + 周期 full checkpoint。
//
// 恢复基底 = checkpoint 完整快照 + 其后的 log 重放;恢复时 checkpoint 缺失/损坏按
// §4.4.1 的四类降级策略处理(降级逻辑在持久化层接线时实现,本模块提供纯函数:
// should_checkpoint 判定 + recover 重放)。
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::op::PatchOp;
use super::Confidence;

/// changelog 写者来源(§7-ChangelogEntry.source)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangelogSource {
    Agent,
    Manual,
    ContractInit,
    Rollback,
    Import,
    Repair,
}

/// 单条 changelog 记录(§7-ChangelogEntry):append-only。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangelogEntry {
    /// 全局单调序号(commit 串行器分配)
    pub seq: u64,
    /// 产生该变更的轮次
    pub turn_id: u64,
    /// 生效的 op(回放/审计的权威操作)
    pub op: PatchOp,
    /// 目标字段路径
    pub path: String,
    /// 变更前值(回滚基线)
    pub old: Value,
    /// 变更后值
    pub new: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    pub confidence: Confidence,
    pub source: ChangelogSource,
}

/// checkpoint 触发原因(§7-StateCheckpoint.reason)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointReason {
    Periodic,
    Manual,
    ContractChange,
    Rollback,
    Import,
    Repair,
}

/// 周期 full checkpoint(§7-StateCheckpoint):stat_data 完整快照,恢复基底。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateCheckpoint {
    /// 快照时的全局 seq(恢复 = 本快照 + seq 大于此值的 log 重放)
    pub seq: u64,
    pub reason: CheckpointReason,
    /// stat_data 领域术语,序列化保持 snake_case
    #[serde(rename = "stat_data")]
    pub stat_data: Value,
    /// 创建时间(Unix 毫秒)
    pub created_at: u64,
}

/// changelog 可配置参数(§4.4.1 全参数可配置)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangelogConfig {
    /// 轮次触发:距上次 checkpoint 满 N 轮
    pub checkpoint_interval_rounds: u32,
    /// 时间触发:距上次 checkpoint 满 N 小时
    pub checkpoint_interval_hours: u32,
    /// 体积触发:未归档 log 序列化 ≥ N KB
    pub checkpoint_log_size_kb: u32,
    /// 近期 append-only log 条数上限(超限触发 checkpoint 后裁剪最旧)
    pub max_log_entries: usize,
    /// 恢复性能上限(ms)
    pub replay_max_ms: u64,
    /// log 重放条数硬上限
    pub replay_max_entries: usize,
}

impl Default for ChangelogConfig {
    fn default() -> Self {
        ChangelogConfig {
            checkpoint_interval_rounds: 50,
            checkpoint_interval_hours: 24,
            checkpoint_log_size_kb: 100,
            max_log_entries: 1000,
            replay_max_ms: 2000,
            replay_max_entries: 5000,
        }
    }
}

/// 是否应写 checkpoint(§4.4.1 三条件先到先触发,纯函数)。
pub fn should_checkpoint(
    cfg: &ChangelogConfig,
    rounds_since_checkpoint: u64,
    hours_since_checkpoint: f64,
    log_size_bytes: u64,
) -> bool {
    rounds_since_checkpoint >= u64::from(cfg.checkpoint_interval_rounds)
        || hours_since_checkpoint >= f64::from(cfg.checkpoint_interval_hours)
        || log_size_bytes / 1024 >= u64::from(cfg.checkpoint_log_size_kb)
}

/// 从 checkpoint + 其后的 log 重放恢复 stat_data(纯函数)。
///
/// 只应用 seq 大于 checkpoint.seq 的条目;entries 未排序时先按 seq 升序排序,
/// 保证重放顺序与 commit 顺序一致。
pub fn recover(checkpoint: &StateCheckpoint, entries: &[ChangelogEntry]) -> Result<Value, String> {
    let mut sorted: Vec<&ChangelogEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| e.seq);
    let mut tree = checkpoint.stat_data.clone();
    for entry in sorted {
        if entry.seq <= checkpoint.seq {
            continue;
        }
        super::op::apply_ops(&mut tree, std::slice::from_ref(&entry.op))?;
    }
    Ok(tree)
}

/// 由「应用前后的树 + 生效 op 列表」生成 changelog 条目(P5 运行态接线,纯函数)。
///
/// 每条 entry:old 取 tree_before 按路径取值(取不到按 Null),new 取 tree_after 同路径;
/// confidence 缺省视为 High(与置信度门控语义一致);seq 置 0(commit 串行器回填权威值)。
/// 路径取值语义与 AssistantVars::apply_patches 一致(split_path/path_get 同源)。
pub fn entries_from_applied(
    tree_before: &Value,
    tree_after: &Value,
    turn_id: u64,
    applied: &[PatchOp],
    source: ChangelogSource,
) -> Vec<ChangelogEntry> {
    applied
        .iter()
        .map(|op| ChangelogEntry {
            seq: 0,
            turn_id,
            op: op.clone(),
            path: op.path.clone(),
            old: path_get(tree_before, &op.path),
            new: path_get(tree_after, &op.path),
            rationale: op.rationale.clone(),
            confidence: op.confidence.unwrap_or(Confidence::High),
            source,
        })
        .collect()
}

/// 按点路径读树值(与 AssistantVars 路径语义同源:剥 stat_data 前缀、点/斜杠分隔);取不到 Null。
fn path_get(tree: &Value, path: &str) -> Value {
    let segs = crate::parsing::assistant::split_path(path);
    crate::parsing::assistant::path_get(tree, &segs)
        .cloned()
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(seq: u64, path: &str, op: PatchOp, new: Value) -> ChangelogEntry {
        ChangelogEntry {
            seq,
            turn_id: 1,
            op,
            path: path.to_string(),
            old: Value::Null,
            new,
            rationale: None,
            confidence: Confidence::High,
            source: ChangelogSource::Agent,
        }
    }

    /// should_checkpoint 三条件各自独立触发,任一满足即 true。
    #[test]
    fn should_checkpoint_any_condition_triggers() {
        let cfg = ChangelogConfig::default();
        // 轮次满 50
        assert!(should_checkpoint(&cfg, 50, 0.0, 0));
        assert!(!should_checkpoint(&cfg, 49, 0.0, 0));
        // 时间满 24h
        assert!(should_checkpoint(&cfg, 0, 24.0, 0));
        assert!(!should_checkpoint(&cfg, 0, 23.9, 0));
        // 体积满 100KB
        assert!(should_checkpoint(&cfg, 0, 0.0, 100 * 1024));
        assert!(!should_checkpoint(&cfg, 0, 0.0, 99 * 1024));
        // 都不满足
        assert!(!should_checkpoint(&cfg, 0, 0.0, 0));
    }

    /// recover:checkpoint 快照 + 其后 log 重放,顺序按 seq 升序。
    #[test]
    fn recover_replays_entries_after_checkpoint() {
        let checkpoint = StateCheckpoint {
            seq: 10,
            reason: CheckpointReason::Periodic,
            stat_data: json!({ "a": 1 }),
            created_at: 0,
        };
        let entries = vec![
            // seq 12 在 checkpoint 之后,先插入到中间位置测试排序
            entry(
                12,
                "a",
                PatchOp {
                    op: super::super::OpKind::Replace,
                    path: "a".into(),
                    from: None,
                    value: Some(json!(3)),
                    confidence: None,
                    rationale: None,
                },
                json!(3),
            ),
            // seq 11 也在之后,但插入顺序靠后,验证按 seq 排序
            entry(
                11,
                "b",
                PatchOp {
                    op: super::super::OpKind::Add,
                    path: "b".into(),
                    from: None,
                    value: Some(json!(2)),
                    confidence: None,
                    rationale: None,
                },
                json!(2),
            ),
            // seq 9 在 checkpoint 之前 → 忽略
            entry(
                9,
                "a",
                PatchOp {
                    op: super::super::OpKind::Replace,
                    path: "a".into(),
                    from: None,
                    value: Some(json!(99)),
                    confidence: None,
                    rationale: None,
                },
                json!(99),
            ),
        ];
        let tree = recover(&checkpoint, &entries).unwrap();
        // seq 11 先应用(b=2),seq 12 再应用(a=3);seq 9 忽略
        assert_eq!(tree, json!({ "a": 3, "b": 2 }));
    }

    /// ChangelogEntry 序列化字段名对齐 §7 camelCase。
    #[test]
    fn changelog_entry_serializes_camel_case() {
        let mut e = entry(
            1,
            "a",
            PatchOp {
                op: super::super::OpKind::Replace,
                path: "a".into(),
                from: None,
                value: Some(json!(5)),
                confidence: Some(Confidence::Medium),
                rationale: Some("剧情推进".into()),
            },
            json!(5),
        );
        e.confidence = Confidence::Medium;
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["seq"], 1);
        assert_eq!(v["turnId"], 1);
        assert_eq!(v["confidence"], "medium");
        assert_eq!(v["source"], "agent");
    }

    /// entries_from_applied:replace 前后 old/new 取值正确、置信度缺省 High、seq 置 0。
    #[test]
    fn entries_from_applied_replace_old_new() {
        let before = json!({ "心之所向": { "好感度": 0 } });
        let after = json!({ "心之所向": { "好感度": 150 } });
        let applied = vec![PatchOp::replace("心之所向.好感度", json!(150))];
        let entries = entries_from_applied(&before, &after, 3, &applied, ChangelogSource::Agent);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.seq, 0, "seq 由 commit 串行器回填,生成时置 0");
        assert_eq!(e.turn_id, 3);
        assert_eq!(e.path, "心之所向.好感度");
        assert_eq!(e.old, json!(0));
        assert_eq!(e.new, json!(150));
        assert_eq!(e.confidence, Confidence::High, "缺省置信度视为 High");
        assert_eq!(e.source, ChangelogSource::Agent);
    }

    /// entries_from_applied:add(新增路径)old 为 Null、remove 后 new 为 Null;
    /// 斜杠路径与点路径同义。
    #[test]
    fn entries_from_applied_add_remove_and_slash_path() {
        let before = json!({ "a": { "x": 1 } });
        let after = json!({ "a": { "y": 2 } });
        let applied = vec![
            PatchOp {
                op: super::super::OpKind::Add,
                path: "a.y".into(),
                from: None,
                value: Some(json!(2)),
                confidence: Some(Confidence::Medium),
                rationale: Some("新增理由".into()),
            },
            PatchOp {
                op: super::super::OpKind::Remove,
                path: "/a/x".into(),
                from: None,
                value: None,
                confidence: None,
                rationale: None,
            },
        ];
        let entries = entries_from_applied(&before, &after, 1, &applied, ChangelogSource::Agent);
        assert_eq!(entries.len(), 2);
        // add:old 取不到 → Null;置信度/理由透传
        assert_eq!(entries[0].old, Value::Null);
        assert_eq!(entries[0].new, json!(2));
        assert_eq!(entries[0].confidence, Confidence::Medium);
        assert_eq!(entries[0].rationale.as_deref(), Some("新增理由"));
        // remove(斜杠路径):new 取不到 → Null
        assert_eq!(entries[1].old, json!(1));
        assert_eq!(entries[1].new, Value::Null);
    }

    /// entries_from_applied:空 applied → 空列表(无契约/全被拒时不产生 entries)。
    #[test]
    fn entries_from_applied_empty() {
        let tree = json!({ "a": 1 });
        let entries = entries_from_applied(&tree, &tree, 7, &[], ChangelogSource::Agent);
        assert!(entries.is_empty());
    }
}
