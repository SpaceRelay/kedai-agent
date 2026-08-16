import { beforeEach, describe, expect, it, vi } from 'vitest';
import { expandMacros, parseVarsLines } from './macros';
import { resetApiTokenForTest } from './client';

describe('macros API', () => {
  beforeEach(() => {
    resetApiTokenForTest();
    vi.restoreAllMocks();
  });

  it('POST /macros/expand:URL 与方法正确,ctx 序列化传入 body', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ expanded: '雪芽 你好' }), { status: 200 }));

    const got = await expandMacros('{{char}} {{user}}', {
      character_name: '雪芽',
      user_name: '你好',
      vars: { 好感: 88 },
    });
    expect(got).toBe('雪芽 你好');
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    expect(String(req[0])).toBe('/api/macros/expand');
    const init = req[1] as RequestInit;
    expect(init.method).toBe('POST');
    expect(JSON.parse(String(init.body))).toEqual({
      text: '{{char}} {{user}}',
      ctx: { character_name: '雪芽', user_name: '你好', vars: { 好感: 88 } },
    });
  });

  it('无 ctx 时 body 携带空对象,不报错', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ expanded: '' }), { status: 200 }));

    const got = await expandMacros('');
    expect(got).toBe('');
    const req = vi.mocked(globalThis.fetch).mock.calls[1];
    const init = req[1] as RequestInit;
    expect(JSON.parse(String(init.body))).toEqual({ text: '', ctx: {} });
  });

  it('请求失败时抛出服务端 error 信息', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: 't' }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ error: '展开失败' }), { status: 500 }));

    await expect(expandMacros('{{char}}')).rejects.toThrow('展开失败');
  });
});

describe('parseVarsLines(面板变量区解析纯函数)', () => {
  it('key::value 行解析为 map,支持字符串/整数/布尔', () => {
    expect(parseVarsLines('好感::88\n开关::true\n名字::雪芽')).toEqual({
      好感: 88,
      开关: true,
      名字: '雪芽',
    });
  });

  it('空行、无 :: 分隔、key 为空的行忽略', () => {
    expect(parseVarsLines('\n\n好感::88\n纯文本行\n::值\n  \n')).toEqual({ 好感: 88 });
  });

  it('值含 :: 时只按第一个 :: 分割,值原样保留', () => {
    expect(parseVarsLines('地址::a::b')).toEqual({ 地址: 'a::b' });
  });

  it('key 与值两侧空白被去除', () => {
    expect(parseVarsLines('  好感  ::  88  ')).toEqual({ 好感: 88 });
  });
});
