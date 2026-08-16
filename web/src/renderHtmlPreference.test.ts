import { describe, expect, it } from 'vitest';
import { LocalRenderHtmlPreferenceStore, parseRenderHtmlOverrides, type StorageLike } from './renderHtmlPreference';

function memoryStorage(initial: Record<string, string> = {}): StorageLike {
  const data = { ...initial };
  return {
    getItem: (key) => (key in data ? data[key] : null),
    setItem: (key, value) => { data[key] = value; },
  };
}

describe('HTML 渲染开关按角色卡记忆', () => {
  it('空存储读回空表', () => {
    expect(parseRenderHtmlOverrides(null)).toEqual({});
    expect(parseRenderHtmlOverrides('')).toEqual({});
  });

  it('损坏 JSON 回退空表,不抛出', () => {
    expect(parseRenderHtmlOverrides('{oops')).toEqual({});
    expect(parseRenderHtmlOverrides('"string"')).toEqual({});
    expect(parseRenderHtmlOverrides('[1,2]')).toEqual({});
    expect(parseRenderHtmlOverrides('42')).toEqual({});
  });

  it('仅保留 角色id -> boolean,非法字段丢弃', () => {
    const raw = JSON.stringify({
      'card-a': true,
      'card-b': 'yes',
      'card-c': 1,
      '': false,
      'card-d': { nested: true },
    });
    expect(parseRenderHtmlOverrides(raw)).toEqual({ 'card-a': true });
  });

  it('write 后可 read 回同一份记忆', () => {
    const storage = memoryStorage();
    const store = new LocalRenderHtmlPreferenceStore(storage);
    store.write({ 'card-a': true, 'card-b': false });
    expect(store.read()).toEqual({ 'card-a': true, 'card-b': false });
  });

  it('单条记忆更新不丢其他卡(按卡独立)', () => {
    const storage = memoryStorage();
    const store = new LocalRenderHtmlPreferenceStore(storage);
    store.write({ 'card-a': true, 'card-b': false });
    store.write({ ...store.read(), 'card-b': true });
    expect(store.read()).toEqual({ 'card-a': true, 'card-b': true });
  });
});
