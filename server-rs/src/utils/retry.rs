// 截断自愈(truncation heal)预算决策的公共实现(2026-09-13 批次 4.1)。
//
// 背景:上游 finish_reason=length(推理 token 吃光预算,JSON/正文腰斩)时,
// 「翻倍预算原样重发一次」在四条路径上各自实现过一遍,算法等价但常量与触发条件
// 抄了四份,任一路径改口径(例如调高封顶)不会同步,容易出现「同一现象四处语义漂移」:
//   1. api/chat.rs           卡片生成 generate-raw(封顶 32768,最多重发 2 次)
//   2. agents/engine/executor.rs 引擎单轮自愈(封顶 131072;触发条件由 truncation_heal_cause 判定)
//   3. services/task_engine/team.rs  team 审计/终审/汇总(封顶 131072,单次)
//   4. services/task_service/executor.rs 步骤/汇总空输出重试(封顶 131072,封顶后仍同预算重试)
// 本模块只提供三个纯函数,各路径保留各自薄封装(常量与场景注释留在原处),行为不变。

/// 预算翻倍并封顶;返回 None 表示「重发无意义」——结果未超过 current 时:
/// 已达/超过上限(cap)、或 current 为 0(0 翻倍不增长,重发只会用同一预算空烧一次)。
///
/// `saturating_mul` 保证 u32::MAX 附近不溢出(先饱和到 MAX 再与 cap 取小)。
pub fn doubled_heal_budget(current: u32, cap: u32) -> Option<u32> {
    let doubled = current.saturating_mul(2).min(cap);
    (doubled > current).then_some(doubled)
}

/// 带下限的截断自愈预算(2026-09-15 实测):在翻倍之上再取 `floor` 下限,最后封顶。
///
/// 为什么需要下限:调用方可能只给了很小的预算(实测子任务被传入 max_tokens=64),
/// 翻倍后仍是 128 这种「必然被推理烧光」的档位——重发一次照样截断,白烧一个 LLM 往返。
/// 抬到 floor 才能让这一轮有实际产出机会。
///
/// 语义与 `doubled_heal_budget` 兼容:结果不大于 current 时同样返回 None
///(已封顶等),调用方「None 即不重发」的既有判断不变。
pub fn heal_budget_with_floor(current: u32, cap: u32, floor: u32) -> Option<u32> {
    let target = current.saturating_mul(2).max(floor).min(cap);
    (target > current).then_some(target)
}

/// 截断自愈重发的预算下限(2026-09-15 实测):推理模型的 reasoning 与正文共用
/// max_tokens,实测单个子任务光推理就能烧掉近万 token(`reasoning_tokens=9894`)。
/// 低于该值的重发档位(如 64→128)必然「推理耗尽、正文为空」,重发白烧一个往返,
/// 故各路径重发时至少给到这个下限。
pub const HEAL_BUDGET_FLOOR: u32 = 16384;

/// 截断自愈预算:`finish_reason == Some("length")` 且轮次未用尽(rounds < max_rounds)
/// 时按 cap 翻倍,其余情况(非截断原因、reason 缺失、轮次用尽、已封顶)返回 None。
pub fn truncation_heal_budget(
    finish_reason: Option<&str>,
    used: u32,
    rounds: u32,
    cap: u32,
    max_rounds: u32,
) -> Option<u32> {
    if finish_reason == Some("length") && rounds < max_rounds {
        doubled_heal_budget(used, cap)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 常规翻倍:未触及上限时返回两倍值
    #[test]
    fn doubled_grows_below_cap() {
        assert_eq!(doubled_heal_budget(1024, 32768), Some(2048));
        assert_eq!(doubled_heal_budget(4096, 65536), Some(8192));
    }

    /// 精确命中上限:翻倍结果恰好等于 cap 时仍返回 cap(相对 current 仍是增长)
    #[test]
    fn doubled_reaches_cap_exactly() {
        assert_eq!(doubled_heal_budget(16384, 32768), Some(32768));
        // 8192 翻倍即达 8192 封顶(cap 本身不大于 current*2)
        assert_eq!(doubled_heal_budget(4096, 8192), Some(8192));
    }

    /// 已在上限 / 超过上限:结果不再增长 → None(重发无意义)
    #[test]
    fn doubled_none_when_already_capped() {
        assert_eq!(doubled_heal_budget(32768, 32768), None, "已达封顶");
        assert_eq!(doubled_heal_budget(65536, 32768), None, "已超封顶");
    }

    /// 饱和相乘:u32::MAX 附近不得溢出,也不得把「无增长」误判成可重发
    #[test]
    fn doubled_saturates_without_overflow() {
        assert_eq!(doubled_heal_budget(u32::MAX, u32::MAX), None);
        assert_eq!(doubled_heal_budget(u32::MAX, 65536), None);
        assert_eq!(
            doubled_heal_budget(u32::MAX / 2, u32::MAX),
            Some(u32::MAX - 1),
            "饱和相乘不得回绕"
        );
    }

    /// 边界:cap=0 或 current=0 时都无法增长,一律 None
    #[test]
    fn doubled_none_on_zero_boundaries() {
        assert_eq!(doubled_heal_budget(1024, 0), None);
        assert_eq!(doubled_heal_budget(0, 0), None);
        assert_eq!(doubled_heal_budget(0, 32768), None);
    }

    /// 带下限的自愈预算(2026-09-15):小预算抬到 floor,下限之上仍按翻倍,
    /// 结果一律受 cap 约束;无增长时返回 None(与 doubled 语义兼容)。
    #[test]
    fn heal_budget_with_floor_lifts_small_budgets() {
        // 实测子任务形态:64 翻倍只有 128,必然被推理烧光 → 抬到下限
        assert_eq!(heal_budget_with_floor(64, 131_072, 16_384), Some(16_384));
        assert_eq!(heal_budget_with_floor(1024, 131_072, 16_384), Some(16_384));
        // 下限之上:翻倍仍生效(下限不得压低既有增长)
        assert_eq!(
            heal_budget_with_floor(16_384, 131_072, 16_384),
            Some(32_768)
        );
        assert_eq!(
            heal_budget_with_floor(40_000, 131_072, 16_384),
            Some(80_000)
        );
        // 封顶优先于下限
        assert_eq!(
            heal_budget_with_floor(70_000, 131_072, 16_384),
            Some(131_072)
        );
        // 已达封顶:无增长即 None
        assert_eq!(heal_budget_with_floor(131_072, 131_072, 16_384), None);
        // floor 高于 cap 时不得越界 cap
        assert_eq!(heal_budget_with_floor(1024, 8192, 16_384), Some(8192));
        // floor=0 时退化为既有 doubled 语义
        assert_eq!(heal_budget_with_floor(1024, 32768, 0), Some(2048));
        assert_eq!(heal_budget_with_floor(0, 32768, 0), None);
    }

    /// 截断自愈:仅 finish_reason=length 触发;轮次用尽后不再重发
    #[test]
    fn truncation_heal_only_on_length_within_rounds() {
        assert_eq!(
            truncation_heal_budget(Some("length"), 8192, 0, 32768, 2),
            Some(16384)
        );
        assert_eq!(
            truncation_heal_budget(Some("length"), 8192, 1, 32768, 2),
            Some(16384)
        );
        assert_eq!(
            truncation_heal_budget(Some("length"), 8192, 2, 32768, 2),
            None,
            "轮次用尽"
        );
        assert_eq!(
            truncation_heal_budget(Some("stop"), 1024, 0, 32768, 2),
            None,
            "非截断原因不重发"
        );
        assert_eq!(
            truncation_heal_budget(None, 1024, 0, 32768, 2),
            None,
            "无 finish_reason 不重发"
        );
    }

    /// 单次重发路径(team):max_rounds=1 时首轮触发,第 1 轮起不再触发
    #[test]
    fn truncation_heal_single_round_limit() {
        assert_eq!(
            truncation_heal_budget(Some("length"), 40000, 0, 65536, 1),
            Some(65536)
        );
        assert_eq!(
            truncation_heal_budget(Some("length"), 40000, 1, 65536, 1),
            None
        );
    }
}
