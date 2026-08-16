import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getScriptTree, saveScriptTree } from './scripts';
import { resetApiTokenForTest } from './client';

describe('scripts API', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  const tree = [
    {
      type: 'script' as const,
      enabled: true,
      name: '自动回复',
      id: 'a1',
      content: "console.log('hi')",
      data: { count: 1 },
    },
  ];

  it('GET global:不带 character_id,返回 trees 数组', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ scope: 'global', trees: tree }), { status: 200 }));

    const got = await getScriptTree('global');
    expect(got).toEqual(tree);
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    expect(String(req[0])).toBe('/api/scripts/tree?scope=global');
  });

  it('GET character:携带 character_id', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ scope: 'character', trees: [] }), { status: 200 }));

    await getScriptTree('character', 'cid-1');
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    expect(String(req[0])).toBe('/api/scripts/tree?scope=character&character_id=cid-1');
  });

  it('PUT:全量保存,body 携带 trees', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ok: true }), { status: 200 }));

    await saveScriptTree(tree, 'character', 'cid-2');
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    expect(String(req[0])).toBe('/api/scripts/tree?scope=character&character_id=cid-2');
    const init = req[1] as RequestInit;
    expect(init.method).toBe('PUT');
    expect(JSON.parse(String(init.body))).toEqual({ trees: tree });
  });

  it('PUT global:仅 scope,body 仍带 trees', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ok: true }), { status: 200 }));

    await saveScriptTree(tree, 'global');
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    expect(String(req[0])).toBe('/api/scripts/tree?scope=global');
  });
});
