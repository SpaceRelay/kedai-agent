import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createQuickReply, deleteQuickReply, listQuickReplies, updateQuickReply } from './quickReplies';
import { resetApiTokenForTest } from './client';

describe('quickReplies API', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  const record = {
    id: 1,
    name: 'greet',
    label: '打招呼',
    content: '你好,我是测试助手',
    enabled: true,
    position: 0,
    sort_order: 100,
  };

  it('GET 默认:不带 query,返回 quick_replies 数组(仅启用项)', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ quick_replies: [record] }), { status: 200 }));

    const got = await listQuickReplies();
    expect(got).toEqual([record]);
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    expect(String(req[0])).toBe('/api/quick-replies');
  });

  it('GET includeDisabled:携带 all=true', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ quick_replies: [] }), { status: 200 }));

    await listQuickReplies(true);
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    expect(String(req[0])).toBe('/api/quick-replies?all=true');
  });

  it('POST:创建,body 携带输入字段,返回 quick_reply', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ok: true, quick_reply: record }), { status: 201 }));

    const { id: _id, ...input } = record;
    const got = await createQuickReply(input);
    expect(got).toEqual(record);
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    expect(String(req[0])).toBe('/api/quick-replies');
    const init = req[1] as RequestInit;
    expect(init.method).toBe('POST');
    expect(JSON.parse(String(init.body))).toEqual(input);
  });

  it('PUT:按 id 更新', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ok: true, quick_reply: record }), { status: 200 }));

    const { id, ...input } = record;
    await updateQuickReply(id, input);
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    expect(String(req[0])).toBe('/api/quick-replies/1');
    const init = req[1] as RequestInit;
    expect(init.method).toBe('PUT');
    expect(JSON.parse(String(init.body))).toEqual(input);
  });

  it('DELETE:按 id 删除', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));

    await deleteQuickReply(1);
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    expect(String(req[0])).toBe('/api/quick-replies/1');
    expect((req[1] as RequestInit).method).toBe('DELETE');
  });
});
