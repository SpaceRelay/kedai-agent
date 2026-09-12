import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createSSRApp, h } from 'vue';
import { renderToString } from 'vue/server-renderer';
import { createPinia, setActivePinia, type Pinia } from 'pinia';
import RepoIndexModal from './RepoIndexModal.vue';
import * as api from '../api';
import { resetApiTokenForTest } from '../api/client';

// 仓库索引面板 SSR 渲染测试(沿用 CacheHealthPanel.test.ts 的零依赖模式):
// renderToString + onServerPrefetch 预取,断言 available=false 空态 / 文件列表 /
// 搜索与类别筛选控件 / 展开详情结构。交互(点击展开)在纯函数 repoIndex.test.ts 覆盖。

// node 环境无 localStorage,子 store 初始化即访问(uiPrefs 的开关记忆),补内存桩
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

/** 样例索引项(字段与后端 RepoIndexItem 契约对齐) */
function item(overrides: Partial<api.RepoIndexItem> = {}): api.RepoIndexItem {
  return {
    path: 'server-rs/src/api/memory.rs',
    kind: 'rust',
    module: 'server-rs/src/api',
    lines: 240,
    bytes: 9216,
    importance: 9,
    summary: '记忆库路由:蒸馏 / 列表 / 检索 / 清理',
    deepSummary: '深摘要:记忆槽注入排序与容量淘汰',
    usageCount: 12,
    category: 'api',
    note: '',
    symbols: [
      { name: 'distill', kind: 'function', line: 60 },
      { name: 'search', kind: 'function', line: 133 },
    ],
    ...overrides,
  };
}

/** mock fetch:第一跳 bootstrap,后续按序消费 */
function mockFetch(...responses: Array<{ body: unknown; status?: number }>): ReturnType<typeof vi.spyOn> {
  const spy = vi.spyOn(globalThis, 'fetch');
  spy.mockResolvedValueOnce(new Response(JSON.stringify({ token: 'test-secret' }), { status: 200 }));
  for (const r of responses) {
    spy.mockResolvedValueOnce(new Response(JSON.stringify(r.body), { status: r.status ?? 200 }));
  }
  return spy;
}

/** SSR 渲染通道:渲染组件为 HTML 字符串 */
async function renderModal(
  body: unknown,
  status = 200,
): Promise<{ html: string; calls: Array<[unknown, ...unknown[]]> }> {
  const pinia: Pinia = createPinia();
  setActivePinia(pinia);
  const spy = mockFetch({ body, status });
  const app = createSSRApp({ render: () => h(RepoIndexModal) });
  app.use(pinia);
  const html = await renderToString(app);
  return { html, calls: spy.mock.calls as Array<[unknown, ...unknown[]]> };
}

beforeEach(() => {
  memStorage.clear();
  resetApiTokenForTest();
  vi.restoreAllMocks();
});

describe('RepoIndexModal(SSR 渲染)', () => {
  it('索引未生成(available=false)→ 空态提示运行 build.mjs --full,并展示 reason', async () => {
    const { html, calls } = await renderModal({
      available: false,
      reason: '索引文件不存在',
    });
    expect(String(calls[1][0])).toBe('/api/repo-index');
    expect(html).toContain('索引未生成');
    expect(html).toContain('node .kedai-index/build.mjs --full');
    expect(html).toContain('索引文件不存在');
    // 空态下不渲染文件列表控件
    expect(html).not.toContain('搜索路径');
  });

  it('available=true → 渲染文件数 / 搜索框 / 类别筛选 / 文件行(路径与重要性)', async () => {
    const { html } = await renderModal({
      available: true,
      generatedAt: '2026-09-09T03:36:14.303Z',
      gitHead: '6987460d',
      fileCount: 2,
      items: [
        item(),
        item({ path: 'web/src/App.vue', kind: 'vue', module: 'web/src', importance: 10, bytes: 2048, summary: '根组件' }),
      ],
    });
    expect(html).toContain('6987460d');
    expect(html).toContain('搜索路径');
    expect(html).toContain('全部类别');
    expect(html).toContain('memory.rs');
    expect(html).toContain('server-rs/src/api/memory.rs');
    expect(html).toContain('重要性 9');
    expect(html).toContain('9.0KB');
    expect(html).toContain('2 / 2 个文件');
    // 类别下拉:动态 kind 选项
    expect(html).toContain('rust');
    expect(html).toContain('vue');
  });

  it('默认折叠:文件行渲染摘要但不渲染符号列表;展开态才输出符号', async () => {
    const { html } = await renderModal({
      available: true,
      fileCount: 1,
      items: [item()],
    });
    expect(html).toContain('记忆库路由:蒸馏 / 列表 / 检索 / 清理');
    expect(html).toContain('展开');
    expect(html).not.toContain('符号(');
    expect(html).not.toContain('深摘要:记忆槽注入排序与容量淘汰');
  });

  it('加载失败 → 展示「加载失败」错误反馈', async () => {
    const { html } = await renderModal({ error: '读取索引失败' }, 500);
    expect(html).toContain('加载失败:读取索引失败');
  });
});
