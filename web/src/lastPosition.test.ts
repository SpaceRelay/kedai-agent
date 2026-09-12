import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  readLastCharacterId,
  readLastSessionId,
  removeCharacterPosition,
  writeLastCharacterId,
  writeLastSessionId,
} from './lastPosition';

// node 环境无 localStorage,补内存桩(与 storeFacade.test.ts 同款)
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

beforeEach(() => memStorage.clear());

describe('上次浏览位置记忆', () => {
  it('写入并读回上次角色', () => {
    expect(readLastCharacterId()).toBeNull();
    writeLastCharacterId('c1');
    expect(readLastCharacterId()).toBe('c1');
    writeLastCharacterId(null);
    expect(readLastCharacterId()).toBeNull();
  });

  it('按角色记忆上次会话,互不影响', () => {
    writeLastSessionId('c1', 's1');
    writeLastSessionId('c2', 's2');
    expect(readLastSessionId('c1')).toBe('s1');
    expect(readLastSessionId('c2')).toBe('s2');
    expect(readLastSessionId('c3')).toBeNull();
    // 覆盖写
    writeLastSessionId('c1', 's9');
    expect(readLastSessionId('c1')).toBe('s9');
  });

  it('删卡时清理该卡位置记忆;若是当前角色一并清掉', () => {
    writeLastCharacterId('c1');
    writeLastSessionId('c1', 's1');
    writeLastSessionId('c2', 's2');
    removeCharacterPosition('c1');
    expect(readLastSessionId('c1')).toBeNull();
    expect(readLastCharacterId()).toBeNull();
    expect(readLastSessionId('c2')).toBe('s2');
  });

  it('损坏的 JSON 记忆静默回退空值', () => {
    memStorage.set('kedai.char-session.v1', '{bad json');
    expect(readLastSessionId('c1')).toBeNull();
    // 损坏后仍可写入恢复
    writeLastSessionId('c1', 's1');
    expect(readLastSessionId('c1')).toBe('s1');
  });
});
