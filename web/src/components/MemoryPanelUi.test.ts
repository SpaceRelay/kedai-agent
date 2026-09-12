import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createSSRApp, h } from 'vue';
import { renderToString } from 'vue/server-renderer';
import MemoryPanel from './MemoryPanel.vue';
import * as api from '../api';
import { resetApiTokenForTest } from '../api/client';

// 记忆库面板 SSR 渲染测试(沿用 CacheHealthPanel.test.ts 的零依赖模式):
// renderToString + onServerPrefetch 预取,断言空态 / 列表 / kind 标签 /
// 蒸馏按钮可用性 / 加载失败反馈。
// 交互链路(蒸馏 / 添加 / 编辑 / 注入开关 / 删除两段式确认)在
// composables/useMemoryPanel.test.ts 直测 composable;
// 文件名带 Ui 后缀:Windows 文件系统大小写不敏感,MemoryPanel.test.ts 会与
// 纯函数测试 memoryPanel.test.ts 视为同一文件互相覆盖,故不可用。

/** 样例条目(与 api/memory.test.ts 同源字段) */
function memEntry(overrides: Partial<api.MemoryEntry> = {}): api.MemoryEntry {
  return {
    id: 7,
    character_id: 'charA',
    source_session_id: 's1',
    kind: 'distilled',
    content: '用户与角色在图书馆初识',
    usage_count: 3,
    last_usage: '2026-08-15T10:30:00Z',
    selected: true,
    pinned: false,
    created_at: '2026-08-14T08:00:00Z',
    updated_at: '2026-08-15T10:30:00Z',
    ...overrides,
  };
}

/** 三条样例:蒸馏(已选/有使用/置顶)、工具(未选/未使用)、手动(已选) */
function sampleList(): api.MemoryEntry[] {
  return [
    memEntry({ pinned: true }),
    memEntry({ id: 8, kind: 'tool', content: '工具写入的记忆', selected: false, usage_count: 0, last_usage: null, source_session_id: null }),
    memEntry({ id: 9, kind: 'manual', content: '手动添加的记忆', source_session_id: null }),
  ];
}

/** mock fetch:第一跳 bootstrap,后续按序消费 */
function mockFetch(...responses: Array<{ body: unknown; status?: number }>): ReturnType<typeof vi.spyOn> {
  const spy = vi.spyOn(globalThis, 'fetch');
  spy.mockResolvedValueOnce(new Response(JSON.stringify({ token: 'test-secret' }), { status: 200 }));
  for (const r of responses) {
    spy.mockResolvedValueOnce(new Response(r.body === null ? null : JSON.stringify(r.body), { status: r.status ?? 200 }));
  }
  return spy;
}

/** SSR 渲染通道:按 props 渲染组件为 HTML 字符串 */
async function renderPanel(
  props: { characterId?: string | null; sessionId?: string | null },
  ...responses: Array<{ body: unknown; status?: number }>
): Promise<{ html: string; calls: Array<[unknown, ...unknown[]]> }> {
  const spy = mockFetch(...responses);
  const app = createSSRApp({ setup: () => () => h(MemoryPanel, props) });
  const html = await renderToString(app);
  return { html, calls: spy.mock.calls as Array<[unknown, ...unknown[]]> };
}

beforeEach(() => {
  resetApiTokenForTest();
  vi.restoreAllMocks();
});

describe('MemoryPanel(SSR 渲染)', () => {
  it('无角色时空态提示「请先选择角色」,不发起列表请求、不渲染操作区', async () => {
    const { html, calls } = await renderPanel({});
    expect(calls).toHaveLength(0); // 不发任何请求(token 为惰性获取,无目标请求则 bootstrap 也不发)
    expect(html).toContain('请先选择角色');
    expect(html).not.toContain('蒸馏当前会话');
    expect(html).not.toContain('添加');
  });

  it('有角色时按 characterId 请求 GET /api/memory,渲染 kind 标签(三色)/ 使用次数 / 时间 / 注入开关', async () => {
    const { html, calls } = await renderPanel(
      { characterId: 'charA', sessionId: 's1' },
      { body: { memories: sampleList() } },
    );
    expect(String(calls[1][0])).toBe('/api/memory?character_id=charA');
    // kind 标签:中文文案 + 配色 class
    expect(html).toContain('kind-distilled');
    expect(html).toContain('kind-tool');
    expect(html).toContain('kind-manual');
    expect(html).toContain('蒸馏');
    expect(html).toContain('工具');
    expect(html).toContain('手动');
    // 计数与时间:格式化时间 / 未使用
    expect(html).toContain('使用 3 次');
    expect(html).toContain('2026-08-15 10:30');
    expect(html).toContain('未使用');
    // 内容与注入开关(已选条目渲染 checked 属性)
    expect(html).toContain('用户与角色在图书馆初识');
    expect(html).toContain('工具写入的记忆');
    expect(html).toMatch(/type="checkbox"[^>]*checked/);
    // 操作按钮
    expect(html).toContain('蒸馏当前会话');
    expect(html).toContain('编辑');
    expect(html).toContain('删除');
    expect(html).toContain('添加');
  });

  it('有角色但记忆为空 → 「暂无记忆」引导文案,蒸馏按钮可点(有会话)', async () => {
    const { html } = await renderPanel(
      { characterId: 'charA', sessionId: 's1' },
      { body: { memories: [] } },
    );
    expect(html).toContain('暂无记忆');
    expect(html).toContain('蒸馏当前会话');
    expect(html).not.toContain('disabled');
  });

  it('无当前会话 → 蒸馏按钮禁用', async () => {
    const { html } = await renderPanel(
      { characterId: 'charA' },
      { body: { memories: [] } },
    );
    expect(html).toContain('蒸馏当前会话');
    expect(html).toContain('disabled');
  });

  it('列表加载失败 → 展示「加载失败」错误反馈', async () => {
    const { html } = await renderPanel(
      { characterId: 'charA', sessionId: 's1' },
      { body: { error: '数据库不可用' }, status: 500 },
    );
    expect(html).toContain('加载失败:数据库不可用');
  });

  it('搜索/筛选控件与清理入口常驻渲染(有角色时)', async () => {
    const { html } = await renderPanel(
      { characterId: 'charA', sessionId: 's1' },
      { body: { memories: sampleList() } },
    );
    // 搜索框(占位文案 + type=search)
    expect(html).toContain('搜索记忆内容');
    expect(html).toMatch(/type="search"/);
    // kind 筛选下拉:四项(全部类型 / 蒸馏 / 工具 / 手动)
    expect(html).toContain('全部类型');
    expect(html).toContain('蒸馏');
    expect(html).toContain('工具');
    expect(html).toContain('手动');
    // 清理已归档(危险操作入口)
    expect(html).toContain('清理已归档');
  });

  it('置顶条目:渲染「置顶」标记与「取消置顶」按钮;未置顶渲染「置顶」按钮', async () => {
    const { html } = await renderPanel(
      { characterId: 'charA', sessionId: 's1' },
      { body: { memories: sampleList() } },
    );
    expect(html).toContain('memory-pinned'); // 置顶行样式钩子
    expect(html).toContain('memory-pin-tag'); // 置顶标记
    expect(html).toContain('取消置顶'); // 置顶行的按钮
    expect(html.match(/>置顶<\/button>/g)).toHaveLength(2); // 两条未置顶行的按钮
  });

  it('空态:搜索/筛选无结果时提示调整条件;无筛选时提示暂无记忆', async () => {
    const { html } = await renderPanel(
      { characterId: 'charA', sessionId: 's1' },
      { body: { memories: [] } },
    );
    expect(html).toContain('暂无记忆');
    expect(html).toContain('搜索记忆内容'); // 空态下控件仍在
  });
});
