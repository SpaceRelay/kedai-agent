// 流式与上传请求底座的测试(模块二:把 chat/tasks 重复的 SSE 读循环与 4 处上传的
// 手写错误分支收敛到 stream.ts;chat.test.ts / client.test.ts 覆盖各自既有语义)
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError, resetApiTokenForTest } from './client';
import { pumpSseFrames, readErrorParts, toApiError, uploadForm } from './stream';

/** 用若干字节块构造一个可流式读取的 Response(模拟 SSE 分块到达) */
function streamingResponse(chunks: string[], init: ResponseInit = {}): Response {
  const encoder = new TextEncoder();
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const c of chunks) controller.enqueue(encoder.encode(c));
      controller.close();
    },
  });
  return new Response(stream, { status: 200, ...init });
}

/** 让全局 fetch 依次返回 bootstrap token、随后给定响应 */
function mockFetchOnce(...responses: Response[]): ReturnType<typeof vi.spyOn> {
  const mock = vi.spyOn(globalThis, 'fetch');
  responses.forEach((r) => mock.mockResolvedValueOnce(r));
  return mock;
}

describe('readErrorParts / toApiError(非 2xx 错误体解析,与 request 同口径)', () => {
  it('带 code 的错误体:解出 status / code / detail', async () => {
    const res = new Response(JSON.stringify({ error: '会话不存在', code: 'NOT_FOUND' }), {
      status: 404,
    });
    await expect(readErrorParts(res)).resolves.toEqual({
      status: 404,
      code: 'NOT_FOUND',
      detail: '会话不存在',
    });
  });

  it('非 JSON 错误体:不抛异常,code 缺失(旧端点/代理返回 HTML)', async () => {
    const res = new Response('<html>502 Bad Gateway</html>', { status: 502 });
    await expect(readErrorParts(res)).resolves.toEqual({
      status: 502,
      code: undefined,
      detail: undefined,
    });
  });

  it('401 未带 code 时兜底为 UNAUTHORIZED(与 request 一致)', async () => {
    const res = new Response(JSON.stringify({ error: 'unauthorized' }), { status: 401 });
    await expect(readErrorParts(res)).resolves.toMatchObject({ code: 'UNAUTHORIZED' });
  });

  it('toApiError 的 message 走 apiErrorMessage 分类(Db/Internal → 统一提示)', async () => {
    const res = new Response(JSON.stringify({ error: 'SQLite: no such column', code: 'DB' }), {
      status: 500,
    });
    const err = await toApiError(res);
    expect(err).toBeInstanceOf(ApiError);
    expect(err.status).toBe(500);
    expect(err.code).toBe('DB');
    // 原文保留在 detail 供排查,用户提示为统一文案
    expect(err.detail).toBe('SQLite: no such column');
    expect(err.message).toBe('服务端错误,请查看日志');
  });
});

describe('pumpSseFrames(SSE 读循环:chat 与 tasks 共用)', () => {
  it('按帧回调 data 文本;跨 chunk 的帧边界正确重组', async () => {
    // 第 2 块把 `data: {"a"` 与 `:1}\n\n` 切断,验证跨块缓冲
    const res = streamingResponse(['data: {"a"', ':1}\n\ndata: second\n\n']);
    const frames: string[] = [];
    await pumpSseFrames(res, (data) => frames.push(data));
    expect(frames).toEqual(['{"a":1}', 'second']);
  });

  it('兼容 CRLF 分帧;KeepAlive 注释行(无 data)跳过', async () => {
    const res = streamingResponse([': keep-alive\r\n\r\ndata: x\r\n\r\n']);
    const frames: string[] = [];
    await pumpSseFrames(res, (data) => frames.push(data));
    expect(frames).toEqual(['x']);
  });

  it('EOF 时无空行结尾的残留帧照常派发', async () => {
    const res = streamingResponse(['data: tail']);
    const frames: string[] = [];
    await pumpSseFrames(res, (data) => frames.push(data));
    expect(frames).toEqual(['tail']);
  });

  it('无 body 的响应:直接返回(不抛异常,调用方按错误分支处理)', async () => {
    const res = new Response(null, { status: 204 });
    await expect(pumpSseFrames(res, () => {})).resolves.toBeUndefined();
  });
});

describe('uploadForm(multipart 上传统一底座)', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('成功:解析 JSON 返回,表单含文件与附加字段', async () => {
    const mock = mockFetchOnce(
      new Response(JSON.stringify({ token: 't' }), { status: 200 }),
      new Response(JSON.stringify({ id: 'wb1' }), { status: 200 }),
    );

    const file = new File(['{}'], 'book.json', { type: 'application/json' });
    const out = await uploadForm<{ id: string }>('/world-books/upload', file, {
      character_id: 'c1',
    });

    expect(out.id).toBe('wb1');
    const init = mock.mock.calls[1][1] as RequestInit;
    expect(init.method).toBe('POST');
    expect(init.body).toBeInstanceOf(FormData);
    const form = init.body as FormData;
    expect(form.get('character_id')).toBe('c1');
    expect((form.get('file') as File).name).toBe('book.json');
    // Content-Type 必须留给浏览器自动带 boundary(故 authorizedHeaders 的 json=false)
    expect(new Headers(init.headers).has('Content-Type')).toBe(false);
  });

  it('失败:抛 ApiError(而非裸 Error),用户提示按 code 分类', async () => {
    mockFetchOnce(
      new Response(JSON.stringify({ token: 't' }), { status: 200 }),
      new Response(JSON.stringify({ error: '插件文件须为 .json 格式', code: 'VALIDATION' }), {
        status: 400,
      }),
    );

    const file = new File(['x'], 'bad.txt', { type: 'text/plain' });
    const err = await uploadForm('/plugins/tools/upload', file).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).code).toBe('VALIDATION');
    expect((err as ApiError).message).toBe('插件文件须为 .json 格式');
  });

  it('401:丢弃缓存 token 重新引导并重试一次(服务端重启 token 轮换)', async () => {
    const mock = mockFetchOnce(
      new Response(JSON.stringify({ token: 'old' }), { status: 200 }),
      new Response(JSON.stringify({ error: 'unauthorized', code: 'UNAUTHORIZED' }), { status: 401 }),
      new Response(JSON.stringify({ token: 'new' }), { status: 200 }),
      new Response(JSON.stringify({ id: 'ok' }), { status: 200 }),
    );

    const file = new File(['{}'], 'b.json', { type: 'application/json' });
    const out = await uploadForm<{ id: string }>('/world-books/upload', file);

    expect(out.id).toBe('ok');
    expect(mock).toHaveBeenCalledTimes(4);
    const last = mock.mock.calls[3][1] as RequestInit;
    expect(new Headers(last.headers).get('Authorization')).toBe('Bearer new');
  });

  it('重试后仍 401:抛 UNAUTHORIZED,不无限重试', async () => {
    const mock = mockFetchOnce(
      new Response(JSON.stringify({ token: 'old' }), { status: 200 }),
      new Response(JSON.stringify({ error: 'x', code: 'UNAUTHORIZED' }), { status: 401 }),
      new Response(JSON.stringify({ token: 'new' }), { status: 200 }),
      new Response(JSON.stringify({ error: 'x', code: 'UNAUTHORIZED' }), { status: 401 }),
    );

    const file = new File(['{}'], 'b.json', { type: 'application/json' });
    await expect(uploadForm('/world-books/upload', file)).rejects.toMatchObject({
      code: 'UNAUTHORIZED',
      status: 401,
    });
    expect(mock).toHaveBeenCalledTimes(4);
  });
});
