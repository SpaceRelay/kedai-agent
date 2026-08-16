// 多步 Agent 死循环熔断(§10.1):防「固执的局部最优」。
//
// maxSteps 挡住无限循环,但挡不住兜圈子——Agent 反复输出同一条被拒绝/低置信 op。
// 熔断算法:每步把「失败原因 + 被拒 op + 低置信 op」的结构化哈希写入步历史(近 N 步
// 环形缓冲);近 N 步内同一 op 哈希重复 ≥3 次 → 判定 loop_broken,停止并转 pending。
// 纯函数,可单测。
use super::op::PatchOp;

/// 环形缓冲大小(近 N 步历史,§10.1 默认 N=8)。
pub const BREAK_WINDOW: usize = 8;
/// 同一 op 哈希重复阈值(≥3 判定熔断)。
pub const BREAK_THRESHOLD: usize = 3;

/// 结构化 op 哈希:op 种类 + path + value(被拒/低置信 op 的指纹)。
/// 失败原因类条目用固定 tag 加进哈希空间,避免与正常 op 冲突。
pub fn op_hash(op: &PatchOp) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let tag = match op.op {
        super::OpKind::Replace => "replace",
        super::OpKind::Delta => "delta",
        super::OpKind::Add => "add",
        super::OpKind::Remove => "remove",
        super::OpKind::Move => "move",
    };
    for b in tag.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    for b in op.path.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    if let Some(v) = &op.value {
        for b in v.to_string().bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    if let Some(f) = &op.from {
        for b in f.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

/// 失败/被拒条目哈希(与 op 哈希区分,记录「为什么失败」)。
pub fn failure_hash(reason: &str, op: Option<&PatchOp>) -> u64 {
    let mut h: u64 = 0x9e3779b97f4a7c15;
    for b in reason.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x5f356495);
    }
    if let Some(op) = op {
        h ^= op_hash(op).wrapping_mul(0x100000001b3);
    }
    h
}

/// 在环形历史里记录一个新哈希,返回是否触发熔断。
/// history 按时间序保留最近 BREAK_WINDOW 个;count 记录各哈希出现次数。
/// 返回 true = 近窗口内同一哈希 ≥ BREAK_THRESHOLD 次 → 应熔断。
pub fn record_and_check(history: &mut Vec<u64>, hash: u64) -> bool {
    history.push(hash);
    while history.len() > BREAK_WINDOW {
        history.remove(0);
    }
    let count = history.iter().filter(|h| **h == hash).count();
    count >= BREAK_THRESHOLD
}

/// 直接判定:给定窗口内哈希列表,是否触发熔断(无副作用,供测试/纯函数复用)。
pub fn would_break(history: &[u64], hash: u64) -> bool {
    history.iter().filter(|h| **h == hash).count() + 1 >= BREAK_THRESHOLD
}

/// 汇总 rejected/pending 为失败哈希(供多步循环每步记录)。
pub fn rejection_hashes(rejected: &[super::RejectedOp], pending: &[PatchOp]) -> Vec<u64> {
    let mut out = Vec::new();
    for r in rejected {
        out.push(failure_hash(&r.reason, Some(&r.op)));
    }
    for p in pending {
        out.push(op_hash(p));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn replace(path: &str, value: i64) -> PatchOp {
        PatchOp {
            op: super::super::OpKind::Replace,
            path: path.to_string(),
            from: None,
            value: Some(Value::from(value)),
            confidence: None,
            rationale: None,
        }
    }

    /// 相同 op(同 path+value)哈希一致;不同 op 哈希不同。
    #[test]
    fn op_hash_stable_and_distinct() {
        assert_eq!(op_hash(&replace("a", 1)), op_hash(&replace("a", 1)));
        assert_ne!(op_hash(&replace("a", 1)), op_hash(&replace("a", 2)));
        assert_ne!(op_hash(&replace("a", 1)), op_hash(&replace("b", 1)));
    }

    /// 近窗口内同一哈希出现 3 次 → 熔断;2 次不熔断。
    #[test]
    fn record_and_check_breaks_after_three() {
        let h = op_hash(&replace("a", 1));
        let mut history: Vec<u64> = Vec::new();
        assert!(!record_and_check(&mut history, h));
        assert!(!record_and_check(&mut history, h));
        assert!(record_and_check(&mut history, h), "第 3 次应熔断");
    }

    /// 窗口滑动:超过 8 步后旧哈希被挤出,不参与计数。
    #[test]
    fn window_slides_out_old_entries() {
        let h = op_hash(&replace("a", 1));
        let mut history: Vec<u64> = Vec::new();
        // 先记录 7 个不同哈希,再连续 2 次 h(第 8、9 步)
        for i in 0..7 {
            history.push(op_hash(&replace("x", i)));
        }
        assert!(!record_and_check(&mut history, h)); // 第 8 步
        assert!(!record_and_check(&mut history, h)); // 第 9 步,窗口内 h 仅 2 次(最早的 7 个不同哈希把 h 挤出)
        assert_eq!(history.len(), BREAK_WINDOW);
    }

    /// rejected(带原因)与 pending 生成熔断用哈希。
    #[test]
    fn rejection_hashes_covers_both() {
        let rejected = vec![super::super::RejectedOp {
            op: replace("a", 5),
            reason: "not_owner".into(),
        }];
        let pending = vec![replace("b", 7)];
        let hashes = rejection_hashes(&rejected, &pending);
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes[0], failure_hash("not_owner", Some(&replace("a", 5))));
        assert_eq!(hashes[1], op_hash(&replace("b", 7)));
    }
}
