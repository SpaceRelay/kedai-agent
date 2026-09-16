import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  countTokens,
  getGlobalTotalTokens,
  getModel,
  getPromptInject,
  getPromptPreview,
  getSessionTotalTokens,
  listModels,
  refreshModels,
  settingsInfo,
} from './settings';
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

// 形状闸门(批次 1):这些封装在 `request<T>()` 后直接解构属性,形状不对会把
// undefined 带进设置页/Token 统计(报错点远离真正原因)。
describe('settings API 形状闸门', () => {
  function mock(body: unknown): void {
    resetApiTokenForTest();
    const spy = vi.spyOn(globalThis, 'fetch');
    spy.mockReset();
    spy.mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }));
    spy.mockResolvedValueOnce(new Response(JSON.stringify(body), { status: 200 }));
  }

  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('getModel 缺 model 字符串时透出服务端 error 原文', async () => {
    mock({ error: '连接器未就绪' });
    await expect(getModel()).rejects.toThrow('连接器未就绪');
  });

  it('getModel 缺字段且无原文时抛「模型信息响应格式异常」', async () => {
    mock({ models: [] });
    await expect(getModel()).rejects.toThrow('模型信息响应格式异常');
  });

  it('listModels 缺 models 数组时抛错', async () => {
    mock({ models: 'gpt-4o' });
    await expect(listModels()).rejects.toThrow('模型列表响应格式异常');
  });

  it('refreshModels 缺 models 数组时抛错', async () => {
    mock({ ok: true });
    await expect(refreshModels()).rejects.toThrow('模型列表响应格式异常');
  });

  it('settingsInfo 返回非对象时抛错', async () => {
    mock([]);
    await expect(settingsInfo()).rejects.toThrow('连接器信息响应格式异常');
  });

  it('getPromptInject 缺 config 对象时抛错', async () => {
    mock({ ok: true });
    await expect(getPromptInject()).rejects.toThrow('提示词注入配置响应格式异常');
  });

  it('countTokens 缺 total 数字时抛错(字符串数字也不认,避免隐式算术错误)', async () => {
    mock({ total: '42' });
    await expect(countTokens([{ role: 'user', content: 'x' }])).rejects.toThrow(
      'Token 计数响应格式异常',
    );
  });

  it('getSessionTotalTokens / getGlobalTotalTokens 缺 total_tokens 时抛错', async () => {
    mock({ total: 1 });
    await expect(getSessionTotalTokens('s1')).rejects.toThrow('会话 Token 累计响应格式异常');
    mock({});
    await expect(getGlobalTotalTokens()).rejects.toThrow('全局 Token 累计响应格式异常');
  });

  it('合法形状原样通过(不误伤正常响应)', async () => {
    mock({ model: 'mock-demo' });
    await expect(getModel()).resolves.toBe('mock-demo');
    mock({ models: ['a', 'b'] });
    await expect(listModels()).resolves.toEqual(['a', 'b']);
    mock({ total: 0 });
    await expect(countTokens([])).resolves.toBe(0);
  });
});
