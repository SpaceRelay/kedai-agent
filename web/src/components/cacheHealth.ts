// 缓存健康面板展示纯函数(优化面板「缓存健康」分区):
// 命中率 / 金额 / 时间格式化、四级水位横条的点亮段数与 gap 文案、趋势条形数据。
// 与 contextStats.ts 同风格:纯函数供组件渲染与 Vitest 共用。
import type { CacheUsageEntry, CacheWatermark } from '../api';

/** 单条请求命中率:hit / (hit + miss);hit 与 miss 全 0(无缓存字段)→ null。
 *  与 `contextStats.ts` 的 `computeHitRate(u)` 口径等价(后端 prompt_tokens = hit + miss)。 */
export function entryHitRate(hit: number, miss: number): number | null {
  const denom = hit + miss;
  return denom > 0 ? hit / denom : null;
}

/** 命中率展示文本:0..1 → 一位小数百分比;null → 暂无缓存数据(不显示 0%) */
export function formatHitRate(rate: number | null): string {
  if (rate === null) return '暂无缓存数据';
  return `${(rate * 100).toFixed(1)}%`;
}

/** 金额展示(元,后端已算好):0 → ¥0;其余保留 4 位小数 */
export function formatCny(v: number): string {
  if (v === 0) return '¥0';
  return `¥${v.toFixed(4)}`;
}

/** 明细时间展示:ISO / 本地格式统一截断到分(秒与毫秒不展示) */
export function formatEntryTime(created_at: string): string {
  return created_at.replace('T', ' ').slice(0, 16);
}

/** 四级水位横条档位(缓存感知管线:soft 0.5 / snip 0.6 / compact 0.8 / force 0.9) */
export const WATERMARK_STEPS = ['soft', 'snip', 'compact', 'force'] as const;

/** 当前档点亮的横条段数:ok → 0 段,soft → 1 … force → 4;unknown(未配置上限)→ 0 */
export function watermarkLitCount(level: string): number {
  const idx = (WATERMARK_STEPS as readonly string[]).indexOf(level);
  return idx < 0 ? 0 : idx + 1;
}

/** 距下一档文案:unknown → 未知提示;force(无下一档)→ 已在最高档;否则差 N token */
export function watermarkGapText(wm: CacheWatermark): string {
  if (wm.level === 'unknown') return '未知(未设置 max_context_tokens)';
  if (!wm.next_level) return '已在最高档 force';
  const gap = wm.gap_tokens ?? 0;
  return `距 ${wm.next_level} 档还差 ${gap} token`;
}

/** 趋势条形数据:逐条命中率 + 换算为 0..100 高度百分比(无缓存数据 → 高度 0 灰色占位) */
export function trendBars(entries: CacheUsageEntry[]): Array<{ rate: number | null; heightPct: number }> {
  return entries.map((e) => {
    const rate = entryHitRate(e.hit, e.miss);
    return { rate, heightPct: rate === null ? 0 : Math.round(rate * 100) };
  });
}
