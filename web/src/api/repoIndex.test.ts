import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fetchRepoIndex, normalizeRepoItem, type RepoIndexItem } from './repoIndex';
import { resetApiTokenForTest } from './client';

// 仓库索引 API 测试:GET /api/repo-index(可选 q),解包原样返回。

function item(overrides: Partial<RepoIndexItem> = {}): RepoIndexItem {
  return {
    path: 'web/src/App.vue',
    kind: 'vue',
    module: 'web/src',
    lines: 210,
    bytes: 4096,
    importance: 10,
    summary: '根组件',
    deepSummary: '',
    usageCount: 3,
    category: 'ui',
    note: '',
    symbols: [{ name: 'useAppStore', kind: 'function', line: 7 }],
    ...overrides,
  };
}

/** mock 两跳 fetch:第一跳 bootstrap 取 token,第二跳为目标请求 */
function mockFetchSequence(body: unknown, status = 200): ReturnType<typeof vi.spyOn> {
  const spy = vi.spyOn(globalThis, 'fetch');
  spy.mockResolvedValueOnce(new Response(JSON.stringify({ token: 'test-secret' }), { status: 200 }));
  spy.mockResolvedValueOnce(new Response(JSON.stringify(body), { status }));
  return spy;
}

describe('fetchRepoIndex', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('不传 q 时请求裸路径 /api/repo-index,原样返回 available/items', async () => {
    const spy = mockFetchSequence({ available: true, fileCount: 1, items: [item()] });
    const res = await fetchRepoIndex();
    expect(String(spy.mock.calls[1][0])).toBe('/api/repo-index');
    expect(spy.mock.calls[1][1].method).toBeUndefined(); // GET
    expect(res.available).toBe(true);
    expect(res.items?.[0].path).toBe('web/src/App.vue');
  });

  it('传 q 时进入查询串(URL 编码)', async () => {
    const spy = mockFetchSequence({ available: true, items: [] });
    await fetchRepoIndex('记忆 库');
    expect(String(spy.mock.calls[1][0])).toBe(
      '/api/repo-index?q=%E8%AE%B0%E5%BF%86%20%E5%BA%93',
    );
  });

  it('空白 q 视为不传(裸路径)', async () => {
    const spy = mockFetchSequence({ available: true, items: [] });
    await fetchRepoIndex('   ');
    expect(String(spy.mock.calls[1][0])).toBe('/api/repo-index');
  });

  it('索引未生成:available=false 与 reason 原样返回', async () => {
    mockFetchSequence({ available: false, reason: '索引文件不存在' });
    const res = await fetchRepoIndex();
    expect(res.available).toBe(false);
    expect(res.reason).toBe('索引文件不存在');
  });

  it('后端 500 时抛出 error 文案', async () => {
    mockFetchSequence({ error: '读取索引失败' }, 500);
    await expect(fetchRepoIndex()).rejects.toThrow('读取索引失败');
  });

  it('异常响应(缺字段 / items 非数组)→ 归一化兜底,不打崩面板', async () => {
    mockFetchSequence({ available: true, items: [{ path: 'a.ts' }, null, 'x'] });
    const res = await fetchRepoIndex();
    expect(res.items).toHaveLength(1);
    expect(res.items?.[0]).toMatchObject({ path: 'a.ts', kind: '', lines: 0, symbols: [] });

    mockFetchSequence({ available: true, items: 'oops' });
    const res2 = await fetchRepoIndex();
    expect(res2.items).toEqual([]);
  });
});

describe('normalizeRepoItem', () => {
  it('合法对象补全字段与符号;非对象返回 null', () => {
    expect(normalizeRepoItem({ path: 'a.rs', kind: 'rust', symbols: [{ name: 'f', kind: 'function', line: 3 }] }))
      .toMatchObject({
        path: 'a.rs',
        kind: 'rust',
        lines: 0,
        bytes: 0,
        symbols: [{ name: 'f', kind: 'function', line: 3 }],
      });
    expect(normalizeRepoItem(null)).toBeNull();
    expect(normalizeRepoItem('x')).toBeNull();
  });

  it('symbols 非法项被过滤,数字字段非有限值回 0', () => {
    const got = normalizeRepoItem({ path: 'a.ts', lines: Number.NaN, symbols: [null, { name: 'g' }] });
    expect(got?.lines).toBe(0);
    expect(got?.symbols).toEqual([{ name: 'g', kind: '', line: 0 }]);
  });
});
