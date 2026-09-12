import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getPromptPreview } from './settings';
import { resetApiTokenForTest } from './client';

describe('settings API', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('GET /settings/prompt-preview:携带 session/character/mode 查询参数', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ok: true, note: '', layers: [] }), { status: 200 }));

    const got = await getPromptPreview('s1', 'c1', 'task');
    expect(got.ok).toBe(true);
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    expect(String(req[0])).toBe('/api/settings/prompt-preview?session_id=s1&character_id=c1&mode=task');
  });

  it('不传参数时裸路径(旧行为不变)', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ok: true, note: '', layers: [] }), { status: 200 }));

    await getPromptPreview();
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    expect(String(req[0])).toBe('/api/settings/prompt-preview');
  });
});
