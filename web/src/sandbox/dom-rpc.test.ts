// dom-rpc.test.ts — 宿主端选择器映射与注入标记(node 环境无 DOM,用最小元素桩驱动 applyJq)。
// 覆盖:覆层根优先映射($('head')/$('body') → 覆层根)、注入节点 data-kd-injected、
// 覆层根直系子节点 pointer-events:auto。
import { describe, expect, it } from 'vitest';
import { applyJq, applyRpc, overlayRootOf } from './dom-rpc';
import type { JqOperation } from './protocol';

class StubEl {
  tagName = 'DIV';
  nodeType = 1;
  attrs: Record<string, string> = {};
  style = {
    cssText: '',
    pointerEvents: '',
    setProperty: (_k: string, _v: string): void => {},
    getPropertyValue: (): string => '',
    item: (): string => '',
    length: 0,
  };
  dataset: Record<string, string> = {};
  classList = { add: (): void => {}, remove: (): void => {}, toggle: (): void => {}, contains: (): boolean => false };
  children: StubEl[] = [];
  childNodes: StubEl[] = [];
  parentElement: StubEl | null = null;
  id = '';
  textContent = '';
  innerHTML = '';
  value = '';
  checked = false;
  disabled = false;
  scrollTop = 0;
  scrollLeft = 0;
  scrollHeight = 0;
  scrollWidth = 0;
  clientHeight = 0;
  clientWidth = 0;
  offsetHeight = 0;
  offsetWidth = 0;

  constructor(readonly ownerDocument: { createElement: (t: string) => StubEl }) {}

  setAttribute(k: string, v: string): void {
    this.attrs[k] = String(v);
  }

  getAttribute(k: string): string | null {
    return k in this.attrs ? this.attrs[k] : null;
  }

  removeAttribute(k: string): void {
    delete this.attrs[k];
  }

  appendChild(child: StubEl): StubEl {
    child.parentElement = this;
    this.children.push(child);
    this.childNodes.push(child);
    return child;
  }

  // 模拟插入:每次追加一个游离节点(用于 append 路径的新增节点差集打标)
  insertAdjacentHTML(_pos: string, _html: string): void {
    const doc = this.ownerDocument;
    this.appendChild(doc.createElement('div'));
  }

  querySelectorAll(_sel?: string): StubEl[] {
    return [];
  }

  querySelector(_sel?: string): StubEl | null {
    return null;
  }

  addEventListener(): void {}
  removeEventListener(): void {}
  contains(): boolean {
    return true;
  }
  closest(): null {
    return null;
  }
  remove(): void {}
  focus(): void {}
  dispatchEvent(): void {}
  getBoundingClientRect(): { left: number; top: number; width: number; height: number } {
    return { left: 0, top: 0, width: 0, height: 0 };
  }
}

/** 容器桩:querySelector 只认覆层根,querySelectorAll 返回配置的匹配元素 */
function makeContainer(overlayRoot: StubEl | null, matches: StubEl[] = []): StubEl {
  const doc = { createElement: (_t: string): StubEl => new StubEl(doc) };
  const c = new StubEl(doc);
  c.attrs['data-kd-scope'] = 'card-c1';
  c.querySelector = (sel: string): StubEl | null => (sel === '[data-kd-overlay-root]' ? overlayRoot : null);
  c.querySelectorAll = (): StubEl[] => matches;
  return c;
}

function apply(container: StubEl, ref: JqOperation['ref'], method: string, args: unknown[] = []): unknown {
  return applyJq(
    container as unknown as HTMLElement,
    ref,
    method,
    args,
    null,
    'n1',
    new Map(),
    () => 1,
    () => {},
  );
}

const sel = (value: string): JqOperation['ref'] => ({ kind: 'selector', value });

/** 覆层根桩:带 data-kd-overlay-root 属性(pointer-events:auto 的判定依据) */
function makeOverlayRoot(): StubEl {
  const root = makeContainer(null).ownerDocument.createElement('div');
  root.attrs['data-kd-overlay-root'] = '';
  return root;
}

describe('overlayRootOf(覆层根优先映射)', () => {
  it('容器内有覆层根时返回覆层根;无则回退容器(消息级行为不变)', () => {
    const root = makeOverlayRoot();
    const withRoot = makeContainer(root);
    expect(overlayRootOf(withRoot as unknown as HTMLElement)).toBe(root);
    const without = makeContainer(null);
    expect(overlayRootOf(without as unknown as HTMLElement)).toBe(without);
  });

  it("$('head')/$('body')/$('html') 映射到覆层根(css 落点)", () => {
    const root = makeOverlayRoot();
    const container = makeContainer(root);
    for (const s of ['head', 'body', 'html', 'html body']) {
      apply(container, sel(s), 'css', ['display', 'none']);
    }
    // 三次都落在覆层根(container 自身未被改)
    expect(root.style.setProperty).toBeDefined();
    expect(container.style.cssText).toBe('');
  });
});

describe('注入节点标记(data-kd-injected)', () => {
  it('appendCreated 落 DOM 的顶层节点带 data-kd-injected,覆层根下补 pointer-events:auto', () => {
    const root = makeOverlayRoot();
    const container = makeContainer(root);
    apply(container, sel('body'), 'appendCreated', [
      { kind: 'created', html: '<div></div>', handlers: [], children: [], attrs: { id: 'fx' }, css: { position: 'fixed' } },
    ]);
    expect(root.children).toHaveLength(1);
    const created = root.children[0];
    expect(created.attrs['data-kd-injected']).toBe('1');
    expect(created.attrs.id).toBe('fx');
    expect(created.style.pointerEvents).toBe('auto');
  });

  it('append 的 insertAdjacentHTML 路径对新增子节点打标', () => {
    const root = makeOverlayRoot();
    const container = makeContainer(root);
    apply(container, sel('body'), 'append', ['<style>.a{color:red}</style>']);
    expect(root.children).toHaveLength(1);
    expect(root.children[0].attrs['data-kd-injected']).toBe('1');
  });

  it('html 路径对新增子节点打标', () => {
    const root = makeOverlayRoot();
    const container = makeContainer(root);
    // 模拟 innerHTML 赋值后产生一个子节点
    const doc = root.ownerDocument;
    Object.defineProperty(root, 'innerHTML', {
      configurable: true,
      get: () => '',
      set: () => {
        const child = doc.createElement('div');
        child.parentElement = root;
        root.childNodes.push(child);
        root.children.push(child);
      },
    });
    apply(container, sel('body'), 'html', ['<span>x</span>']);
    expect(root.children[0].attrs['data-kd-injected']).toBe('1');
  });

  it('appendCreated 的 spec.draggable 落 DOM 时绑定拖拽(dataset 标记)', () => {
    // th-5 链路:p$('<div>').attr('id',…).css({…}) 创建 → 落 DOM 后 .draggable 仍生效
    const root = makeOverlayRoot();
    const container = makeContainer(root);
    apply(container, sel('body'), 'appendCreated', [
      {
        kind: 'created',
        html: '<div></div>',
        handlers: [],
        children: [],
        attrs: { id: 'fx-floating-ball' },
        draggable: { start: 0, drag: 0, stop: 0, containment: 'window', distance: 3 },
      },
    ]);
    const created = root.children[0];
    expect(created.dataset.uiDraggable).toBe('1');
    expect(created.attrs['data-kd-injected']).toBe('1');
  });
});

// ===== 实跑问题 7 R3:事件委托 / 命名空间 / off 真解绑(宿主端) =====
describe('applyJq 事件绑定(on/off,实跑问题 7 R3)', () => {
  /** 带监听器捕获的元素桩(closest 命中委托目标) */
  class EvEl extends StubEl {
    listeners: Array<{ type: string; fn: (ev: unknown) => void }> = [];
    closestResult: EvEl | null = null;
    addEventListenerFn(type: string, fn: (ev: unknown) => void): void {
      this.listeners.push({ type, fn });
    }
    fire(type: string, target?: unknown): void {
      for (const l of this.listeners.filter((x) => x.type === type)) l.fn({ type, target: target ?? this });
    }
  }

  function setup(matches: EvEl[]) {
    const doc = { createElement: (_t: string): StubEl => new StubEl(doc) };
    const container = new StubEl(doc);
    container.attrs['data-kd-scope'] = 'card-c1';
    container.querySelector = () => null;
    container.querySelectorAll = () => matches;
    // EvEl 需要真实的 addEventListener 行为(getters 走镜像)
    for (const m of matches) {
      (m as unknown as { addEventListener: unknown }).addEventListener = m.addEventListenerFn.bind(m);
      (m as unknown as { removeEventListener: unknown }).removeEventListener = (): void => {};
      (m as unknown as { closest: unknown }).closest = (): EvEl | null => m.closestResult;
    }
    const posted: Array<Record<string, unknown>> = [];
    const targetWindow = { postMessage: (m: Record<string, unknown>) => void posted.push(m) } as unknown as Window;
    const bindings: Array<Record<string, unknown>> = [];
    const run = (method: string, args: unknown[]): unknown =>
      applyJq(
        container as unknown as HTMLElement,
        // 非 body 选择器:走 querySelectorAll(本桩返回 matches)
        sel('.item'),
        method,
        args,
        targetWindow,
        'n1',
        new Map(),
        () => 1,
        () => {},
        bindings as never,
      );
    return { run, posted, bindings, matches };
  }

  it('命名空间剥离:on("click.myNS") 用基础事件名绑定', () => {
    const el = new EvEl({} as never);
    const { run } = setup([el]);
    run('on', ['click.myNS', 7, null]);
    expect(el.listeners.map((l) => l.type)).toEqual(['click']);
  });

  it('委托绑定:事件命中 closest(selector) 时回发该目标状态;未命中不回发', () => {
    const el = new EvEl({} as never);
    const hit = new EvEl({} as never);
    hit.id = 'modal-btn';
    const { run, posted } = setup([el]);
    run('on', ['click', 9, '.modal-btn']);
    // 未命中:目标元素 closest 返回 null → 忽略
    el.closestResult = null;
    el.fire('click', { closest: () => null });
    expect(posted).toHaveLength(0);
    // 命中:回发 jq-event,携带命中目标 id
    el.closestResult = hit;
    el.fire('click', { closest: () => hit });
    const ev = posted.find((p) => p.type === 'jq-event');
    expect(ev).toBeTruthy();
    expect(ev?.jqId).toBe(9);
    expect((ev?.target as { id: number })?.id).toBeGreaterThan(0);
  });

  it('off(evt) 真解绑已登记的该事件监听(旧实现 no-op,重复绑定累积)', () => {
    const el = new EvEl({} as never);
    const { run, bindings } = setup([el]);
    run('on', ['click', 3, null]);
    expect(bindings).toHaveLength(1);
    run('off', ['click']);
    expect(bindings).toHaveLength(0);
  });

  it('clipboard-write 无剪贴板时抛错(让沙箱侧 reject,而非谎报成功)', () => {
    const doc = { createElement: (_t: string): StubEl => new StubEl(doc) };
    const container = new StubEl(doc);
    const context = {
      container: container as unknown as HTMLElement,
      characterId: 'c1',
    } as never;
    // node 环境无 navigator.clipboard。抛错是刻意契约:宿主 rpc-result(ok:false) 会让沙箱侧
    // writeText 的 Promise reject,作者脚本 .catch() 才能弹真实失败提示。若降级成 false,
    // 作者脚本 .then() 仍会弹「已复制」而剪贴板为空(实测缺口)。
    expect(() => applyRpc(context, 'clipboard-write', ['要复制的文本'])).toThrow('剪贴板');
  });

  it('clipboard-write 超长文本抛错(既有长度上限不变)', () => {
    const doc = { createElement: (_t: string): StubEl => new StubEl(doc) };
    const container = new StubEl(doc);
    const context = { container: container as unknown as HTMLElement, characterId: 'c1' } as never;
    expect(() => applyRpc(context, 'clipboard-write', ['x'.repeat(1_000_001)])).toThrow('过大');
  });

  it('纯数据 op 未注入扩展点时返回 null 而非抛选择器错误(实跑问题 7 主因兜底)', () => {
    const doc = { createElement: (_t: string): StubEl => new StubEl(doc) };
    const container = new StubEl(doc);
    const context = { container: container as unknown as HTMLElement, characterId: 'c1' } as never;
    // chat-messages 首参是楼层序号;旧实现把它当 CSS 选择器 → safeSelector 抛错 → 沙箱拆除
    expect(applyRpc(context, 'chat-messages', [0])).toBeNull();
    expect(applyRpc(context, 'lorebook-entries', ['主世界书'])).toBeNull();
  });
});
