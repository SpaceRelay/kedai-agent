import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError, apiErrorMessage, authorizedFetch, request, resetApiTokenForTest } from './client';

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

  it('bootstrap 失败不缓存 rejected promise:下次调用重新引导', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockRejectedValueOnce(new Error('网络错误')) // 首次 bootstrap 失败
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't2' }), { status: 200 })) // 重新引导成功
      .mockResolvedValueOnce(new Response('{}', { status: 200 })); // 目标请求

    await expect(authorizedFetch('/api/chat/send', { method: 'POST', body: '{}' })).rejects.toThrow('网络错误');
    await authorizedFetch('/api/chat/sessions');

    expect(fetchMock).toHaveBeenCalledTimes(3);
    const third = fetchMock.mock.calls[2];
    const headers = new Headers((third[1] as RequestInit).headers);
    expect(headers.get('Authorization')).toBe('Bearer t2');
  });

  it('401 时丢弃缓存 token 重新引导并重试一次(服务端重启 token 轮换)', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 'old' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ error: 'unauthorized', code: 'UNAUTHORIZED' }), { status: 401 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 'new' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ok: true }), { status: 200 }));

    const res = await request<{ ok: boolean }>('/chat/sessions');

    expect(res.ok).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(4);
    const last = fetchMock.mock.calls[3];
    expect(new Headers((last[1] as RequestInit).headers).get('Authorization')).toBe('Bearer new');
  });

  it('重试后仍 401 才抛 UNAUTHORIZED(不无限重试)', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 'old' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ error: 'x', code: 'UNAUTHORIZED' }), { status: 401 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 'new' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ error: 'x', code: 'UNAUTHORIZED' }), { status: 401 }));

    await expect(request('/chat/sessions')).rejects.toMatchObject({ code: 'UNAUTHORIZED', status: 401 });
  });
});

describe('结构化错误码(ApiError)', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  /** 模拟一次请求:bootstrap 取 token 后,目标请求返回指定错误体 */
  function mockErrorResponse(status: number, body: unknown): ReturnType<typeof vi.spyOn> {
    return vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(body), { status }));
  }

  async function catchApiError(status: number, body: unknown): Promise<ApiError> {
    // 每次调用前重置 token 缓存:否则第二次起不再走 bootstrap 取 token,
    // fetch 次数变化会让 mock 响应队列错位(目标请求吃到 bootstrap 的 200)
    resetApiTokenForTest();
    mockErrorResponse(status, body);
    try {
      await request('/chat/sessions');
    } catch (e) {
      expect(e).toBeInstanceOf(ApiError);
      return e as ApiError;
    }
    throw new Error('request 应当抛错');
  }

  it('NOT_FOUND / CONFLICT / VALIDATION:用户提示透传服务端原文', async () => {
    const nf = await catchApiError(404, { error: '会话不存在', code: 'NOT_FOUND' });
    expect(nf.message).toBe('会话不存在');
    expect(nf.code).toBe('NOT_FOUND');
    expect(nf.status).toBe(404);

    const cf = await catchApiError(409, { error: '该会话正在生成中', code: 'CONFLICT' });
    expect(cf.message).toBe('该会话正在生成中');

    const va = await catchApiError(400, { error: '缺少 session_id', code: 'VALIDATION' });
    expect(va.message).toBe('缺少 session_id');
  });

  it('DB / INTERNAL:提示「服务端错误,请查看日志」,原文保留在 detail', async () => {
    const db = await catchApiError(500, { error: 'DB 任务执行失败: x', code: 'DB' });
    expect(db.message).toBe('服务端错误,请查看日志');
    expect(db.detail).toBe('DB 任务执行失败: x');

    const internal = await catchApiError(500, { error: '运行态 stat_data 损坏', code: 'INTERNAL' });
    expect(internal.message).toBe('服务端错误,请查看日志');
  });

  it('UNAUTHORIZED:提示重新加载页面;401 无 code 时按 token 失效兜底', async () => {
    const ua = await catchApiError(401, { error: '缺少或无效的 bearer token', code: 'UNAUTHORIZED' });
    expect(ua.message).toBe('登录状态已失效,请重新加载页面');

    const uaNoCode = await catchApiError(401, { error: 'unauthorized' });
    expect(uaNoCode.code).toBe('UNAUTHORIZED');
    expect(uaNoCode.message).toBe('登录状态已失效,请重新加载页面');
  });

  it('无 code 的旧端点:兜底服务端原文;无原文时回退状态码文案', async () => {
    const legacy = await catchApiError(400, { error: '旧参数错误' });
    expect(legacy.code).toBeUndefined();
    expect(legacy.message).toBe('旧参数错误');

    const bare = await catchApiError(502, {});
    expect(bare.message).toBe('请求失败 502');
  });

  it('apiErrorMessage 分类口径(与 errors.rs 契约一致)', () => {
    expect(apiErrorMessage(404, 'NOT_FOUND', '角色卡不存在')).toBe('角色卡不存在');
    expect(apiErrorMessage(409, 'CONFLICT', '锚点无效')).toBe('锚点无效');
    expect(apiErrorMessage(400, 'VALIDATION', '缺参')).toBe('缺参');
    expect(apiErrorMessage(500, 'DB', 'pool exhausted')).toBe('服务端错误,请查看日志');
    expect(apiErrorMessage(500, 'INTERNAL', 'x')).toBe('服务端错误,请查看日志');
    expect(apiErrorMessage(502, 'UPSTREAM', 'x')).toBe('服务端错误,请查看日志');
    expect(apiErrorMessage(401, 'UNAUTHORIZED', 'x')).toBe('登录状态已失效,请重新加载页面');
    expect(apiErrorMessage(500, undefined, '原文')).toBe('原文');
    expect(apiErrorMessage(500, undefined, undefined)).toBe('请求失败 500');
  });
});

