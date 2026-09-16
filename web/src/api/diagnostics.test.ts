import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getCacheDiagnostics } from './diagnostics';
import { resetApiTokenForTest } from './client';

// 样例数据与后端 server-rs/src/services/cache_diagnostics.rs 测试同源:
// hit 1300 / miss 700 / completion 1200(单价 0.27/2/8 元每百万)
// → hit_rate 0.65、cost 0.011351、saved 0.002249
const sample = {
  window: 20,
  session_id: null,
  totals: {
    count: 2,
    total_prompt: 2000,
    total_completion: 1200,
    total_hit: 1300,
    total_miss: 700,
    hit_rate: 0.65,
  },
  entries: [
    { session_id: 's1', created_at: '2026-08-16T00:00:00Z', prompt_tokens: 1000, completion_tokens: 200, hit: 700, miss: 300 },
    { session_id: 's1', created_at: '2026-08-16T00:01:00Z', prompt_tokens: 1000, completion_tokens: 1000, hit: 600, miss: 400 },
  ],
  pricing: { cache_hit_per_m: 0.27, input_per_m: 2, output_per_m: 8, currency_hint: 'CNY/1M' },
  cost: 0.011351,
  saved: 0.002249,
  watermark: { level: 'snip', ratio: 0.6, input_tokens: 6000, max_context_tokens: 10000, next_level: 'compact', gap_tokens: 2000 },
};

/** mock 两跳 fetch:第一跳 bootstrap 取 token,后续为目标请求 */
function mockFetchSequence(...responses: Array<{ body: unknown; status?: number }>): ReturnType<typeof vi.spyOn> {
  const spy = vi.spyOn(globalThis, 'fetch');
  spy.mockResolvedValueOnce(new Response(JSON.stringify({ token: 'test-secret' }), { status: 200 }));
  for (const r of responses) {
    spy.mockResolvedValueOnce(new Response(JSON.stringify(r.body), { status: r.status ?? 200 }));
  }
  return spy;
}

describe('getCacheDiagnostics', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('无参数时请求 /api/diagnostics/cache(不带 query,窗口由后端默认 20)', async () => {
    const spy = mockFetchSequence({ body: sample });
    const data = await getCacheDiagnostics();
    expect(String(spy.mock.calls[1][0])).toBe('/api/diagnostics/cache');
    expect(data.totals.hit_rate).toBeCloseTo(0.65);
  });

  it('window 与 sessionId 组装到 query(窗口切换重新请求的参数通道)', async () => {
    const spy = mockFetchSequence({ body: sample });
    await getCacheDiagnostics({ sessionId: 'sess-1', window: 50 });
    expect(String(spy.mock.calls[1][0])).toBe('/api/diagnostics/cache?session_id=sess-1&window=50');
  });

  it('完整解析 totals / entries / pricing / cost / saved / watermark 字段', async () => {
    mockFetchSequence({ body: sample });
    const data = await getCacheDiagnostics({ window: 20 });
    expect(data.window).toBe(20);
    expect(data.totals).toEqual(sample.totals);
    expect(data.entries).toHaveLength(2);
    expect(data.entries[0].hit).toBe(700);
    expect(data.pricing.currency_hint).toBe('CNY/1M');
    expect(data.cost).toBeCloseTo(0.011351);
    expect(data.saved).toBeCloseTo(0.002249);
    expect(data.watermark.level).toBe('snip');
    expect(data.watermark.gap_tokens).toBe(2000);
  });

  it('非 2xx 时抛出后端 error 文案', async () => {
    // 状态码与后端实现对齐:失败走 500 + code(见 api/diagnostics.rs 的 err_with_code)
    mockFetchSequence({ body: { error: '读取缓存统计失败', code: 'DB' }, status: 500 });
    await expect(getCacheDiagnostics()).rejects.toThrow('服务端错误,请查看日志');
  });

  it('200 但载荷是 {error} 时抛错,不透传给调用方', async () => {
    // 回归:后端曾「失败也返回 200」,载荷为 {error};若直接透传,调用方访问
    // data.totals.hit_rate 会抛 TypeError(可选链挡不住形状不对的真值对象)。
    // 形状闸门应把它变成可捕获的 Error,并透出后端原文便于定位。
    mockFetchSequence({ body: { error: '读取缓存统计失败: Wrong number of parameters passed to query' } });
    await expect(getCacheDiagnostics()).rejects.toThrow(/Wrong number of parameters/);
  });

  it('200 但缺少 totals/entries/watermark 时抛格式异常', async () => {
    mockFetchSequence({ body: { window: 20, session_id: null } });
    await expect(getCacheDiagnostics()).rejects.toThrow('缓存统计响应格式异常');
  });
});
