import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  fetchAgentTrace,
  fetchHistory,
  fetchInitVars,
  listAllSessions,
  listSessions,
} from './sessions';
import { resetApiTokenForTest } from './client';

// 会话 / 消息 API 形状闸门(批次 1)。fetchHistory 直接驱动聊天窗口渲染,
// listSessions 落侧栏列表;形状不对时 undefined 会让窗口空白且无从提示。

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

describe('api/sessions 形状闸门', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('listSessions 缺 sessions 数组时透出服务端 error 原文', async () => {
    mock({ error: '会话服务异常' });
    await expect(listSessions('c1')).rejects.toThrow('会话服务异常');
  });

  it('listSessions 缺字段且无原文时抛「会话列表响应格式异常」', async () => {
    mock({ ok: true });
    await expect(listSessions('c1')).rejects.toThrow('会话列表响应格式异常');
  });

  it('listAllSessions 缺 sessions 数组时抛错', async () => {
    mock({});
    await expect(listAllSessions()).rejects.toThrow('会话列表响应格式异常');
  });

  it('fetchHistory 缺 messages 数组时抛错(不能让聊天窗口静默空白)', async () => {
    mock({ session_id: 's1' });
    await expect(fetchHistory('s1')).rejects.toThrow('聊天历史响应格式异常');
  });

  it('fetchAgentTrace:trace 显式为 null 合法(无记录态)', async () => {
    mock({ trace: null });
    await expect(fetchAgentTrace('s1')).resolves.toBeNull();
  });

  it('fetchAgentTrace:trace 字段缺失抛错(缺字段 ≠ 无记录)', async () => {
    mock({ ok: true });
    await expect(fetchAgentTrace('s1')).rejects.toThrow('Agent 记录响应格式异常');
  });

  it('fetchInitVars 缺 entries 对象时抛错', async () => {
    mock({ character_id: 'c1' });
    await expect(fetchInitVars('c1')).rejects.toThrow('初始变量响应格式异常');
  });

  it('合法形状原样通过(空数组/空对象是合法值)', async () => {
    mock({ sessions: [] });
    await expect(listSessions('c1')).resolves.toEqual([]);
    mock({ messages: [] });
    await expect(fetchHistory('s1')).resolves.toEqual([]);
    mock({ entries: {} });
    await expect(fetchInitVars('c1')).resolves.toEqual({});
  });
});
