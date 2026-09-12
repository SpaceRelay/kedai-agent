// shared-globals.test.ts — 跨 realm 卡级共享全局桥宿主侧单元测试
// 验证:发布→读取一致、localStorage 持久化与惰性水合、键/值安全边界(黑名单/
// 函数/超大值拒绝)、订阅与退订。内存快照 Map 无重置 API,用例间用唯一角色 id 隔离。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  getCardSharedGlobals,
  isValidSharedKey,
  publishCardSharedGlobals,
  sanitizeSharedGlobals,
  subscribeCardSharedGlobals,
} from './shared-globals';

function memoryStorage() {
  const m = new Map<string, string>();
  return {
    getItem: (k: string): string | null => m.get(k) ?? null,
    setItem: (k: string, v: string): void => void m.set(k, String(v)),
    removeItem: (k: string): void => void m.delete(k),
    clear: (): void => void m.clear(),
    key: (i: number): string | null => [...m.keys()][i] ?? null,
    get length(): number {
      return m.size;
    },
  };
}

let ls: ReturnType<typeof memoryStorage>;

beforeEach(() => {
  ls = memoryStorage();
  vi.stubGlobal('localStorage', ls);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('shared-globals(发布/读取/持久化)', () => {
  it('publish 浅合并入内存与 localStorage;get 返回一致快照', () => {
    publishCardSharedGlobals('sg-a', { WuWaShared: { ready: true }, story: 'S' });
    expect(getCardSharedGlobals('sg-a')).toEqual({ WuWaShared: { ready: true }, story: 'S' });
    expect(ls.getItem('kedai.card-shared.sg-a')).toContain('"ready":true');
    // 再次发布浅合并:旧键保留,新键追加
    publishCardSharedGlobals('sg-a', { story: 'S2' });
    expect(getCardSharedGlobals('sg-a')).toEqual({ WuWaShared: { ready: true }, story: 'S2' });
  });

  it('get 惰性从 localStorage 水合(跨会话重开仍可读);损坏/非法数据按空快照', () => {
    ls.setItem('kedai.card-shared.sg-b', JSON.stringify({ Foo: { a: 1 } }));
    expect(getCardSharedGlobals('sg-b')).toEqual({ Foo: { a: 1 } });
    ls.setItem('kedai.card-shared.sg-c', 'not-json');
    expect(getCardSharedGlobals('sg-c')).toEqual({});
  });

  it('水合时污染键(黑名单/非法标识符)被清洗,不进快照', () => {
    ls.setItem('kedai.card-shared.sg-d', JSON.stringify({ window: 'x', location: 'y', Foo: { a: 1 } }));
    expect(getCardSharedGlobals('sg-d')).toEqual({ Foo: { a: 1 } });
  });
});

describe('shared-globals(安全边界)', () => {
  it('isValidSharedKey:合法 JS 标识符、非黑名单、非 __kd 前缀', () => {
    expect(isValidSharedKey('WuWaShared')).toBe(true);
    expect(isValidSharedKey('_ok$1')).toBe(true);
    expect(isValidSharedKey('window')).toBe(false);
    expect(isValidSharedKey('localStorage')).toBe(false);
    expect(isValidSharedKey('fetch')).toBe(false);
    expect(isValidSharedKey('__kdSecret')).toBe(false);
    expect(isValidSharedKey('1abc')).toBe(false);
    expect(isValidSharedKey('a-b')).toBe(false);
  });

  it('sanitizeSharedGlobals:黑名单键与函数/不可序列化值整项跳过,数组值保留', () => {
    const clean = sanitizeSharedGlobals({
      window: 'x',
      fetch: () => 1,
      list: [1, 2],
      ok: { n: 3 },
    });
    expect(clean).toEqual({ list: [1, 2], ok: { n: 3 } });
  });

  it('单值超 256KB 静默跳过(JSON 子集上限,防 postMessage 压力)', () => {
    const big = 'x'.repeat(300 * 1024);
    const clean = sanitizeSharedGlobals({ ok: 1, big });
    expect(clean).toEqual({ ok: 1 });
  });

  it('publish 全非法时是 no-op(不写 storage、不通知订阅者)', () => {
    const cb = vi.fn();
    subscribeCardSharedGlobals('sg-e', cb);
    publishCardSharedGlobals('sg-e', { window: 'x', fn: () => 1 });
    expect(cb).not.toHaveBeenCalled();
    expect(ls.getItem('kedai.card-shared.sg-e')).toBeNull();
  });
});

describe('shared-globals(订阅/退订)', () => {
  it('publish 同步通知订阅者(带合并后快照);退订后不再收到', () => {
    const seen: Array<Record<string, unknown>> = [];
    const unsub = subscribeCardSharedGlobals('sg-f', (g) => seen.push(g));
    publishCardSharedGlobals('sg-f', { a: 1 });
    expect(seen.length).toBe(1);
    expect(seen[0]).toEqual({ a: 1 });
    unsub();
    publishCardSharedGlobals('sg-f', { b: 2 });
    expect(seen.length).toBe(1);
  });

  it('单订阅者异常不扩散到其余订阅者', () => {
    const bad = vi.fn(() => {
      throw new Error('boom');
    });
    const good = vi.fn();
    subscribeCardSharedGlobals('sg-g', bad);
    subscribeCardSharedGlobals('sg-g', good);
    expect(() => publishCardSharedGlobals('sg-g', { a: 1 })).not.toThrow();
    expect(good).toHaveBeenCalledTimes(1);
  });
});
