// protocol.ts — 沙箱通信协议:频道名/消息字节上限/消息类型定义与宿主-沙箱公共契约类型
import type { MvuVariables } from '../mvu/variables';

const CHANNEL = 'kedai-character-script-v1';

/**
 * 沙箱 boot 消息上限。角色卡开场/状态栏脚本动辄几十 KB(如 WuWa 状态栏 107KB,
 * 开场 40KB),64KB 会把它们整段拒掉 → 协议弹窗/悬浮球/状态栏永不显示。
 * postMessage 跨窗口传输无浏览器级硬上限(与内存一致),这里放宽到 1MB 防内存压力。
 */
const MAX_MESSAGE_BYTES = 1024 * 1024;

interface JqOperation {
  ref:
    | { kind: 'selector'; value: string }
    | { kind: 'target'; id: number }
    | { kind: 'selector-index'; value: string; index: number }
    // $(document)/$(window):作者脚本以窗口为事件源(如 $(window).on('unload', …)),
    // 宿主侧仅用于事件绑定/解绑,查询类操作一律空结果(避免误改全局 DOM)
    | { kind: 'document' }
    | { kind: 'window' };
  method: string;
  args: unknown[];
}

/** draggable 可序列化选项:回调函数已换成 jqId(0 表示未提供),宿主按 id 回发 jq-event */
interface DraggableWire {
  start?: number;
  drag?: number;
  stop?: number;
  handle?: string;
  cancel?: string;
  containment?: string;
  distance?: number;
  cursor?: string;
}

/** $('<div>') 游离元素规格:沙箱侧记录 HTML/事件/子元素,append 时宿主统一落 DOM */
interface CreatedElementSpec {
  kind: 'created';
  html: string;
  handlers: Array<{ evt: string; jqId: number }>;
  children: CreatedElementSpec[];
  addClass?: string;
  text?: string;
  innerHtml?: string;
  /** .attr(k,v)/.attr({…}) 记录的属性(宿主落 DOM 时 setAttribute) */
  attrs?: Record<string, string>;
  /** .css(k,v)/.css({…}) 记录的内联样式(宿主落 DOM 时写 style) */
  css?: Record<string, string>;
  /** .draggable(opts) 记录(宿主落 DOM 时绑 pointer 拖拽);'destroy' 表示解绑 */
  draggable?: DraggableWire | string;
}

interface SandboxRequest {
  channel: typeof CHANNEL;
  nonce: string;
  // shared-publish:沙箱 diff 上报 window 新增/变更的纯数据全局(跨 realm 共享桥,见 shared-globals.ts);
  // 反向「宿主→沙箱」对应 type:'shared-update'(globals 全量快照,字面量 postMessage,与 mvu-event 同风格)
  type: 'ready' | 'rpc' | 'batch' | 'done' | 'error' | 'warn' | 'jq-event' | 'audio' | 'local-storage' | 'shared-publish';
  id?: number;
  op?: string;
  args?: unknown[];
  ops?: JqOperation[];
  value?: unknown;
  message?: string;
  jqId?: number;
  key?: string;
  globals?: unknown;
}

/** inline 事件降级桥支持的事件名(data-kd-on<name> 属性 → 真实监听) */
const INLINE_EVENT_NAMES = [
  'click', 'dblclick', 'input', 'change', 'submit',
  'mousedown', 'mouseup', 'mouseover', 'mouseout',
  'keydown', 'keyup', 'focus', 'blur',
] as const;

/** 宿主容器几何镜像(沙箱内 window/document.body/frameElement 的映射源) */
interface ContainerGeo {
  top: number;
  left: number;
  width: number;
  height: number;
  scrollHeight: number;
  scrollWidth: number;
  hostH: number;
  hostW: number;
}

/**
 * 卡级多脚本合并执行的段(单 realm 逐 <script> 注入):
 * name 供错误上报前缀([卡脚本 段名])与日志定位;code 为脚本正文
 */
export interface ScriptSegment {
  name: string;
  code: string;
}

export interface SandboxExecutionContext {
  container: HTMLElement;
  variables: MvuVariables;
  /** 角色 id:沙箱全局变量(getVariables({type:'global'}))按角色持久化到宿主 localStorage */
  characterId?: string;
  /** 启动时下发的全局变量快照(脚本 loadSettings 同步读;见 readCardGlobals) */
  globals?: Record<string, unknown>;
  /** 主世界书名(角色卡内嵌 character_book.name):随 boot 注入,TavernHelper 世界书 API 的同步数据源 */
  lorebookName?: string | null;
  /**
   * 启动时注入 window 的跨 realm 共享全局快照(见 shared-globals.ts)。
   * 未显式传入且 characterId 存在时,由执行器自动填 getCardSharedGlobals(characterId)。
   */
  sharedGlobals?: Record<string, unknown>;
  /**
   * 异步 RPC 扩展点(可选):处理内置 audio-snapshot/applyRpc 之外的纯数据 op
   * (卡级脚本的世界书读写、聊天消息读取——需 fetch,走不了同步分发)。
   * 返回 handled=true 时 value 直接回包;handled=false 回退内置同步分发。
   */
  rpcExtensions?: (op: string, args: unknown[]) => Promise<{ handled: boolean; value?: unknown }>;
}

export interface SandboxEnvironment {
  document: Pick<Document, 'createElement' | 'body'>;
  window: Pick<Window, 'addEventListener' | 'removeEventListener'>;
  timeoutMs?: number;
}

export type SandboxCleanup = () => void;

export const MAX_BOOT_MESSAGE_BYTES = MAX_MESSAGE_BYTES;

function cloneData<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function utf8Bytes(value: unknown): number {
  return new TextEncoder().encode(JSON.stringify(value)).byteLength;
}

export {
  CHANNEL,
  MAX_MESSAGE_BYTES,
  INLINE_EVENT_NAMES,
  cloneData,
  utf8Bytes,
  type JqOperation,
  type CreatedElementSpec,
  type DraggableWire,
  type SandboxRequest,
  type ContainerGeo,
};
