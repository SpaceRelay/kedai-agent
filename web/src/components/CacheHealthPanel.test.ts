import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createSSRApp, h } from 'vue';
import { renderToString } from 'vue/server-renderer';
import CacheHealthPanel from './CacheHealthPanel.vue';
import { resetApiTokenForTest } from '../api/client';

// 项目无 jsdom / @vue/test-utils,沿用 node 纯测试模式:
// 用 vue/server-renderer 的 renderToString 断言 SSR 渲染输出;
// 组件内 onServerPrefetch 预取数据(renderToString 会等待其完成),
// 从而在零新依赖下覆盖「mock fetch → 渲染结果」链路。
// 交互(切换窗口下拉)无法在 SSR 中模拟,由 api/diagnostics.test.ts 的 query 组装 +
// 本文件对下拉选项渲染的断言共同覆盖。

/** 样例响应(与 api/diagnostics.test.ts 同源数字) */
function sampleResponse(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
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
    ...overrides,
  };
}

/** mock fetch(bootstrap + 诊断一次)并渲染组件为 HTML 字符串 */
async function renderPanel(body: Record<string, unknown>): Promise<{ html: string; calls: Array<[unknown, ...unknown[]]> }> {
  const spy = vi.spyOn(globalThis, 'fetch');
  spy.mockResolvedValueOnce(new Response(JSON.stringify({ token: 'test-secret' }), { status: 200 }));
  spy.mockResolvedValueOnce(new Response(JSON.stringify(body), { status: 200 }));
  const app = createSSRApp({ render: () => h(CacheHealthPanel) });
  const html = await renderToString(app);
  return { html, calls: spy.mock.calls as Array<[unknown, ...unknown[]]> };
}

function countActiveSteps(html: string): number {
  // SSR 输出动态 class 在前:class="wm-soft active cache-wm-step"
  return (html.match(/class="[^"]*active[^"]*"/g) ?? []).filter((c) => c.includes('cache-wm-step')).length;
}

describe('CacheHealthPanel(缓存健康面板)', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('加载时按初始窗口 20 发起诊断请求,并渲染命中率 / 费用 / 节省', async () => {
    const { html, calls } = await renderPanel(sampleResponse());
    expect(String(calls[1][0])).toBe('/api/diagnostics/cache?window=20');
    expect(html).toContain('65.0%');
    expect(html).toContain('¥0.0114'); // 窗口花费
    expect(html).toContain('¥0.0022'); // 缓存节省
  });

  it('渲染窗口切换器(近 20/50/100 轮)与 token 汇总', async () => {
    const { html } = await renderPanel(sampleResponse());
    expect(html).toContain('近 20 轮');
    expect(html).toContain('近 50 轮');
    expect(html).toContain('近 100 轮');
    expect(html).toContain('1300'); // 命中 token
    expect(html).toContain('700'); // 未命中 token
  });

  it('渲染趋势条形图(按条目高度递进的 div 条)', async () => {
    const { html } = await renderPanel(sampleResponse());
    // 两条目:70% 与 60% 高度(style 内联百分比)
    expect(html).toContain('70%');
    expect(html).toContain('60%');
    expect(html).toContain('cache-trend-bar');
  });

  it('水位档高亮:snip 档点亮前 2 段,并展示 gap 文案与占比', async () => {
    const { html } = await renderPanel(sampleResponse());
    expect(countActiveSteps(html)).toBe(2);
    expect(html).toContain('距 compact 档还差 2000 token');
    expect(html).toContain('6000');
    expect(html).toContain('10000');
  });

  it('水位 unknown(未设置 max_context_tokens)→ 0 段点亮 + 未知文案', async () => {
    const { html } = await renderPanel(
      sampleResponse({
        watermark: { level: 'unknown', ratio: null, input_tokens: 0, max_context_tokens: 0, next_level: null, gap_tokens: null },
      }),
    );
    expect(countActiveSteps(html)).toBe(0);
    expect(html).toContain('未知(未设置 max_context_tokens)');
  });

  it('空态:hit_rate 为 null 时显示「暂无缓存数据」而非 0%', async () => {
    const { html } = await renderPanel(
      sampleResponse({
        totals: { count: 1, total_prompt: 100, total_completion: 0, total_hit: 0, total_miss: 0, hit_rate: null },
        entries: [{ session_id: 's1', created_at: '2026-08-16T00:00:00Z', prompt_tokens: 100, completion_tokens: 0, hit: 0, miss: 0 }],
      }),
    );
    expect(html).toContain('暂无缓存数据');
    expect(html).not.toContain('0.0%');
  });

  it('明细默认折叠(details),展开标记存在且含时间与 hit/miss', async () => {
    const { html } = await renderPanel(sampleResponse());
    expect(html).toContain('<details');
    expect(html).toContain('2026-08-16 00:01');
    expect(html).toContain('70.0%'); // 明细行单条命中率
  });

  it('请求失败时展示错误反馈而不渲染数据', async () => {
    const spy = vi.spyOn(globalThis, 'fetch');
    spy.mockResolvedValueOnce(new Response(JSON.stringify({ token: 'test-secret' }), { status: 200 }));
    spy.mockResolvedValueOnce(new Response(JSON.stringify({ error: '读取缓存统计失败: db' }), { status: 500 }));
    const app = createSSRApp({ render: () => h(CacheHealthPanel) });
    const html = await renderToString(app);
    expect(html).toContain('读取缓存统计失败');
    expect(html).not.toContain('65.0%');
  });
});
