/**
 * 资源界面存储快照(父页面侧持久化桥)。
 *
 * 背景:资源卡片 iframe 沙箱(无 allow-same-origin)内,作者页面的
 * localStorage/sessionStorage/Cache API 由宿主文档(resource_frame_template.html)
 * 的内存 shim 承接;shim 每次变更经 postMessage(type:'store-sync')同步到本模块,
 * 内存聚合后防抖持久化——storage 快照存 localStorage(量小),Cache 体(base64)
 * 存 IndexedDB(量大,如下载的资源包)。
 *
 * 打通的链路:作者页面下载完成 → shim 同步到本模块持久化 → 作者页面
 * location.reload() 自举(或用户重启 App)→ 宿主文档重新 ready → 父页面用
 * 缓存 HTML + 最新快照重新 boot → 作者脚本初始化时命中 Cache/localStorage
 * 快照 → 跳过下载页直接进游戏界面。
 *
 * 快照按资源 URL 键控(同一作者页在不同消息/角色间共享下载成果)。
 */

export interface ResourceCacheEntry {
  headers: Record<string, string>;
  /** 响应体(结构化克隆直传/直存 IndexedDB,零编解码);null = 只存了 headers(body 读取失败的回退) */
  body: ArrayBuffer | null;
}

export interface ResourceSnapshot {
  local: Record<string, string>;
  session: Record<string, string>;
  /** cacheName → reqUrl → 响应快照 */
  caches: Record<string, Record<string, ResourceCacheEntry>>;
}

/** iframe 宿主文档 store-sync 消息(去掉 channel/nonce/type 后的载荷) */
export interface ResourceSyncMessage {
  subtype: 'storage' | 'cache';
  which?: 'local' | 'session';
  op: 'set' | 'remove' | 'clear' | 'put' | 'delete';
  key?: string;
  value?: string;
  cache?: string;
  req?: string;
  headers?: Record<string, string>;
  /** Cache 体:优先 ArrayBuffer(postMessage 结构化克隆直传,零编解码开销);
   * bodyB64 仅为旧格式兼容读入,不再产生 */
  bodyBuf?: ArrayBuffer | null;
  bodyB64?: string | null;
}

export function emptyResourceSnapshot(): ResourceSnapshot {
  return { local: {}, session: {}, caches: {} };
}

/**
 * 单条 Cache 体上限(原始字节)。
 * 已知真实资源包:吸血鬼卡 v2.1.bin ≈ 96MB——上限过低会把下载成果丢出快照,
 * reload/重启后缓存必然 miss,陷入「下载 96MB→丢→重下」死循环。
 * 仍设上限防异常页面撑爆 IndexedDB。
 */
const MAX_BODY_BYTES = 192 * 1024 * 1024;
/** 单个快照 storage 键数上限(同上的防护) */
const MAX_STORAGE_KEYS = 5000;

/** 旧格式(base64 字符串)兼容解码:本功能首版曾以 base64 入库 */
function b64ToBuf(b64: string): ArrayBuffer | null {
  try {
    const s = atob(b64);
    const u = new Uint8Array(s.length);
    for (let i = 0; i < s.length; i++) u[i] = s.charCodeAt(i);
    return u.buffer;
  } catch {
    return null;
  }
}

/** 从同步消息取 Cache 体:优先 ArrayBuffer 直传,旧格式 base64 兜底解码 */
function pickBody(m: ResourceSyncMessage): ArrayBuffer | null {
  if (m.bodyBuf instanceof ArrayBuffer && m.bodyBuf.byteLength <= MAX_BODY_BYTES) return m.bodyBuf;
  if (typeof m.bodyB64 === 'string' && m.bodyB64.length <= MAX_BODY_BYTES * 2) return b64ToBuf(m.bodyB64);
  return null;
}

/**
 * 纯函数:把一条 shim 同步消息应用到快照(与宿主文档 shim 的发送逻辑一一对应)。
 * 非法/超界消息静默忽略(来源是不可信的作者页面,仅作尽力持久化)。
 */
export function applySyncToSnapshot(snap: ResourceSnapshot, m: ResourceSyncMessage): void {
  if (!m || typeof m !== 'object') return;
  if (m.subtype === 'storage') {
    const bag = m.which === 'session' ? snap.session : m.which === 'local' ? snap.local : null;
    if (!bag) return;
    if (m.op === 'set' && typeof m.key === 'string' && typeof m.value === 'string') {
      if (Object.keys(bag).length < MAX_STORAGE_KEYS || m.key in bag) bag[m.key] = m.value;
    } else if (m.op === 'remove' && typeof m.key === 'string') {
      delete bag[m.key];
    } else if (m.op === 'clear') {
      for (const k of Object.keys(bag)) delete bag[k];
    }
    return;
  }
  if (m.subtype === 'cache') {
    if (typeof m.cache !== 'string' || typeof m.req !== 'string') return;
    if (m.op === 'put') {
      const body = pickBody(m);
      const cache = (snap.caches[m.cache] ??= {});
      cache[m.req] = { headers: m.headers && typeof m.headers === 'object' ? { ...m.headers } : {}, body };
    } else if (m.op === 'delete') {
      const cache = snap.caches[m.cache];
      if (cache) delete cache[m.req];
    }
  }
}

// ---------------------------------------------------------------- 持久化层

const STORAGE_KEY = 'kedai.resource-storage.v1';
const IDB_NAME = 'kedai-resource-cache';
const IDB_STORE = 'caches';

/** 内存聚合表(资源 URL → 快照) */
const memory = new Map<string, ResourceSnapshot>();
const loaded = new Set<string>();
const persistTimers = new Map<string, ReturnType<typeof setTimeout>>();

function hasLocalStorage(): boolean {
  try {
    return typeof localStorage !== 'undefined';
  } catch {
    return false;
  }
}

/** localStorage 部分:全部 URL 的 {local, session} 存在一个 JSON 键下 */
function readStoragePart(): Record<string, { local?: Record<string, string>; session?: Record<string, string> }> {
  if (!hasLocalStorage()) return {};
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    return parsed && typeof parsed === 'object' ? (parsed as Record<string, { local?: Record<string, string>; session?: Record<string, string> }>) : {};
  } catch {
    return {};
  }
}

function writeStoragePart(): void {
  if (!hasLocalStorage()) return;
  try {
    const out: Record<string, { local: Record<string, string>; session: Record<string, string> }> = {};
    for (const [url, snap] of memory) {
      if (Object.keys(snap.local).length || Object.keys(snap.session).length) {
        out[url] = { local: snap.local, session: snap.session };
      }
    }
    localStorage.setItem(STORAGE_KEY, JSON.stringify(out));
  } catch {
    // 配额满等:不阻断主流程,内存快照仍可用于本次会话内的 reload 自举
  }
}

let dbPromise: Promise<IDBDatabase | null> | null = null;

function openDb(): Promise<IDBDatabase | null> {
  if (dbPromise) return dbPromise;
  dbPromise = new Promise((resolve) => {
    if (typeof indexedDB === 'undefined') {
      resolve(null);
      return;
    }
    try {
      const req = indexedDB.open(IDB_NAME, 1);
      req.onupgradeneeded = () => {
        req.result.createObjectStore(IDB_STORE);
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => resolve(null);
    } catch {
      resolve(null);
    }
  });
  return dbPromise;
}

async function idbGet(url: string): Promise<Record<string, Record<string, ResourceCacheEntry>> | null> {
  const db = await openDb();
  if (!db) return null;
  return new Promise((resolve) => {
    try {
      const tx = db.transaction(IDB_STORE, 'readonly');
      const req = tx.objectStore(IDB_STORE).get(url);
      req.onsuccess = () => {
        const v = req.result as { caches?: Record<string, Record<string, ResourceCacheEntry>> } | undefined;
        resolve(v && v.caches && typeof v.caches === 'object' ? v.caches : null);
      };
      req.onerror = () => resolve(null);
    } catch {
      resolve(null);
    }
  });
}

async function idbPut(url: string, caches: Record<string, Record<string, ResourceCacheEntry>>): Promise<void> {
  const db = await openDb();
  if (!db) return;
  return new Promise((resolve) => {
    try {
      const tx = db.transaction(IDB_STORE, 'readwrite');
      tx.objectStore(IDB_STORE).put({ caches }, url);
      tx.oncomplete = () => resolve();
      tx.onerror = () => resolve();
    } catch {
      resolve();
    }
  });
}

/**
 * 读取资源 URL 的累计快照(内存优先,首次访问时从 localStorage + IndexedDB 恢复)。
 * 返回的是内存活对象;boot 时经 postMessage 结构化克隆传给 iframe,无需拷贝。
 */
export async function loadResourceSnapshot(url: string): Promise<ResourceSnapshot> {
  const hit = memory.get(url);
  if (hit && loaded.has(url)) return hit;
  const snap = hit ?? emptyResourceSnapshot();
  if (!loaded.has(url)) {
    loaded.add(url);
    const storagePart = readStoragePart()[url];
    if (storagePart?.local) Object.assign(snap.local, storagePart.local);
    if (storagePart?.session) Object.assign(snap.session, storagePart.session);
    const caches = await idbGet(url);
    if (caches) {
      // 旧格式兼容:首版以 bodyB64(base64 字符串)入库,统一转成 body(ArrayBuffer)
      for (const name of Object.keys(caches)) {
        for (const req of Object.keys(caches[name] ?? {})) {
          const e = caches[name][req] as ResourceCacheEntry & { bodyB64?: string | null };
          if (e && e.body == null && typeof e.bodyB64 === 'string') {
            e.body = b64ToBuf(e.bodyB64);
            delete e.bodyB64;
          }
        }
        snap.caches[name] = { ...(snap.caches[name] ?? {}), ...caches[name] };
      }
    }
  }
  memory.set(url, snap);
  return snap;
}

/** 应用一条 iframe shim 同步消息并调度防抖持久化 */
export function applyResourceSync(url: string, m: ResourceSyncMessage): void {
  let snap = memory.get(url);
  if (!snap) {
    snap = emptyResourceSnapshot();
    memory.set(url, snap);
    // 异步回填持久化部分(与本次操作合并不冲突:操作直接落在活对象上)
    void loadResourceSnapshot(url);
  }
  applySyncToSnapshot(snap, m);
  const prev = persistTimers.get(url);
  if (prev) clearTimeout(prev);
  persistTimers.set(
    url,
    setTimeout(() => {
      persistTimers.delete(url);
      writeStoragePart();
      const s = memory.get(url);
      if (s) void idbPut(url, s.caches);
    }, 800),
  );
}

/** 供测试:清空内存状态(不影响持久化层) */
export function resetResourceStoreForTest(): void {
  memory.clear();
  loaded.clear();
  for (const t of persistTimers.values()) clearTimeout(t);
  persistTimers.clear();
  dbPromise = null;
}
