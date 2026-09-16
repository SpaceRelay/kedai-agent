// 界面偏好 store:各弹窗/面板开关、启动动画、安全 HTML 渲染开关(按角色卡记忆 + 全局默认)。
// 从 store.ts 按领域拆分。跨 store 依赖(M5 断环后)均为叶子模块:
//   - currentCharacterId 读回调桥(storeBridge.ts,owner 为 character store);
//   - 保存全局 render_html 走回调桥(storeBridge.ts,由 genSettings 注册队列保存);
//   - 反向注册两个 sink(renderHtml 默认值通知 / task 用的界面偏好能力)。
// 本 store 不顶层 import 任何其他 store,成为依赖图的汇点。
import { defineStore } from 'pinia';
import { ref, watch } from 'vue';
import { LocalRenderHtmlPreferenceStore } from '../renderHtmlPreference';
import {
  currentCharacterIdValue,
  queueSettingsSave,
  registerRenderHtmlDefaultSink,
  registerUiPrefsSink,
} from './storeBridge';

export const useUiPrefsStore = defineStore('app.uiPrefs', () => {
  // ===== 弹窗/面板开关 =====
  // 弹窗开关(settingsOpen…chatRecordsOpen)与 web/src/modals.ts 的 MODALS 注册表一一对应:
  // 逐个声明的目的是保住 store.xxxOpen 的布尔类型与模板绑定;一致性由
  // web/src/modals.test.ts 元测试锁定(解析契约:弹窗开关均为 ref(false) 的具名声明)。
  const settingsOpen = ref(false);
  /** 提示词顺序管理面板开关 */
  const promptsOpen = ref(false);
  /** 右侧 Agent 面板开关(默认收起;唯一可见性来源,不再由生成状态强行覆盖) */
  const agentPanelOpen = ref(false);
  /**
   * Agent 面板「本会话内不再自动展开」抑制标记(仅内存态,不持久化)。
   * 用户主动收起后置位:本轮会话内生成不再自动弹出面板,尊重用户选择;
   * 新建/切换会话时复位,恢复「生成开始时自动展开一次」的便利。
   */
  const agentPanelAutoSuppressed = ref(false);
  /**
   * 左侧功能区抽屉开关(仅移动端有效:窄屏下 Sidebar 由常驻栏改为覆盖式抽屉)。
   * 桌面端恒为 false 且不影响布局——CSS 只在 max-width 断点内响应 .open 类。
   */
  const sidebarOpen = ref(false);
  const worldBooksOpen = ref(false);
  const quickRepliesOpen = ref(false);
  const audioOpen = ref(false);
  /** 插件管理弹窗开关 */
  const pluginsOpen = ref(false);
  const skillsOpen = ref(false);
  const contractsOpen = ref(false);
  const scriptsOpen = ref(false);
  const macrosOpen = ref(false);
  const eventsOpen = ref(false);
  const optimizeOpen = ref(false);
  /** 记忆库弹窗开关(跨会话记忆检索/置顶/清理;侧边栏一级入口) */
  const memoryOpen = ref(false);
  /** 仓库索引面板开关(只读展示 .kedai-index 生成的代码索引) */
  const repoIndexOpen = ref(false);
  /** 聊天记录面板开关 */
  const chatRecordsOpen = ref(false);
  /**
   * 退出确认弹窗开关。打开时机:桌面壳下发 kedai://close-requested(用户点了窗口关闭),
   * 或 Android 返回键已无弹窗/抽屉可退。确认后发 kedai://exit-app 退出,
   * 取消则发 kedai://close-cancelled 让壳复位二次关闭兜底标记。
   */
  const exitConfirmOpen = ref(false);
  /** 启动动画是否完成 */
  const splashDone = ref(false);
  /** 弹窗懒加载失败提示(defineAsyncComponent onError 写入;flag 为重试所需的面板开关键) */
  const modalLoadError = ref<{ name: string; flag: string } | null>(null);
  /** 启动/切换时的数据加载失败提示(角色/会话/历史加载 catch 写入;下次加载成功时清除) */
  const dataLoadError = ref<string | null>(null);
  /**
   * 全局未捕获异常提示(计划批次 5.1;写入方为 main.ts 注册的 globalErrorHandlers)。
   * 与 dataLoadError 分开的原因:后者的横幅挂着一个「重试 = 重载角色列表」按钮,
   * 而未捕获异常没有对应的重试动作——混用会让用户点到一个与错误无关的重试。
   * 仅内存态:下次异常覆盖、用户关闭即清除,不持久化。
   */
  const globalError = ref<string | null>(null);
  /**
   * 上传角色卡请求计数器(跨组件通信,取代 document.querySelector 戳 Sidebar 内部 DOM):
   * 发起方(如 SettingsHub 快速操作)自增;Sidebar watch 本计数器触发自身隐藏 file input 的 click。
   * 计数器语义而非布尔开关:连续点两次也要各触发一次,不丢请求。
   */
  const characterUploadRequested = ref(0);

  // ===== 合并面板内部分区记忆(原为批次 3 L3 独立「调用情况」面板开关;面板合并后改作 tab 记忆) =====
  // localStorage 键保持不变(kedai.call-trace-open.v1),旧偏好平滑迁移:
  // 之前开着调用面板的用户,升级后打开合并面板落在「调用情况」tab。
  const CALL_TRACE_KEY = 'kedai.call-trace-open.v1';
  /** Agent 合并面板内部分区:true = 打开面板时落在「调用情况」tab;false = 「Agent 状态」tab */
  const callTraceOpen = ref<boolean>(readStoredCallTrace());
  function readStoredCallTrace(): boolean {
    try {
      return localStorage.getItem(CALL_TRACE_KEY) === '1';
    } catch {
      return false;
    }
  }
  watch(callTraceOpen, (v) => {
    try {
      localStorage.setItem(CALL_TRACE_KEY, v ? '1' : '0');
    } catch {
      /* 忽略 */
    }
  });

  // ===== 任务模式「成果汇总」卡显示开关(实跑问题 1) =====
  // 逐轮对话记录区成为任务产出的权威视图后,单一「成果汇总」卡默认关闭(避免与
  // 各轮气泡重复);需要一眼看合并成果的用户可手动打开,选择持久化到 localStorage。
  const TASK_RESULT_SUMMARY_KEY = 'kedai.task-result-summary.v1';
  const taskResultSummaryOpen = ref<boolean>(readStoredTaskResultSummary());
  function readStoredTaskResultSummary(): boolean {
    try {
      return localStorage.getItem(TASK_RESULT_SUMMARY_KEY) === '1';
    } catch {
      return false;
    }
  }
  watch(taskResultSummaryOpen, (v) => {
    try {
      localStorage.setItem(TASK_RESULT_SUMMARY_KEY, v ? '1' : '0');
    } catch {
      /* 忽略 */
    }
  });

  // ===== 安全 HTML 渲染开关 =====
  /**
   * 消息 HTML 渲染开关(只控制安全 HTML,与 JavaScript 授权独立)。
   * 按角色卡记忆:每张卡在 localStorage 记录各自的开关,无记忆时回退全局默认
   * (settings.json 的 render_html)。当前 ref 恒为「当前角色卡的生效值」。
   */
  const renderHtml = ref(false);
  /** 全局默认 HTML 渲染开关(设置面板;无角色卡记忆时生效) */
  const defaultRenderHtml = ref(false);
  /** 每张角色卡的 HTML 渲染开关记忆(characterId -> boolean) */
  const renderHtmlPreferenceStore = new LocalRenderHtmlPreferenceStore(localStorage);
  const renderHtmlOverrides = ref<Record<string, boolean>>(renderHtmlPreferenceStore.read());

  /**
   * 按当前角色卡同步 HTML 渲染开关生效值:有角色卡记忆用记忆,否则用全局默认。
   * 程序性赋值走 restoring 闩锁,不触发持久化写入(避免为默认值凭空生成记忆)。
   */
  function syncRenderHtmlToCurrent(): void {
    const cid = currentCharacterIdValue();
    const v = cid && cid in renderHtmlOverrides.value
      ? renderHtmlOverrides.value[cid]
      : defaultRenderHtml.value;
    if (renderHtml.value === v) return;
    if (renderHtmlSaveTimer) {
      clearTimeout(renderHtmlSaveTimer);
      renderHtmlSaveTimer = null;
    }
    restoringRenderHtml = true;
    renderHtml.value = v;  }

  /**
   * 程序性设置 HTML 渲染开关(引导条一键开启 / 顶栏与设置面板切换共用)。
   * 与 syncRenderHtmlToCurrent 的区别:后者是「按当前角色卡回读持久化值」,
   * 本函数是「用户显式改值」——赋值后由下方 watch 节流持久化(有卡写卡记忆,
   * 无卡写全局默认)。统一入口避免组件直接赋值绕过持久化闩锁与失败回滚。
   */
  function setRenderHtml(v: boolean): void {
    renderHtml.value = v;
  }

  /** 切换 HTML 渲染开关(等价于 setRenderHtml(!renderHtml)) */
  function toggleRenderHtml(): void {
    renderHtml.value = !renderHtml.value;
  }

  /** 删除角色卡时清理其 HTML 渲染开关记忆(随卡删除,不留孤儿数据) */
  function removeRenderHtmlOverride(characterId: string): void {
    if (!(characterId in renderHtmlOverrides.value)) return;
    const next = { ...renderHtmlOverrides.value };
    delete next[characterId];
    renderHtmlOverrides.value = next;
    renderHtmlPreferenceStore.write(next);
  }

  // renderHtml 变更时自动持久化(节流:300ms 内不重复保存)。
  // 有当前角色卡 → 写入该卡的 localStorage 记忆(不碰全局设置);
  // 无角色卡 → 写入全局默认(settings.json render_html)。
  let renderHtmlSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let confirmedRenderHtml = renderHtml.value;
  let restoringRenderHtml = false;
  watch(renderHtml, (v) => {
    if (restoringRenderHtml) {
      restoringRenderHtml = false;
      return;
    }
    if (renderHtmlSaveTimer) clearTimeout(renderHtmlSaveTimer);
    renderHtmlSaveTimer = setTimeout(() => {
      const cid = currentCharacterIdValue();
      if (cid) {
        // 按角色卡记忆:仅写 localStorage,不改全局默认
        const next = { ...renderHtmlOverrides.value, [cid]: v };
        renderHtmlOverrides.value = next;
        renderHtmlPreferenceStore.write(next);
        confirmedRenderHtml = v;
      } else {
        void queueSettingsSave({ render_html: v })
          .then(() => { confirmedRenderHtml = v; })
          .catch((error) => {
            console.error('HTML 渲染设置保存失败', error);
            restoringRenderHtml = true;
            renderHtml.value = confirmedRenderHtml;
          });
      }
    }, 300);
  });

  // 注册给回调桥(M5 断环):genSettings 读到新的全局 render_html 默认值后通知这里。
  // 顺序与原实现一致:先写 defaultRenderHtml,再由 syncRenderHtmlToCurrent 按
  // 角色卡记忆重算生效值(有卡记忆优先,无卡才跟随全局默认)。
  registerRenderHtmlDefaultSink((v) => {
    defaultRenderHtml.value = v;
    syncRenderHtmlToCurrent();
  });

  // 注册给回调桥(M5 断环):task store 需要的三个界面操作,经桥调用免去
  // task → uiPrefs 的顶层 import。
  registerUiPrefsSink({
    collapseAgentPanel,
    autoOpenAgentPanel,
    isCallTraceOpen: () => callTraceOpen.value,
  });

  // ===== Agent 面板开合动作 =====
  // 语义分层:一次性的自动展开(生成开始时)与用户显式开合分开,避免「生成中
  // 面板被强行撑开、关闭按钮无效」的锁死体验(详见 AgentPanel.showPanel 与
  // chat.startStream 的调用点)。
  /** 用户显式展开:清除自动展开抑制(下一次生成仍可自动展开) */
  function openAgentPanel(): void {
    agentPanelOpen.value = true;
    agentPanelAutoSuppressed.value = false;
  }
  /** 用户显式收起:记下抑制标记,本会话内生成不再自动展开 */
  function collapseAgentPanel(): void {
    agentPanelOpen.value = false;
    agentPanelAutoSuppressed.value = true;
  }
  /** 切换开合(桌面右缘竖条 / 顶栏开关 / 底部导航共用) */
  function toggleAgentPanel(): void {
    if (agentPanelOpen.value) collapseAgentPanel();
    else openAgentPanel();
  }
  /** 生成/任务开始时的自动展开:用户已收起过则不再打扰 */
  function autoOpenAgentPanel(): void {
    if (agentPanelAutoSuppressed.value) return;
    agentPanelOpen.value = true;
  }
  /** 复位自动展开抑制(新建/切换会话时调用,语义为「本会话内不再自动弹」) */
  function resetAgentPanelAutoSuppress(): void {
    agentPanelAutoSuppressed.value = false;
  }

  return {
    settingsOpen,
    promptsOpen,
    agentPanelOpen,
    agentPanelAutoSuppressed,
    openAgentPanel,
    collapseAgentPanel,
    toggleAgentPanel,
    autoOpenAgentPanel,
    resetAgentPanelAutoSuppress,
    sidebarOpen,
    worldBooksOpen,
    quickRepliesOpen,
    audioOpen,
    pluginsOpen,
    skillsOpen,
    contractsOpen,
    scriptsOpen,
    macrosOpen,
    eventsOpen,
    optimizeOpen,
    memoryOpen,
    repoIndexOpen,
    chatRecordsOpen,
    exitConfirmOpen,
    splashDone,
    modalLoadError,
    dataLoadError,
    globalError,
    characterUploadRequested,
    callTraceOpen,
    taskResultSummaryOpen,
    renderHtml,
    defaultRenderHtml,
    renderHtmlOverrides,
    syncRenderHtmlToCurrent,
    setRenderHtml,
    toggleRenderHtml,
    removeRenderHtmlOverride,
  };
});
