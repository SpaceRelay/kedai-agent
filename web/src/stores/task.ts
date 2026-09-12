// 任务模式 store:顶层模式(roleplay/task)切换与持久化、任务列表/详情、
// 任务事件 SSE 订阅(WP5:取代原 1s REST 轮询与本地合成伪事件)。
// 从 store.ts 按领域拆分。跨 store 引用(genSettings.loadSettings / uiPrefs.agentPanelOpen /
// chat.onSseEvent 事件上报)均在动作运行时解析,setup 阶段不实例化其他 store。
import { defineStore } from 'pinia';
import { ref, watch } from 'vue';
import * as api from '../api';
import { useChatStore } from './chat';
import { useGenSettingsStore } from './genSettings';
import { useUiPrefsStore } from './uiPrefs';

/** 顶层模式持久化键:刷新后停留在上次模式 */
const APP_MODE_KEY = 'kedai.appMode';
/** 新建任务模式持久化键(批次 4 六模式):刷新后保持上次选择 */
const TASK_RUN_MODE_KEY = 'kedai.taskRunMode.v1';
/** 当前选中任务持久化键(实跑问题 4):重启后恢复上次查看的任务详情与调用记录 */
const CURRENT_TASK_KEY = 'kedai.currentTaskId.v1';

/** 任务模式可选值(写持久化前的白名单校验;未知值回退 legacy) */
const TASK_RUN_MODES = new Set(['legacy', 'solo', 'multi', 'plan', 'team', 'custom']);

function readStoredCurrentTaskId(): string | null {
  try {
    const v = localStorage.getItem(CURRENT_TASK_KEY);
    return v && v.trim() ? v : null;
  } catch {
    return null;
  }
}

function persistCurrentTaskId(id: string | null): void {
  try {
    if (id) localStorage.setItem(CURRENT_TASK_KEY, id);
    else localStorage.removeItem(CURRENT_TASK_KEY);
  } catch {
    /* 忽略 */
  }
}

/** SSE 断线重连:退避起步 1s,指数翻倍,封顶 15s */
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 15000;
/** 连接建立成功的判定窗口:失败(非 2xx/网络错误)通常立即触发 onClose,
 *  存活超过该窗口即视为已恢复(复位退避、停兜底轮询、全量刷新补偿) */
const CONNECT_SETTLE_MS = 1500;
/** 兜底轮询间隔:SSE 断开期间的低频保活(仅任务列表 + 当前任务详情) */
const FALLBACK_POLL_MS = 5000;
/** 终态任务状态:事件到达时刷新全局 token 累计 */
const TERMINAL_STATUSES = new Set(['done', 'partial', 'error', 'ended']);

function readStoredAppMode(): 'roleplay' | 'task' {
  try {
    return localStorage.getItem(APP_MODE_KEY) === 'task' ? 'task' : 'roleplay';
  } catch {
    return 'roleplay';
  }
}

function readStoredTaskRunMode(): api.TaskRunMode {
  try {
    const v = localStorage.getItem(TASK_RUN_MODE_KEY) ?? '';
    return (TASK_RUN_MODES.has(v) ? v : 'legacy') as api.TaskRunMode;
  } catch {
    return 'legacy';
  }
}

export const useTaskStore = defineStore('app.task', () => {
  // ===== 状态 =====
  /** 顶层模式:roleplay = 角色扮演,task = 任务工作台;持久化到 localStorage */
  const appMode = ref<'roleplay' | 'task'>(readStoredAppMode());
  /** 新建任务的执行模式(批次 4 六模式;持久化到 localStorage,写时经 watch) */
  const taskRunMode = ref<api.TaskRunMode>(readStoredTaskRunMode());
  const tasks = ref<api.TaskRecord[]>([]);
  const currentTaskId = ref<string | null>(null);
  /** 当前任务详情(含子任务) */
  const currentTask = ref<api.TaskDetail | null>(null);
  /** 当前任务 token 累计(由详情带出的 usage_total;任务模式统计块数据源) */
  const currentTaskUsage = ref<api.TaskUsageTotal | null>(null);
  /** 全部任务 token 累计(任务模式「全局累计」) */
  const globalTaskUsage = ref<api.TaskUsageTotal | null>(null);
  /** 当前任务 LLM 调用记录(批次 3 L3 调用追踪面板;GET /api/tasks/{id}/calls) */
  const taskCalls = ref<api.TaskLlmCall[]>([]);

  /**
   * 流式增量缓冲(批次 R4 任务模式流式输出):key = `${phase}:${step_index ?? ''}`,
   * 值为该调用已攒的正文增量。**delta 事件就地追加进本 Map,绝不走 contentSignature
   * 全量重拉链**(每 token 全量拉取 + stringify 不可行;delta 已由后端攒批节流)。
   * 对齐口径:llm_call 落库事件到达时清对应 key——delta 是暂态展示,权威数据以
   * task_llm_calls 落库行(loadTaskCalls 全量重拉)为准;切任务/删除任务全清。
   * 整表替换(而非原地 set)保持引用语义与既有状态一致,消费方 computed 依赖即刷新。
   */
  const liveBuffers = ref(new Map<string, string>());

  /** delta 缓冲 key(与后端 phase/step_index 透出契约一致;缺 phase 的事件不入缓冲) */
  function liveKey(phase: string, stepIndex?: number): string {
    return `${phase}:${stepIndex ?? ''}`;
  }

  /** 追加一条 delta 增量(仅当前任务;detail/phase 缺失视为旧服务端事件,忽略) */
  function appendLiveDelta(ev: api.TaskEvent): void {
    if (ev.task_id !== currentTaskId.value || !ev.detail || !ev.phase) return;
    const key = liveKey(ev.phase, ev.step_index);
    const next = new Map(liveBuffers.value);
    next.set(key, (next.get(key) ?? '') + ev.detail);
    liveBuffers.value = next;
  }

  /** 清某次调用的流式缓冲(llm_call 落库事件到达 = 该调用已有权威行) */
  function clearLiveDelta(phase: string, stepIndex?: number): void {
    const key = liveKey(phase, stepIndex);
    if (!liveBuffers.value.has(key)) return;
    const next = new Map(liveBuffers.value);
    next.delete(key);
    liveBuffers.value = next;
  }

  /** 清空全部流式缓冲(切任务/删除任务/任务终态让位) */
  function clearAllLiveDeltas(): void {
    if (liveBuffers.value.size === 0) return;
    liveBuffers.value = new Map();
  }

  // ===== SSE 订阅与重连(非响应式内部状态) =====
  /** 当前事件订阅的关闭函数(null = 未订阅) */
  let closeTaskEvents: (() => void) | null = null;
  /** 退避重连定时器 */
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  /** 当前退避延迟(重连成功复位到 RECONNECT_BASE_MS) */
  let reconnectDelay = RECONNECT_BASE_MS;
  /** 连接 settle 判定定时器(见 CONNECT_SETTLE_MS) */
  let settleTimer: ReturnType<typeof setTimeout> | null = null;
  /** 兜底轮询定时器(SSE 断开期间) */
  let taskPollTimer: ReturnType<typeof setInterval> | null = null;

  function persistAppMode(): void {
    try {
      localStorage.setItem(APP_MODE_KEY, appMode.value);
    } catch {
      /* 忽略 */
    }
  }

  /** 持久化新建任务模式(仿 persistAppMode:try/catch 容错,写失败静默) */
  function persistTaskRunMode(): void {
    try {
      localStorage.setItem(TASK_RUN_MODE_KEY, taskRunMode.value);
    } catch {
      /* 忽略 */
    }
  }
  watch(taskRunMode, persistTaskRunMode);

  // ===== 加载(in-flight 合并防事件风暴) =====

  /**
   * 事件驱动刷新的内容签名(非响应式;multi/team 模式 agent_status/usage 事件高频,
   * 签名一致说明数据未变,跳过整体替换——引用不变则依赖该状态的 TaskBoard/AgentPanel/Sidebar
   * 零重渲染,避免「每次 SSE 事件 → 全量 patch + markdown 重跑」的卡顿链)。
   */
  let detailSignature = '';
  let listSignature = '';
  /** 内容签名:JSON 序列化(任务详情/列表规模在数十 KB 内,每次 <1ms);失败回退空串(总是替换,退化为旧行为)。
   *  纪律:必须全量序列化——plan 数组的 name/goal/status/result 全字段都在签名内,plan 模式
   *  逐步回写(status/result)与批准替换(goal/name)才会触发引用替换、面板实时更新;
   *  勿回退为字段子集签名(旧轮询期 taskDetailSig 只取 status,漏 result/goal 变更的形态)。 */
  function contentSignature(v: unknown): string {
    try {
      return JSON.stringify(v) ?? '';
    } catch {
      return '';
    }
  }

  /** 任务列表刷新:in-flight 期间的重复调用合并为结束后补一次(SSE 事件密集时不打爆接口) */
  let tasksInflight = false;
  let tasksPending = false;
  async function loadTasks(): Promise<void> {
    if (tasksInflight) {
      tasksPending = true;
      return;
    }
    tasksInflight = true;
    try {
      do {
        tasksPending = false;
        try {
          const list = await api.listTasks();
          // 内容签名去重:列表未变时保持原引用(侧栏任务列表零重渲染)
          const sig = contentSignature(list);
          if (sig !== listSignature) {
            listSignature = sig;
            tasks.value = list;
          }
        } catch (e) {
          console.error('加载任务列表失败', e);
        }
      } while (tasksPending);
    } finally {
      tasksInflight = false;
    }
  }

  /** 全局 token 累计刷新:同款 in-flight 合并(usage 事件高频) */
  let usageTotalInflight = false;
  let usageTotalPending = false;
  async function loadGlobalTaskUsage(): Promise<void> {
    if (usageTotalInflight) {
      usageTotalPending = true;
      return;
    }
    usageTotalInflight = true;
    try {
      do {
        usageTotalPending = false;
        try {
          globalTaskUsage.value = await api.getTaskUsageTotal();
        } catch (e) {
          console.error('加载任务 token 累计失败', e);
        }
      } while (usageTotalPending);
    } finally {
      usageTotalInflight = false;
    }
  }

  /** 任务详情刷新:按 task_id 的 in-flight 合并——同 id 请求进行中时,事件驱动的
   *  重复刷新只记录一次「待重放」意图,请求结束后补发一次,不叠加并发请求;
   *  响应与现状签名一致时跳过赋值(数据未变,引用保持,面板零重渲染) */
  const detailInflight = new Map<string, boolean>();
  async function loadTaskDetail(id: string): Promise<void> {
    if (detailInflight.has(id)) {
      detailInflight.set(id, true);
      return;
    }
    detailInflight.set(id, false);
    try {
      do {
        detailInflight.set(id, false);
        try {
          const detail = await api.getTask(id);
          const sig = contentSignature(detail);
          // id 检查兜底:切任务后旧签名残留时,即便内容碰巧相同也必须替换(现状是别的任务/null)
          if (sig !== detailSignature || currentTask.value?.task.id !== id) {
            detailSignature = sig;
            currentTask.value = detail;
            currentTaskUsage.value = detail.usage_total ?? null;
          }
        } catch (e) {
          console.error('加载任务详情失败', e);
          break;
        }
      } while (detailInflight.get(id));
    } finally {
      detailInflight.delete(id);
    }
  }

  /** 调用追踪记录刷新:失败静默(保持旧值),「调用情况」tab 激活时由 llm_call 事件驱动;
   *  同款内容签名去重(multi/team 并行调用落库事件密集,记录未变时保持引用,面板零重渲染) */
  let callsSignature = '';
  async function loadTaskCalls(taskId: string): Promise<void> {
    try {
      const calls = await api.getTaskCalls(taskId);
      const sig = contentSignature(calls);
      if (sig !== callsSignature) {
        callsSignature = sig;
        taskCalls.value = calls;
      }
    } catch {
      // 静默:调用追踪是辅助面板,失败不影响主流程
    }
  }

  async function selectTask(id: string): Promise<void> {
    currentTaskId.value = id;
    persistCurrentTaskId(id);
    // 切换任务时清空旧任务的调用记录(「调用情况」tab 由 watch/事件重新加载)
    // 与流式缓冲(批次 R4:旧任务的 delta 不带入新任务);
    // 同步重置内容签名,防止新任务首屏记录与旧任务签名碰巧相同而被去重跳过
    taskCalls.value = [];
    callsSignature = contentSignature([]);
    clearAllLiveDeltas();
    await loadTaskDetail(id);
  }

  /** 清空当前任务选择(任务被删除/列表已无该 id 时;同步清持久化) */
  function clearSelectedTask(): void {
    currentTaskId.value = null;
    currentTask.value = null;
    currentTaskUsage.value = null;
    taskCalls.value = [];
    clearAllLiveDeltas();
    persistCurrentTaskId(null);
  }

  /**
   * 恢复上次选中的任务(实跑问题 4):重启后 currentTaskId 若仍是内存态,
   * Agent 面板与调用记录将无入口加载,用户感知为「记录丢失」。
   * 在任务列表加载完成后调用:仅当持久化的 id 仍在列表中才恢复,
   * 否则清空(任务已被删除时不留下悬空选择)。
   */
  async function restoreSelectedTask(): Promise<void> {
    if (currentTaskId.value) return; // 已有选择(会话内切换路径)不覆盖
    const saved = readStoredCurrentTaskId();
    if (!saved) return;
    if (!tasks.value.some((t) => t.id === saved)) {
      persistCurrentTaskId(null);
      return;
    }
    await selectTask(saved);
    await loadTaskCalls(saved);
  }

  /** 切换顶层模式;进入任务模式时加载任务并启动 SSE 订阅,退出时停止订阅与兜底轮询 */
  function setAppMode(mode: 'roleplay' | 'task'): void {
    appMode.value = mode;
    if (mode === 'task') {
      void loadTasks().then(() => restoreSelectedTask());
      void loadGlobalTaskUsage();
      useUiPrefsStore().agentPanelOpen = false;
      startTaskEvents();
    } else {
      stopTaskEvents();
    }
    void useGenSettingsStore().loadSettings();
    persistAppMode();
  }

  // ===== 任务事件 SSE 订阅(WP5) =====

  /** 事件分发:按 kind 驱动局部刷新;所有事件原样透传事件监控面板(DevTools) */
  function onTaskEvent(ev: api.TaskEvent): void {
    useChatStore().onSseEvent(ev);
    switch (ev.kind) {
      case 'created':
        void loadTasks();
        break;
      case 'status':
      case 'plan':
      case 'subtask':
        // 侧栏状态跳动 + 当前任务详情刷新;终态时补全局 token 累计
        void loadTasks();
        if (ev.task_id === currentTaskId.value) void loadTaskDetail(ev.task_id);
        if (ev.kind === 'status' && ev.status && TERMINAL_STATUSES.has(ev.status)) {
          void loadGlobalTaskUsage();
        }
        break;
      case 'usage':
        // 高频事件:详情刷新有 in-flight 合并兜底,不会叠加并发请求
        if (ev.task_id === currentTaskId.value) void loadTaskDetail(ev.task_id);
        void loadGlobalTaskUsage();
        break;
      case 'llm_call':
        // LLM 调用落库:仅当前任务且「调用情况」tab 被记忆为激活时才拉取(避免无谓请求;
        // 面板合并后 callTraceOpen 语义为合并面板内部 tab 记忆,判断逻辑不变)
        if (ev.task_id === currentTaskId.value && useUiPrefsStore().callTraceOpen) {
          void loadTaskCalls(ev.task_id);
        }
        // 批次 R4 流式缓冲对齐:该调用的暂态 delta 已由落库行取代,清对应缓冲
        // (旧服务端事件无 phase 字段 → 不清,残留缓冲随切任务/终态清理)
        if (ev.task_id === currentTaskId.value && ev.phase) {
          clearLiveDelta(ev.phase, ev.step_index);
        }
        // finish_reason(可观测性问题①截断标记)透传通道:事件原样进事件监控面板
        // (onSseEvent 已透传 ev.finish_reason),面板数据源由 loadTaskCalls 全量行携带
        // (TaskLlmCall.finish_reason);'length' 即 max_tokens 截断,此处不做增量合并
        break;
      case 'delta':
        // 批次 R4 流式输出:攒批后的正文增量就地追加进 liveBuffers,**绕开**
        // contentSignature 全量重拉链(每 token 全量拉取 + stringify 不可行);
        // 缓冲由 TaskBoard「正在生成」块与 CallTracePanel「进行中」伪行消费
        appendLiveDelta(ev);
        break;
      case 'approval_required':
        // plan 模式计划已生成待批准:刷新列表(状态徽标)与当前任务详情(批准区)
        void loadTasks();
        if (ev.task_id === currentTaskId.value) void loadTaskDetail(ev.task_id);
        break;
      case 'agent_status':
        // 主/子 agent 状态迁移(detail 为简述,前端不解析):当前任务刷新详情;
        // 详情刷新有按 id 的 in-flight 合并兜底,事件密集时不叠加并发请求
        if (ev.task_id === currentTaskId.value) void loadTaskDetail(ev.task_id);
        break;
      case 'deleted':
        void loadTasks();
        if (currentTaskId.value === ev.task_id) {
          currentTaskId.value = null;
          currentTask.value = null;
          currentTaskUsage.value = null;
          taskCalls.value = [];
          callsSignature = contentSignature([]);
          clearAllLiveDeltas(); // 批次 R4:任务删除,其流式缓冲一并失效
        }
        break;
      default:
        // kind 缺失(向后兼容):仅透传事件面板,不触发刷新
        break;
    }
  }

  /** 建立一次事件订阅;settle 窗口内未断开视为连接成功(复位退避 + 停兜底轮询 + 全量刷新) */
  function connectTaskEvents(): void {
    closeTaskEvents = api.streamTaskEvents(onTaskEvent, onTaskEventsClosed);
    settleTimer = setTimeout(() => {
      settleTimer = null;
      reconnectDelay = RECONNECT_BASE_MS;
      stopTaskPolling();
      // 补偿断开期间可能遗漏的变更
      void loadTasks();
      if (currentTaskId.value) void loadTaskDetail(currentTaskId.value);
      void loadGlobalTaskUsage();
    }, CONNECT_SETTLE_MS);
  }

  /** SSE 关闭回调(对端断开/网络错误):启动兜底轮询,按指数退避安排重连 */
  function onTaskEventsClosed(_err?: Error): void {
    closeTaskEvents = null;
    if (settleTimer) {
      clearTimeout(settleTimer);
      settleTimer = null;
    }
    if (appMode.value !== 'task') return; // 已退出任务模式,不再重连
    startTaskPolling();
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      connectTaskEvents();
    }, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, RECONNECT_MAX_MS);
  }

  /** 启动任务事件订阅(幂等:已订阅/重连中不重复建立) */
  function startTaskEvents(): void {
    if (closeTaskEvents || reconnectTimer) return;
    reconnectDelay = RECONNECT_BASE_MS;
    connectTaskEvents();
  }

  /** 停止订阅与一切兜底/重连定时器(退出任务模式时调用) */
  function stopTaskEvents(): void {
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    if (settleTimer) {
      clearTimeout(settleTimer);
      settleTimer = null;
    }
    stopTaskPolling();
    if (closeTaskEvents) {
      closeTaskEvents();
      closeTaskEvents = null;
    }
  }

  // ===== 兜底轮询(SSE 断开期间的 5s 低频保活:仅列表 + 当前任务详情) =====
  function startTaskPolling(): void {
    if (taskPollTimer) return;
    taskPollTimer = setInterval(() => {
      void loadTasks();
      if (currentTaskId.value) void loadTaskDetail(currentTaskId.value);
    }, FALLBACK_POLL_MS);
  }

  function stopTaskPolling(): void {
    if (taskPollTimer) {
      clearInterval(taskPollTimer);
      taskPollTimer = null;
    }
  }

  // ===== CRUD(本地乐观更新保留;状态变化由 SSE 事件驱动后续刷新,不再合成伪事件) =====

  async function createTask(title: string, characterId?: string): Promise<api.TaskRecord> {
    const task = await api.createTask(title, characterId, taskRunMode.value);
    tasks.value = [task, ...tasks.value];
    listSignature = contentSignature(tasks.value); // 本地乐观改写后同步签名(下次事件刷新同内容时不再替换)
    await selectTask(task.id);
    return task;
  }

  async function runTask(id: string): Promise<void> {
    await api.runTask(id);
    // 任务开始执行时自动展开一次 Agent 面板(进度可见性);用户若已主动收起则不打扰
    useUiPrefsStore().autoOpenAgentPanel();
    await loadTaskDetail(id);
  }

  async function stopTask(id: string): Promise<void> {
    await api.stopTask(id);
    await loadTaskDetail(id);
  }

  /** 批准计划(plan 模式):plan 可选(修改后批准);成功后刷新该任务详情 */
  async function approveTask(id: string, plan?: api.TaskStep[]): Promise<void> {
    await api.approveTask(id, plan);
    // 批准后任务进入执行:同样自动展开一次
    useUiPrefsStore().autoOpenAgentPanel();
    await loadTaskDetail(id);
  }

  /** 终态追加指令(批次 R2a;R2b+ 扩 mode):仅终态可调用(后端门禁 409);
   *  mode=append(缺省)追加进 result,mode=replace 整体替换(段标「修订 N」);
   *  成功后刷新详情(messages 指令历史 + result 随详情带出) */
  async function followupTask(
    id: string,
    content: string,
    mode: 'append' | 'replace' = 'append',
  ): Promise<void> {
    await api.followupTask(id, content, mode);
    await loadTaskDetail(id);
  }

  /** 批准环节规划对话(批次 R2b):仅 planned 态;同步等待修订完成,
   *  成功后刷新详情(修订计划 + plan_chat 对话记录随详情带出) */
  async function planChatTask(id: string, message: string): Promise<void> {
    await api.planChatTask(id, message);
    await loadTaskDetail(id);
  }

  async function deleteTask(id: string): Promise<void> {
    await api.deleteTask(id);
    tasks.value = tasks.value.filter((t) => t.id !== id);
    listSignature = contentSignature(tasks.value); // 同步签名(同上)
    if (currentTaskId.value === id) {
      currentTaskId.value = null;
      currentTask.value = null;
      currentTaskUsage.value = null;
      taskCalls.value = [];
      callsSignature = contentSignature([]);
      clearAllLiveDeltas(); // 批次 R4:任务删除,其流式缓冲一并失效
      persistCurrentTaskId(null); // 实跑问题 4:删除当前任务时同步清持久化选择
    }
  }

  return {
    appMode,
    taskRunMode,
    tasks,
    currentTaskId,
    currentTask,
    currentTaskUsage,
    globalTaskUsage,
    taskCalls,
    liveBuffers,
    setAppMode,
    loadTasks,
    loadGlobalTaskUsage,
    loadTaskCalls,
    createTask,
    selectTask,
    clearSelectedTask,
    restoreSelectedTask,
    loadTaskDetail,
    runTask,
    stopTask,
    approveTask,
    followupTask,
    planChatTask,
    deleteTask,
    startTaskEvents,
    stopTaskEvents,
    startTaskPolling,
    stopTaskPolling,
  };
});
