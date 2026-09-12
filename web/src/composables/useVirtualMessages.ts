// 消息列表轻量虚拟滚动(零依赖,IntersectionObserver 实现)
// 设计:
// - 消息数超过阈值才启用;以下全部真实挂载(短会话零开销)。
// - 尾部 tailKeep 条常驻真实挂载(流式生成目标与「思考中」占位总在尾部,保证自动吸底)。
// - 其余消息按 IntersectionObserver(root=滚动容器,rootMargin=上下各 overscanPx)上报:
//   进入缓冲区 → 真实挂载;离开缓冲区 → 记录实测高度后降级为等高占位 div。
// - 占位高度:渲染过的用实测缓存,未渲染过的按角色估算;会话切换时整体重置。
// DOM 结构语义不变:占位 div 与真实消息平级,不引入包裹层(style.css 无相关层级选择器)。

import { computed, onBeforeUnmount, onMounted, reactive, watch, nextTick } from 'vue';
import type { Ref } from 'vue';

/** 虚拟滚动可调参数 */
export interface VirtualListOptions {
  /** 消息总数超过该值才启用虚拟化(默认 80) */
  threshold?: number;
  /** 尾部常驻真实挂载条数(默认 30;流式目标与最新上下文总在尾部) */
  tailKeep?: number;
  /** 视口上下缓冲区像素(IntersectionObserver rootMargin,默认 1200) */
  overscanPx?: number;
}

/** 未渲染消息的占位高度估算(按角色;assistant 通常含 markdown 卡片,偏高) */
export const ESTIMATED_HEIGHTS: Record<string, number> = {
  user: 72,
  assistant: 180,
  system: 64,
};

/** 占位估算兜底(未知角色) */
export const ESTIMATED_HEIGHT_FALLBACK = 96;

/**
 * 虚拟滚动核心状态(纯逻辑,无 DOM 依赖,便于单测)。
 * visible/heights 为 Vue reactive 集合:模板按 isActive 读取,集合变化精准触发对应行重渲染。
 */
export function createVirtualListState(options: VirtualListOptions = {}) {
  const threshold = options.threshold ?? 80;
  const tailKeep = options.tailKeep ?? 30;

  /** 当前处于「真实挂载」集合内的消息 id(IO 上报进入缓冲区的非尾部消息) */
  const visible = reactive(new Set<number>());
  /** 已渲染消息的实测高度缓存(离开缓冲区时记录,占位等高) */
  const heights = reactive(new Map<number, number>());

  /** 是否启用虚拟化(未超阈值全部真实挂载) */
  function isEnabled(count: number): boolean {
    return count > threshold;
  }

  /**
   * 该行是否真实挂载。
   * @param id 消息 id;@param index 在列表中的下标;@param count 列表总长;@param pinnedId 强制挂载 id(编辑中)
   */
  function isActive(id: number, index: number, count: number, pinnedId: number | null = null): boolean {
    if (!isEnabled(count)) return true;
    if (id === pinnedId) return true;
    if (index >= count - tailKeep) return true;
    return visible.has(id);
  }

  /** IO 上报:进入视口缓冲区 → 真实挂载 */
  function markVisible(id: number): void {
    visible.add(id);
  }

  /** IO 上报:离开视口缓冲区 → 记录实测高度并降级为占位 */
  function markHidden(id: number, measuredHeight: number): void {
    if (measuredHeight > 0) heights.set(id, measuredHeight);
    visible.delete(id);
  }

  /** 占位高度:优先实测缓存,否则按角色估算 */
  function placeholderHeight(id: number, role: string): number {
    return heights.get(id) ?? ESTIMATED_HEIGHTS[role] ?? ESTIMATED_HEIGHT_FALLBACK;
  }

  /** 会话切换等场景整体重置 */
  function reset(): void {
    visible.clear();
    heights.clear();
  }

  return { threshold, tailKeep, visible, heights, isEnabled, isActive, markVisible, markHidden, placeholderHeight, reset };
}

export type VirtualListState = ReturnType<typeof createVirtualListState>;

/** 行宿主元素解析目标:模板 ref 回调收到的组件实例(取 expose 的 rootEl)或原生元素 */
type RowRefTarget = { rootEl?: HTMLElement | null } | HTMLElement | null;

function resolveRowEl(target: RowRefTarget): HTMLElement | null {
  if (!target) return null;
  if (target instanceof HTMLElement) return target;
  return target.rootEl ?? null;
}

export interface UseVirtualMessagesOptions extends VirtualListOptions {
  /** 消息列表(store 响应式引用) */
  messages: Ref<Array<{ id: number; role: string }>>;
  /** 滚动容器元素 ref */
  scrollArea: Ref<HTMLElement | null>;
  /** 强制真实挂载的消息 id(编辑中的消息;无则传 Ref<null>) */
  pinnedId: Ref<number | null>;
  /** 行挂载状态变化(占位↔真实)后的回调:父组件用于补跑资源水合/脚本执行 */
  onRowsChanged?: () => void;
  /** 会话标识:变化时重置高度缓存与可见集 */
  resetKey?: Ref<string | null | undefined>;
}

/**
 * 消息列表虚拟滚动 composable:模板配合 `v-if="!vm.isActive(...)"` 渲染占位 div,
 * 并通过 `:ref="(el) => vm.bindRow(m.id, el)"` 登记行元素(占位与真实行都登记)。
 */
export function useVirtualMessages(opts: UseVirtualMessagesOptions) {
  const state = createVirtualListState(opts);
  const overscanPx = opts.overscanPx ?? 1200;

  const enabled = computed(() => state.isEnabled(opts.messages.value.length));

  /** 该行是否真实挂载(模板每行调用;依赖 reactive visible 集合,精准重渲染) */
  function isActive(id: number, index: number): boolean {
    return state.isActive(id, index, opts.messages.value.length, opts.pinnedId.value);
  }

  /** 占位 div 内联样式:等高 + 与 .sv-msg 相同的下间距(28px),不新增全局样式 */
  function placeholderStyle(m: { id: number; role: string }): { height: string; marginBottom: string } {
    return { height: `${state.placeholderHeight(m.id, m.role)}px`, marginBottom: '28px' };
  }

  // ===== IntersectionObserver 接线(DOM 侧薄胶水,核心逻辑在 createVirtualListState) =====
  let observer: IntersectionObserver | null = null;
  /** 已观察的行元素(按消息 id) */
  const observed = new Map<number, HTMLElement>();

  function handleEntries(entries: IntersectionObserverEntry[]): void {
    let changed = false;
    for (const entry of entries) {
      const el = entry.target as HTMLElement;
      const id = Number(el.dataset.kdVmId);
      if (!Number.isFinite(id)) continue;
      if (entry.isIntersecting) {
        if (!state.visible.has(id)) {
          state.markVisible(id);
          changed = true;
        }
      } else if (state.visible.has(id)) {
        // 离开缓冲区:此时真实元素仍挂载,记录实测高度供占位等高
        state.markHidden(id, el.offsetHeight || Math.round(entry.boundingClientRect.height));
        changed = true;
      }
    }
    // 占位↔真实切换后,新挂载的消息可能含资源卡片/渲染面板/脚本容器,通知父组件补跑
    if (changed) void nextTick(() => opts.onRowsChanged?.());
  }

  /** 模板行 ref 回调:登记/注销行元素并纳入观察(占位与真实行共用) */
  function bindRow(id: number, target: RowRefTarget): void {
    const prev = observed.get(id);
    if (prev) {
      observer?.unobserve(prev);
      observed.delete(id);
    }
    const el = resolveRowEl(target);
    if (!el) return;
    el.dataset.kdVmId = String(id);
    observed.set(id, el);
    observer?.observe(el);
  }

  onMounted(() => {
    if (typeof IntersectionObserver === 'undefined') return;
    observer = new IntersectionObserver(handleEntries, {
      root: opts.scrollArea.value ?? null,
      rootMargin: `${overscanPx}px 0px`,
    });
    // 挂载前已登记的行(ref 回调先于 onMounted)统一补观察
    for (const el of observed.values()) observer.observe(el);
  });

  onBeforeUnmount(() => {
    observer?.disconnect();
    observer = null;
    observed.clear();
  });

  // 会话切换:重置可见集与高度缓存(旧 id 的实测高度对新会话无意义)
  if (opts.resetKey) {
    watch(opts.resetKey, () => state.reset());
  }

  return {
    enabled,
    isActive,
    placeholderStyle,
    bindRow,
    /** 内部状态(测试与调试可见) */
    state,
  };
}
