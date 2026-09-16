// stores/storeBridge.ts — store 间依赖的**类型化注册桥**(断环机制)。
//
// 代际: L2(中层·干 / Orchestration)。
// 为什么存在: 前端 store 曾构成一个强连通分量(character/chat/genSettings/uiPrefs/task
//   互相可达),顶层 import 成环会让初始化顺序变成隐式契约、且无法单独替换任一 store。
//   `tools/check-arch.mjs` 规则 A 对此**硬 FAIL**。
// 机制: 被依赖方在 setup 期**注册回调**,依赖方在 action 运行期取值——依赖方向强制
//   单向指向叶子模块,环被拆解。`KNOWN_CYCLES` 因此为空数组,规则 A 实测 0 环。
// 纪律: 新增跨 store 调用必须走本桥(owner 必填 + 显式降级 + 配对测试),不得直接 import。
import type * as api from '../api';

// ---------- 桥的通用结构 ----------

/** 类型化回调桥:一个注册口 + 一个取用口。 */
export interface StoreBridge<T> {
  /**
   * 注册实现。owner 必填(如 'character' / 'genSettings'):
   * 同一 owner 重复注册 = 幂等覆盖;不同 owner 覆盖 = 桥的 owner 约定被打破,
   * 打印警告便于定位误注册(仍以最后一次注册为准)。
   */
  register(owner: string, fn: T): void;
  /** 取用实现;未注册时返回构造时声明的显式降级实现。 */
  get(): T;
}

/** 造一条桥。name 仅用于诊断日志;fallback 是未注册时的显式降级行为。 */
function createBridge<T>(name: string, fallback: T): StoreBridge<T> {
  let impl: T | null = null;
  let owner: string | null = null;
  return {
    register(nextOwner: string, fn: T): void {
      if (owner !== null && owner !== nextOwner) {
        console.warn(`[kedai] storeBridge「${name}」被不同 owner 重复注册:${owner} → ${nextOwner}(后者生效)`);
      }
      impl = fn;
      owner = nextOwner;
    },
    get(): T {
      return impl ?? fallback;
    },
  };
}

// ---------- 六条桥的定义 ----------

/** 读当前选中角色 id。 */
export type CharacterIdProvider = () => string | null;

/** 队列化保存运行期设置(genSettings.queueSettingsSave)。 */
export type SettingsSaver = (patch: api.RuntimeSettingsPatch) => Promise<void>;

/** 重新加载运行期设置(genSettings.loadSettings)。 */
export type SettingsLoader = () => Promise<void>;

/**
 * 「全局 render_html 默认值已变更」的通知口。
 * 由 uiPrefs 注册:收到后写 defaultRenderHtml 并按角色卡记忆重算生效值。
 */
export type RenderHtmlDefaultSink = (renderHtmlDefault: boolean) => void;

/** 聊天侧 SSE 事件上报口(chat.onSseEvent):任务事件原样透传事件监控面板。 */
export type ChatEventSink = (ev: api.SseEvent) => void;

/**
 * uiPrefs 侧能力(task store 需要的三个界面操作)。
 * 以整体注册而非三组函数,便于 uiPrefs 一处声明、语义集中。
 */
export type UiPrefsSink = {
  /** 进入任务模式时收起 Agent 面板(走 collapseAgentPanel,记抑制标记) */
  collapseAgentPanel: () => void;
  /** 新建/切换任务时自动展开 Agent 面板(受用户抑制标记约束) */
  autoOpenAgentPanel: () => void;
  /** 调用追踪面板是否展开(决定 llm_call 事件是否触发重拉) */
  isCallTraceOpen: () => boolean;
};

/**
 * 六条桥(owner 与降级行为集中登记;业务调用点走下方具名包装,owner 已固定)。
 *
 * 降级语义一览(未注册时 get() 的行为):
 *   - characterId → () => null(等同「未选角色」);
 *   - settingsSaver → reject(调用方本就有 catch,走既有「保存失败回滚」分支,
 *     与网络失败同路径,不会静默丢数据);
 *   - settingsLoader → no-op;
 *   - renderHtmlDefault → no-op(仅影响该展示值);
 *   - chatEvent → no-op;
 *   - uiPrefs → 安全空实现(不崩、不误展开面板,isCallTraceOpen 恒 false)。
 */
export const storeBridges = {
  /** 读当前角色 id(owner:character store;读取方:chat / uiPrefs) */
  characterId: createBridge<CharacterIdProvider>('characterId', () => null),
  /** 设置保存(owner:genSettings;调用方:uiPrefs / task) */
  settingsSaver: createBridge<SettingsSaver>('settingsSaver', () =>
    Promise.reject(new Error('设置服务尚未就绪(genSettings store 未初始化),已跳过保存')),
  ),
  /** 设置重载(owner:genSettings;调用方:task 切换顶层模式后) */
  settingsLoader: createBridge<SettingsLoader>('settingsLoader', async () => {}),
  /** 全局 render_html 默认值通知(owner:uiPrefs;通知方:genSettings) */
  renderHtmlDefault: createBridge<RenderHtmlDefaultSink>('renderHtmlDefault', () => {}),
  /** 聊天事件上报(owner:chat;调用方:task 的 SSE 分发) */
  chatEvent: createBridge<ChatEventSink>('chatEvent', () => {}),
  /** 界面偏好能力(owner:uiPrefs;调用方:task) */
  uiPrefs: createBridge<UiPrefsSink>('uiPrefs', {
    collapseAgentPanel: () => {},
    autoOpenAgentPanel: () => {},
    isCallTraceOpen: () => false,
  }),
} as const;

// ---------- 具名包装(向后兼容:调用点名字与签名不变,owner 在此固定) ----------

/** 由 character store 在 setup 阶段注册(传入读自己 currentCharacterId 的闭包)。 */
export function registerCharacterIdProvider(fn: CharacterIdProvider): void {
  storeBridges.characterId.register('character', fn);
}

/** 当前选中角色 id;未注册(character store 未初始化)时为 null(等同「未选角色」)。 */
export function currentCharacterIdValue(): string | null {
  return storeBridges.characterId.get()();
}

/** 由 genSettings store 在 setup 阶段注册(同一 owner 重复注册为幂等覆盖)。 */
export function registerSettingsSaver(fn: SettingsSaver): void {
  storeBridges.settingsSaver.register('genSettings', fn);
}

/**
 * 队列化保存设置(uiPrefs 持久化 render_html 默认值用)。
 * 未注册(genSettings 从未被实例化)时返回 reject —— 调用方本就有 catch,
 * 走既有的「保存失败回滚」分支,与网络失败同路径,不会静默丢数据。
 */
export function queueSettingsSave(patch: api.RuntimeSettingsPatch): Promise<void> {
  return storeBridges.settingsSaver.get()(patch);
}

/** 由 genSettings store 在 setup 阶段注册。 */
export function registerSettingsLoader(fn: SettingsLoader): void {
  storeBridges.settingsLoader.register('genSettings', fn);
}

/** 重新加载设置(切换顶层模式后按新模式覆盖层刷新)。未注册时静默跳过。 */
export function reloadSettings(): void {
  void storeBridges.settingsLoader.get()();
}

/** 由 uiPrefs store 在 setup 阶段注册。 */
export function registerRenderHtmlDefaultSink(fn: RenderHtmlDefaultSink): void {
  storeBridges.renderHtmlDefault.register('uiPrefs', fn);
}

/** 通知全局 render_html 默认值变更。未注册时静默跳过(仅影响该展示值)。 */
export function notifyRenderHtmlDefault(renderHtmlDefault: boolean): void {
  storeBridges.renderHtmlDefault.get()(renderHtmlDefault);
}

/** 由 chat store 在 setup 阶段注册。 */
export function registerChatEventSink(fn: ChatEventSink): void {
  storeBridges.chatEvent.register('chat', fn);
}

/** 上报聊天侧 SSE 事件。未注册时静默跳过。 */
export function reportChatEvent(ev: api.SseEvent): void {
  storeBridges.chatEvent.get()(ev);
}

/** 由 uiPrefs store 在 setup 阶段注册。 */
export function registerUiPrefsSink(fn: UiPrefsSink): void {
  storeBridges.uiPrefs.register('uiPrefs', fn);
}

/** 取 uiPrefs 能力;未注册时返回安全的空实现(不崩、不误展开面板)。 */
export function uiPrefsBridge(): UiPrefsSink {
  return storeBridges.uiPrefs.get();
}
