// draggable.test.ts — 宿主侧 jQuery UI draggable 等价实现(沙箱 .draggable() 转发终点)。
// vitest environment='node' 无 DOM:用最小元素/文档桩驱动 pointer 事件,验证位移、钳制、
// handle/cancel 过滤、destroy 摘除、jq-event 回包与监听器登记。
import { describe, expect, it } from 'vitest';
import { applyDraggable, bindGlobalEvents, destroyDraggable, type DomEventContext } from './draggable';

/** 最小事件目标桩:记录监听器并可派发 */
class StubTarget {
  listeners = new Map<string, Set<EventListener>>();

  addEventListener(name: string, fn: EventListener): void {
    if (!this.listeners.has(name)) this.listeners.set(name, new Set());
    this.listeners.get(name)!.add(fn);
  }

  removeEventListener(name: string, fn: EventListener): void {
    this.listeners.get(name)?.delete(fn);
  }
  dispatch(name: string, event: Record<string, unknown>): void {
    for (const fn of Array.from(this.listeners.get(name) ?? [])) fn(event as unknown as Event);
  }

  count(name: string): number {
    return this.listeners.get(name)?.size ?? 0;
  }
}

interface StubEl extends StubTarget {
  style: Record<string, string> & { setProperty?: unknown };
  dataset: Record<string, string>;
  offsetWidth: number;
  offsetHeight: number;
  getBoundingClientRect: () => { left: number; top: number; width: number; height: number };
  contains: (node: unknown) => boolean;
  closest: (sel: string) => unknown;
}

function makeEl(over: Partial<{ left: number; top: number; width: number; height: number }> = {}): StubEl {
  const base = new StubTarget();
  const el = base as unknown as StubEl;
  const left = over.left ?? 100;
  const top = over.top ?? 100;
  const width = over.width ?? 50;
  const height = over.height ?? 50;
  el.style = { left: `${left}px`, top: `${top}px`, right: '20px' };
  el.dataset = {};
  el.offsetWidth = width;
  el.offsetHeight = height;
  el.getBoundingClientRect = () => ({ left, top, width, height });
  el.contains = () => true;
  el.closest = () => null;
  return el;
}

/** 宿主上下文桩:document/window 用 StubTarget,postMessage 收集回包 */
function makeCtx(over: Partial<{ innerWidth: number; innerHeight: number }> = {}) {
  const doc = new StubTarget();
  const win = new StubTarget() as StubTarget & { innerWidth: number; innerHeight: number };
  win.innerWidth = over.innerWidth ?? 1000;
  win.innerHeight = over.innerHeight ?? 800;
  const messages: Array<Record<string, unknown>> = [];
  const registered: Array<{ element: EventTarget; eventName: string; listener: EventListener; capture?: boolean }> = [];
  const ctx: DomEventContext = {
    hostDocument: doc as unknown as Document,
    hostWindow: win as unknown as Window,
    targetWindow: { postMessage: (m: Record<string, unknown>) => void messages.push(m) } as unknown as Window,
    nonce: 'n1',
    container: makeEl() as unknown as HTMLElement,
    controlsProvider: () => [],
    registerEventListener: (element, eventName, listener, capture) =>
      registered.push({ element, eventName, listener, capture }),
  };
  return { ctx, doc, win, messages, registered };
}

function pointerDown(el: StubEl, x: number, y: number, target?: unknown): void {
  el.dispatch('pointerdown', { button: 0, clientX: x, clientY: y, target: target ?? el });
}

describe('applyDraggable(宿主侧拖拽)', () => {
  it('pointerdown→move 改变 left/top;containment=window 时钳制不越界', () => {
    const el = makeEl({ left: 100, top: 100, width: 50, height: 50 });
    const { ctx, doc } = makeCtx({ innerWidth: 1000, innerHeight: 800 });
    applyDraggable(el as unknown as HTMLElement, { containment: 'window' }, ctx);
    pointerDown(el, 120, 120);
    doc.dispatch('pointermove', { clientX: 200, clientY: 180 });
    expect(el.style.left).toBe('180px');
    expect(el.style.top).toBe('160px');
    expect(el.style.right).toBe('auto');
    // 拖出视口右下 → 钳制到 (1000-50, 800-50)
    doc.dispatch('pointermove', { clientX: 2000, clientY: 2000 });
    expect(el.style.left).toBe('950px');
    expect(el.style.top).toBe('750px');
    // 拖出左上 → 钳制到 0
    doc.dispatch('pointermove', { clientX: -500, clientY: -500 });
    expect(el.style.left).toBe('0px');
    expect(el.style.top).toBe('0px');
  });

  it('handle 未命中不启动;cancel 命中忽略拖拽', () => {
    const el = makeEl();
    const { ctx, doc } = makeCtx();
    // handle 命中点须落在元素内:contains 只认同一元素,外部节点返回 false
    el.contains = (node: unknown) => node === el;
    applyDraggable(el as unknown as HTMLElement, { handle: '.head', cancel: '.btn' }, ctx);
    // handle 命中点在元素外 → 忽略
    const outside = { closest: (sel: string) => (sel === '.head' ? { unrelated: true } : null) };
    pointerDown(el, 120, 120, outside);
    doc.dispatch('pointermove', { clientX: 300, clientY: 300 });
    expect(el.style.left).toBe('100px');
    // handle 命中元素内但 cancel 也命中 → 忽略
    const inCancel = { closest: (sel: string) => (sel === '.head' || sel === '.btn' ? el : null) };
    pointerDown(el, 120, 120, inCancel);
    doc.dispatch('pointermove', { clientX: 300, clientY: 300 });
    expect(el.style.left).toBe('100px');
    // 仅 handle 命中且不在 cancel 内 → 正常拖动
    const inHandle = { closest: (sel: string) => (sel === '.head' ? el : null) };
    pointerDown(el, 120, 120, inHandle);
    doc.dispatch('pointermove', { clientX: 300, clientY: 300 });
    expect(el.style.left).toBe('280px');
  });

  it('distance 未达到不启动,达到后按当前位移定位', () => {
    const el = makeEl({ left: 10, top: 10 });
    const { ctx, doc } = makeCtx();
    applyDraggable(el as unknown as HTMLElement, { distance: 5 }, ctx);
    pointerDown(el, 100, 100);
    doc.dispatch('pointermove', { clientX: 102, clientY: 102 });
    expect(el.style.left).toBe('10px'); // 位移 2 < 5
    doc.dispatch('pointermove', { clientX: 120, clientY: 110 });
    expect(el.style.left).toBe('30px');
    expect(el.style.top).toBe('20px');
  });

  it('destroy 后 move 不再改位置;data("ui-draggable") 语义由 dataset 标记', () => {
    const el = makeEl();
    const { ctx, doc } = makeCtx();
    applyDraggable(el as unknown as HTMLElement, {}, ctx);
    expect(el.dataset.uiDraggable).toBe('1');
    destroyDraggable(el as unknown as HTMLElement, ctx);
    expect(el.dataset.uiDraggable).toBeUndefined();
    pointerDown(el, 120, 120);
    doc.dispatch('pointermove', { clientX: 400, clientY: 400 });
    expect(el.style.left).toBe('100px');
    expect(el.style.top).toBe('100px');
  });

  it('jq-event 回包带 event.ui.position(start/drag/stop 三个回调)', () => {
    const el = makeEl({ left: 100, top: 100 });
    const { ctx, doc, messages } = makeCtx();
    applyDraggable(el as unknown as HTMLElement, { start: 11, drag: 22, stop: 33, containment: 'window' }, ctx);
    pointerDown(el, 120, 120);
    doc.dispatch('pointermove', { clientX: 160, clientY: 140 });
    doc.dispatch('pointerup', {});
    const byId = (id: number) => messages.find((m) => m.jqId === id) as {
      type?: string;
      event?: { type?: string; ui?: { position?: { left: number; top: number } } };
    };
    expect(byId(11).event?.ui?.position).toEqual({ left: 100, top: 100 });
    expect(byId(22).event?.ui?.position).toEqual({ left: 140, top: 120 });
    expect(byId(33).event?.ui?.position).toEqual({ left: 140, top: 120 });
    expect(byId(22).type).toBe('jq-event');
  });

  it('监听器全部登记进 registerEventListener,cleanup 可摘除', () => {
    const el = makeEl();
    const { ctx, doc, registered } = makeCtx();
    applyDraggable(el as unknown as HTMLElement, {}, ctx);
    // pointerdown 一个
    expect(registered).toHaveLength(1);
    expect(registered[0].eventName).toBe('pointerdown');
    pointerDown(el, 120, 120);
    // 拖拽中追加 document pointermove/pointerup 两个
    expect(registered.map((r) => r.eventName)).toEqual(['pointerdown', 'pointermove', 'pointerup']);
    // document 级 move/up 以 capture=true 注册:登记必须带 capture,否则 cleanup
    // 用默认 capture=false 摘不掉,拖拽中切卡会残留宿主 document 监听
    expect(registered[1].capture).toBe(true);
    expect(registered[2].capture).toBe(true);
    // cleanup 语义:按登记摘除后 move 不再生效
    for (const r of registered) r.element.removeEventListener(r.eventName, r.listener);
    doc.dispatch('pointermove', { clientX: 400, clientY: 400 });
    expect(el.style.left).toBe('100px');
  });

  it('draggable("destroy") 字符串直接解绑', () => {
    const el = makeEl();
    const { ctx, doc } = makeCtx();
    applyDraggable(el as unknown as HTMLElement, {}, ctx);
    applyDraggable(el as unknown as HTMLElement, 'destroy', ctx);
    pointerDown(el, 120, 120);
    doc.dispatch('pointermove', { clientX: 400, clientY: 400 });
    expect(el.style.left).toBe('100px');
  });
});

describe('bindGlobalEvents($(window)/$(document) 事件绑定)', () => {
  it('window 引用绑到宿主 window,document 引用绑到宿主 document,回包结构含 jqId', () => {
    const { ctx, doc, win, messages, registered } = makeCtx();
    bindGlobalEvents({ kind: 'window' }, ['unload', 'pagehide'], 7, ctx);
    bindGlobalEvents({ kind: 'document' }, ['scroll'], 8, ctx);
    expect(win.count('unload')).toBe(1);
    expect(win.count('pagehide')).toBe(1);
    expect(doc.count('scroll')).toBe(1);
    expect(registered.map((r) => r.eventName)).toEqual(['unload', 'pagehide', 'scroll']);
    win.dispatch('unload', {});
    expect(messages.some((m) => m.jqId === 7 && m.type === 'jq-event')).toBe(true);
  });
});
