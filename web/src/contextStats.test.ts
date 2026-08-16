import { describe, expect, it } from 'vitest';
import {
  computeHitRate,
  computeCompactionNeed,
  RECOMMENDED_SETTINGS,
  buildRecommendedPatch,
  type RecommendedSetting,
} from './contextStats';
import type { TokenUsage, RuntimeSettingsPatch } from './api';

describe('computeHitRate(缓存命中率,6d)', () => {
  function usage(partial: Partial<TokenUsage>): TokenUsage {
    return {
      prompt_tokens: 1000,
      completion_tokens: 0,
      total_tokens: 1000,
      context_tokens: 1000,
      prompt_cache_hit_tokens: 0,
      ...partial,
    };
  }

  it('无用量返回 null', () => {
    expect(computeHitRate(null)).toBeNull();
    expect(computeHitRate(undefined)).toBeNull();
  });

  it('prompt_tokens 为 0 返回 null', () => {
    expect(computeHitRate(usage({ prompt_tokens: 0 }))).toBeNull();
  });

  it('无缓存命中返回 0', () => {
    expect(computeHitRate(usage({ prompt_cache_hit_tokens: 0 }))).toBe(0);
  });

  it('全部命中返回 100', () => {
    expect(computeHitRate(usage({ prompt_cache_hit_tokens: 1000 }))).toBe(100);
  });

  it('部分命中四舍五入取整', () => {
    // 500/1000 = 50%
    expect(computeHitRate(usage({ prompt_cache_hit_tokens: 500 }))).toBe(50);
    // 333/1000 = 33.3% → 33
    expect(computeHitRate(usage({ prompt_cache_hit_tokens: 333 }))).toBe(33);
    // 99/100 = 99%
    expect(computeHitRate(usage({ prompt_tokens: 100, prompt_cache_hit_tokens: 99 }))).toBe(99);
  });

  it('命中率钳制在 100 内', () => {
    expect(computeHitRate(usage({ prompt_tokens: 10, prompt_cache_hit_tokens: 999 }))).toBe(100);
  });
});

describe('推荐设置合并(6d)', () => {
  it('勾选项合并为 patch,仅含勾选键', () => {
    const selected: RecommendedSetting[] = [
      RECOMMENDED_SETTINGS[0], // mvu_vars_position
      RECOMMENDED_SETTINGS[2], // bypass_mode
    ];
    const patch = buildRecommendedPatch(selected);
    expect(patch).toEqual({
      mvu_vars_position: 'system',
      bypass_mode: true,
    });
    expect('render_html' in patch).toBe(false);
    expect('max_tool_rounds' in patch).toBe(false);
  });

  it('未勾选任何项返回空 patch', () => {
    expect(buildRecommendedPatch([])).toEqual({});
  });

  it('全部勾选时覆盖全部推荐键', () => {
    const patch = buildRecommendedPatch(RECOMMENDED_SETTINGS);
    expect(Object.keys(patch)).toHaveLength(5);
    expect(patch).toMatchObject<RuntimeSettingsPatch>({
      mvu_vars_position: 'system',
      render_html: true,
      bypass_mode: true,
      max_tool_rounds: 32,
      max_context_tokens: 65536,
    });
  });
});

describe('computeCompactionNeed(上下文压缩触发,借鉴 harness)', () => {
  it('窗口或 token 无效时返回 null', () => {
    expect(computeCompactionNeed(0, 65536, 0.8).ratio).toBeNull();
    expect(computeCompactionNeed(8000, 0, 0.8).ratio).toBeNull();
    expect(computeCompactionNeed(-1, 65536, 0.8).ratio).toBeNull();
  });

  it('占比按 token/窗口计算并钳制到 1', () => {
    expect(computeCompactionNeed(32768, 65536, 0.8).ratio).toBeCloseTo(0.5);
    expect(computeCompactionNeed(100000, 65536, 0.8).ratio).toBe(1);
  });

  it('达到阈值触发,未达不触发', () => {
    // 0.81 > 0.8 触发
    expect(computeCompactionNeed(53085, 65536, 0.8).shouldCompress).toBe(true);
    // 0.79 < 0.8 不触发
    expect(computeCompactionNeed(51773, 65536, 0.8).shouldCompress).toBe(false);
  });

  it('阈值越界时不触发', () => {
    expect(computeCompactionNeed(60000, 65536, 1.0).shouldCompress).toBe(false);
    expect(computeCompactionNeed(60000, 65536, 0.4).shouldCompress).toBe(false);
  });
});
