// 全局错误兜底(计划批次 5.1 / 遗留「项 10」):把「组件内未捕获异常静默消失」
// 变成用户可见、可诊断的一条提示。
//
// 背景:此前 web/src/main.ts 只有 createApp().use(createPinia()).mount('#app'),
// 既无 app.config.errorHandler,也无 unhandledrejection / window error 监听——组件里
// 抛出的异常只会在控制台留一行,界面上留下一块空白,用户与排查者都无从下手。
// 唯一同类代码在角色卡沙箱 iframe 内部(boot-script.ts),管不到宿主。
//
// 覆盖三条路径(它们的触发场景互不重叠,必须都接):
//   1. app.config.errorHandler —— Vue 组件渲染/生命周期/侦听器/钩子内抛出的同步异常;
//   2. window 'unhandledrejection' —— 未被 await/catch 的 Promise 拒绝(含 async 组件);
//   3. window 'error' —— 组件外(定时器、事件回调、脚本加载)的未捕获同步异常。
//
// 隐私口径:只上报异常的**消息文本**,不读取、不拼接任何聊天正文或消息内容;
// 消息截断到 MAX_MESSAGE_LEN,避免把整段堆栈铺进横幅。
//
// 防刷屏:同一消息在 DEDUPE_WINDOW_MS 内只上报一次。异常若发生在响应式渲染路径上,
// 会以每帧一次的频率重复抛出,没有去重会把横幅刷成跑马灯,并让 setState 陷入自激。
import type { ComponentPublicInstance } from 'vue';

/** 单条错误消息的最大展示长度(超出截断并加省略号) */
const MAX_MESSAGE_LEN = 300;
/** 同一消息的去重窗口:窗口内重复的相同消息只上报一次 */
const DEDUPE_WINDOW_MS = 3000;

/** 把任意抛出值归一为可展示的一行文本 */
export function describeError(value: unknown): string {
  let text: string;
  if (value instanceof Error) {
    text = value.message || value.name || '未知错误';
  } else if (typeof value === 'string') {
    text = value;
  } else if (value && typeof value === 'object' && 'message' in value) {
    // 跨 realm 的 Error(如 iframe 抛出)不是本 realm 的 Error 实例,但仍有 message
    const msg = (value as { message?: unknown }).message;
    text = typeof msg === 'string' && msg ? msg : String(value);
  } else {
    text = String(value);
  }
  text = text.trim() || '未知错误';
  return text.length > MAX_MESSAGE_LEN ? `${text.slice(0, MAX_MESSAGE_LEN)}…` : text;
}

/**
 * Vue app 的最小结构依赖。签名必须与 Vue 内部的 `errorHandler` 字段逐字一致
 * (`node_modules/@vue/runtime-core` 的 App.config.errorHandler):Vue 未导出
 * `ErrorHandler` 类型别名,而函数参数在 strictFunctionTypes 下按逆变检查,
 * 把 instance 写成 `unknown` 会让真实的 App 实例无法赋值进来(TS2322)。
 * 用 `ComponentPublicInstance | null` 即与 Vue 定义同构。
 */
interface VueAppLike {
  config: {
    errorHandler?: (
      err: unknown,
      instance: ComponentPublicInstance | null,
      info: string,
    ) => void;
  };
}

export interface InstallOptions {
  /** 错误上报回调(生产传入 uiPrefs store 的 globalError 写入) */
  onError: (message: string) => void;
  /** 注入点(测试用);默认取全局 window */
  win?: Pick<Window, 'addEventListener' | 'removeEventListener'>;
  /** Vue app 实例(测试用);不传则只装两条 window 路径 */
  app?: VueAppLike;
  /** 当前时间源(测试用);默认 Date.now */
  now?: () => number;
}

/**
 * 注册全局错误兜底,返回卸载函数(测试用;生产进程生命周期内不卸载)。
 *
 * 卸载移除本模块自己注册的两条 window 监听,并把 app.config.errorHandler 复位为
 * **安装前的值**(通常是 undefined)——不留下悬挂引用。
 */
export function installGlobalErrorHandlers(opts: InstallOptions): () => void {
  const win = opts.win ?? (typeof window !== 'undefined' ? window : undefined);
  const now = opts.now ?? Date.now;
  // 去重台账:消息 → 上次上报时间。只增不删会随错误种类累积,故在上报时顺带清过期项。
  const lastSeen = new Map<string, number>();

  const report = (value: unknown): void => {
    const message = describeError(value);
    const ts = now();
    const prev = lastSeen.get(message);
    if (prev !== undefined && ts - prev < DEDUPE_WINDOW_MS) return;
    lastSeen.set(message, ts);
    if (lastSeen.size > 64) {
      for (const [k, v] of lastSeen) {
        if (ts - v >= DEDUPE_WINDOW_MS) lastSeen.delete(k);
      }
    }
    // 控制台始终留痕(即使横幅被用户关掉),带仓库既有 [kedai] 前缀
    console.error('[kedai] 未捕获异常:', value);
    try {
      opts.onError(message);
    } catch (e) {
      // 上报通道自身出错绝不能反过来再抛:那会与 unhandledrejection 形成递归
      console.error('[kedai] 错误横幅写入失败:', e);
    }
  };

  const onRejection = (e: PromiseRejectionEvent): void => report(e.reason);
  // 'error' 事件里 value 优先取 e.error(真实异常对象,有 message),退回 e.message
  const onWindowError = (e: ErrorEvent): void => report(e.error ?? e.message);

  win?.addEventListener('unhandledrejection', onRejection as EventListener);
  win?.addEventListener('error', onWindowError as EventListener);

  const hadHandler = opts.app?.config.errorHandler;
  if (opts.app) {
    opts.app.config.errorHandler = (err: unknown) => report(err);
  }

  return () => {
    win?.removeEventListener('unhandledrejection', onRejection as EventListener);
    win?.removeEventListener('error', onWindowError as EventListener);
    if (opts.app) {
      // 恢复安装前的值(通常是 undefined),不留下悬挂引用
      opts.app.config.errorHandler = hadHandler;
    }
  };
}
