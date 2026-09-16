// 通用循环熔断守卫(2026-09-14,P0-2)。
//
// 背景:系统里有两类「模型反复做同一件事」的死循环风险,此前只有一处有防护:
//   1. 契约多步变量路径 —— 已有熔断(`contracts/multi_step.rs`,基于 PatchOp 哈希);
//   2. **普通 agent 工具循环 —— 没有任何重复调用检测**,唯一终止条件是
//      `max_tool_rounds`。实测(2026-09-14)plan 模式任务在单步反复调用同一工具时,
//      7 分钟烧 150 万 prompt token 仍未收敛,必须人工 stop。
//
// 本模块提供与契约路径同口径的通用算法,供工具循环复用,避免再写一份:
// 把每步的「结构化指纹」写入近 N 步环形缓冲,同一指纹在窗口内出现 ≥K 次即判定熔断。
// 纯函数/无状态依赖(除自身缓冲),符合 utils 层(L1)纪律。
use std::collections::VecDeque;

/// 默认窗口大小(近 N 步历史),与契约路径 BREAK_WINDOW 同口径。
pub const DEFAULT_WINDOW: usize = 8;
/// 默认重复阈值(窗口内同一指纹出现 ≥K 次判定熔断),与契约路径同口径。
pub const DEFAULT_THRESHOLD: usize = 3;

/// FNV-1a 64 位哈希:对多个字符串分段依次混入。用于把「工具名 + 参数」这类
/// 结构化指纹压成一个 u64,避免在缓冲里存完整参数字符串(参数可能极大)。
pub fn fnv1a_hash(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for part in parts {
        for b in part.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        // 分隔符:防止 ("ab","c") 与 ("a","bc") 撞哈希
        h ^= 0xff;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 循环熔断守卫:近 `window` 步内同一指纹出现 ≥ `threshold` 次即触发。
pub struct LoopGuard {
    window: usize,
    threshold: usize,
    history: VecDeque<u64>,
}

impl LoopGuard {
    pub fn new(window: usize, threshold: usize) -> Self {
        Self {
            window: window.max(1),
            threshold: threshold.max(2),
            history: VecDeque::new(),
        }
    }

    /// 使用默认口径(N=8, K=3)。
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_WINDOW, DEFAULT_THRESHOLD)
    }

    /// 记录一个指纹,返回是否应熔断(窗口内该指纹累计次数 ≥ threshold)。
    pub fn record(&mut self, fingerprint: u64) -> bool {
        self.history.push_back(fingerprint);
        while self.history.len() > self.window {
            self.history.pop_front();
        }
        self.history.iter().filter(|h| **h == fingerprint).count() >= self.threshold
    }

    /// 当前窗口内的指纹数量(诊断用)
    pub fn len(&self) -> usize {
        self.history.len()
    }

    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// 窗口内某指纹的出现次数(日志/事件诊断用)
    pub fn count_of(&self, fingerprint: u64) -> usize {
        self.history.iter().filter(|h| **h == fingerprint).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_is_stable_and_separator_aware() {
        assert_eq!(fnv1a_hash(&["a", "b"]), fnv1a_hash(&["a", "b"]));
        assert_ne!(fnv1a_hash(&["a", "b"]), fnv1a_hash(&["b", "a"]));
        // 分隔符必须生效:("ab","c") != ("a","bc")
        assert_ne!(fnv1a_hash(&["ab", "c"]), fnv1a_hash(&["a", "bc"]));
    }

    /// 同指纹连续 3 次熔断;2 次不熔断
    #[test]
    fn breaks_after_threshold_repeats() {
        let mut g = LoopGuard::new(8, 3);
        let h = fnv1a_hash(&["read", "{\"path\":\"/a\"}"]);
        assert!(!g.record(h));
        assert!(!g.record(h));
        assert!(g.record(h), "窗口内第 3 次同指纹应熔断");
    }

    /// 不同指纹交替出现不误判(正常工作的特征:每次调用参数不同,如读不同文件)
    #[test]
    fn distinct_fingerprints_do_not_break() {
        let mut g = LoopGuard::new(8, 3);
        // 合法场景:对多个不同目标依次读-改,每次指纹都不同
        for i in 0..30 {
            let read = fnv1a_hash(&["read", &format!("{{\"path\":\"/file{i}\"}}")]);
            let write = fnv1a_hash(&["replace", &format!("{{\"path\":\"/file{i}\"}}")]);
            assert!(!g.record(read), "第 {i} 轮 read 不应误判");
            assert!(!g.record(write), "第 {i} 轮 replace 不应误判");
        }
    }

    /// 同参数交替重复**属于死循环**(模型在两个相同动作间来回打转,无新信息),
    /// 这是有意熔断的场景——与「参数不同」的合法交替区别在此。
    #[test]
    fn same_args_alternation_is_treated_as_loop() {
        let mut g = LoopGuard::new(8, 3);
        let a = fnv1a_hash(&["bash", "{\"cmd\":\"ls\"}"]);
        let b = fnv1a_hash(&["read", "{\"path\":\"/same\"}"]);
        let mut broke = false;
        for _ in 0..10 {
            if g.record(a) || g.record(b) {
                broke = true;
                break;
            }
        }
        assert!(broke, "同参数在两个动作间来回应判定循环并中止");
    }

    /// 窗口滑动:指纹被挤出窗口后不再计数
    #[test]
    fn old_fingerprints_fall_out_of_window() {
        let mut g = LoopGuard::new(3, 3);
        let h = fnv1a_hash(&["x"]);
        assert!(!g.record(h));
        assert!(!g.record(h));
        // 插入 3 个其它指纹,把前两次 h 挤出窗口
        g.record(fnv1a_hash(&["y1"]));
        g.record(fnv1a_hash(&["y2"]));
        g.record(fnv1a_hash(&["y3"]));
        assert_eq!(g.count_of(h), 0, "h 应已被挤出窗口");
        assert!(!g.record(h), "挤出后重新计数,不应触发");
    }

    /// threshold 下限保护:配置 1 会被钳为 2(避免「首次调用即熔断」)
    #[test]
    fn threshold_is_clamped_to_at_least_two() {
        let mut g = LoopGuard::new(8, 1);
        let h = fnv1a_hash(&["x"]);
        assert!(!g.record(h), "首次不应熔断");
        assert!(g.record(h), "第二次应熔断(阈值被钳为 2)");
    }
}
