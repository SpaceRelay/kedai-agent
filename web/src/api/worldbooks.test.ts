import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  addWorldBookEntry,
  getCharacterWorldEntries,
  getWorldBookEntries,
  listWorldBooks,
  saveWorldBookEntries,
} from './worldbooks';
import { resetApiTokenForTest } from './client';

// 世界书 API 形状闸门(批次 1)。这些封装在 `request<T>()` 后直接解构 entries /
// world_books 并落列表渲染;形状不对时 undefined 会让面板空白且无从提示。

/**
 * mock 两跳 fetch:第一跳 bootstrap 取 token,第二跳为目标请求。
 * 同一次测试内多次调用会重置 spy 与 token 缓存(否则第二次调用不会走 bootstrap,
 * 队列里的 token 会被当作目标响应消费,产生误导性失败)。
 */
function mock(body: unknown, status = 200): void {
  resetApiTokenForTest();
  const spy = vi.spyOn(globalThis, 'fetch');
  spy.mockReset();
  spy.mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }));
  spy.mockResolvedValueOnce(new Response(JSON.stringify(body), { status }));
}

describe('api/worldbooks 形状闸门', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('listWorldBooks 缺 world_books 数组时透出服务端 error 原文', async () => {
    mock({ error: '世界书服务异常' });
    await expect(listWorldBooks()).rejects.toThrow('世界书服务异常');
  });

  it('listWorldBooks 缺字段且无原文时抛「世界书列表响应格式异常」', async () => {
    mock({ ok: true });
    await expect(listWorldBooks()).rejects.toThrow('世界书列表响应格式异常');
  });

  it('getWorldBookEntries 缺 entries 数组时抛错', async () => {
    mock({ id: 'wb1' });
    await expect(getWorldBookEntries('wb1')).rejects.toThrow('世界书条目响应格式异常');
  });

  it('saveWorldBookEntries 缺 entries 数组时抛错', async () => {
    mock({ ok: true });
    await expect(saveWorldBookEntries('wb1', [])).rejects.toThrow('世界书条目响应格式异常');
  });

  it('addWorldBookEntry 缺 entry 对象时抛错', async () => {
    mock({ ok: true });
    await expect(addWorldBookEntry('wb1')).rejects.toThrow('世界书条目响应格式异常');
  });

  it('getCharacterWorldEntries 缺 entries 数组时抛错', async () => {
    mock({ character_id: 'c1' });
    await expect(getCharacterWorldEntries('c1')).rejects.toThrow('角色卡世界书条目响应格式异常');
  });

  it('合法形状原样通过(空数组也是合法值)', async () => {
    mock({ world_books: [] });
    await expect(listWorldBooks()).resolves.toEqual([]);
    mock({ entries: [] });
    await expect(getWorldBookEntries('wb1')).resolves.toEqual([]);
  });
});
