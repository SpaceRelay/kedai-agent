// genSettings 模式分流测试:loadSettings/saveSettings 按 useTaskStore().appMode
// 向 api 层传 mode 参数(task / roleplay 两套提示词在服务端独立存储),
// 且 appMode 切换后再次调用参数跟随切换。mock 方式:vi.mock 替换 api 模块的
// getSettings/saveSettings(保留其余导出),断言调用入参。
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useGenSettingsStore } from './genSettings';
import { useTaskStore } from './task';
import * as api from '../api';
import type { RuntimeSettings } from '../api';

// node 环境无 localStorage(task store setup 即读 appMode 持久化),补内存桩(同 task.test.ts)
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

vi.mock('../api', async (importOriginal) => {
  const orig = await importOriginal<typeof import('../api')>();
  return {
    ...orig,
    getSettings: vi.fn(),
    saveSettings: vi.fn(),
  };
});

const getSettingsMock = vi.mocked(api.getSettings);
const saveSettingsMock = vi.mocked(api.saveSettings);

/** 构造最小可用的 RuntimeSettings 响应(仅关心本测试断言字段,其余取默认) */
function makeSettings(overrides: Partial<RuntimeSettings> = {}): RuntimeSettings {
  return {
    openai_base_url: '',
    api_key_masked: '',
    has_api_key: false,
    model: 'test-model',
    default_temperature: 0.8,
    default_top_p: 0.9,
    default_max_tokens: 1024,
    max_context_tokens: 65536,
    agent_system_prompt: '',
    search_endpoint: '',
    mvu_vars_position: 'system',
    reflect_prompt: '',
    preset_tail_prompt: '',
    preset_tail_role: 'user',
    reflect_advice_prompt: '',
    reflect_advice_role: 'user',
    bypass_mode: false,
    authorization_mode: 'loose',
    bypass_blacklist: [],
    tool_authorization_timeout_secs: 300,
    task_tool_policy: 'deny_dangerous',
    task_tool_allowlist: [],
    max_tool_rounds: 32,
    tool_history_keep_rounds: 4,
    tool_history_budget_tokens: 16384,
    render_html: false,
    compaction_mode: 'off',
    compaction_threshold: 0.8,
    compaction_keep_recent: 4,
    compaction_snip_bytes: 8192,
    llm_request_log: false,
    memory_distill_enabled: false,
    memory_inject_limit: 8,
    memory_inject_char_budget: 2000,
    memory_max_entries: 200,
    embedding_enabled: false,
    embedding_base_url: '',
    embedding_api_key_masked: '',
    has_embedding_api_key: false,
    embedding_model: '',
    embedding_dim: 0,
    skill_progressive_disclosure: true,
    subagent_max_depth: 2,
    subagent_max_concurrency: 6,
    subagent_result_max_chars: 2000,
    undo_enabled: true,
    mcp_enabled: false,
    mcp_servers: [],
    task_persona_full: false,
    task_prompt_inject_enabled: false,
    ...overrides,
  };
}

describe('genSettings 模式分流:loadSettings/saveSettings 按 appMode 传 mode 参数', () => {
  beforeEach(() => {
    memStorage.clear();
    setActivePinia(createPinia());
    getSettingsMock.mockReset().mockResolvedValue(makeSettings());
    saveSettingsMock.mockReset().mockResolvedValue({ ok: true, settings: makeSettings() });
  });

  it('appMode=task 时 loadSettings 与 saveSettings 均带 mode=task', async () => {
    const task = useTaskStore();
    // 直接写 ref,避开 setAppMode 的任务列表加载等副作用(本测试只关注 mode 参数)
    task.appMode = 'task';
    const store = useGenSettingsStore();

    await store.loadSettings();
    expect(getSettingsMock).toHaveBeenCalledWith('task');

    await store.saveSettings({ agent_system_prompt: '任务专用词' });
    expect(saveSettingsMock).toHaveBeenCalledWith({ agent_system_prompt: '任务专用词' }, 'task');
  });

  it("appMode=roleplay 时 loadSettings 与 saveSettings 均带 mode=roleplay", async () => {
    const task = useTaskStore();
    task.appMode = 'roleplay';
    const store = useGenSettingsStore();

    await store.loadSettings();
    expect(getSettingsMock).toHaveBeenCalledWith('roleplay');

    await store.saveSettings({ agent_system_prompt: '人设词' });
    expect(saveSettingsMock).toHaveBeenCalledWith({ agent_system_prompt: '人设词' }, 'roleplay');
  });

  it('切换 appMode 后再次 loadSettings,mode 参数跟随切换', async () => {
    const task = useTaskStore();
    const store = useGenSettingsStore();

    task.appMode = 'task';
    await store.loadSettings();
    expect(getSettingsMock).toHaveBeenLastCalledWith('task');

    task.appMode = 'roleplay';
    await store.loadSettings();
    expect(getSettingsMock).toHaveBeenLastCalledWith('roleplay');
    expect(getSettingsMock).toHaveBeenCalledTimes(2);

    task.appMode = 'task';
    await store.loadSettings();
    expect(getSettingsMock).toHaveBeenLastCalledWith('task');
  });
});

describe('genSettings 记忆槽预算与容量上限(B2/B3)', () => {
  beforeEach(() => {
    memStorage.clear();
    setActivePinia(createPinia());
  });

  it('loadSettings 回填服务端值;缺字段兜底 2000 / 200', async () => {
    const store = useGenSettingsStore();
    getSettingsMock.mockReset().mockResolvedValue(
      makeSettings({ memory_inject_char_budget: 4000, memory_max_entries: 500 }),
    );
    await store.loadSettings();
    expect(store.memoryInjectCharBudget).toBe(4000);
    expect(store.memoryMaxEntries).toBe(500);

    // 旧服务端/异常响应缺字段:回退默认值
    getSettingsMock.mockReset().mockResolvedValue(
      makeSettings({
        memory_inject_char_budget: undefined as unknown as number,
        memory_max_entries: undefined as unknown as number,
      }),
    );
    await store.loadSettings();
    expect(store.memoryInjectCharBudget).toBe(2000);
    expect(store.memoryMaxEntries).toBe(200);
  });

  it('saveSettings 响应回填两个字段(与 loadSettings 同口径)', async () => {
    const store = useGenSettingsStore();
    saveSettingsMock.mockReset().mockResolvedValue({
      ok: true,
      settings: makeSettings({ memory_inject_char_budget: 0, memory_max_entries: 0 }),
    });
    await store.saveSettings({ memory_inject_char_budget: 0, memory_max_entries: 0 });
    expect(store.memoryInjectCharBudget).toBe(0);
    expect(store.memoryMaxEntries).toBe(0);
  });
});

describe('genSettings 执行者人设开关(R3a task_persona_full)', () => {
  beforeEach(() => {
    memStorage.clear();
    setActivePinia(createPinia());
  });

  it('loadSettings 回填服务端值;缺字段(null/undefined)兜底 false(精简)', async () => {
    const store = useGenSettingsStore();
    getSettingsMock.mockReset().mockResolvedValue(makeSettings({ task_persona_full: true }));
    await store.loadSettings();
    expect(store.taskPersonaFull).toBe(true);

    // 旧服务端/异常响应缺字段:不得污染 store,回退精简
    getSettingsMock.mockReset().mockResolvedValue(
      makeSettings({ task_persona_full: undefined as unknown as boolean }),
    );
    await store.loadSettings();
    expect(store.taskPersonaFull).toBe(false);
  });

  it('saveSettings 响应回填 taskPersonaFull(与 loadSettings 同口径)', async () => {
    const store = useGenSettingsStore();
    saveSettingsMock.mockReset().mockResolvedValue({
      ok: true,
      settings: makeSettings({ task_persona_full: true }),
    });
    await store.saveSettings({ task_persona_full: true });
    expect(saveSettingsMock).toHaveBeenCalledWith({ task_persona_full: true }, 'roleplay');
    expect(store.taskPersonaFull).toBe(true);
  });
});

describe('genSettings 授权模式三档(2026-09 授权改造)', () => {
  beforeEach(() => {
    memStorage.clear();
    setActivePinia(createPinia());
  });

  it('loadSettings 读取 authorization_mode 三档', async () => {
    const store = useGenSettingsStore();
    for (const mode of ['strict', 'loose', 'bypass'] as const) {
      getSettingsMock.mockReset().mockResolvedValue(makeSettings({ authorization_mode: mode }));
      await store.loadSettings();
      expect(store.authorizationMode).toBe(mode);
    }
  });

  it('旧配置只有 bypass_mode 时映射:true→bypass,false→strict', async () => {
    const store = useGenSettingsStore();
    // 模拟旧服务端:authorization_mode 缺失,仅 bypass_mode
    getSettingsMock.mockReset().mockResolvedValue(
      makeSettings({
        authorization_mode: undefined as unknown as 'loose',
        bypass_mode: true,
      }),
    );
    await store.loadSettings();
    expect(store.authorizationMode).toBe('bypass');

    getSettingsMock.mockReset().mockResolvedValue(
      makeSettings({
        authorization_mode: undefined as unknown as 'loose',
        bypass_mode: false,
      }),
    );
    await store.loadSettings();
    expect(store.authorizationMode).toBe('strict');
  });

  it('setAuthorizationMode 持久化 authorization_mode 并回填', async () => {
    const store = useGenSettingsStore();
    saveSettingsMock.mockReset().mockResolvedValue({
      ok: true,
      settings: makeSettings({ authorization_mode: 'strict' }),
    });
    await store.setAuthorizationMode('strict');
    expect(saveSettingsMock).toHaveBeenCalledWith({ authorization_mode: 'strict' }, 'roleplay');
    expect(store.authorizationMode).toBe('strict');
  });

  it('setAuthorizationMode 失败时回滚本地值', async () => {
    const store = useGenSettingsStore();
    store.authorizationMode = 'loose';
    saveSettingsMock.mockReset().mockRejectedValue(new Error('网络错误'));
    await store.setAuthorizationMode('bypass');
    expect(store.authorizationMode).toBe('loose');
  });

  it('loadSettings 回填始终需授权清单、超时与任务工具策略', async () => {
    const store = useGenSettingsStore();
    getSettingsMock.mockReset().mockResolvedValue(
      makeSettings({
        bypass_blacklist: ['write', 'replace'],
        tool_authorization_timeout_secs: 120,
        task_tool_policy: 'all',
        task_tool_allowlist: ['read'],
      }),
    );
    await store.loadSettings();
    expect(store.authorizationAlwaysRequired).toEqual(['write', 'replace']);
    expect(store.toolAuthorizationTimeoutSecs).toBe(120);
    expect(store.taskToolPolicy).toBe('all');
    expect(store.taskToolAllowlist).toEqual(['read']);
  });
});
