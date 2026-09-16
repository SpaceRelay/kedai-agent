// 弹窗单一事实来源(批次 5.2)。
//
// 本文件集中声明全部模态弹窗(flag 开关名 / 中文 label / 懒加载组件),派生关系如下:
//   - App.vue 的弹窗渲染由 MODALS v-for 派生(声明顺序 = 渲染顺序,后声明者叠在上层);
//   - App.vue 的 Android 返回键按 MODAL_FLAGS 逆序关闭(即先关最上层弹窗);
//   - tools/check-arch.mjs 的「组件直改 state」弹窗白名单由本文件解析派生;
//   - SettingsHub 的 openPanel 参数类型 ModalFlag 取自本文件;
//   - uiPrefs.ts 的开关 ref 仍逐个声明(保 store.settingsOpen 的布尔类型与绑定活),
//     其与 MODALS 的一致性由 web/src/modals.test.ts 元测试锁定。
// 新增一个弹窗 = 本文件加一条 + uiPrefs.ts 加一个 ref(共 2 处),其余全自动。
// 解析契约(tools/check-arch.mjs 依赖):每个弹窗必须是单独一行
// `modal('flag', '中文 label', () => import('./components/X.vue')),`。
import type { AsyncComponentLoader, Component } from 'vue';
import { lazyModal } from './asyncModal';

/** 一个弹窗的声明。 */
export interface ModalDecl<F extends string = string> {
  /** uiPrefs 中的开关 ref 名(同时是 store 门面透传键与 App.vue 渲染判断依据) */
  readonly flag: F;
  /** 中文名(懒加载失败提示条展示;亦作 lazyModal 的 name) */
  readonly label: string;
  /** 懒加载组件(失败自动重试一次,仍失败写 uiPrefs.modalLoadError) */
  readonly component: Component;
}

/** 建一条弹窗声明:flag 在本文件只写一次(lazyModal 的重试 flag 与之同源)。 */
function modal<F extends string, T extends Component>(
  flag: F,
  label: string,
  loader: AsyncComponentLoader<T>,
): ModalDecl<F> {
  return { flag, label, component: lazyModal(loader, label, flag) };
}

/**
 * 弹窗注册表。声明顺序 = App.vue 渲染顺序:后声明者 DOM 更靠后,叠在上层;
 * Android 返回键按 MODAL_FLAGS 逆序关闭,即「先关最上层那个」。
 */
export const MODALS = [
  modal('settingsOpen', '综合设置', () => import('./components/SettingsHub.vue')),
  modal('worldBooksOpen', '世界书', () => import('./components/WorldBooksModal.vue')),
  modal('chatRecordsOpen', '聊天记录', () => import('./components/ChatRecords.vue')),
  modal('pluginsOpen', '插件', () => import('./components/PluginsModal.vue')),
  modal('skillsOpen', '技能库', () => import('./components/SkillsModal.vue')),
  modal('contractsOpen', '契约编辑', () => import('./components/ContractsModal.vue')),
  modal('promptsOpen', '提示词管理', () => import('./components/PromptManager.vue')),
  modal('scriptsOpen', '脚本管理', () => import('./components/ScriptsModal.vue')),
  modal('macrosOpen', '宏调试', () => import('./components/MacrosModal.vue')),
  modal('eventsOpen', '事件监控', () => import('./components/DevToolsModal.vue')),
  modal('optimizeOpen', '优化面板', () => import('./components/OptimizeModal.vue')),
  modal('memoryOpen', '记忆库', () => import('./components/MemoryModal.vue')),
  modal('repoIndexOpen', '仓库索引', () => import('./components/RepoIndexModal.vue')),
  modal('quickRepliesOpen', '快速回复', () => import('./components/QuickRepliesModal.vue')),
  // 退出确认(桌面:壳下发 kedai://close-requested;Android:返回键无处可退)。
  // 声明在**末尾** = 渲染在最上层:Android 返回键按 MODAL_FLAGS 逆序关闭,先关它。
  modal('exitConfirmOpen', '退出确认', () => import('./components/ExitConfirmModal.vue')),
] as const;

/** 弹窗 flag 字面量联合(设置 Hub 的 openPanel 等参数类型直接取用)。 */
export type ModalFlag = (typeof MODALS)[number]['flag'];

/** 全部弹窗 flag(顺序同渲染顺序)。 */
export const MODAL_FLAGS: readonly ModalFlag[] = MODALS.map((m) => m.flag);
