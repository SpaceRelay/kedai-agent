import { describe, it, expect, beforeEach } from 'vitest';
import {
  applySyncToSnapshot,
  applyResourceSync,
  loadResourceSnapshot,
  emptyResourceSnapshot,
  resetResourceStoreForTest,
} from './resourceStore';

// localStorage 内存桩(node 环境无 localStorage/indexedDB;
// 持久化桥要求 shim 变更跨 reload/重启存活,storage 部分走 localStorage)
function installLocalStorageStub(): Map<string, string> {
  const map = new Map<string, string>();
  const stub = {
    getItem: (k: string) => (map.has(k) ? map.get(k)! : null),
    setItem: (k: string, v: string) => void map.set(k, String(v)),
    removeItem: (k: string) => void map.delete(k),
    clear: () => map.clear(),
    key: (i: number) => [...map.keys()][i] ?? null,
    get length() {
      return map.size;
    },
  };
  (globalThis as { localStorage?: unknown }).localStorage = stub;
  return map;
}

describe('applySyncToSnapshot(资源界面 shim 同步纯函数)', () => {
  it('storage set/remove/clear 分别落到 local/session', () => {
    const snap = emptyResourceSnapshot();
    applySyncToSnapshot(snap, { subtype: 'storage', which: 'local', op: 'set', key: 'token', value: 'abc' });
    applySyncToSnapshot(snap, { subtype: 'storage', which: 'session', op: 'set', key: 'step', value: '2' });
    expect(snap.local).toEqual({ token: 'abc' });
    expect(snap.session).toEqual({ step: '2' });
    applySyncToSnapshot(snap, { subtype: 'storage', which: 'local', op: 'remove', key: 'token' });
    expect(snap.local).toEqual({});
    applySyncToSnapshot(snap, { subtype: 'storage', which: 'session', op: 'clear' });
    expect(snap.session).toEqual({});
  });

  it('cache put/delete 记录 headers 与 ArrayBuffer 体(结构化克隆直传)', () => {
    const snap = emptyResourceSnapshot();
    const body = new TextEncoder().encode('ABC').buffer;
    applySyncToSnapshot(snap, {
      subtype: 'cache',
      op: 'put',
      cache: 'bby-pack',
      req: 'https://files.yuzuki-rii.xyz/bby/pack.zip',
      headers: { 'x-sha256': 'deadbeef' },
      bodyBuf: body,
    });
    expect(snap.caches['bby-pack']['https://files.yuzuki-rii.xyz/bby/pack.zip']).toEqual({
      headers: { 'x-sha256': 'deadbeef' },
      body,
    });
    applySyncToSnapshot(snap, { subtype: 'cache', op: 'delete', cache: 'bby-pack', req: 'https://files.yuzuki-rii.xyz/bby/pack.zip' });
    expect(snap.caches['bby-pack']).toEqual({});
  });

  it('旧格式 base64 体兼容解码', () => {
    const snap = emptyResourceSnapshot();
    applySyncToSnapshot(snap, {
      subtype: 'cache',
      op: 'put',
      cache: 'c',
      req: 'r',
      headers: {},
      bodyB64: 'QUJD', // "ABC"
    });
    const e = snap.caches['c']['r'];
    expect(e.body).toBeInstanceOf(ArrayBuffer);
    expect(new TextDecoder().decode(e.body!)).toBe('ABC');
  });

  it('非法消息静默忽略(来源是不可信作者页面)', () => {
    const snap = emptyResourceSnapshot();
    applySyncToSnapshot(snap, null as unknown as Parameters<typeof applySyncToSnapshot>[1]);
    applySyncToSnapshot(snap, { subtype: 'storage', which: 'nope' as 'local', op: 'set', key: 'a', value: 'b' });
    applySyncToSnapshot(snap, { subtype: 'storage', which: 'local', op: 'set' });
    applySyncToSnapshot(snap, { subtype: 'cache', op: 'put' });
    // subtype 不在联合内(运行时 'other'):不匹配 storage/cache 任何分支,静默忽略;
    // op 是类型必填字段,补合法值满足类型(不影响运行时忽略路径)
    applySyncToSnapshot(snap, { subtype: 'other' as 'storage', op: 'set' });
    expect(snap).toEqual(emptyResourceSnapshot());
  });

  it('超大体(>192MB)降级为 null(防撑爆 IndexedDB)', () => {
    const snap = emptyResourceSnapshot();
    applySyncToSnapshot(snap, {
      subtype: 'cache',
      op: 'put',
      cache: 'c',
      req: 'r',
      headers: {},
      bodyBuf: new ArrayBuffer(193 * 1024 * 1024),
    });
    expect(snap.caches['c']['r'].body).toBeNull();
  });

  it('96MB 资源包必须保留——吸血鬼卡真实包体,旧 48MB 上限会丢下载成果', () => {
    const snap = emptyResourceSnapshot();
    const big = new ArrayBuffer(96 * 1024 * 1024);
    applySyncToSnapshot(snap, {
      subtype: 'cache',
      op: 'put',
      cache: 'rii-asset-pack-v1',
      req: 'https://cache.rii/pack?src=v2.1.bin',
      headers: { 'x-sha256': 'abc' },
      bodyBuf: big,
    });
    expect(snap.caches['rii-asset-pack-v1']['https://cache.rii/pack?src=v2.1.bin'].body).toBe(big);
  });
});

describe('resourceStore 持久化桥', () => {
  beforeEach(() => {
    resetResourceStoreForTest();
    installLocalStorageStub();
  });

  it('storage 变更防抖后写入 localStorage(按资源 URL 键),重新加载可恢复', async () => {
    const url = 'https://files.yuzuki-rii.xyz/bby/v2.1.html';
    applyResourceSync(url, { subtype: 'storage', which: 'local', op: 'set', key: 'agreed', value: '1' });
    // 防抖 800ms
    await new Promise((r) => setTimeout(r, 900));
    const raw = (globalThis.localStorage as Storage).getItem('kedai.resource-storage.v1');
    expect(raw).toBeTruthy();
    expect(JSON.parse(raw!)[url].local).toEqual({ agreed: '1' });
    // 模拟重启:清内存,重新 load 应从 localStorage 恢复
    resetResourceStoreForTest();
    const snap = await loadResourceSnapshot(url);
    expect(snap.local).toEqual({ agreed: '1' });
  });

  it('loadResourceSnapshot 返回同一内存活对象(增量同步直接落在 boot 快照上)', async () => {
    const url = 'https://a.b/c.html';
    const a = await loadResourceSnapshot(url);
    applyResourceSync(url, { subtype: 'storage', which: 'local', op: 'set', key: 'k', value: 'v' });
    const b = await loadResourceSnapshot(url);
    expect(b).toBe(a);
    expect(b.local.k).toBe('v');
  });
});
