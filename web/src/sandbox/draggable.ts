// draggable.ts — 宿主侧 jQuery UI draggable 等价实现(沙箱 jq 子集 draggable 的转发终点)
//
// 沙箱内没有 jQuery UI,作者脚本(如 WuWa 三个悬浮窗)却用 .draggable({handle,cancel,
// containment,start,drag,stop}) 实现拖拽。此处用 pointer 事件在宿主侧复刻最小语义:
//   - 定位统一改为 left/top(首次拖拽时从 getBoundingClientRect 读初值,right 置 auto);
//   - containment:'window' 时按宿主视口钳制(等价脚本手写 clamp);
//   - start/drag/stop 回调经 jq-event 回发沙箱,payload 带 event.ui.position。
// 监听器全部经 registerEventListener 登记,随沙箱 cleanup 统一摘除。
import { CHANNEL, type DraggableWire } from './protocol';

/** 宿主侧事件上下文(dom-rpc 每次 applyJq 调用时组装) */
export interface DomEventContext {
  hostDocument: Document;
  hostWindow: Window;
  targetWindow: Window | null;
  nonce: string;
  container: HTMLElement;
  /** 事件回包附带的全量表单控件镜像(沙箱回调里读其它控件) */
  controlsProvider: () => Array<Record<string, unknown>>;
  /** 登记监听器供沙箱 cleanup 统一摘除;capture 需与注册时一致,否则摘不掉 */
  registerEventListener: (
    element: EventTarget,
    eventName: string,
    listener: EventListener,
    capture?: boolean,
  ) => void;
  /** 元素状态快照(委托事件回发目标状态用;由 dom-rpc 注入,避免模块循环依赖) */
  elementStateProvider?: (el: HTMLElement) => Record<string, unknown>;
}

interface DragState {
  pointerDown: EventListener;
  move?: EventListener;
  up?: EventListener;
  opts: DraggableWire;
  startX: number;
  startY: number;
  origLeft: number;
  origTop: number;
  started: boolean;
  lastLeft: number;
  lastTop: number;
}

// 元素 → 拖拽状态(destroy 与重复应用时摘除)
const dragStates = new WeakMap<HTMLElement, DragState>();

/** 位置钳制(containment:'window' 等价实现):视口内,不小于 0 */
function clampToWindow(ctx: DomEventContext, el: HTMLElement, left: number, top: number): { left: number; top: number } {
  const maxLeft = Math.max(0, (ctx.hostWindow.innerWidth || 0) - (el.offsetWidth || 0));
  const maxTop = Math.max(0, (ctx.hostWindow.innerHeight || 0) - (el.offsetHeight || 0));
  return {
    left: Math.min(Math.max(0, left), maxLeft),
    top: Math.min(Math.max(0, top), maxTop),
  };
}

/** 回发 jq-event(拖拽回调与全局事件共用;jqId 为 0 表示作者未提供该回调) */
function postEvent(
  ctx: DomEventContext,
  jqId: number,
  type: string,
  ui?: { position: { left: number; top: number } },
  delegateTargetId = 0,
): void {
  if (!ctx.targetWindow || !Number.isFinite(jqId) || jqId <= 0) return;
  const delegate = delegateTargetId > 0 ? globalDelegateTargets.get(delegateTargetId) : undefined;
  const target = delegate
    ? {
        kind: 'target',
        id: delegateTargetId,
        data: Object.fromEntries(Object.entries(delegate.dataset)),
        state: elementStateFor(ctx, delegate),
      }
    : { kind: 'target', id: 0, data: {}, state: {} };
  ctx.targetWindow.postMessage(
    {
      channel: CHANNEL,
      nonce: ctx.nonce,
      type: 'jq-event',
      jqId,
      event: ui ? { type, ui } : { type },
      target,
      mirror: { controls: ctx.controlsProvider() },
    },
    '*',
  );
}

/** 委托目标的元素状态快照(elementStateProvider 由 dom-rpc 注入,缺失时退回最小字段) */
function elementStateFor(ctx: DomEventContext, el: HTMLElement): Record<string, unknown> {
  if (ctx.elementStateProvider) return ctx.elementStateProvider(el);
  return {
    n: 1,
    id: el.id || undefined,
    text: (el.textContent ?? '').slice(0, 100_000),
    html: el.innerHTML.slice(0, 100_000),
    val: typeof (el as HTMLInputElement).value === 'string' ? (el as HTMLInputElement).value : '',
    checked: !!(el as HTMLInputElement).checked,
    disabled: !!(el as HTMLInputElement).disabled,
    classes: Array.from(el.classList).slice(0, 64),
  };
}

function onPointerMove(ev: Event, el: HTMLElement, state: DragState, ctx: DomEventContext): void {
  const e = ev as PointerEvent;
  const dx = e.clientX - state.startX;
  const dy = e.clientY - state.startY;
  if (!state.started) {
    const distance = typeof state.opts.distance === 'number' ? state.opts.distance : 0;
    if (Math.abs(dx) < distance && Math.abs(dy) < distance) return;
    state.started = true;
    // 首次拖拽:从视口矩形换算 left/top 定位(fixed/absolute 均以视口原点为基准)
    el.style.left = `${state.origLeft}px`;
    el.style.top = `${state.origTop}px`;
    el.style.right = 'auto';
    postEvent(ctx, Number(state.opts.start ?? 0), 'dragstart', {
      position: { left: state.origLeft, top: state.origTop },
    });
  }
  let left = state.origLeft + dx;
  let top = state.origTop + dy;
  if (state.opts.containment === 'window') ({ left, top } = clampToWindow(ctx, el, left, top));
  state.lastLeft = left;
  state.lastTop = top;
  el.style.left = `${left}px`;
  el.style.top = `${top}px`;
  postEvent(ctx, Number(state.opts.drag ?? 0), 'drag', { position: { left, top } });
}

function onPointerUp(_ev: Event, el: HTMLElement, state: DragState, ctx: DomEventContext): void {
  if (state.move) ctx.hostDocument.removeEventListener('pointermove', state.move, true);
  if (state.up) ctx.hostDocument.removeEventListener('pointerup', state.up, true);
  state.move = undefined;
  state.up = undefined;
  if (state.started) {
    postEvent(ctx, Number(state.opts.stop ?? 0), 'dragstop', {
      position: { left: state.lastLeft, top: state.lastTop },
    });
  }
  state.started = false;
}

/** 摘除元素上的拖拽:pointerdown 监听与进行中的 move/up 状态(draggable('destroy')) */
export function destroyDraggable(el: HTMLElement, ctx: DomEventContext): void {
  const state = dragStates.get(el);
  if (!state) return;
  if (typeof el.removeEventListener === 'function') el.removeEventListener('pointerdown', state.pointerDown);
  if (state.move) ctx.hostDocument.removeEventListener('pointermove', state.move, true);
  if (state.up) ctx.hostDocument.removeEventListener('pointerup', state.up, true);
  dragStates.delete(el);
  if (el.dataset) delete el.dataset.uiDraggable;
}

/**
 * 应用 draggable:字符串 'destroy' 解绑;对象绑定 pointerdown 并记录起始位置。
 * 命中判定:handle 命中元素须在元素内,cancel 命中则忽略。
 */
export function applyDraggable(el: HTMLElement, opts: DraggableWire | string, ctx: DomEventContext): void {
  if (typeof opts === 'string') {
    if (opts === 'destroy') destroyDraggable(el, ctx);
    return;
  }
  destroyDraggable(el, ctx);
  const o: DraggableWire = opts && typeof opts === 'object' ? opts : {};
  const state: DragState = {
    pointerDown: () => {},
    opts: o,
    startX: 0,
    startY: 0,
    origLeft: 0,
    origTop: 0,
    started: false,
    lastLeft: 0,
    lastTop: 0,
  };
  const pointerDown = (ev: Event): void => {
    const e = ev as PointerEvent;
    if (typeof e.button === 'number' && e.button !== 0) return;
    const target = e.target as Element | null;
    if (o.handle) {
      const hit = typeof target?.closest === 'function' ? target.closest(o.handle) : null;
      // handle 命中点必须落在元素自身或其后代内(防冒泡自其它元素)
      if (!hit || !(hit === el || (typeof el.contains === 'function' && el.contains(hit)))) return;
    }
    if (o.cancel && typeof target?.closest === 'function' && target.closest(o.cancel)) return;
    state.startX = e.clientX;
    state.startY = e.clientY;
    const rect = typeof el.getBoundingClientRect === 'function' ? el.getBoundingClientRect() : null;
    state.origLeft = rect ? rect.left : 0;
    state.origTop = rect ? rect.top : 0;
    state.lastLeft = state.origLeft;
    state.lastTop = state.origTop;
    state.started = false;
    const move = (mev: Event): void => onPointerMove(mev, el, state, ctx);
    const up = (uev: Event): void => onPointerUp(uev, el, state, ctx);
    state.move = move;
    state.up = up;
    ctx.hostDocument.addEventListener('pointermove', move, true);
    ctx.hostDocument.addEventListener('pointerup', up, true);
    ctx.registerEventListener(ctx.hostDocument, 'pointermove', move, true);
    ctx.registerEventListener(ctx.hostDocument, 'pointerup', up, true);
  };
  state.pointerDown = pointerDown;
  el.addEventListener('pointerdown', pointerDown);
  ctx.registerEventListener(el, 'pointerdown', pointerDown);
  dragStates.set(el, state);
  if (el.dataset) el.dataset.uiDraggable = '1';
}

/**
 * $(document)/$(window) 的事件绑定:目标换成宿主 document/window,回包与元素 on() 同结构
 * (作者脚本 $(window).on('unload', destroy) 的 destroy 钩子在真实卸载时才有语义,
 * 此处绑定本身不报错,cleanup 时统一摘除)。
 */
export function bindGlobalEvents(
  ref: { kind: 'document' | 'window' },
  eventNames: string[],
  jqId: number,
  ctx: DomEventContext,
  selector: string | null = null,
  bindings?: Array<{
    jqId: number;
    element: EventTarget;
    eventName: string;
    listener: EventListener;
    selector: string | null;
    capture?: boolean;
  }>,
): void {
  if (!ctx.targetWindow || eventNames.length === 0 || !Number.isFinite(jqId)) return;
  const target: EventTarget = ref.kind === 'window' ? ctx.hostWindow : ctx.hostDocument;
  for (const eventName of eventNames) {
    const listener = (event: Event): void => {
      // 委托写法 $(document).on('click', '.btn', fn):以 closest 命中的目标回发,
      // 未被委托选择器命中则忽略(实跑问题 7 R3)
      let hitId = 0;
      if (selector) {
        const raw = event.target as Element | null;
        const found = raw && typeof raw.closest === 'function' ? raw.closest(selector) : null;
        if (!found || !(found instanceof HTMLElement)) return;
        hitId = bindDelegateTarget(found);
        postEvent(ctx, jqId, eventName, undefined, hitId);
        return;
      }
      postEvent(ctx, jqId, eventName);
    };
    target.addEventListener(eventName, listener);
    ctx.registerEventListener(target, eventName, listener);
    bindings?.push({ jqId, element: target, eventName, listener, selector, capture: undefined });
  }
}

/** 全局委托目标的临时句柄表(与元素级 targets 分开;仅按 id 回发状态) */
const globalDelegateTargets = new Map<number, HTMLElement>();
let globalDelegateSeq = 0;
function bindDelegateTarget(el: HTMLElement): number {
  const id = ++globalDelegateSeq;
  globalDelegateTargets.set(id, el);
  return id;
}
