import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  createMemory,
  deleteMemory,
  distillMemory,
  listMemories,
  pruneMemories,
  searchMemories,
  updateMemory,
  type MemoryEntry,
} from './memory';
import { resetApiTokenForTest } from './client';

// 样例条目(与后端 server-rs/src/services/memory_service.rs 的 MemoryEntry 字段对齐)
function entry(overrides: Partial<MemoryEntry> = {}): MemoryEntry {
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

/** mock 两跳 fetch:第一跳 bootstrap 取 token,后续为目标请求(与 diagnostics.test.ts 同模式) */
function mockFetchSequence(...responses: Array<{ body: unknown; status?: number }>): ReturnType<typeof vi.spyOn> {
  const spy = vi.spyOn(globalThis, 'fetch');
  spy.mockResolvedValueOnce(new Response(JSON.stringify({ token: 'test-secret' }), { status: 200 }));
  for (const r of responses) {
    spy.mockResolvedValueOnce(new Response(JSON.stringify(r.body), { status: r.status ?? 200 }));
  }
  return spy;
}

describe('listMemories', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('请求 GET /api/memory?character_id=(id 做 URL 编码)并解包 memories', async () => {
    const spy = mockFetchSequence({ body: { memories: [entry(), entry({ id: 8, kind: 'manual' })] } });
    const list = await listMemories('char A/1');
    expect(String(spy.mock.calls[1][0])).toBe('/api/memory?character_id=char%20A%2F1');
    expect(spy.mock.calls[1][1].method).toBeUndefined(); // 不传 method = GET
    expect(list).toHaveLength(2);
    expect(list[0].kind).toBe('distilled');
    expect(list[1].kind).toBe('manual');
  });

  it('后端 400(缺少 character_id)时抛出 error 文案', async () => {
    mockFetchSequence({ body: { error: '缺少 character_id' }, status: 400 });
    await expect(listMemories(' ')).rejects.toThrow('缺少 character_id');
  });
});

describe('distillMemory', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('请求 POST /api/memory/distill,body 携带 session_id', async () => {
    const spy = mockFetchSequence({ body: { ok: true, inserted: 3, character_id: 'charA' } });
    const res = await distillMemory('s1');
    expect(String(spy.mock.calls[1][0])).toBe('/api/memory/distill');
    expect(spy.mock.calls[1][1]).toMatchObject({ method: 'POST' });
    expect(JSON.parse(String(spy.mock.calls[1][1].body))).toEqual({ session_id: 's1' });
    expect(res.inserted).toBe(3);
    expect(res.character_id).toBe('charA');
  });

  it('未开启蒸馏时后端返回 400 与指引文案,原样抛出', async () => {
    mockFetchSequence({
      body: { error: '跨会话记忆蒸馏未开启,请先在设置中打开 memory_distill_enabled' },
      status: 400,
    });
    await expect(distillMemory('s1')).rejects.toThrow(
      '跨会话记忆蒸馏未开启,请先在设置中打开 memory_distill_enabled',
    );
  });
});

describe('searchMemories', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('请求 GET /api/memory/search,携带 character_id/q/limit(URL 编码)并解包 memories', async () => {
    const spy = mockFetchSequence({ body: { memories: [entry({ pinned: true })] } });
    const list = await searchMemories('char A/1', '图书馆 初识');
    expect(String(spy.mock.calls[1][0])).toBe(
      '/api/memory/search?character_id=char+A%2F1&q=%E5%9B%BE%E4%B9%A6%E9%A6%86+%E5%88%9D%E8%AF%86&limit=20',
    );
    expect(spy.mock.calls[1][1].method).toBeUndefined(); // 不传 method = GET
    expect(list).toHaveLength(1);
    expect(list[0].pinned).toBe(true);
  });

  it('自定义 limit 进入查询串', async () => {
    const spy = mockFetchSequence({ body: { memories: [] } });
    await searchMemories('charA', 'q', 5);
    expect(String(spy.mock.calls[1][0])).toContain('limit=5');
  });

  it('缺 q 时后端 400 原样抛出', async () => {
    mockFetchSequence({ body: { error: '缺少 q' }, status: 400 });
    await expect(searchMemories('charA', ' ')).rejects.toThrow('缺少 q');
  });
});

describe('pruneMemories', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('请求 POST /api/memory/prune,body 携带 character_id,把后端 removed 映射为 deleted', async () => {
    const spy = mockFetchSequence({ body: { ok: true, removed: 4 } });
    const res = await pruneMemories('charA');
    expect(String(spy.mock.calls[1][0])).toBe('/api/memory/prune');
    expect(spy.mock.calls[1][1]).toMatchObject({ method: 'POST' });
    expect(JSON.parse(String(spy.mock.calls[1][1].body))).toEqual({ character_id: 'charA' });
    expect(res).toEqual({ ok: true, deleted: 4 });
  });

  it('缺 character_id 400 时抛出后端文案', async () => {
    mockFetchSequence({ body: { error: '缺少 character_id' }, status: 400 });
    await expect(pruneMemories(' ')).rejects.toThrow('缺少 character_id');
  });
});

describe('createMemory', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('请求 POST /api/memory,body 携带 character_id 与 content,解包 memory', async () => {
    const created = entry({ id: 9, kind: 'manual', source_session_id: null });
    const spy = mockFetchSequence({ body: { ok: true, memory: created }, status: 201 });
    const res = await createMemory('charA', '用户喜欢薄荷茶');
    expect(String(spy.mock.calls[1][0])).toBe('/api/memory');
    expect(spy.mock.calls[1][1]).toMatchObject({ method: 'POST' });
    expect(JSON.parse(String(spy.mock.calls[1][1].body))).toEqual({
      character_id: 'charA',
      content: '用户喜欢薄荷茶',
    });
    expect(res.id).toBe(9);
    expect(res.kind).toBe('manual');
  });

  it('空白内容 400 时抛出「记忆内容不能为空」', async () => {
    mockFetchSequence({ body: { error: '记忆内容不能为空' }, status: 400 });
    await expect(createMemory('charA', '  ')).rejects.toThrow('记忆内容不能为空');
  });
});

describe('updateMemory', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('请求 PATCH /api/memory/:id,body 仅携带给定字段,解包 memory', async () => {
    const updated = entry({ selected: false });
    const spy = mockFetchSequence({ body: { ok: true, memory: updated } });
    const res = await updateMemory(7, { selected: false });
    expect(String(spy.mock.calls[1][0])).toBe('/api/memory/7');
    expect(spy.mock.calls[1][1]).toMatchObject({ method: 'PATCH' });
    expect(JSON.parse(String(spy.mock.calls[1][1].body))).toEqual({ selected: false });
    expect(res.selected).toBe(false);
  });

  it('支持 pinned 字段(PATCH body 仅携带 pinned,解包 pinned=true)', async () => {
    const spy = mockFetchSequence({ body: { ok: true, memory: entry({ pinned: true }) } });
    const res = await updateMemory(7, { pinned: true });
    expect(JSON.parse(String(spy.mock.calls[1][1].body))).toEqual({ pinned: true });
    expect(res.pinned).toBe(true);
  });

  it('不存在或被拒绝的 id 404 时抛出后端文案', async () => {
    mockFetchSequence({ body: { error: '记忆 9999 不存在或更新被拒绝' }, status: 404 });
    await expect(updateMemory(9999, { content: 'x' })).rejects.toThrow('记忆 9999 不存在或更新被拒绝');
  });
});

describe('deleteMemory', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('请求 DELETE /api/memory/:id,204 无正文时正常返回', async () => {
    const spy = vi.spyOn(globalThis, 'fetch');
    spy.mockResolvedValueOnce(new Response(JSON.stringify({ token: 'test-secret' }), { status: 200 }));
    spy.mockResolvedValueOnce(new Response(null, { status: 204 }));
    await expect(deleteMemory(7)).resolves.toBeUndefined();
    expect(String(spy.mock.calls[1][0])).toBe('/api/memory/7');
    expect(spy.mock.calls[1][1]).toMatchObject({ method: 'DELETE' });
  });

  it('不存在的 id 404 时抛出后端文案', async () => {
    mockFetchSequence({ body: { error: '记忆 9999 不存在' }, status: 404 });
    await expect(deleteMemory(9999)).rejects.toThrow('记忆 9999 不存在');
  });
});
