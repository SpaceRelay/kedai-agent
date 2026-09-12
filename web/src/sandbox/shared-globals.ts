// shared-globals.ts — 跨 realm 卡级共享全局快照桥(JSON 子集)
//
// 背景:沙箱「每脚本一个不透明源 iframe」下,卡级脚本挂在 window 上的纯数据全局
// (如 wuwa MVU 卡的 window.WuWaShared = {STORY_MAP, rawStoryText, isReady})无法被
// 其余 realm(消息级开场白沙箱等)直接读取——跨源读 top/parent 会抛 SecurityError。
// 本模块是宿主侧枢纽:卡级沙箱 diff 上报 window 新增/变更的合法全局(shared-publish),
// 宿主按角色浅合并入内存 + localStorage 持久化,并无防抖通知订阅者;
// 订阅者(同角色其余沙箱)经 shared-update 注入,降级读法(sources=[ST_WIN, globalThis])
// 在第二顺位 globalThis 上读到真值。
//
// 安全边界:仅 JSON 子集——键必须是合法 JS 标识符且不在黑名单(防覆写宿主/沙箱
// window 关键属性与 __kd 内部前缀),值必须 JSON 可序列化且单值 ≤256KB。

/** 单值序列化上限(256KB):剧情数据库级全局够用,超限静默跳过防 postMessage 压力 */
const MAX_VALUE_BYTES = 256 * 1024;

/** 黑名单:宿主/沙箱 window 关键属性,防脚本借共享桥覆写通信基建与环境门面 */
const KEY_BLACKLIST = new Set([
  'window', 'document', 'top', 'parent', 'self', 'globalThis', 'frames',
  'location', 'localStorage', 'sessionStorage', 'eval', 'Function',
  'fetch', 'XMLHttpRequest', 'open', 'close',
]);

const KEY_RE = /^[A-Za-z_$][A-Za-z0-9_$]*$/;

/** 键合法:JS 标识符 + 非黑名单 + 非 __kd 内部前缀(镜像:boot-script.ts 内联副本 __kdSharedKeyOk,语义同步) */
export function isValidSharedKey(key: string): boolean {
  return KEY_RE.test(key) && !KEY_BLACKLIST.has(key) && !key.startsWith('__kd');
}

/** 逐值校验:非法键/不可 JSON 序列化(函数/循环引用)/超 256KB 的项静默跳过 */
export function sanitizeSharedGlobals(input: unknown): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  if (!input || typeof input !== 'object' || Array.isArray(input)) return out;
  for (const [key, value] of Object.entries(input as Record<string, unknown>)) {
    if (!isValidSharedKey(key)) {
      console.debug('[kedai-shared] 拒绝非法共享全局键:', key);
      continue;
    }
    try {
      const serialized = JSON.stringify(value);
      if (typeof serialized !== 'string') {
        console.debug('[kedai-shared] 拒绝不可序列化共享全局:', key);
        continue;
      }
      if (new TextEncoder().encode(serialized).byteLength > MAX_VALUE_BYTES) {
        console.debug('[kedai-shared] 拒绝超大共享全局(>256KB):', key);
        continue;
      }
      // 深拷贝隔离:快照与沙箱内对象不共享引用,后续原地修改不会泄漏进快照
      out[key] = JSON.parse(serialized) as unknown;
    } catch {
      console.debug('[kedai-shared] 拒绝循环引用共享全局:', key);
    }
  }
  return out;
}

/** 内存快照(角色 id → 已发布全局);惰性自 localStorage 水合,发布即更新 */
const store = new Map<string, Record<string, unknown>>();
const subscribers = new Map<string, Set<(globals: Record<string, unknown>) => void>>();

const storageKey = (characterId: string): string => `kedai.card-shared.${characterId}`;

/** 读角色共享全局快照(无则惰性从 localStorage 水合;跨会话重开仍可读) */
export function getCardSharedGlobals(characterId: string): Record<string, unknown> {
  if (!characterId) return {};
  let current = store.get(characterId);
  if (!current) {
    current = {};
    try {
      const raw = localStorage.getItem(storageKey(characterId));
      const parsed: unknown = raw ? JSON.parse(raw) : null;
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        current = sanitizeSharedGlobals(parsed);
      }
    } catch {
      /* localStorage 不可用或数据损坏:按空快照 */
    }
    store.set(characterId, current);
  }
  return current;
}

/** 发布共享全局:浅合并入内存 + localStorage,同步通知订阅者(无防抖:快照小、频率低) */
export function publishCardSharedGlobals(characterId: string, globals: unknown): void {
  if (!characterId) return;
  const clean = sanitizeSharedGlobals(globals);
  if (Object.keys(clean).length === 0) return;
  const current = getCardSharedGlobals(characterId);
  Object.assign(current, clean);
  try {
    localStorage.setItem(storageKey(characterId), JSON.stringify(current));
  } catch {
    /* 配额/localStorage 不可用:内存快照仍生效,持久化失败不阻断脚本 */
  }
  const subs = subscribers.get(characterId);
  if (!subs) return;
  for (const cb of subs) {
    try {
      cb(current);
    } catch {
      /* 单个订阅者异常不扩散 */
    }
  }
}

/** 订阅角色共享全局更新;返回退订函数(沙箱 cleanup 时退订) */
export function subscribeCardSharedGlobals(
  characterId: string,
  cb: (globals: Record<string, unknown>) => void,
): () => void {
  if (!characterId || typeof cb !== 'function') return () => {};
  let set = subscribers.get(characterId);
  if (!set) {
    set = new Set();
    subscribers.set(characterId, set);
  }
  set.add(cb);
  return () => {
    set.delete(cb);
    if (set.size === 0) subscribers.delete(characterId);
  };
}
