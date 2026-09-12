// dom-rpc.ts — DOM RPC 宿主端:选择器解析/jQuery 操作应用/状态镜像采集/游离元素落 DOM,及数据 op 分发(全局变量与 localStorage 持久化桥)
import sanitizeHtml from 'sanitize-html';
import { SCRIPT_HTML_WHITELIST, sanitizeScriptHtmlWithStyles } from './sanitize';
import { applyDraggable, bindGlobalEvents, type DomEventContext } from './draggable';
import {
  CHANNEL,
  type CreatedElementSpec,
  type JqOperation,
  type SandboxExecutionContext,
} from './protocol';

/** 卡级覆层根选择器:cardScriptHost 在容器内创建,承载脚本注入的悬浮层 DOM */
const OVERLAY_ROOT_SELECTOR = '[data-kd-overlay-root]';

/** 覆层根查找(消息级沙箱无覆层根时回退容器,行为不变) */
export function overlayRootOf(container: HTMLElement): HTMLElement {
  if (typeof container.querySelector !== 'function') return container;
  const root = container.querySelector<HTMLElement>(OVERLAY_ROOT_SELECTOR);
  return root ?? container;
}

/** 注入节点标记:cleanup 按此摘除本次沙箱注入的 DOM。
 *  父为覆层根时补 pointer-events:auto(根自身 none,不拦截聊天交互,子节点仍需可点)。 */
function markInjected(el: HTMLElement): void {
  if (typeof el.setAttribute === 'function') el.setAttribute('data-kd-injected', '1');
  const parent = el.parentElement;
  const underOverlay =
    !!parent && typeof parent.getAttribute === 'function' && parent.getAttribute('data-kd-overlay-root') !== null;
  if (underOverlay && el.style) el.style.pointerEvents = 'auto';
}

function safeSelector(value: unknown): string {
  if (typeof value !== 'string' || value.length > 1024 || /[\0\r\n]/.test(value)) {
    throw new Error('选择器无效');
  }
  return value;
}

function safeText(value: unknown): string {
  // 状态栏脚本单次 html() 可达百余 KB(wuwa 状态栏 161KB),20KB 上限会整段拒掉 → 界面空白
  if (typeof value !== 'string' || value.length > 512_000) throw new Error('文本无效');
  return value;
}

/**
 * 作者 HTML 进宿主 DOM 的清洗入口(html()/append()/appendCreated 共用):
 * 容器带 data-kd-scope(渲染块自带,卡级宿主由 cardScriptHost 设置)时 <style> 段
 * 经声明级清洗 + 容器作用域化后保留;无 scope 时退回纯白名单(不作用域化)。
 */
function sanitizeForContainer(container: HTMLElement, html: string): string {
  const scopeId =
    typeof container.getAttribute === 'function' ? container.getAttribute('data-kd-scope') : null;
  if (scopeId) return sanitizeScriptHtmlWithStyles(html, scopeId);
  return sanitizeHtml(html, SCRIPT_HTML_WHITELIST);
}

/** 按角色持久化的卡内全局变量(wuwa 状态栏 loadSettings/saveSettings 读写酒馆全局变量的落点) */
const cardGlobalsKey = (characterId: string): string => `kedai.card-globals.${characterId}`;

export function readCardGlobals(characterId: string | undefined): Record<string, unknown> {
  if (!characterId) return {};
  try {
    const raw = localStorage.getItem(cardGlobalsKey(characterId));
    const v: unknown = raw ? JSON.parse(raw) : null;
    return v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

function writeCardGlobals(characterId: string | undefined, globals: unknown): void {
  if (!characterId || !globals || typeof globals !== 'object' || Array.isArray(globals)) return;
  try {
    localStorage.setItem(cardGlobalsKey(characterId), JSON.stringify(globals));
  } catch {
    /* 配额异常忽略:设置持久化失败不阻断脚本 */
  }
}

/**
 * 沙箱 localStorage 的宿主持久化(对齐酒馆:卡脚本 localStorage 落在源级存储,重载后仍在,
 * 如 wuwa 协议 ww_agreement_accepted_v1 只需同意一次)。全角色共用一份,语义与 ST 同源一致。
 */
const SCRIPT_LOCAL_STORAGE_KEY = 'kedai.sandbox-localstorage.v1';

export function readScriptLocalStorage(): Record<string, string> {
  try {
    const raw = localStorage.getItem(SCRIPT_LOCAL_STORAGE_KEY);
    const v: unknown = raw ? JSON.parse(raw) : null;
    if (!v || typeof v !== 'object' || Array.isArray(v)) return {};
    const out: Record<string, string> = {};
    for (const [k, val] of Object.entries(v as Record<string, unknown>)) out[k] = String(val ?? '');
    return out;
  } catch {
    return {};
  }
}

export function applyScriptLocalStorageOp(op: string, key: unknown, value: unknown): void {
  const store = readScriptLocalStorage();
  if (op === 'set' && typeof key === 'string') store[key] = String(value ?? '');
  else if (op === 'remove' && typeof key === 'string') delete store[key];
  else if (op === 'clear') for (const k of Object.keys(store)) delete store[k];
  else return;
  try {
    localStorage.setItem(SCRIPT_LOCAL_STORAGE_KEY, JSON.stringify(store));
  } catch {
    /* 配额异常忽略 */
  }
}

function queryScoped(container: HTMLElement, selector: string): HTMLElement[] {
  const safe = safeSelector(selector);
  // 作者脚本把容器当文档用:$('body')/html/head 指代整个状态栏容器(沙箱无真实 body/head 可管)。
  // 卡级覆层根存在时映射到覆层根(脱离消息流,不随自动吸底滚动);消息级回退容器。
  if (safe === 'body' || safe === 'html' || safe === 'html body' || safe === 'head') {
    return [overlayRootOf(container)];
  }
  const positional = /:(first|last)\s*$/.exec(safe);
  const base = positional ? safe.slice(0, positional.index).trim() : safe;
  if (!base) throw new Error('选择器无效');
  const matches = Array.from(container.querySelectorAll<HTMLElement>(base));
  if (positional?.[1] === 'first') return matches.slice(0, 1);
  if (positional?.[1] === 'last') return matches.slice(-1);
  return matches;
}

/** selector-index 引用(first/last/eq/数字索引产生):按序号取匹配元素 */
function resolveRefElements(container: HTMLElement, ref: JqOperation['ref'], targets: Map<number, HTMLElement>): HTMLElement[] {
  if (ref.kind === 'selector') return queryScoped(container, ref.value);
  if (ref.kind === 'target') return targets.get(ref.id) ? [targets.get(ref.id)!] : [];
  if (ref.kind === 'selector-index') {
    const all = queryScoped(container, ref.value);
    const idx = ref.index < 0 ? all.length + ref.index : ref.index;
    const el = idx >= 0 && idx < all.length ? all[idx] : undefined;
    return el ? [el] : [];
  }
  // document/window 仅用于事件绑定路径;查询类方法一律空结果(避免误改全局 DOM)
  return [];
}

/** 单元素状态(getter 镜像条目;text/html 截断防巨型回包) */
export function elementState(el: HTMLElement): Record<string, unknown> {
  const inp = el as HTMLInputElement;
  return {
    n: 1,
    // id/name 供沙箱侧事件 this 门面的 getAttribute 与选择器回查
    id: el.id || undefined,
    name: inp.name || undefined,
    // tag:测试 mock 元素可能无 tagName,容错为空串
    tag: typeof el.tagName === 'string' ? el.tagName.toLowerCase() : '',
    text: (el.textContent ?? '').slice(0, 100_000),
    html: el.innerHTML.slice(0, 100_000),
    val: typeof inp.value === 'string' ? inp.value.slice(0, 20_000) : '',
    checked: !!inp.checked,
    disabled: !!inp.disabled,
    classes: Array.from(el.classList).slice(0, 64),
    // select 状态:作者脚本(getSelectedOrCustomText 型)读 selectedIndex/value/options[i].text
    ...readSelectState(el),
    // 内联样式快照:沙箱 css getter 与 is(':visible'|':hidden') 的数据源
    css: readInlineCss(el),
    // 滚动/几何度量:协议卡 checkBottom($card[0].scrollTop+clientHeight>=scrollHeight-8)
    // 与定位脚本 outerHeight 等读这些真值,缺了则 NaN 比较恒 false(勾选框永远 locked)
    scrollTop: el.scrollTop,
    scrollLeft: el.scrollLeft,
    scrollHeight: el.scrollHeight,
    scrollWidth: el.scrollWidth,
    clientHeight: el.clientHeight,
    clientWidth: el.clientWidth,
    offsetHeight: el.offsetHeight,
    offsetWidth: el.offsetWidth,
  };
}

/** 单次快照内所有 select 选项的全局预算:选项明细随每次事件回包下发,卡内 select 多、
 *  选项多时可能撑爆消息上限;超预算的 select 只留 selectedIndex(门面回退空 options)。 */
const SELECT_OPTIONS_BUDGET = 400;

/** <select> 的选项快照:selectedIndex + options[{value,text}](单项上限 100,另有全局预算)。
 *  非 select 返回空对象(镜像字段不出现,沙箱门面回退 -1/[])。
 *  `budget` 为调用方持有的剩余选项额度对象(跨元素累计递减)。 */
function readSelectState(
  el: HTMLElement,
  budget?: { left: number },
): Record<string, unknown> {
  if (el.tagName !== 'SELECT') return {};
  const sel = el as HTMLSelectElement;
  const options: Array<{ value: string; text: string }> = [];
  try {
    const list = sel.options;
    const cap = budget ? Math.min(100, budget.left) : 100;
    for (let i = 0; i < list.length && i < cap; i++) {
      const o = list[i];
      options.push({ value: String(o.value ?? ''), text: String(o.text ?? '') });
    }
    if (budget) budget.left -= options.length;
  } catch {
    /* 非标准 select 实现:忽略选项明细 */
  }
  return { selectedIndex: sel.selectedIndex, options };
}

/** 内联样式(仅显式声明项;camelCase → kebab 两种键都填,沙箱 getter 按传入键名读) */
function readInlineCss(el: HTMLElement): Record<string, string> {
  const out: Record<string, string> = {};
  const style = el.style;
  if (!style || typeof style.length !== 'number' || typeof style.item !== 'function') return out;
  for (let i = 0; i < style.length && i < 64; i++) {
    const prop = style.item(i);
    if (!prop) continue;
    const value = style.getPropertyValue(prop);
    out[prop] = value;
    out[prop.replace(/-([a-z])/g, (_m, c: string) => c.toUpperCase())] = value;
  }
  return out;
}

/** 集合元素快照(jQuery .each/数字索引 getter 真实化;上限 50 条、字段截断防巨型回包) */
function gatherItems(els: HTMLElement[]): Array<Record<string, unknown>> {
  return els.slice(0, 50).map((el) => {
    const inp = el as HTMLInputElement;
    return {
      text: (el.textContent ?? '').slice(0, 500),
      html: el.innerHTML.slice(0, 2000),
      val: typeof inp.value === 'string' ? inp.value.slice(0, 500) : '',
      checked: !!inp.checked,
      disabled: !!inp.disabled,
      classes: Array.from(el.classList).slice(0, 32),
      id: el.id || undefined,
      name: inp.name || undefined,
      tag: el.tagName.toLowerCase(),
    };
  });
}

/** 选择器状态(jQuery getter 语义:集合大小 + 首元素状态 + items;无效选择器不抛出) */
function gatherSelectorState(container: HTMLElement, selector: string): Record<string, unknown> {
  try {
    const els = queryScoped(container, selector);
    const first = els[0];
    if (!first) return { n: 0 };
    return { ...elementState(first), n: els.length, items: gatherItems(els) };
  } catch {
    return { n: 0 };
  }
}

/** 容器内全部表单控件状态(协议勾选/输入读取的目标;上限 200 防巨型容器)。
 *  容器缺 querySelectorAll(测试 mock/异常容器)时返回空表,与 gatherIdMap 同款防御。 */
export function gatherControls(container: HTMLElement): Array<Record<string, unknown>> {
  const out: Array<Record<string, unknown>> = [];
  if (typeof container?.querySelectorAll !== 'function') return out;
  const els = Array.from(container.querySelectorAll<HTMLElement>('input, select, textarea, button')).slice(0, 200);
  const budget = { left: SELECT_OPTIONS_BUDGET };
  for (const el of els) {
    const inp = el as HTMLInputElement;
    out.push({
      id: el.id || undefined,
      name: inp.name || undefined,
      tag: typeof el.tagName === 'string' ? el.tagName.toLowerCase() : '',
      val: typeof inp.value === 'string' ? inp.value.slice(0, 2000) : '',
      checked: !!inp.checked,
      disabled: !!inp.disabled,
      // select 状态(见 readSelectState):沙箱 getElementById 门面读 value/selectedIndex/options
      ...readSelectState(el, budget),
    });
  }
  return out;
}

/** 容器内带 id 元素的轻量类状态表(沙箱 attr('id') 复合类选择器回查的索引底;
 *  boot 注入 + 每批 ack 顺带,键为 '#id',值仅 id/classes 两字段,上限 300) */
export function gatherIdMap(container: HTMLElement, into: Record<string, unknown> = {}): Record<string, unknown> {
  try {
    if (typeof container.querySelectorAll !== 'function') return into;
    const els = container.querySelectorAll<HTMLElement>('[id]');
    const cap = Math.min(els.length, 300);
    for (let i = 0; i < cap; i++) {
      const el = els[i];
      if (!el.id) continue;
      const key = `#${el.id}`;
      if (!(key in into)) into[key] = { id: el.id, classes: Array.from(el.classList).slice(0, 32), n: 1 };
    }
  } catch {
    /* 容器异常:返回已收集部分 */
  }
  return into;
}

/** 批处理回包镜像:批内涉及的选择器状态(操作已应用后的最新值)+ 全量表单控件 */
export function buildStateMirror(
  container: HTMLElement,
  ops: JqOperation[],
): { selectors: Record<string, unknown>; controls: Array<Record<string, unknown>> } {
  const selectors: Record<string, unknown> = {};
  for (const op of ops) {
    if (op?.ref?.kind === 'selector') {
      const key = String(op.ref.value ?? '');
      if (key && !(key in selectors)) selectors[key] = gatherSelectorState(container, key);
    } else if (op?.ref?.kind === 'selector-index') {
      const ref = op.ref;
      const key = `${String(ref.value ?? '')}@@${ref.index}`;
      if (!(key in selectors)) {
        try {
          const all = queryScoped(container, ref.value);
          const idx = ref.index < 0 ? all.length + ref.index : ref.index;
          const el = idx >= 0 && idx < all.length ? all[idx] : undefined;
          selectors[key] = el ? { ...elementState(el), n: all.length } : { n: all.length };
        } catch {
          selectors[key] = { n: 0 };
        }
      }
    }
  }
  // 顺带并入 id 轻量表:沙箱 attr('id') 复合类选择器回查的索引底(结构变化后保持新鲜)
  gatherIdMap(container, selectors);
  return { selectors, controls: gatherControls(container) };
}

/** 游离元素落 DOM(appendCreated):清洗 HTML、绑定沙箱侧延迟注册的事件、递归子元素 */
function buildCreatedElement(
  spec: CreatedElementSpec,
  targetWindow: Window | null,
  nonce: string,
  targets: Map<number, HTMLElement>,
  nextTargetId: () => number,
  registerEventListener: (
    element: EventTarget,
    eventName: string,
    listener: EventListener,
    capture?: boolean,
  ) => void,
  container: HTMLElement,
  domCtx?: DomEventContext,
): HTMLElement {
  const doc = container.ownerDocument;
  if (!doc) throw new Error('容器缺少 ownerDocument');
  const tmp = doc.createElement('div');
  tmp.innerHTML = sanitizeForContainer(container, String(spec.html ?? ''));
  const el = (tmp.firstElementChild as HTMLElement | null) ?? doc.createElement('div');
  if (spec.addClass) el.classList.add(...String(spec.addClass).split(/\s+/).filter(Boolean));
  // attr/css 记录的属性与内联样式落 DOM(悬浮球 id/position:fixed/z-index 由此生效)
  if (spec.attrs && typeof spec.attrs === 'object') {
    for (const [k, v] of Object.entries(spec.attrs)) el.setAttribute(k, String(v ?? ''));
  }
  if (spec.css && typeof spec.css === 'object' && el.style) {
    for (const [k, v] of Object.entries(spec.css)) el.style.setProperty(cssPropName(k), String(v ?? ''));
  }
  if (typeof spec.text === 'string') el.textContent = spec.text;
  else if (typeof spec.innerHtml === 'string') el.innerHTML = sanitizeForContainer(container, spec.innerHtml);
  for (const h of Array.isArray(spec.handlers) ? spec.handlers : []) {
    if (!targetWindow || !h || !Number.isFinite(h.jqId)) continue;
    const jqId = Number(h.jqId);
    // 与 applyJq 'on' 一致:空格分隔的多事件名拆开逐个绑定
    const eventNames = String(h.evt ?? '').split(/\s+/).filter(Boolean);
    if (eventNames.length === 0) continue;
    const targetId = nextTargetId();
    targets.set(targetId, el);
    for (const eventName of eventNames) {
      const listener = (): void => {
        const data = Object.fromEntries(Object.entries(el.dataset));
        targetWindow.postMessage(
          {
            channel: CHANNEL,
            nonce,
            type: 'jq-event',
            jqId,
            event: { type: eventName },
            target: { kind: 'target', id: targetId, data, state: elementState(el) },
            mirror: { controls: gatherControls(container) },
          },
          '*',
        );
      };
      el.addEventListener(eventName, listener);
      registerEventListener(el, eventName, listener);
    }
  }
  for (const child of Array.isArray(spec.children) ? spec.children : []) {
    if (child && child.kind === 'created') {
      el.appendChild(buildCreatedElement(child, targetWindow, nonce, targets, nextTargetId, registerEventListener, container, domCtx));
    }
  }
  // draggable 记录:宿主侧绑指针拖拽(游离元素无独立 RPC 通道,随落 DOM 一次应用)
  if (spec.draggable && domCtx) applyDraggable(el, spec.draggable, domCtx);
  return el;
}

/** CSS 键名归一:JS 驼峰(zIndex)与 CSS 短横(z-index)都接受 */
function cssPropName(key: string): string {
  return /[A-Z]/.test(key) ? key.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`) : key;
}

/** jQuery 风格 DOM 操作(经 RPC 转发到宿主容器;事件 target 仅使用本次执行内的受控句柄) */
export function applyJq(
  container: HTMLElement,
  ref: JqOperation['ref'],
  method: string,
  args: unknown[],
  targetWindow: Window | null,
  nonce: string,
  targets: Map<number, HTMLElement>,
  nextTargetId: () => number,
  registerEventListener: (
    element: EventTarget,
    eventName: string,
    listener: EventListener,
    capture?: boolean,
  ) => void,
  /** 事件绑定登记表(沙箱级,调用方持有):off() 真解绑所需 */
  bindings?: JqBinding[],
): unknown {
  const els = resolveRefElements(container, ref, targets);
  const first = els[0];
  // document/window 引用仅走事件绑定(见 on/off 分支);查询/写入类操作落在空集合上无副作用
  const globalRef = ref.kind === 'document' || ref.kind === 'window' ? ref : null;
  // 宿主全局只在需要事件绑定的分支读取:纯查询/属性操作在无 DOM 的测试环境下不触碰 window/document
  const domCtx = (): DomEventContext => ({
    hostDocument: (typeof document !== 'undefined' ? document : container.ownerDocument) as Document,
    hostWindow: (typeof window !== 'undefined' ? window : ({} as Window)) as Window,
    targetWindow,
    nonce,
    container,
    controlsProvider: () => gatherControls(container),
    registerEventListener,
    elementStateProvider: elementState,
  });
  switch (method) {
    case 'count':
      return els.length;
    case 'probe':
      // 镜像探测:不改动 DOM;该选择器的状态随本批回包镜像(buildStateMirror)带回沙箱
      return true;
    case 'text':
      if (args.length === 0) return first?.textContent ?? '';
      for (const el of els) el.textContent = safeText(args[0]);
      return true;
    case 'html': {
      if (args.length === 0) return first?.innerHTML ?? '';
      const html = safeText(args[0]);
      for (const el of els) {
        el.innerHTML = sanitizeForContainer(container, html);
        markInjectedChildren(el);
      }
      return true;
    }
    case 'css':
      if (args.length === 1) return first ? first.style.getPropertyValue(cssPropName(String(args[0]))) : '';
      // scrollTop/scrollLeft 不是 CSS 属性:沙箱 animate({scrollTop}) 转发到真实滚动位置
      if (String(args[0]) === 'scrollTop' || String(args[0]) === 'scrollLeft') {
        const key = String(args[0]) as 'scrollTop' | 'scrollLeft';
        const value = Number(args[1]);
        if (Number.isFinite(value)) for (const el of els) el[key] = value;
        return true;
      }
      for (const el of els) el.style.setProperty(cssPropName(String(args[0])), String(args[1] ?? ''));
      return true;
    case 'addClass':
      for (const el of els) el.classList.add(...String(args[0] ?? '').split(/\s+/).filter(Boolean));
      return true;
    case 'removeClass':
      for (const el of els) el.classList.remove(...String(args[0] ?? '').split(/\s+/).filter(Boolean));
      return true;
    case 'toggleClass': {
      const force = args.length > 1 && typeof args[1] === 'boolean' ? (args[1] as boolean) : undefined;
      for (const el of els) {
        for (const cls of String(args[0] ?? '').split(/\s+/).filter(Boolean)) {
          if (force === undefined) el.classList.toggle(cls);
          else el.classList.toggle(cls, force);
        }
      }
      return true;
    }
    case 'hasClass':
      return first ? first.classList.contains(String(args[0] ?? '')) : false;
    case 'val':
      if (args.length === 0) return (first as HTMLInputElement | undefined)?.value ?? '';
      for (const el of els) (el as HTMLInputElement).value = String(args[0] ?? '');
      return true;
    case 'prop': {
      // jQuery prop:checkbox/select 的 checked/disabled 等布尔属性
      const name = String(args[0] ?? '');
      const value = args[1];
      if (name === 'checked') {
        for (const el of els) (el as HTMLInputElement).checked = Boolean(value);
      } else if (name === 'disabled') {
        for (const el of els) (el as HTMLInputElement).disabled = Boolean(value);
      } else if (name === 'value') {
        for (const el of els) (el as HTMLInputElement).value = String(value ?? '');
      } else if (value === undefined) {
        return first ? (first as HTMLInputElement)[name as keyof HTMLInputElement] : undefined;
      } else {
        // name 为脚本传入的动态属性名:keyof HTMLInputElement 的键集含 DOM lib 只读属性,直接索引赋值触发 TS2540;
        // 断言为可变视图仅是类型层面放行(对齐项目内 as unknown as Record 惯例),运行时仍是 el[name] = value
        for (const el of els) (el as unknown as Record<string, unknown>)[name] = value;
      }
      return true;
    }
    case 'attr':
      if (args.length === 1) return first ? first.getAttribute(String(args[0])) ?? '' : '';
      for (const el of els) el.setAttribute(String(args[0]), String(args[1] ?? ''));
      return true;
    case 'trigger': {
      // 触发事件(change 等);宿主已注册的监听器(经 on RPC)会收到 dispatchEvent
      const eventName = String(args[0] ?? '');
      for (const el of els) el.dispatchEvent(new Event(eventName, { bubbles: true }));
      return true;
    }
    case 'append': {
      // 追加 HTML(select 填充 option、容器追加节点等);与 innerHTML 一样经 sanitize 清洗。
      // insertAdjacentHTML 无法直接拿到新增节点,用前后 childNodes 差集打注入标记。
      const html = safeText(args[0]);
      for (const el of els) {
        const before = new Set(Array.from(el.childNodes));
        el.insertAdjacentHTML('beforeend', sanitizeForContainer(container, html));
        for (const node of Array.from(el.childNodes)) {
          if (!before.has(node) && node.nodeType === 1) markInjected(node as HTMLElement);
        }
      }
      return true;
    }
    case 'appendCreated': {
      // $('<div>') 游离元素落 DOM(wuwa 行动页 opt-list 构建路径)
      const spec = args[0] as CreatedElementSpec;
      if (!spec || spec.kind !== 'created') return true;
      for (const el of els) {
        const created = buildCreatedElement(spec, targetWindow, nonce, targets, nextTargetId, registerEventListener, container, domCtx());
        // 先入 DOM 再打标:markInjected 需读 parentElement 判断是否落在覆层根下
        el.appendChild(created);
        markInjected(created);
      }
      return true;
    }
    case 'draggable': {
      const opts = args[0] as (CreatedElementSpec['draggable'] | undefined);
      if (!opts) return true;
      for (const el of els) applyDraggable(el, opts, domCtx());
      return true;
    }
    case 'focus':
      for (const el of els) (el as HTMLElement).focus();
      return true;
    case 'remove':
      for (const el of els) el.remove();
      return true;
    case 'empty':
      for (const el of els) el.innerHTML = '';
      return true;
    case 'off': {
      // jQuery 语义:off(evt) 解绑该事件名下的全部监听(含委托);off() 解绑全部。
      // 旧实现是 no-op,重复绑定会不断累积(实跑问题 7 R3)。清理范围含 cleanup
      // 统一登记表(bindings 由调用方持有;缺失时退回 no-op 不报错)。
      const names = String(args[0] ?? '')
        .split(/\s+/)
        .map(stripEventNamespace)
        .filter(Boolean);
      if (bindings) {
        for (let i = bindings.length - 1; i >= 0; i -= 1) {
          const b = bindings[i];
          if (names.length > 0 && !names.includes(b.eventName)) continue;
          if (typeof (b.element as HTMLElement).removeEventListener === 'function') {
            (b.element as HTMLElement).removeEventListener(b.eventName, b.listener, b.capture);
          }
          bindings.splice(i, 1);
        }
      }
      return true;
    }
    case 'on': {
      // jQuery 语义:'scroll wheel' 这类空格分隔的多事件名要拆开逐个绑定;
      // 事件名可带命名空间('click.myNS',命名空间仅标识用途,绑定用基础名)。
      // 三参写法 on(evt, selector, fn) = 事件委托:宿主只绑一个监听,事件到达时
      // 用 closest(selector) 命中真正目标并回发其状态——动态弹窗/输入框最依赖此种
      // 写法,旧实现把 selector 字符串当回调存,事件到来时 fn.call 抛 TypeError 被吞掉
      // (实跑问题 7 R3)。
      const eventNames = String(args[0] ?? '')
        .split(/\s+/)
        .map(stripEventNamespace)
        .filter(Boolean);
      const jqId = Number(args[1]);
      const selector = typeof args[2] === 'string' && args[2].trim() ? args[2].trim() : null;
      if (!targetWindow || eventNames.length === 0 || !Number.isFinite(jqId)) return true;
      // $(window)/$(document):绑宿主 window/document($(window).on('unload', …) 不再静默失效)
      if (globalRef) {
        bindGlobalEvents(globalRef, eventNames, jqId, domCtx(), selector, bindings);
        return true;
      }
      for (const el of els) {
        for (const eventName of eventNames) {
          const listener = (event: Event): void => {
            // 委托:以 closest 命中的真实目标为准;未命中(或超出绑定元素)则忽略
            let hit: HTMLElement | null = el;
            if (selector) {
              const raw = event.target as Element | null;
              const found =
                raw && typeof raw.closest === 'function' ? raw.closest(selector) : null;
              if (!found || !(found === el || el.contains(found))) return;
              hit = found as HTMLElement;
            }
            const targetId = nextTargetId();
            targets.set(targetId, hit);
            const data = Object.fromEntries(Object.entries(hit.dataset));
            targetWindow.postMessage(
              {
                channel: CHANNEL,
                nonce,
                type: 'jq-event',
                jqId,
                event: { type: eventName },
                // 事件时刻的目标状态(getter 真实化:勾选/取值/滚动度量读到事件当时的真值)
                target: { kind: 'target', id: targetId, data, state: elementState(hit) },
                // 全量表单控件状态:协议脚本在回调里读其它控件($('#agree') 等)
                mirror: { controls: gatherControls(container) },
              },
              '*',
            );
          };
          el.addEventListener(eventName, listener);
          registerEventListener(el, eventName, listener);
          bindings?.push({ jqId, element: el, eventName, listener, selector, capture: undefined });
        }
      }
      return true;
    }
    default:
      throw new Error(`不兼容的角色卡脚本操作: ${method}`);
  }
}

/** jQuery 事件命名空间剥离:'click.myNS' → 'click'(命名空间仅作标识,不参与绑定) */
export function stripEventNamespace(name: string): string {
  const i = name.indexOf('.');
  return i < 0 ? name : name.slice(0, i);
}

/** 事件绑定登记(沙箱级):off() 真解绑与委托支持所需 */
export interface JqBinding {
  jqId: number;
  element: EventTarget;
  eventName: string;
  listener: EventListener;
  /** 委托选择器(null = 直接绑定) */
  selector: string | null;
  capture?: boolean;
}

/** 给元素的新增子节点打注入标记(innerHTML 后调用;覆层根直系子节点获得 pointer-events:auto) */
function markInjectedChildren(parent: HTMLElement): void {
  for (const node of Array.from(parent.childNodes)) {
    if (node.nodeType === 1) markInjected(node as HTMLElement);
  }
}

export function applyRpc(
  context: SandboxExecutionContext,
  op: string,
  args: unknown[],
): unknown {
  // 全局变量保存(状态栏 saveSettings):无选择器参数,必须先于 selector 提取分支
  if (op === 'global-save') {
    writeCardGlobals(context.characterId, args[0]);
    return true;
  }
  // 剪贴板写入桥(实跑问题 7 R4):宿主页面持有真实用户手势与 clipboard-write 权限,
  // 沙箱侧 navigator.clipboard 已被 boot 覆写为转发到此。仅放行文本写入,长度设限。
  if (op === 'clipboard-write') {
    const text = typeof args[0] === 'string' ? args[0] : String(args[0] ?? '');
    if (text.length > 1_000_000) throw new Error('剪贴板文本过大');
    const nav = typeof navigator !== 'undefined' ? navigator : undefined;
    const clip = nav?.clipboard;
    if (!clip?.writeText) throw new Error('宿主页面不提供剪贴板写入');
    // 失败必须走 reject:沙箱侧 writeText 直接 resolve 本 Promise,若此处把失败降级成
    // false,作者脚本的 .then() 会照常弹「已复制」而剪贴板其实是空的。抛错后由 host 的
    // rpc-result(ok:false) 转成沙箱侧 reject,落到作者脚本 .catch() 分支;
    // 未捕获时仅触发 boot 的 unhandledrejection 上报 warn,不会拆除整个沙箱。
    return clip.writeText(text).then(() => true);
  }
  // 纯数据 op(数据 RPC 未注入扩展点时的兜底):首参不是 CSS 选择器,不得走
  // querySelector —— 旧实现会把楼层序号/世界书名当选择器,safeSelector 抛错 →
  // RPC reject → 沙箱大清理摘除全部监听(实跑问题 7 主因之一)。返回空数据,
  // 脚本读不到内容但不致界面死亡。
  if (!isDomSelectorRpc(op)) {
    return null;
  }
  const selector = safeSelector(args[0]);
  const target = context.container.querySelector<HTMLElement>(selector);
  if (!target) throw new Error(`未找到状态栏元素: ${selector}`);
  if (op === 'setText') {
    target.textContent = safeText(args[1]);
    return true;
  }
  if (op === 'setHtml') {
    const html = safeText(args[1]);
    target.innerHTML = sanitizeHtml(html, {
      allowedTags: ['span', 'b', 'strong', 'i', 'em', 'small', 'br'],
      allowedAttributes: { '*': ['class', 'aria-label'] },
      allowedSchemes: [],
    });
    return true;
  }
  if (op === 'setAttribute') {
    const name = String(args[1] ?? '');
    if (!['class', 'title', 'aria-label', 'data-value'].includes(name)) throw new Error('属性不在白名单');
    target.setAttribute(name, safeText(args[2]));
    return true;
  }
  throw new Error(`不兼容的角色卡脚本操作: ${op}`);
}

/** DOM 选择器类 RPC 白名单:其余 op(chat-messages/lorebook-entries/clipboard-write 等
 *  纯数据请求)不按选择器解析。数据 RPC 正常应由 rpcExtensions 或上方专用分支处理;
 *  未注入时此处兜底返回 null。 */
function isDomSelectorRpc(op: string): boolean {
  return op === 'setText' || op === 'setHtml' || op === 'setAttribute';
}
