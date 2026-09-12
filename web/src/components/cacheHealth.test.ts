import { describe, expect, it } from 'vitest';
import {
  entryHitRate,
  formatCny,
  formatEntryTime,
  formatHitRate,
  trendBars,
  watermarkGapText,
  watermarkLitCount,
} from './cacheHealth';

describe('entryHitRate(单条请求命中率)', () => {
  it('hit/(hit+miss) 加权口径', () => {
    expect(entryHitRate(700, 300)).toBeCloseTo(0.7);
    expect(entryHitRate(0, 400)).toBe(0);
  });
  it('hit 与 miss 全 0(提供商不回报缓存字段)→ null', () => {
    expect(entryHitRate(0, 0)).toBeNull();
  });
});

describe('formatHitRate(命中率展示文本)', () => {
  it('null → 暂无缓存数据(而不是 0%)', () => {
    expect(formatHitRate(null)).toBe('暂无缓存数据');
  });
  it('保留一位小数百分比', () => {
    expect(formatHitRate(0.65)).toBe('65.0%');
    expect(formatHitRate(1)).toBe('100.0%');
    expect(formatHitRate(0)).toBe('0.0%');
    expect(formatHitRate(0.3333)).toBe('33.3%');
  });
});

describe('formatCny(金额展示)', () => {
  it('小金额保留 4 位小数', () => {
    expect(formatCny(0.011351)).toBe('¥0.0114');
    expect(formatCny(0.002249)).toBe('¥0.0022');
  });
  it('零金额显示 ¥0', () => {
    expect(formatCny(0)).toBe('¥0');
  });
});

describe('formatEntryTime(明细时间展示)', () => {
  it('ISO 字符串 → 本地可读格式(分,截断秒)', () => {
    expect(formatEntryTime('2026-08-16T00:01:00Z')).toBe('2026-08-16 00:01');
  });
  it('已是本地格式或短串时原样截断', () => {
    expect(formatEntryTime('2026-08-16 00:01:05')).toBe('2026-08-16 00:01');
  });
});

describe('watermarkLitCount(四级水位横条点亮段数)', () => {
  it('ok → 0 段;soft → 1;snip → 2;compact → 3;force → 4', () => {
    expect(watermarkLitCount('ok')).toBe(0);
    expect(watermarkLitCount('soft')).toBe(1);
    expect(watermarkLitCount('snip')).toBe(2);
    expect(watermarkLitCount('compact')).toBe(3);
    expect(watermarkLitCount('force')).toBe(4);
  });
  it('unknown(未配置上限)→ 0 段', () => {
    expect(watermarkLitCount('unknown')).toBe(0);
  });
});

describe('watermarkGapText(距下一档文案)', () => {
  it('unknown → 提示未设置 max_context_tokens', () => {
    expect(
      watermarkGapText({ level: 'unknown', ratio: null, input_tokens: 6000, max_context_tokens: 0, next_level: null, gap_tokens: null }),
    ).toBe('未知(未设置 max_context_tokens)');
  });
  it('force 档无下一档 → 已在最高档', () => {
    expect(
      watermarkGapText({ level: 'force', ratio: 0.95, input_tokens: 9500, max_context_tokens: 10000, next_level: null, gap_tokens: null }),
    ).toBe('已在最高档 force');
  });
  it('有下一档 → 距该档还差 N token', () => {
    expect(
      watermarkGapText({ level: 'snip', ratio: 0.6, input_tokens: 6000, max_context_tokens: 10000, next_level: 'compact', gap_tokens: 2000 }),
    ).toBe('距 compact 档还差 2000 token');
    expect(
      watermarkGapText({ level: 'ok', ratio: 0.4, input_tokens: 4000, max_context_tokens: 10000, next_level: 'soft', gap_tokens: 1000 }),
    ).toBe('距 soft 档还差 1000 token');
  });
});

describe('trendBars(趋势条形数据)', () => {
  it('逐条换算为高度百分比(保留原 rate 供悬停提示)', () => {
    const bars = trendBars([
      { session_id: 's1', created_at: '2026-08-16T00:00:00Z', prompt_tokens: 1000, completion_tokens: 0, hit: 700, miss: 300 },
      { session_id: 's1', created_at: '2026-08-16T00:01:00Z', prompt_tokens: 1000, completion_tokens: 0, hit: 600, miss: 400 },
    ]);
    expect(bars).toEqual([
      { rate: 0.7, heightPct: 70 },
      { rate: 0.6, heightPct: 60 },
    ]);
  });
  it('无缓存数据的条目高度为 0 且 rate 为 null(灰色占位)', () => {
    const bars = trendBars([
      { session_id: 's1', created_at: '2026-08-16T00:00:00Z', prompt_tokens: 100, completion_tokens: 0, hit: 0, miss: 0 },
    ]);
    expect(bars).toEqual([{ rate: null, heightPct: 0 }]);
  });
  it('空列表 → 空数组', () => {
    expect(trendBars([])).toEqual([]);
  });
});
