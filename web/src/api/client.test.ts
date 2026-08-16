import { beforeEach, describe, expect, it, vi } from 'vitest';
import { authorizedFetch, resetApiTokenForTest } from './client';

describe('API bearer token', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('经同源 bootstrap 获取一次 token 并自动用于后续请求', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 'test-secret' }), { status: 200 }))
      .mockResolvedValueOnce(new Response('{}', { status: 200 }));

    await authorizedFetch('/api/chat/send', { method: 'POST', body: '{}' });

    expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/bootstrap', expect.objectContaining({ cache: 'no-store' }));
    const second = fetchMock.mock.calls[1];
    const headers = new Headers((second[1] as RequestInit).headers);
    expect(headers.get('Authorization')).toBe('Bearer test-secret');
    expect(headers.get('Content-Type')).toBe('application/json');
    expect(String(second[0])).not.toContain('test-secret');
  });
});
