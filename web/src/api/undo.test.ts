import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { listUndoSnapshots, restoreUndoSnapshot } from './undo';
import { resetApiTokenForTest } from './client';

// api/undo.ts 封装层测试(批次 6.1b):REST 路径/方法/请求体契约,范式同 tasks.test.ts。

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

describe('api/undo REST 封装', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.spyOn(globalThis, 'fetch').mockImplementation(async (input, init) => {
      const url = String(input);
      const method = (init?.method ?? 'GET').toUpperCase();
      if (url.endsWith('/api/bootstrap')) return json({ token: 't' });
      if (url.endsWith('/api/chat/sessions/s1/undo') && method === 'GET') {
        return json({
          snapshots: [
            {
              id: 'u2',
              tool_name: 'write',
              label: '写入文件 notes.md',
              anchor_message_id: 12,
              created_at: '2026-08-29T01:00:00.000Z',
            },
            {
              id: 'u1',
              tool_name: 'update_variables',
              label: '更新变量树',
              anchor_message_id: null,
              created_at: '2026-08-29T00:00:00.000Z',
            },
          ],
        });
      }
      if (url.endsWith('/api/undo/u2/restore') && method === 'POST') return json({ ok: true });
      return json({ error: `未 mock 的请求: ${method} ${url}` }, 404);
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('listUndoSnapshots:GET 会话 undo 路径,返回 snapshots 数组(新→旧)', async () => {
    const snapshots = await listUndoSnapshots('s1');
    expect(snapshots).toHaveLength(2);
    expect(snapshots[0].id).toBe('u2');
    expect(snapshots[0].tool_name).toBe('write');
    expect(snapshots[0].anchor_message_id).toBe(12);
    expect(snapshots[1].anchor_message_id).toBeNull();

    const calls = vi.mocked(globalThis.fetch).mock.calls;
    const listCall = calls.find(([input]) => String(input).endsWith('/api/chat/sessions/s1/undo'));
    expect(listCall, '应请求 GET /api/chat/sessions/s1/undo').toBeDefined();
    expect((listCall![1]?.method ?? 'GET').toUpperCase()).toBe('GET');
  });

  it('restoreUndoSnapshot:POST /api/undo/{id}/restore,空 JSON 体', async () => {
    await restoreUndoSnapshot('u2');

    const calls = vi.mocked(globalThis.fetch).mock.calls;
    const restoreCall = calls.find(([input]) => String(input).endsWith('/api/undo/u2/restore'));
    expect(restoreCall, '应请求 POST /api/undo/u2/restore').toBeDefined();
    expect(restoreCall![1]?.method).toBe('POST');
    expect(restoreCall![1]?.body).toBe('{}');
  });
});
