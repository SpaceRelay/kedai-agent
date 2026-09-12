// 缓存诊断(缓存感知管线):纯函数模块,供 /api/diagnostics/cache 端点复用。
// 职责:
//   - 汇总近 N 条请求的缓存命中/未命中 token(加权命中率、费用与节省估算);
//   - 上下文四级水位报告(soft 0.5 / snip 0.6 / compact 0.8 / force 0.9,
//     与 docs/learn-harness-2026-08.md 的 estimate.mjs level+gaps 算法对齐):
//     输入 tokens 对照 max_context_tokens 判档,并给出距下一档的 token gap。
// 单价为「每百万 token」结构体(默认 DeepSeek 参考价:命中 0.27 / 输入 2 / 输出 8 元),
// 不做货币换算;后续可挪到设置项。
use serde::Serialize;

/// 每百万 token 单价(元/百万):cache_hit=input 命中价、input=未命中全价、output=输出价
#[derive(Debug, Clone, PartialEq)]
pub struct CachePricing {
    pub cache_hit_per_m: f64,
    pub input_per_m: f64,
    pub output_per_m: f64,
}

impl Default for CachePricing {
    /// DeepSeek 参考价(2026-08):命中 ¥0.27/M、输入 ¥2/M、输出 ¥8/M
    fn default() -> Self {
        CachePricing {
            cache_hit_per_m: 0.27,
            input_per_m: 2.0,
            output_per_m: 8.0,
        }
    }
}

/// 单条请求的缓存统计(来自 llm_requests 行)
#[derive(Debug, Clone)]
pub struct CacheUsageRow {
    pub session_id: String,
    pub created_at: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub hit: i64,
    pub miss: i64,
}

/// 汇总结果(端点 JSON 的 totals 部分)
#[derive(Debug, Clone, Serialize)]
pub struct CacheSummary {
    pub count: usize,
    pub total_prompt: i64,
    pub total_completion: i64,
    pub total_hit: i64,
    pub total_miss: i64,
    /// 加权 token 口径命中率:total_hit / (total_hit + total_miss);
    /// 无缓存数据的请求(hit+miss 全 0)不参与分母,全无数据时为 None
    pub hit_rate: Option<f64>,
    /// 估算费用(元):命中部分按命中价 + 未命中部分按输入价 + 输出部分按输出价
    pub cost: f64,
    /// 估算节省(元):命中 token 本应按输入全价支付,节省 = hit × (input - hit 单价差)
    pub saved: f64,
}

/// 汇总近 N 条请求(时间正序)的缓存统计
pub fn summarize(rows: &[CacheUsageRow], pricing: &CachePricing) -> CacheSummary {
    let mut total_prompt = 0i64;
    let mut total_completion = 0i64;
    let mut total_hit = 0i64;
    let mut total_miss = 0i64;
    for r in rows {
        total_prompt += r.prompt_tokens;
        total_completion += r.completion_tokens;
        total_hit += r.hit;
        total_miss += r.miss;
    }
    let denom = total_hit + total_miss;
    let hit_rate = (denom > 0).then(|| total_hit as f64 / denom as f64);
    let cost = (total_hit as f64 * pricing.cache_hit_per_m
        + total_miss as f64 * pricing.input_per_m
        + total_completion as f64 * pricing.output_per_m)
        / 1_000_000.0;
    let unit_gap = pricing.input_per_m - pricing.cache_hit_per_m;
    let saved = if unit_gap > 0.0 {
        total_hit as f64 * unit_gap / 1_000_000.0
    } else {
        0.0
    };
    CacheSummary {
        count: rows.len(),
        total_prompt,
        total_completion,
        total_hit,
        total_miss,
        hit_rate,
        cost,
        saved,
    }
}

/// 上下文四级水位报告(输入侧)
#[derive(Debug, Clone, Serialize)]
pub struct WatermarkReport {
    /// ok(<0.5) / soft(>=0.5) / snip(>=0.6) / compact(>=0.8) / force(>=0.9) / unknown(未配置上限)
    pub level: &'static str,
    /// input_tokens / max_context;未配置上限时 None
    pub ratio: Option<f64>,
    /// 下一档位(None = 已在最高档 force)
    pub next_level: Option<&'static str>,
    /// 距下一档还差多少 token(向上取整到整数;None = 无下一档或未配置)
    pub gap_tokens: Option<i64>,
}

/// 四级水位阶梯(缓存感知管线):(阈值, 档位, 下一档)
const WATERMARK_LADDER: [(f64, &str, &str); 4] = [
    (0.5, "soft", "snip"),
    (0.6, "snip", "compact"),
    (0.8, "compact", "force"),
    (0.9, "force", "force"),
];

/// 判定输入 token 的水位档位并计算距下一档 gap。
/// max_context 为 0(未配置)时返回 unknown,不参与档位判定。
pub fn watermark(input_tokens: i64, max_context: u32) -> WatermarkReport {
    if max_context == 0 {
        return WatermarkReport {
            level: "unknown",
            ratio: None,
            next_level: None,
            gap_tokens: None,
        };
    }
    let max = max_context as i64;
    let ratio = input_tokens as f64 / max as f64;
    // 找到第一个已达阈值的档;全未达到 → ok,下一档 soft
    let mut current: Option<(f64, &str, &str)> = None;
    for (threshold, level, next) in WATERMARK_LADDER {
        if ratio >= threshold {
            current = Some((threshold, level, next));
        }
    }
    match current {
        None => {
            let next_at = (0.5 * max as f64).round() as i64;
            WatermarkReport {
                level: "ok",
                ratio: Some(ratio),
                next_level: Some("soft"),
                gap_tokens: Some((next_at - input_tokens).max(0)),
            }
        }
        Some((_, "force", _)) => WatermarkReport {
            level: "force",
            ratio: Some(ratio),
            next_level: None,
            gap_tokens: None,
        },
        Some((threshold, level, next)) => {
            // 下一档阈值:阶梯中 level 之后的第一个更高阈值
            let next_threshold = WATERMARK_LADDER
                .iter()
                .find(|(t, _, _)| *t > threshold)
                .map(|(t, _, _)| *t)
                .unwrap_or(0.9);
            let next_at = (next_threshold * max as f64).round() as i64;
            WatermarkReport {
                level,
                ratio: Some(ratio),
                next_level: Some(next),
                gap_tokens: Some((next_at - input_tokens).max(0)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(hit: i64, miss: i64, completion: i64) -> CacheUsageRow {
        CacheUsageRow {
            session_id: "s1".into(),
            created_at: "2026-08-16T00:00:00Z".into(),
            prompt_tokens: hit + miss,
            completion_tokens: completion,
            hit,
            miss,
        }
    }

    /// 加权命中率与费用/节省:期望值来自独立手算
    /// (hit 1300、miss 700、completion 1200;单价 0.27/2/8 元每百万)
    #[test]
    fn summarize_computes_weighted_rate_and_cost() {
        let rows = vec![row(700, 300, 200), row(600, 400, 1000)];
        let s = summarize(&rows, &CachePricing::default());
        assert_eq!(s.count, 2);
        assert_eq!(s.total_hit, 1300);
        assert_eq!(s.total_miss, 700);
        assert_eq!(s.total_prompt, 2000);
        assert_eq!(s.total_completion, 1200);
        let rate = s.hit_rate.expect("有缓存数据应有命中率");
        assert!(
            (rate - 0.65).abs() < 1e-9,
            "加权命中率应为 0.65,实际 {rate}"
        );
        // 费用 = (1300*0.27 + 700*2 + 1200*8) / 1e6 = (351+1400+9600)/1e6
        assert!(
            (s.cost - 0.011351).abs() < 1e-9,
            "费用应为 0.011351 元,实际 {}",
            s.cost
        );
        // 节省 = 1300*(2-0.27)/1e6 = 2249/1e6
        assert!(
            (s.saved - 0.002249).abs() < 1e-9,
            "节省应为 0.002249 元,实际 {}",
            s.saved
        );
    }

    /// 无缓存数据(提供商不回报缓存字段)时命中率为 None,费用仍按输入全价计
    #[test]
    fn summarize_without_cache_data_returns_none_rate() {
        let rows = vec![row(0, 0, 100)];
        let s = summarize(&rows, &CachePricing::default());
        assert!(s.hit_rate.is_none(), "无缓存数据命中率应为 null");
        // miss=0 时费用只剩输出:100*8/1e6
        assert!((s.cost - 0.0008).abs() < 1e-12);
    }

    /// 空数据:count 0、rate None、零费用
    #[test]
    fn summarize_empty() {
        let s = summarize(&[], &CachePricing::default());
        assert_eq!(s.count, 0);
        assert!(s.hit_rate.is_none());
        assert_eq!(s.cost, 0.0);
        assert_eq!(s.saved, 0.0);
    }

    /// 自定义单价生效(非 DeepSeek 后端可换价)
    #[test]
    fn summarize_with_custom_pricing() {
        let rows = vec![row(1000, 0, 0)];
        let pricing = CachePricing {
            cache_hit_per_m: 0.5,
            input_per_m: 1.0,
            output_per_m: 2.0,
        };
        let s = summarize(&rows, &pricing);
        assert!((s.cost - 1000.0 * 0.5 / 1e6).abs() < 1e-12);
        assert!((s.saved - 1000.0 * 0.5 / 1e6).abs() < 1e-12);
    }

    /// 四级水位:各档位边界与距下一档 gap(独立手算,max=10000)
    #[test]
    fn watermark_levels_and_gaps() {
        // 0.4999 → ok,距 soft(5000)差 1
        let r = watermark(4999, 10_000);
        assert_eq!(r.level, "ok");
        assert_eq!(r.next_level, Some("soft"));
        assert_eq!(r.gap_tokens, Some(1));
        // 0.5 → soft,距 snip(6000)差 1000
        let r = watermark(5000, 10_000);
        assert_eq!(r.level, "soft");
        assert_eq!(r.next_level, Some("snip"));
        assert_eq!(r.gap_tokens, Some(1000));
        // 0.6 → snip,距 compact(8000)差 2000
        let r = watermark(6000, 10_000);
        assert_eq!(r.level, "snip");
        assert_eq!(r.next_level, Some("compact"));
        assert_eq!(r.gap_tokens, Some(2000));
        // 0.8 → compact,距 force(9000)差 1000
        let r = watermark(8000, 10_000);
        assert_eq!(r.level, "compact");
        assert_eq!(r.next_level, Some("force"));
        assert_eq!(r.gap_tokens, Some(1000));
        // 0.9+ → force,无下一档
        let r = watermark(9000, 10_000);
        assert_eq!(r.level, "force");
        assert_eq!(r.next_level, None);
        assert_eq!(r.gap_tokens, None);
        // 未配置上限 → unknown
        let r = watermark(9000, 0);
        assert_eq!(r.level, "unknown");
        assert!(r.ratio.is_none());
    }

    /// 水位 gap 边界取整:39000/65536 ≈ 0.595 → soft,距 snip(39321.6→39322)差 322
    #[test]
    fn watermark_gap_rounds_threshold_boundary() {
        let r = watermark(39000, 65_536);
        assert_eq!(r.level, "soft");
        assert_eq!(r.next_level, Some("snip"));
        assert_eq!(r.gap_tokens, Some(39_322 - 39_000));
    }
}
