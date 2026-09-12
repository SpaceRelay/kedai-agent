// 界面偏好 store:各弹窗/面板开关、启动动画、安全 HTML 渲染开关(按角色卡记忆 + 全局默认)。
// 从 store.ts 按领域拆分。跨 store 引用(currentCharacterId / queueSettingsSave)均在
// 回调/动作运行时解析,setup 阶段不实例化其他 store,避免初始化环。
import { defineStore } from 'pinia';
import { ref, watch } from 'vue';
import { LocalRenderHtmlPreferenceStore } from '../renderHtmlPreference';
import { useCharacterStore } from './character';
import { useGenSettingsStore } from './genSettings';

export const useUiPrefsStore = defineStore('app.uiPrefs', () => {
  // ===== 弹窗/面板开关 =====
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
  /** 启动动画是否完成 */
  const splashDone = ref(false);
  /** 弹窗懒加载失败提示(defineAsyncComponent onError 写入;flag 为重试所需的面板开关键) */
  const modalLoadError = ref<{ name: string; flag: string } | null>(null);
  /** 启动/切换时的数据加载失败提示(角色/会话/历史加载 catch 写入;下次加载成功时清除) */
  const dataLoadError = ref<string | null>(null);
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
    const cid = useCharacterStore().currentCharacterId;
    const v = cid && cid in renderHtmlOverrides.value
      ? renderHtmlOverrides.value[cid]
      : defaultRenderHtml.value;
    if (renderHtml.value === v) return;
    if (renderHtmlSaveTimer) {
      clearTimeout(renderHtmlSaveTimer);
      renderHtmlSaveTimer = null;
    }
    restoringRenderHtml = true;
    renderHtml.value = v;
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
      const cid = useCharacterStore().currentCharacterId;
      if (cid) {
        // 按角色卡记忆:仅写 localStorage,不改全局默认
        const next = { ...renderHtmlOverrides.value, [cid]: v };
        renderHtmlOverrides.value = next;
        renderHtmlPreferenceStore.write(next);
        confirmedRenderHtml = v;
      } else {
        void useGenSettingsStore().queueSettingsSave({ render_html: v })
          .then(() => { confirmedRenderHtml = v; })
          .catch((error) => {
            console.error('HTML 渲染设置保存失败', error);
            restoringRenderHtml = true;
            renderHtml.value = confirmedRenderHtml;
          });
      }
    }, 300);
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
    splashDone,
    modalLoadError,
    dataLoadError,
    characterUploadRequested,
    callTraceOpen,
    taskResultSummaryOpen,
    renderHtml,
    defaultRenderHtml,
    renderHtmlOverrides,
    syncRenderHtmlToCurrent,
    removeRenderHtmlOverride,
  };
});
