// iframe-lifecycle.ts — 沙箱 iframe 生命周期:创建/boot 注入/消息分发/几何镜像推送/销毁清理,以及存活沙箱注册表广播与音频 RPC 桥
import { getAudioController } from '../audioController';
import type { AudioChannelSettings, AudioChannelType } from '../api/types';
import type { MvuVariables } from '../mvu/variables';
import {
  CHANNEL,
  INLINE_EVENT_NAMES,
  MAX_MESSAGE_BYTES,
  cloneData,
  utf8Bytes,
  type ContainerGeo,
  type SandboxCleanup,
  type SandboxEnvironment,
  type SandboxExecutionContext,
  type SandboxRequest,
  type ScriptSegment,
} from './protocol';
import { sandboxScript } from './boot-script';
import {
  getCardSharedGlobals,
  publishCardSharedGlobals,
  subscribeCardSharedGlobals,
} from './shared-globals';
import {
  applyJq,
  applyRpc,
  applyScriptLocalStorageOp,
  buildStateMirror,
  elementState,
  gatherControls,
  gatherIdMap,
  type JqBinding,
  readScriptLocalStorage,
} from './dom-rpc';

/** 沙箱文档需要一次网络往返(GET /sandbox.html)后才能 boot;大状态栏脚本(如 wuwa
 *  161KB)反序列化 + 首轮批处理耗时较长,8s 易误杀,放宽到 30s */
const DEFAULT_TIMEOUT_MS = 30_000;

/**
 * 沙箱 bootstrap 文档。必须由服务端下发(见 server-rs security::sandbox_headers),
 * 不能用 srcdoc:srcdoc iframe 继承父页面 CSP,而全站 CSP 是 script-src 'self'
 * (无 'unsafe-inline'),内联脚本会被浏览器直接拒绝,状态栏永远停在「加载中」。
 */
const SANDBOX_DOCUMENT_URL = '/sandbox.html';

export function sandboxAttributes(): Readonly<Record<string, string>> {
  // allow-modals:作者脚本(协议确认/提示)可用 alert/confirm(沙箱内弹窗不触及宿主)
  return { sandbox: 'allow-scripts allow-modals', referrerpolicy: 'no-referrer', 'aria-hidden': 'true' };
}

function readContainerGeo(container: HTMLElement): ContainerGeo {
  // 测试 mock 容器未必实现 getBoundingClientRect/滚动度量,全部回退 0/常见尺寸
  const rect =
    typeof container.getBoundingClientRect === 'function'
      ? container.getBoundingClientRect()
      : ({ top: 0, left: 0, width: 0, height: 0 } as DOMRect);
  const num = (v: unknown): number => (typeof v === 'number' && Number.isFinite(v) ? v : 0);
  const hostH = typeof window !== 'undefined' && Number.isFinite(window.innerHeight) ? window.innerHeight : 720;
  const hostW = typeof window !== 'undefined' && Number.isFinite(window.innerWidth) ? window.innerWidth : 1280;
  return {
    top: num(rect.top),
    left: num(rect.left),
    width: num(rect.width),
    height: num(rect.height),
    scrollHeight: num(container.scrollHeight),
    scrollWidth: num(container.scrollWidth),
    hostH,
    hostW,
  };
}

/** 存活沙箱注册表(nonce → contentWindow):MVU 变量更新时向全部在跑的状态栏脚本广播 */
const liveSandboxes = new Map<string, Window>();

/**
 * 向全部存活沙箱广播最新变量树(消息落库应用 UpdateVariable 后由 chat store 调用)。
 * 沙箱内合并变量并触发 eventOn 订阅(Mvu.events.VARIABLE_UPDATE_ENDED),状态栏随之重渲染。
 */
export function broadcastMvuUpdate(variables: MvuVariables): void {
  if (liveSandboxes.size === 0) return;
  let payload: { stat_data: Record<string, unknown>; display_data: Record<string, unknown> };
  try {
    payload = {
      stat_data: cloneData((variables.stat_data ?? {}) as Record<string, unknown>),
      display_data: cloneData((variables.display_data ?? {}) as Record<string, unknown>),
    };
    if (utf8Bytes(payload) > MAX_MESSAGE_BYTES) return; // 变量树超大时跳过广播,重渲染自取新快照
  } catch {
    return;
  }
  for (const [nonce, win] of liveSandboxes) {
    try {
      win.postMessage(
        { channel: CHANNEL, nonce, type: 'mvu-event', event: 'mag_variable_updated', payload, variables: payload },
        '*',
      );
    } catch {
      /* 沙箱已销毁:忽略 */
    }
  }
}

/**
 * 向全部存活沙箱广播卡级事件(swipe 切换等;对齐酒馆助手事件流)。
 * 沙箱内经事件总线 __kdFireEvent 派发给 eventOn 订阅者(如舰娘卡
 * 「随着开场白切换自动开启关闭世界书」脚本监听 tavern_events.MESSAGE_SWIPED)。
 */
export function broadcastCardEvent(name: string, payload?: unknown): void {
  if (liveSandboxes.size === 0 || !name) return;
  let cloned: unknown = payload;
  try {
    cloned = cloneData(payload ?? null);
    if (utf8Bytes(cloned) > MAX_MESSAGE_BYTES) return;
  } catch {
    return;
  }
  for (const [nonce, win] of liveSandboxes) {
    try {
      win.postMessage({ channel: CHANNEL, nonce, type: 'card-event', name, payload: cloned }, '*');
    } catch {
      /* 沙箱已销毁:忽略 */
    }
  }
}

/** 沙箱启动时经 rpc 拉取的音频全量快照(AudioPlayer 未挂载时回退默认) */
function audioSnapshotValue(): unknown {
  const c = getAudioController();
  const defaults = (type: AudioChannelType): AudioChannelSettings =>
    type === 'bgm'
      ? { enabled: true, mode: 'repeat_all', muted: false, volume: 50 }
      : { enabled: true, mode: 'play_one_and_stop', muted: false, volume: 50 };
  const mk = (type: AudioChannelType) => c
    ? { list: c.getAudioList(type), settings: c.getAudioSettings(type), current: c.getCurrentAudio(type) }
    : { list: [], settings: defaults(type), current: { src: '', title: '', playing: false, progress: 0 } };
  const bgm = mk('bgm');
  const ambient = mk('ambient');
  return {
    list: { bgm: bgm.list, ambient: ambient.list },
    settings: { bgm: bgm.settings, ambient: ambient.settings },
    current: { bgm: bgm.current, ambient: ambient.current },
  };
}

/**
 * 沙箱 TavernHelper 音频 setter 转发(op 白名单;AudioPlayer 未挂载时静默 no-op,
 * 与酒馆音频 slash 命令在无 UI 时的行为一致)。type 非 'ambient' 一律按 'bgm' 处理
 * (对齐酒馆 HF 分发的 switch 语义)。
 */
function handleAudioRpc(op: string, args: unknown[]): void {
  const c = getAudioController();
  if (!c) return;
  const type = (args[0] === 'ambient' ? 'ambient' : 'bgm') as AudioChannelType;
  const second = args[1];
  switch (op) {
    case 'playAudio': {
      const a = second && typeof second === 'object'
        ? second as { title?: unknown; url?: unknown }
        : undefined;
      c.playAudio(
        type,
        a && typeof a.url === 'string'
          ? { title: typeof a.title === 'string' ? a.title : undefined, url: a.url }
          : undefined,
      );
      break;
    }
    case 'pauseAudio':
      c.pauseAudio(type);
      break;
    case 'replaceAudioList':
      c.replaceAudioList(type, Array.isArray(second) ? second as Array<{ title?: string; url: string }> : []);
      break;
    case 'appendAudioList':
      c.appendAudioList(type, Array.isArray(second) ? second as Array<{ title?: string; url: string }> : []);
      break;
    case 'setAudioSettings':
      c.setAudioSettings(type, (second && typeof second === 'object' ? second : {}) as Partial<AudioChannelSettings>);
      break;
    default:
      // 未知音频操作:静默忽略(沙箱侧不感知,保持脚本不中断)
      break;
  }
}

export async function executeSandboxedCharacterScript(
  code: string | ScriptSegment[],
  context: SandboxExecutionContext,
  environment: SandboxEnvironment = { document, window },
): Promise<SandboxCleanup> {
  const nonce = crypto.randomUUID();
  const iframe = environment.document.createElement('iframe');
  for (const [name, value] of Object.entries(sandboxAttributes())) iframe.setAttribute(name, value);
  iframe.setAttribute('tabindex', '-1');
  iframe.style.display = 'none';
  iframe.src = `${SANDBOX_DOCUMENT_URL}#${encodeURIComponent(nonce)}`;

  return new Promise((resolve, reject) => {
    let settled = false;
    let disposed = false;
    const targets = new Map<number, HTMLElement>();
    // document/window 级监听也登记在此(EventTarget),cleanup 统一摘除;
    // capture 必须随监听器记录:capture=true 注册的用默认 capture=false 摘不掉
    const eventListeners: Array<{
      element: EventTarget;
      eventName: string;
      listener: EventListener;
      capture?: boolean;
    }> = [];
    // jQuery 事件绑定登记表(off() 真解绑所需;与 eventListeners 并行,后者供 cleanup)
    const jqBindings: JqBinding[] = [];
    let targetId = 0;
    // 跨 realm 共享全局:同角色其余沙箱 publish 时实时推送本沙箱(全量快照);
    // cleanup 退订(挂在下方统一清理路径)
    let unsubscribeShared: (() => void) | undefined;
    if (context.characterId) {
      const characterId = context.characterId;
      unsubscribeShared = subscribeCardSharedGlobals(characterId, (globals) => {
        if (disposed) return;
        try {
          iframe.contentWindow?.postMessage(
            { channel: CHANNEL, nonce, type: 'shared-update', globals: cloneData(globals) },
            '*',
          );
        } catch {
          /* 沙箱已销毁:忽略 */
        }
      });
    }
    const cleanup = (): void => {
      if (disposed) return;
      disposed = true;
      liveSandboxes.delete(nonce);
      unsubscribeShared?.();
      unsubscribeShared = undefined;
      environment.window.removeEventListener('message', onMessage as EventListener);
      environment.window.removeEventListener('scroll', scheduleGeoPush, true);
      environment.window.removeEventListener('resize', scheduleGeoPush);
      if (geoPushTimer !== undefined) {
        clearTimeout(geoPushTimer);
        geoPushTimer = undefined;
      }
      for (const { element, eventName, listener, capture } of eventListeners) {
        element.removeEventListener(eventName, listener, capture);
      }
      eventListeners.length = 0;
      targets.clear();
      // 本次沙箱注入的 DOM(悬浮窗/面板/style 标签)一并摘除:cleanup 只负责
      // 容器内带 data-kd-injected 的节点,不触碰消息渲染块自身结构
      if (context.container && typeof context.container.querySelectorAll === 'function') {
        try {
          for (const el of Array.from(context.container.querySelectorAll<HTMLElement>('[data-kd-injected]'))) {
            el.remove();
          }
        } catch {
          /* 容器已卸载:忽略 */
        }
      }
      iframe.remove();
    };
    // 宿主容器几何镜像推送:聊天区滚动(scroll 不冒泡,capture 捕获)/窗口 resize 时节流推送,
    // 沙箱内 window.innerHeight/frameElement/document.body 矩形随之更新(作者定位代码的数据源)
    let geoPushTimer: ReturnType<typeof setTimeout> | undefined;
    const pushGeo = (): void => {
      if (disposed || !context.container) return;
      const targetWindow = iframe.contentWindow;
      if (!targetWindow) return;
      try {
        targetWindow.postMessage(
          { channel: CHANNEL, nonce, type: 'geometry', geo: readContainerGeo(context.container) },
          '*',
        );
      } catch {
        /* 沙箱已销毁:忽略 */
      }
    };
    const scheduleGeoPush = (): void => {
      if (geoPushTimer !== undefined) return;
      geoPushTimer = setTimeout(() => {
        geoPushTimer = undefined;
        pushGeo();
      }, 120);
    };
    const finish = (error?: Error): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (error && context.container) {
        // 错误提示仅是辅助信息:容器可能已被 Vue 重渲染移除,appendChild 失败不能阻断
        // 后面的 cleanup/reject,否则 Promise 永不 settle。UI 只显示首行,完整栈进控制台。
        try {
          console.error('[kedai-script]', error.message);
          const el = context.container.querySelector<HTMLElement>('.status-card') ?? context.container;
          const msg = environment.document.createElement('p');
          msg.style.cssText = 'color:#f87171;font-size:0.75rem;margin:4px 0 0;opacity:0.8';
          msg.textContent = `[脚本] ${error.message.split('\n')[0]}`;
          el.appendChild(msg);
        } catch {
          /* 忽略:不影响错误传播 */
        }
      }
      if (error) {
        cleanup();
        reject(error);
      } else {
        resolve(cleanup);
      }
    };
    // inline 事件降级桥(宿主半):render.ts 把 onclick 等剥离为 data-kd-on*,这里为容器内
    // 带降级属性的元素绑真实监听,触发时把代码串 + 目标状态 postMessage 回沙箱求值。
    // 静态 HTML 在 ready 时绑;脚本运行中注入的(append/setHtml 经白名单放行 data-*)在 done 补绑。
    const boundInline = new WeakMap<HTMLElement, Set<string>>();
    const bindInlineHandlers = (): void => {
      const targetWindow = iframe.contentWindow;
      if (!targetWindow || !context.container) return;
      let selectorParts: string[] = [];
      for (const name of INLINE_EVENT_NAMES) selectorParts.push(`[data-kd-on${name}]`);
      let elements: HTMLElement[] = [];
      try {
        elements = Array.from(context.container.querySelectorAll<HTMLElement>(selectorParts.join(',')));
      } catch {
        return;
      }
      for (const el of elements) {
        let bound = boundInline.get(el);
        if (!bound) {
          bound = new Set<string>();
          boundInline.set(el, bound);
        }
        for (const eventName of INLINE_EVENT_NAMES) {
          if (bound.has(eventName)) continue;
          const code = el.getAttribute(`data-kd-on${eventName}`);
          if (!code) continue;
          bound.add(eventName);
          const inlineTargetId = ++targetId;
          targets.set(inlineTargetId, el);
          const listener = (): void => {
            const data = Object.fromEntries(Object.entries(el.dataset));
            targetWindow.postMessage(
              {
                channel: CHANNEL,
                nonce,
                type: 'inline-event',
                code,
                event: { type: eventName },
                target: { kind: 'target', id: inlineTargetId, data, state: elementState(el) },
                mirror: { controls: gatherControls(context.container) },
              },
              '*',
            );
          };
          el.addEventListener(eventName, listener);
          eventListeners.push({ element: el, eventName, listener });
        }
      }
    };
    const onMessage = (event: MessageEvent): void => {
      // 身份校验:高熵 nonce + 固定 channel。
      //
      // 为何不校验 event.source / event.origin(M5 评估结论,勿重复尝试):
      // - origin:沙箱为不透明源(无 allow-same-origin),其 origin 恒为 "null",无判别力
      //   (见 render.ts 注释);
      // - source:WebView2 会把跨源 iframe 的 source 包装成**另一个对象**,与
      //   iframe.contentWindow 不相等。若按「不等即拒」处理,会把正常的 ready/done
      //   全部拒掉——scriptRunner.test.ts「接受 WebView2 source wrapper」正是锁定此契约。
      // 故 source 判别在目标平台不可用,nonce 是唯一可靠身份凭据(128 位随机、
      // 仅存在于执行闭包并经 URL fragment 交给沙箱,不落 DOM/存储)。
      const message = event.data as SandboxRequest;
      if (!message || message.channel !== CHANNEL || message.nonce !== nonce) return;
      try {
        if (message.type === 'ready') {
          // 沙箱文档已从 URL fragment 取得 nonce,认证就绪后才投递一次 boot。
          const boot = {
            channel: CHANNEL,
            nonce,
            type: 'boot',
            script: sandboxScript(
              code,
              nonce,
              context.variables,
              context.globals ?? {},
              readScriptLocalStorage(),
              context.container ? readContainerGeo(context.container) : undefined,
              context.container ? gatherIdMap(context.container) : {},
              context.lorebookName ?? null,
              // 跨 realm 共享全局:未显式传入时按角色自动注入宿主快照
              // (消息级开场白沙箱借此读到卡级 realm 的 window.WuWaShared 等)
              context.sharedGlobals ??
                (context.characterId ? getCardSharedGlobals(context.characterId) : {}),
              // 表单控件快照:document.getElementById 门面读 select/input 真值的 boot 底
              context.container ? gatherControls(context.container) : [],
            ),
          };
          if (utf8Bytes(boot) > MAX_MESSAGE_BYTES) throw new Error('角色卡脚本启动消息过大');
          if (iframe.contentWindow) liveSandboxes.set(nonce, iframe.contentWindow);
          // 静态 HTML 的降级 inline 事件(data-kd-on*)在脚本运行前先绑好,
          // 作者脚本 boot 期间的同步点击也能正确路由
          bindInlineHandlers();
          // 几何镜像推送:聊天滚动/窗口 resize 时沙箱内的定位代码(positionOverlay)随之重锚
          environment.window.addEventListener('scroll', scheduleGeoPush, true);
          environment.window.addEventListener('resize', scheduleGeoPush);
          iframe.contentWindow?.postMessage(boot, '*');
        } else if (utf8Bytes(message) > MAX_MESSAGE_BYTES) {
          throw new Error('角色卡脚本消息过大');
        } else if (message.type === 'local-storage') {
          // 沙箱 localStorage 持久化桥:set/remove/clear 落宿主 localStorage(ST 源级语义)
          applyScriptLocalStorageOp(String(message.op ?? ''), message.key, message.value);
        } else if (message.type === 'shared-publish') {
          // 跨 realm 共享全局:沙箱 diff 上报的 window 纯数据全局,按角色持久化并
          // 广播给同角色其余订阅沙箱(消息级 realm 的降级读源)
          if (context.characterId) publishCardSharedGlobals(context.characterId, message.globals);
        } else if (message.type === 'rpc') {
          // audio-snapshot 是沙箱启动时的音频状态预拉取(非 DOM 操作),
          // 与 applyRpc 的 DOM 白名单并列分发。
          // rpcExtensions 是调用方注入的异步扩展点(世界书读写/聊天消息读取等纯数据
          // op,不过 DOM 白名单);handled=true 时优先于内置分发。回包统一走微任务,
          // 同步 op 的解析值与错误文本形状不变。
          const op = String(message.op ?? '');
          const rpcArgs = Array.isArray(message.args) ? message.args : [];
          const reply = (ok: boolean, payload: { value?: unknown; error?: string }): void => {
            iframe.contentWindow?.postMessage({
              channel: CHANNEL, nonce, type: 'rpc-result', id: message.id, ok,
              value: ok ? cloneData(payload.value === undefined ? null : payload.value) : undefined,
              error: ok ? undefined : payload.error,
            }, '*');
          };
          const builtin = (): unknown => (op === 'audio-snapshot' ? audioSnapshotValue() : applyRpc(context, op, rpcArgs));
          void Promise.resolve(context.rpcExtensions ? context.rpcExtensions(op, rpcArgs) : null)
            .then((ext) => (ext?.handled ? ext.value : builtin()))
            .then((value) => reply(true, { value }))
            .catch((error: unknown) => reply(false, {
              error: error instanceof Error ? (error.stack ?? error.message) : String(error),
            }));
        } else if (message.type === 'audio') {
          handleAudioRpc(String(message.op ?? ''), message.args ?? []);
        } else if (message.type === 'batch') {
          // 每批必须有 RPC id,沙箱收到 ack 后才允许报告完成。
          if (!Number.isFinite(message.id)) throw new Error('角色卡脚本 batch 缺少 id');
          const ops = Array.isArray(message.ops) ? message.ops : [];
          for (const operation of ops) {
            try {
              applyJq(
                context.container,
                operation.ref,
                String(operation.method ?? ''),
                Array.isArray(operation.args) ? operation.args : [],
                iframe.contentWindow,
                nonce,
                targets,
                () => ++targetId,
                (element: EventTarget, eventName, listener, capture?: boolean) =>
                  eventListeners.push({ element, eventName, listener, capture }),
                jqBindings,
              );
            } catch (error) {
              iframe.contentWindow?.postMessage({
                channel: CHANNEL, nonce, type: 'rpc-result', id: message.id, ok: false, error: error instanceof Error ? error.message : String(error),
              }, '*');
              return;
            }
          }
          iframe.contentWindow?.postMessage({
            channel: CHANNEL, nonce, type: 'rpc-result', id: message.id, ok: true, value: true,
            // 状态镜像(getter 真实化):批内选择器状态(操作后最新值)+ 全量表单控件
            mirror: buildStateMirror(context.container, ops),
          }, '*');
        } else if (message.type === 'warn') {
          console.warn('[kedai-mvu]', message.message ?? '');
        } else if (message.type === 'done') {
          // 脚本运行中注入的节点(append/setHtml)可能带 data-kd-on*,收尾补绑一轮
          bindInlineHandlers();
          finish();
        } else if (message.type === 'error') {
          finish(new Error(`角色卡脚本不兼容或执行失败: ${message.message ?? '未知错误'}`));
        }
      } catch (error) {
        const text = error instanceof Error ? (error.stack ?? error.message) : String(error);
        if (message.type === 'rpc') {
          iframe.contentWindow?.postMessage({
            channel: CHANNEL, nonce, type: 'rpc-result', id: message.id, ok: false, error: text,
          }, '*');
        } else {
          finish(new Error(text));
        }
      }
    };
    // 超时错误带上脚本指纹(长度+开头):多脚本同消息时能定位是哪一段没跑完;
    // 多段(卡级合并单 realm)附带段数与各段名,便于定位卡在哪一段
    const codeText = typeof code === 'string' ? code : code.map((s) => s.code).join('\n');
    const segmentInfo = typeof code === 'string' ? '' : ` segs=${code.length} names=${JSON.stringify(code.map((s) => s.name))}`;
    const codeFingerprint = `len=${codeText.length}${segmentInfo} head=${JSON.stringify(codeText.slice(0, 60))}`;
    const timer = setTimeout(() => finish(new Error(`角色卡脚本执行超时 (${codeFingerprint})`)), environment.timeoutMs ?? DEFAULT_TIMEOUT_MS);
    environment.window.addEventListener('message', onMessage as EventListener);
    environment.document.body.appendChild(iframe);
  });
}
