// mvu 宿主:在 window 上挂载 MagVarUpdate 兼容的全局 API,
// 供角色卡内嵌的状态栏脚本在受控环境下运行。
// 全局 API 命名兼容 MagVarUpdate 生态(原作者:MagicalAstrogy,
// github.com/MagicalAstrogy/MagVarUpdate,MIT)。Kedai 为独立兼容实现。
// 挂载的全局:Mvu(对象 + getVariables/getMvuData/replaceMvuData/parseMessage 等)、
// getAllVariables、waitGlobalInitialized、errorCatched、_ (lodash 子集 get/set)、$ (mini-jquery)。
import type { MvuVariables } from './variables';
import { pathGet, pathSet } from './variables';
import { miniJQuery, setJQueryRoot, flushJqReady } from './mini-jquery';
import { parseUpdateVariable } from './parser';

/** 脚本执行时的上下文:注入容器 + 当前消息变量树 */
export interface MvuHostContext {
  container: Element;
  variables: MvuVariables;
}

let activeContext: MvuHostContext | null = null;

/** 在每次执行脚本前调用:绑定容器与变量树 */
export function setMvuHostContext(ctx: MvuHostContext | null): void {
  activeContext = ctx;
  setJQueryRoot(ctx ? ctx.container : null);
}

export function getActiveVariables(): MvuVariables | null {
  return activeContext?.variables ?? null;
}

type MvuEventCb = (payload: unknown) => void;
const eventBus = new Map<string, Set<MvuEventCb>>();

function emit(event: string, payload: unknown): void {
  eventBus.get(event)?.forEach((cb) => {
    try {
      cb(payload);
    } catch {
      /* 忽略插件事件错误 */
    }
  });
}

let installed = false;

/** 触发 mvu 事件(外部模块在变量更新后调用,兼容原版 mag_variable_updated) */
export function emitMvuEvent(event: string, payload?: unknown): void {
  emit(event, payload);
}

const isPlainObject = (v: unknown): v is Record<string, unknown> =>
  !!v && typeof v === 'object' && !Array.isArray(v);

/**
 * lodash 子集(状态栏/界面脚本常用:_get/_set/_isEmpty/_forEach/_has 等)。
 * stat_data 叶子为 [新值, 更新条件] 成对数组,_.get 自动解包取 [0](MagVarUpdate 生态约定)。
 * 抽出为纯函数便于单测;installMvuGlobals 挂到 window._。
 */
export function createLodashShim(): Record<string, unknown> {
  return {
    get: (obj: unknown, path: string, def?: unknown) => {
      let v = pathGet(obj, path);
      if (Array.isArray(v) && v.length >= 1) v = v[0];
      return v === undefined || v === null ? def : v;
    },
    set: (obj: Record<string, unknown>, path: string, value: unknown) => {
      pathSet(obj, path, value);
      return obj;
    },
    // 空判定:null/undefined/''/[]/{}(空数组/空对象/空字符串均视为空)
    isEmpty: (v: unknown) => {
      if (v === null || v === undefined) return true;
      if (typeof v === 'string') return v.length === 0;
      if (Array.isArray(v)) return v.length === 0;
      if (isPlainObject(v)) return Object.keys(v).length === 0;
      return false;
    },
    // 遍历数组或对象,回调 (value, key/index);返回集合本身(链式)
    forEach: (collection: unknown, fn: (value: unknown, key: string | number) => void) => {
      if (Array.isArray(collection)) {
        collection.forEach((item, i) => fn(item, i));
      } else if (isPlainObject(collection)) {
        for (const k of Object.keys(collection)) fn(collection[k], k);
      }
      return collection;
    },
    has: (obj: unknown, path: string) => {
      const v = pathGet(obj, path);
      return v !== undefined;
    },
    isArray: (v: unknown) => Array.isArray(v),
    isObject: (v: unknown) => isPlainObject(v),
    isNil: (v: unknown) => v === null || v === undefined,
    keys: (v: unknown) => (isPlainObject(v) ? Object.keys(v) : Array.isArray(v) ? Array.from(v.keys()) : []),
    values: (v: unknown) => (isPlainObject(v) ? Object.values(v) : Array.isArray(v) ? Array.from(v.values()) : []),
  };
}

/** 幂等挂载全部全局 API(window 级别,仅一次) */
export function installMvuGlobals(): void {
  if (installed) return;
  installed = true;

  const w = window as unknown as Record<string, unknown>;

  w.Mvu = {
    getVariables: () => activeContext?.variables ?? { stat_data: {}, display_data: {} },
    // ---- 原版 MagVarUpdate 兼容 API(别名,保留现有 getVariables 不破坏兼容) ----
    /** 当前变量数据快照({ stat_data, display_data }) */
    getMvuData: () => {
      const v = activeContext?.variables ?? { stat_data: {}, display_data: {} };
      return { stat_data: v.stat_data, display_data: v.display_data };
    },
    /** 整体替换变量数据(脚本内主动设置);触发 mag_variable_updated 事件 */
    replaceMvuData: (data: { stat_data?: Record<string, unknown>; display_data?: Record<string, unknown> }) => {
      if (!activeContext || !data || typeof data !== 'object') return;
      const stat = data.stat_data && typeof data.stat_data === 'object' ? data.stat_data : {};
      activeContext.variables.stat_data = stat;
      activeContext.variables.display_data =
        data.display_data && typeof data.display_data === 'object' ? data.display_data : stat;
      emit('mag_variable_updated', { stat_data: stat, display_data: activeContext.variables.display_data });
    },
    /** 解析消息中的 <UpdateVariable> 块,返回 { cleaned, commands }(kedai 解析器) */
    parseMessage: (text: unknown) => {
      try {
        return parseUpdateVariable(String(text ?? ''));
      } catch {
        return { cleaned: String(text ?? ''), commands: [] };
      }
    },
    /** 是否处于额外分析中(兼容原版字段;kedai 无独立分析阶段,恒为 false) */
    isDuringExtraAnalysis: false,
    on: (event: string, cb: MvuEventCb) => {
      if (!eventBus.has(event)) eventBus.set(event, new Set());
      eventBus.get(event)!.add(cb);
    },
    off: (event: string, cb: MvuEventCb) => {
      eventBus.get(event)?.delete(cb);
    },
    emit,
  };

  w.getAllVariables = () => activeContext?.variables ?? { stat_data: {}, display_data: {} };

  w.waitGlobalInitialized = (name: string, timeoutMs = 30000): Promise<void> =>
    new Promise((resolve) => {
      const deadline = Date.now() + timeoutMs;
      const check = (): void => {
        if ((w as Record<string, unknown>)[name] !== undefined || Date.now() >= deadline) {
          resolve(); // 超时也 resolve,避免对不存在的全局名无限轮询
        } else {
          setTimeout(check, 50);
        }
      };
      check();
    });

  /** 把 fn 包装为错误捕获函数(脚本内 $(errorCatched(init)) 模式) */
  w.errorCatched = (fn: (...a: unknown[]) => unknown) =>
    function (this: unknown, ...args: unknown[]) {
      try {
        return fn.apply(this, args);
      } catch (e) {
        console.warn('[kedai-mvu] 脚本执行出错', e);
        return undefined;
      }
    };

  // lodash 子集(状态栏/界面脚本常用:_get/_set/_isEmpty/_forEach/_has 等;见 createLodashShim)
  w._ = createLodashShim();

  w.$ = miniJQuery;
  w.jQuery = miniJQuery;

  // toastr stub:开场界面/状态栏脚本常经 window.parent.toastr 桥接(WuWa 开场协议、
  // 悬浮球、状态栏提示),宿主无 toastr 时脚本调用即 TypeError。挂轻量 stub:
  // 有真实 toastr 时覆盖,无则静默日志,脚本流程不中断。
  w.toastr = {
    info: (m: unknown) => console.info('[kedai-toastr]', m),
    success: (m: unknown) => console.info('[kedai-toastr]', m),
    warning: (m: unknown) => console.warn('[kedai-toastr]', m),
    error: (m: unknown) => console.warn('[kedai-toastr]', m),
    remove: () => undefined,
    clear: () => undefined,
    options: {},
  } as unknown as Record<string, unknown>;

  // 脚本可能直接引用函数名而非 window 前缀(module 脚本顶层作用域非 window),
  // 因此额外挂到 window 的便捷名已由上面完成;flush 由执行方在容器挂载后调用。
}

export { flushJqReady };
