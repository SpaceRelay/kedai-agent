import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useTaskStore } from './task';
import { useUiPrefsStore } from './uiPrefs';
import type { TaskDetail, TaskEvent, TaskLlmCall, TaskRecord, TaskStatus, TaskStep, TaskUsageTotal } from '../api';

// WP5 重写:任务事件 SSE 订阅驱动刷新(取代旧 1s 轮询)。
// 覆盖:事件分发(created/status/plan/subtask/usage/deleted)、终态补全局累计、
// 断线兜底轮询 + 指数退避重连 + settle 后全量补偿、退出 task 模式不再重连。

// node 环境无 localStorage(appMode 持久化),补内存桩(同 storeFacade.test.ts)
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

/** mock 控制句柄(vi.hoisted 保证 vi.mock 工厂内可安全引用) */
const h = vi.hoisted(() => ({
  subscribeCalls: 0,
  closeCalls: 0,
  listFetchCount: 0,
  detailFetchCount: 0,
  usageFetchCount: 0,
  /** getTaskCalls 调用次数(llm_call 事件驱动刷新断言用) */
  callsFetchCount: 0,
  /** true 时 getTaskCalls 抛错(验证 loadTaskCalls 失败静默) */
  callsFail: false,
  /** true 时新订阅在微任务中立即 onClose(模拟连接被拒,用于退避重连场景) */
  autoFailSubscribe: false,
  backendStatus: 'pending' as TaskStatus,
  /** 后端当前计划步骤与最终结果(plan 模式生命周期断言用;默认空计划/空结果) */
  backendPlan: [] as TaskStep[],
  backendResult: '',
  emitEvent: null as null | ((ev: TaskEvent) => void),
  emitClose: null as null | ((err?: Error) => void),
  /** createTask 收到的 task_mode 参数序列(批次 4 透传断言用) */
  createTaskModes: [] as (string | undefined)[],
  /** approveTask 收到的 (id, plan) 参数序列(批次 4 批准断言用) */
  approveCalls: [] as Array<{ id: string; plan?: TaskStep[] }>,
  /** followupTask 收到的 (id, content, mode) 参数序列(批次 R2a;R2b+ 扩 mode) */
  followupCalls: [] as Array<{ id: string; content: string; mode?: string }>,
  /** planChatTask 收到的 (id, message) 参数序列(批次 R2b 规划对话断言用) */
  planChatCalls: [] as Array<{ id: string; message: string }>,
}));

vi.mock('../api', async (importOriginal) => {
  const orig = await importOriginal<typeof import('../api')>();
  return {
    ...orig,
    streamTaskEvents: vi.fn((onEvent: (ev: TaskEvent) => void, onClose: (err?: Error) => void) => {
      h.subscribeCalls += 1;
      h.emitEvent = onEvent;
      h.emitClose = onClose;
      if (h.autoFailSubscribe) queueMicrotask(() => onClose(new Error('连接被拒')));
      return () => {
        h.closeCalls += 1;
      };
    }),
    listTasks: vi.fn(async (): Promise<TaskRecord[]> => {
      h.listFetchCount += 1;
      return [makeTask(h.backendStatus)];
    }),
    getTask: vi.fn(async (): Promise<TaskDetail> => {
      h.detailFetchCount += 1;
      return makeDetail(h.backendStatus);
    }),
    getTaskUsageTotal: vi.fn(async (): Promise<TaskUsageTotal> => {
      h.usageFetchCount += 1;
      return { prompt_tokens: 0, completion_tokens: 0, reasoning_tokens: 0 };
    }),
    getTaskCalls: vi.fn(async (taskId: string): Promise<TaskLlmCall[]> => {
      h.callsFetchCount += 1;
      if (h.callsFail) throw new Error('调用记录加载失败');
      return [makeCall(taskId)];
    }),
    runTask: vi.fn(async () => {}),
    stopTask: vi.fn(async () => {}),
    deleteTask: vi.fn(async () => {}),
    createTask: vi.fn(async (title: string, _characterId?: string, taskMode?: string) => {
      h.createTaskModes.push(taskMode);
      return makeTask('pending', title);
    }),
    approveTask: vi.fn(async (id: string, plan?: TaskStep[]) => {
      h.approveCalls.push({ id, plan });
      return { ok: true };
    }),
    followupTask: vi.fn(async (id: string, content: string, mode?: string) => {
      h.followupCalls.push({ id, content, mode });
      return { ok: true };
    }),
    planChatTask: vi.fn(async (id: string, message: string) => {
      h.planChatCalls.push({ id, message });
      return { ok: true, plan: [] };
    }),
    // setAppMode 会连锁 genSettings.loadSettings → getSettings;mock 掉避免真实 fetch
    getSettings: vi.fn(async () => ({})),
    saveSettings: vi.fn(async () => ({ settings: {} })),
  };
});

function makeTask(status: TaskStatus, title = '写一首关于秋天的短诗'): TaskRecord {
  return {
    id: 't1',
    title,
    status,
    plan: h.backendPlan,
    result: h.backendResult,
    error: '',
    created_at: '2026-08-26T00:00:00.000Z',
    updated_at: '2026-08-26T00:00:00.000Z',
  };
}

function makeDetail(status: TaskStatus): TaskDetail {
  return {
    task: makeTask(status),
    subtasks: [],
    usage_total: { prompt_tokens: 0, completion_tokens: 0, reasoning_tokens: 0 },
  };
}

function makeCall(taskId: string): TaskLlmCall {
  return {
    id: 'c1',
    task_id: taskId,
    phase: 'step',
    step_index: 2,
    model: 'deepseek-chat',
    prompt_summary: '提示词摘要',
    response_summary: '响应摘要',
    prompt_tokens: 10,
    completion_tokens: 20,
    reasoning_tokens: 0,
    elapsed_ms: 123,
    status: 'ok',
    created_at: '2026-08-28T00:00:00.000Z',
  };
}

/** 事件驱动的异步刷新(in-flight 合并)需要让微任务跑完 */
async function flush(): Promise<void> {
  await new Promise((r) => setTimeout(r, 0));
  await new Promise((r) => setTimeout(r, 0));
}

describe('任务事件 SSE 订阅(WP5)', () => {
  beforeEach(() => {
    memStorage.clear();
    setActivePinia(createPinia());
    h.subscribeCalls = 0;
    h.closeCalls = 0;
    h.listFetchCount = 0;
    h.detailFetchCount = 0;
    h.usageFetchCount = 0;
    h.callsFetchCount = 0;
    h.callsFail = false;
    h.autoFailSubscribe = false;
    h.backendStatus = 'pending';
    h.emitEvent = null;
    h.emitClose = null;
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('status 事件驱动当前任务详情与列表刷新;终态补全局 token 累计', async () => {
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    expect(h.subscribeCalls).toBe(1);
    await store.selectTask('t1');
    const baseList = h.listFetchCount;
    const baseDetail = h.detailFetchCount;

    h.backendStatus = 'running';
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'status', status: 'running' });
    await flush();
    expect(h.listFetchCount).toBe(baseList + 1);
    expect(h.detailFetchCount).toBe(baseDetail + 1);
    expect(store.currentTask?.task.status).toBe('running');

    // 终态:额外刷新全局累计
    const baseUsage = h.usageFetchCount;
    h.backendStatus = 'done';
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'status', status: 'done' });
    await flush();
    expect(h.usageFetchCount).toBe(baseUsage + 1);
  });

  it('非当前任务的事件只刷新列表,不拉详情;kind 缺失仅透传不刷新', async () => {
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');
    const baseList = h.listFetchCount;
    const baseDetail = h.detailFetchCount;

    h.emitEvent!({ type: 'task', task_id: 'other', kind: 'status', status: 'running' });
    await flush();
    expect(h.listFetchCount).toBe(baseList + 1);
    expect(h.detailFetchCount, '非当前任务不应拉详情').toBe(baseDetail);

    h.emitEvent!({ type: 'task', task_id: 't1', detail: '旧服务端事件无 kind' });
    await flush();
    expect(h.listFetchCount, 'kind 缺失不触发刷新').toBe(baseList + 1);
  });

  it('created 刷新列表;usage 刷新全局累计与当前详情;deleted 清空当前选中', async () => {
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');
    const baseList = h.listFetchCount;
    const baseUsage = h.usageFetchCount;

    h.emitEvent!({ type: 'task', task_id: 't2', kind: 'created', title: '新任务' });
    await flush();
    expect(h.listFetchCount).toBe(baseList + 1);

    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'usage' });
    await flush();
    expect(h.usageFetchCount).toBe(baseUsage + 1);

    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'deleted' });
    await flush();
    expect(store.currentTaskId).toBeNull();
    expect(store.currentTask).toBeNull();
  });

  it('连接持续失败时按指数退避重连(1→2→4s),断开期间 5s 兜底轮询生效', async () => {
    vi.useFakeTimers();
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await vi.advanceTimersByTimeAsync(0);

    // 首次断线:兜底轮询启动,1s 后重连;此后订阅即被拒(autoFail)
    h.autoFailSubscribe = true;
    h.emitClose!(new Error('network down'));
    expect(h.subscribeCalls, '断线瞬间不重连,等 1s 退避').toBe(1);

    await vi.advanceTimersByTimeAsync(1000); // t=1s:重连→被拒→退避 2s
    expect(h.subscribeCalls).toBe(2);
    await vi.advanceTimersByTimeAsync(1500); // t=2.5s:2s 退避未到
    expect(h.subscribeCalls).toBe(2);

    const baseList = h.listFetchCount;
    await vi.advanceTimersByTimeAsync(2500); // t=5s:兜底轮询 tick;t=3s 已第二次重连失败(退避 4s)
    expect(h.listFetchCount, '兜底轮询 5s tick 应刷新列表').toBe(baseList + 1);
    expect(h.subscribeCalls).toBe(3);

    await vi.advanceTimersByTimeAsync(3900); // t=8.9s:4s 退避(t=7s)应已重连
    expect(h.subscribeCalls).toBe(4);
  });

  it('断线重连成功后(settle 窗口未再断)停兜底轮询并全量刷新补偿', async () => {
    vi.useFakeTimers();
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await vi.advanceTimersByTimeAsync(0);
    await store.selectTask('t1');

    h.emitClose!(new Error('network down'));
    await vi.advanceTimersByTimeAsync(1000); // t=1s:重连(订阅挂起,不再失败)
    expect(h.subscribeCalls).toBe(2);

    const baseList = h.listFetchCount;
    const baseDetail = h.detailFetchCount;
    const baseUsage = h.usageFetchCount;
    await vi.advanceTimersByTimeAsync(1600); // t=2.6s:settle(1500ms)触发全量补偿
    expect(h.listFetchCount, 'settle 后应补刷列表').toBe(baseList + 1);
    expect(h.detailFetchCount, 'settle 后应补刷当前任务详情').toBe(baseDetail + 1);
    expect(h.usageFetchCount, 'settle 后应补刷全局累计').toBe(baseUsage + 1);

    const listAfterSettle = h.listFetchCount;
    await vi.advanceTimersByTimeAsync(20000);
    expect(h.listFetchCount, '兜底轮询已停,不再有周期请求').toBe(listAfterSettle);
    expect(h.subscribeCalls, '连接稳定,不再重连').toBe(2);
  });

  it('退出 task 模式后断线不再重连,订阅与定时器全部清理', async () => {
    vi.useFakeTimers();
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await vi.advanceTimersByTimeAsync(0);

    store.setAppMode('roleplay');
    expect(h.closeCalls, '退出模式应主动关闭订阅').toBe(1);

    // 退出后才收到断线回调(竞态):不得重连
    h.emitClose!();
    await vi.advanceTimersByTimeAsync(30000);
    expect(h.subscribeCalls, '退出 task 模式后不得重连').toBe(1);
  });

  it('断线可观测:每次断线输出一条 [kedai] warn,内含下次退避毫秒数与兜底轮询状态', async () => {
    vi.useFakeTimers();
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await vi.advanceTimersByTimeAsync(0);

    // 连续失败三次:退避 1s → 2s → 4s(warn 里记录的正是 setTimeout 实际使用的值)
    h.autoFailSubscribe = true;
    h.emitClose!(new Error('network down'));
    await vi.advanceTimersByTimeAsync(1000); // t=1s:1s 退避后重连 → 被拒
    await vi.advanceTimersByTimeAsync(2000); // t=3s:2s 退避后重连 → 被拒

    const msgs = warnSpy.mock.calls
      .map((c) => String(c[0]))
      .filter((m) => m.startsWith('[kedai]'));
    expect(msgs, '每次断线一条 warn').toHaveLength(3);
    expect(msgs[0]).toContain('1000ms');
    expect(msgs[1], '退避翻倍(策略未改,仅观测)').toContain('2000ms');
    expect(msgs[2]).toContain('4000ms');
    expect(
      msgs.every((m) => m.includes('兜底轮询')),
      'warn 应说明兜底轮询已接管',
    ).toBe(true);
  });

  it('恢复可观测:settle 窗口内未再断时输出 [kedai] info;首次连接不刷该行', async () => {
    vi.useFakeTimers();
    const infoSpy = vi.spyOn(console, 'info').mockImplementation(() => {});
    const store = useTaskStore();
    store.appMode = 'task';

    // 首次连接成功也走 settle,但 reconnectDelay 仍为起步值 → 不报「恢复」(避免噪声)
    store.startTaskEvents();
    await vi.advanceTimersByTimeAsync(1600);
    expect(
      infoSpy.mock.calls.filter((c) => String(c[0]).includes('[kedai]')),
      '首次连接不产生恢复信号',
    ).toHaveLength(0);

    // 断线后重连并在 settle 内保持稳定 → 报一次恢复
    h.emitClose!(new Error('network down'));
    await vi.advanceTimersByTimeAsync(1000); // t=2.6s:1s 退避后重连(不再失败)
    expect(
      infoSpy.mock.calls.filter((c) => String(c[0]).includes('[kedai]')),
      'settle 未到,不应提前报恢复',
    ).toHaveLength(0);

    await vi.advanceTimersByTimeAsync(1500); // t=4.1s:settle(1500ms)触发
    const infos = infoSpy.mock.calls
      .map((c) => String(c[0]))
      .filter((m) => m.startsWith('[kedai]'));
    expect(infos).toHaveLength(1);
    expect(infos[0]).toContain('已恢复');
  });
});

describe('llm_call 事件与调用追踪(批次 3 L3)', () => {
  beforeEach(() => {
    memStorage.clear();
    setActivePinia(createPinia());
    h.subscribeCalls = 0;
    h.closeCalls = 0;
    h.listFetchCount = 0;
    h.detailFetchCount = 0;
    h.usageFetchCount = 0;
    h.callsFetchCount = 0;
    h.callsFail = false;
    h.autoFailSubscribe = false;
    h.backendStatus = 'pending';
    h.emitEvent = null;
    h.emitClose = null;
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('当前任务且调用面板打开时,llm_call 事件刷新调用记录', async () => {
    const store = useTaskStore();
    const uiPrefs = useUiPrefsStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');
    uiPrefs.callTraceOpen = true;

    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'llm_call', detail: 'step #2 · deepseek-chat · 1234 tokens' });
    await flush();
    expect(h.callsFetchCount).toBe(1);
    expect(store.taskCalls).toHaveLength(1);
    expect(store.taskCalls[0].task_id).toBe('t1');
  });

  it('面板关闭或事件不属于当前任务时不拉取调用记录', async () => {
    const store = useTaskStore();
    const uiPrefs = useUiPrefsStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');

    // 面板关闭(默认):当前任务的 llm_call 也不拉取
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'llm_call' });
    await flush();
    expect(h.callsFetchCount, '面板关闭不应拉取').toBe(0);

    // 面板打开但事件属于其他任务:不拉取
    uiPrefs.callTraceOpen = true;
    h.emitEvent!({ type: 'task', task_id: 'other', kind: 'llm_call' });
    await flush();
    expect(h.callsFetchCount, '非当前任务不应拉取').toBe(0);
  });

  it('loadTaskCalls 失败静默(保持旧值);selectTask/deleteTask 清空 taskCalls', async () => {
    const store = useTaskStore();
    const uiPrefs = useUiPrefsStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');
    uiPrefs.callTraceOpen = true;

    // 先成功拉一次,种下旧值
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'llm_call' });
    await flush();
    expect(store.taskCalls).toHaveLength(1);

    // 失败时静默:旧值保留
    h.callsFail = true;
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'llm_call' });
    await flush();
    expect(h.callsFetchCount).toBe(2);
    expect(store.taskCalls, '失败应保持旧值').toHaveLength(1);

    // 切换任务清空;删除当前任务清空
    await store.selectTask('t2');
    expect(store.taskCalls).toHaveLength(0);

    store.taskCalls = [makeCall('t2')];
    await store.deleteTask('t2');
    expect(store.taskCalls, '删除当前任务应清空调用记录').toHaveLength(0);
  });
});

describe('事件风暴签名去重(multi/team 卡顿修复)', () => {
  beforeEach(() => {
    memStorage.clear();
    setActivePinia(createPinia());
    h.subscribeCalls = 0;
    h.closeCalls = 0;
    h.listFetchCount = 0;
    h.detailFetchCount = 0;
    h.usageFetchCount = 0;
    h.callsFetchCount = 0;
    h.callsFail = false;
    h.autoFailSubscribe = false;
    h.backendStatus = 'pending';
    h.emitEvent = null;
    h.emitClose = null;
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('详情内容未变时,agent_status/usage 重复刷新保持 currentTask 引用(零重渲染)', async () => {
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');
    const refBefore = store.currentTask;
    expect(refBefore).not.toBeNull();
    const baseDetail = h.detailFetchCount;

    // 连续高频事件(后端数据未变):请求仍发(in-flight 合并),引用必须保持
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'agent_status', detail: '子Agent-1 执行中' });
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'agent_status', detail: '子Agent-2 执行中' });
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'usage' });
    await flush();
    expect(h.detailFetchCount).toBeGreaterThan(baseDetail);
    expect(store.currentTask, '数据未变时详情引用应保持,下游面板零重渲染').toBe(refBefore);
  });

  it('详情内容变化时正常替换引用(状态推进可见)', async () => {
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');
    const refBefore = store.currentTask;

    h.backendStatus = 'running';
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'agent_status', detail: '子Agent-1 执行中' });
    await flush();
    expect(store.currentTask, '数据变化必须替换引用').not.toBe(refBefore);
    expect(store.currentTask?.task.status).toBe('running');
  });

  it('任务列表内容未变时,status/plan 事件保持 tasks 引用', async () => {
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');

    // 先种一次列表内容(空 → 有内容必须替换)
    h.emitEvent!({ type: 'task', task_id: 't2', kind: 'created', title: '新任务' });
    await flush();
    expect(store.tasks).toHaveLength(1);
    const listBefore = store.tasks;
    const baseList = h.listFetchCount;

    // 同内容的重复刷新:请求仍发,引用保持
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'status', status: 'pending' });
    h.emitEvent!({ type: 'task', task_id: 't2', kind: 'created', title: '新任务' });
    await flush();
    expect(h.listFetchCount).toBeGreaterThan(baseList);
    expect(store.tasks, '列表未变时引用应保持(侧栏零重渲染)').toBe(listBefore);

    // 列表内容变化(后端状态推进)时替换
    h.backendStatus = 'running';
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'status', status: 'running' });
    await flush();
    expect(store.tasks).not.toBe(listBefore);
  });
});

describe('批次 4 六模式:taskRunMode / approveTask / 新事件分支', () => {
  beforeEach(() => {
    memStorage.clear();
    setActivePinia(createPinia());
    h.subscribeCalls = 0;
    h.closeCalls = 0;
    h.listFetchCount = 0;
    h.detailFetchCount = 0;
    h.usageFetchCount = 0;
    h.callsFetchCount = 0;
    h.callsFail = false;
    h.autoFailSubscribe = false;
    h.backendStatus = 'pending';
    h.emitEvent = null;
    h.emitClose = null;
    h.createTaskModes = [];
    h.approveCalls = [];
    h.followupCalls = [];
    h.planChatCalls = [];
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('taskRunMode 缺省 legacy;写入持久化到 kedai.taskRunMode.v1;createTask 透传 task_mode', async () => {
    const store = useTaskStore();
    expect(store.taskRunMode).toBe('legacy');

    store.taskRunMode = 'plan';
    await flush(); // watch 持久化
    expect(memStorage.get('kedai.taskRunMode.v1')).toBe('plan');

    await store.createTask('目标');
    expect(h.createTaskModes, 'createTask 应透传当前 taskRunMode').toEqual(['plan']);
    expect(store.currentTaskId, '创建后应选中新任务').toBe('t1');
  });

  it('localStorage 中的模式在 store 初始化时恢复;未知值回退 legacy', async () => {
    memStorage.set('kedai.taskRunMode.v1', 'team');
    setActivePinia(createPinia());
    expect(useTaskStore().taskRunMode).toBe('team');

    memStorage.set('kedai.taskRunMode.v1', 'bogus');
    setActivePinia(createPinia());
    expect(useTaskStore().taskRunMode, '未知模式值应回退 legacy').toBe('legacy');
  });

  it('approveTask 透传 (id, plan) 并在成功后刷新任务详情;不给 plan 时 plan 为 undefined', async () => {
    const store = useTaskStore();
    await store.selectTask('t1');
    const baseDetail = h.detailFetchCount;

    const plan: TaskStep[] = [{ name: '步骤一', goal: '目标一', status: 'pending', result: '' }];
    await store.approveTask('t1', plan);
    expect(h.approveCalls[0]).toEqual({ id: 't1', plan });
    expect(h.detailFetchCount, '批准后应刷新详情').toBe(baseDetail + 1);

    await store.approveTask('t1');
    expect(h.approveCalls[1], '按原计划批准不带 plan').toEqual({ id: 't1', plan: undefined });
  });

  it('followupTask 透传 (id, content) 并在成功后刷新任务详情(批次 R2a)', async () => {
    const store = useTaskStore();
    await store.selectTask('t1');
    const baseDetail = h.detailFetchCount;

    // 缺省 mode=append
    await store.followupTask('t1', '再补充一点秋色');
    expect(h.followupCalls).toEqual([{ id: 't1', content: '再补充一点秋色', mode: 'append' }]);
    expect(h.detailFetchCount, '追加成功后应刷新详情(messages/result 随详情带出)').toBe(baseDetail + 1);

    // replace 模式透传(2026-09-10 F5)
    await store.followupTask('t1', '把全文压缩到 200 字', 'replace');
    expect(h.followupCalls.at(-1)).toEqual({
      id: 't1',
      content: '把全文压缩到 200 字',
      mode: 'replace',
    });
  });

  it('planChatTask 透传 (id, message) 并在成功后刷新任务详情(批次 R2b)', async () => {
    const store = useTaskStore();
    await store.selectTask('t1');
    const baseDetail = h.detailFetchCount;

    await store.planChatTask('t1', '把第二步换成先做竞品调研');
    expect(h.planChatCalls).toEqual([{ id: 't1', message: '把第二步换成先做竞品调研' }]);
    expect(h.detailFetchCount, '修订成功后应刷新详情(新计划 + 对话记录随详情带出)').toBe(baseDetail + 1);
  });

  it('approval_required 事件刷新列表与当前任务详情(plan 模式计划待批准)', async () => {
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');
    const baseList = h.listFetchCount;
    const baseDetail = h.detailFetchCount;

    h.backendStatus = 'planned';
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'approval_required', detail: '计划已生成,待批准' });
    await flush();
    expect(h.listFetchCount).toBe(baseList + 1);
    expect(h.detailFetchCount).toBe(baseDetail + 1);
    expect(store.currentTask?.task.status).toBe('planned');
  });

  it('agent_status 事件仅刷新当前任务详情;非当前任务不拉详情', async () => {
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');
    const baseDetail = h.detailFetchCount;
    const baseList = h.listFetchCount;

    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'agent_status', detail: '子Agent-1 执行中' });
    await flush();
    expect(h.detailFetchCount).toBe(baseDetail + 1);
    expect(h.listFetchCount, 'agent_status 不刷新列表').toBe(baseList);

    h.emitEvent!({ type: 'task', task_id: 'other', kind: 'agent_status', detail: '子Agent-2 完成' });
    await flush();
    expect(h.detailFetchCount, '非当前任务不拉详情').toBe(baseDetail + 1);
  });
});

describe('plan 模式生命周期:planned → 批准 → done(签名去重不吞 plan 更新)', () => {
  beforeEach(() => {
    memStorage.clear();
    setActivePinia(createPinia());
    h.subscribeCalls = 0;
    h.closeCalls = 0;
    h.listFetchCount = 0;
    h.detailFetchCount = 0;
    h.usageFetchCount = 0;
    h.callsFetchCount = 0;
    h.callsFail = false;
    h.autoFailSubscribe = false;
    h.backendStatus = 'pending';
    h.backendPlan = [];
    h.backendResult = '';
    h.emitEvent = null;
    h.emitClose = null;
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('计划产出/批准替换/逐步回写/最终结果,每步 plan 全字段变更都替换 currentTask 引用', async () => {
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();

    // 创建后选中:pending + 空计划
    await store.selectTask('t1');
    expect(store.currentTask?.task.status).toBe('pending');
    expect(store.currentTask?.task.plan).toHaveLength(0);

    // 规划完成:planned + 两步计划(名称+目标);plan/status/approval_required 事件驱动刷新
    const planDraft: TaskStep[] = [
      { name: '构思大纲', goal: '产出三幕大纲', status: 'pending', result: '' },
      { name: '撰写正文', goal: '写 2000 字初稿', status: 'pending', result: '' },
    ];
    h.backendStatus = 'planned';
    h.backendPlan = planDraft;
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'plan', detail: '执行计划已更新(共 2 步)' });
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'status', status: 'planned' });
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'approval_required', detail: '计划已产出,待批准' });
    await flush();
    let ref = store.currentTask;
    expect(ref?.task.status).toBe('planned');
    expect(ref?.task.plan.map((s) => s.name)).toEqual(['构思大纲', '撰写正文']);
    expect(ref?.task.plan[0].goal, '目标必须随计划落库可见(批准前审阅内容)').toBe('产出三幕大纲');

    // 修改后批准:后端整体替换计划(goal 变更)——签名含 plan 全字段,goal 变更不得吞
    h.backendPlan = [
      { name: '构思大纲', goal: '产出四幕大纲(用户修改)', status: 'pending', result: '' },
      planDraft[1],
    ];
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'plan', detail: '执行计划已更新(共 2 步)' });
    await flush();
    expect(store.currentTask, '批准替换计划(goal 变更)必须替换引用').not.toBe(ref);
    expect(store.currentTask?.task.plan[0].goal).toBe('产出四幕大纲(用户修改)');
    ref = store.currentTask;

    // 批准后续跑:状态推进 running
    h.backendStatus = 'running';
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'status', status: 'running' });
    await flush();
    expect(store.currentTask).not.toBe(ref);
    expect(store.currentTask?.task.status).toBe('running');
    ref = store.currentTask;

    // 第 1 步执行中(仅 status 字段变:pending → running)
    h.backendPlan = [
      { ...h.backendPlan[0], status: 'running' },
      h.backendPlan[1],
    ];
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'plan' });
    await flush();
    expect(store.currentTask, '步骤 status 回写必须替换引用').not.toBe(ref);
    expect(store.currentTask?.task.plan[0].status).toBe('running');
    ref = store.currentTask;

    // 第 1 步完成(仅 result 字段变)——最易被字段子集签名吞掉的一类更新
    h.backendPlan = [
      { ...h.backendPlan[0], status: 'done', result: '大纲:起承转合' },
      { ...h.backendPlan[1], status: 'running' },
    ];
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'plan' });
    await flush();
    expect(store.currentTask, '步骤 result 回写必须替换引用').not.toBe(ref);
    expect(store.currentTask?.task.plan[0].result).toBe('大纲:起承转合');
    expect(store.currentTask?.task.plan[1].status).toBe('running');
    ref = store.currentTask;

    // 全部完成:步骤全 done + 最终 result;终态 status 事件到达
    h.backendStatus = 'done';
    h.backendPlan = [
      h.backendPlan[0],
      { ...h.backendPlan[1], status: 'done', result: '初稿正文……' },
    ];
    h.backendResult = '最终成果:一篇完整科幻短篇';
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'plan' });
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'status', status: 'done' });
    await flush();
    expect(store.currentTask).not.toBe(ref);
    expect(store.currentTask?.task.status).toBe('done');
    expect(store.currentTask?.task.result).toBe('最终成果:一篇完整科幻短篇');
    expect(store.currentTask?.task.plan.map((s) => s.status)).toEqual(['done', 'done']);
  });
});

describe('delta 流式缓冲(批次 R4 任务模式流式输出)', () => {
  beforeEach(() => {
    memStorage.clear();
    setActivePinia(createPinia());
    h.subscribeCalls = 0;
    h.closeCalls = 0;
    h.listFetchCount = 0;
    h.detailFetchCount = 0;
    h.usageFetchCount = 0;
    h.callsFetchCount = 0;
    h.callsFail = false;
    h.autoFailSubscribe = false;
    h.backendStatus = 'pending';
    h.emitEvent = null;
    h.emitClose = null;
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('delta 事件就地追加进 liveBuffers(key=phase:step_index),不触发任何 REST 重拉', async () => {
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');
    const baseList = h.listFetchCount;
    const baseDetail = h.detailFetchCount;
    const baseCalls = h.callsFetchCount;

    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'delta', phase: 'step', step_index: 0, detail: '第一段' });
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'delta', phase: 'step', step_index: 0, detail: '续写' });
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'delta', phase: 'planner', detail: '计划' });
    await flush();

    expect(store.liveBuffers.get('step:0'), '同 key 增量应就地拼接').toBe('第一段续写');
    expect(store.liveBuffers.get('planner:'), '缺 step_index 的阶段 key 尾段为空').toBe('计划');
    expect(h.listFetchCount, 'delta 不得触发列表重拉').toBe(baseList);
    expect(h.detailFetchCount, 'delta 不得触发详情重拉(绕开 contentSignature 链)').toBe(baseDetail);
    expect(h.callsFetchCount, 'delta 不得触发调用记录重拉').toBe(baseCalls);
  });

  it('非当前任务/缺 phase/缺 detail 的 delta 忽略;llm_call 落库事件清对应缓冲', async () => {
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');

    // 其他任务的 delta、缺 phase、缺 detail:全部不入缓冲
    h.emitEvent!({ type: 'task', task_id: 'other', kind: 'delta', phase: 'step', step_index: 0, detail: '别人的' });
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'delta', detail: '无归属' });
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'delta', phase: 'step' });
    await flush();
    expect(store.liveBuffers.size).toBe(0);

    // 两个调用并行累积;llm_call 落库只清对应 key(权威行取代暂态 delta)
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'delta', phase: 'step', step_index: 0, detail: '甲' });
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'delta', phase: 'step', step_index: 1, detail: '乙' });
    await flush();
    expect(store.liveBuffers.size).toBe(2);

    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'llm_call', phase: 'step', step_index: 0, detail: 'step #1 · mock · 5 tokens' });
    await flush();
    expect(store.liveBuffers.has('step:0'), '落库调用的缓冲应清除').toBe(false);
    expect(store.liveBuffers.get('step:1'), '并行调用的缓冲不受影响').toBe('乙');

    // 旧服务端 llm_call 无 phase 字段:不清缓冲(也不炸)
    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'llm_call', detail: '旧格式' });
    await flush();
    expect(store.liveBuffers.get('step:1')).toBe('乙');
  });

  it('selectTask 与 deleted 清空流式缓冲', async () => {
    const store = useTaskStore();
    store.appMode = 'task';
    store.startTaskEvents();
    await store.selectTask('t1');

    h.emitEvent!({ type: 'task', task_id: 't1', kind: 'delta', phase: 'agent', detail: '流式中' });
    await flush();
    expect(store.liveBuffers.size).toBe(1);

    await store.selectTask('t2');
    expect(store.liveBuffers.size, '切任务应清空旧任务缓冲').toBe(0);

    h.emitEvent!({ type: 'task', task_id: 't2', kind: 'delta', phase: 'agent', detail: '再来' });
    await flush();
    expect(store.liveBuffers.size).toBe(1);
    h.emitEvent!({ type: 'task', task_id: 't2', kind: 'deleted' });
    await flush();
    expect(store.liveBuffers.size, '删除当前任务应清缓冲').toBe(0);
  });
});
