// 生成设置 store:生成参数(温度/Top-P/长度/上下文/工具轮次)、上下文压缩、记忆蒸馏、
// 子代理参数、Agent 设置草稿(系统提示词/搜索端点/变量注入位置/反思系列)、授权模式、
// 提示词注入配置、自定义 Agent 执行流程库,以及 loadSettings/saveSettings 读写服务端设置。
// 从 store.ts 按领域拆分。跨 store 引用(model / appMode / renderHtml 默认)均在动作运行时解析,
// setup 阶段不实例化其他 store,避免初始化环;本 store 被 chat/uiPrefs/task 引用,不反向引用其 setup。
import { defineStore } from 'pinia';
import { ref } from 'vue';
import * as api from '../api';
import { useModelConnStore } from './modelConn';
import { useUiPrefsStore } from './uiPrefs';
import { useTaskStore } from './task';

/** 读取授权模式:新字段优先;旧配置只有 bypass_mode 时按 true→bypass / false→strict 映射 */
function readAuthorizationMode(s: api.RuntimeSettings): api.AuthorizationMode {
  const m = s.authorization_mode as api.AuthorizationMode | undefined;
  if (m === 'strict' || m === 'loose' || m === 'bypass') return m;
  return s.bypass_mode ? 'bypass' : 'strict';
}

export const useGenSettingsStore = defineStore('app.genSettings', () => {
  // ===== 生成参数 =====
  const temperature = ref(0.8);
  const topP = ref(0.9);
  const maxTokens = ref(1024);
  /** 上下文窗口上限(token):历史超出后按时间裁剪(服务端执行;最低 64K,上限 1M) */
  const maxContextTokens = ref(65536);
  /** AGENT/CUSTOM 模式工具循环轮次上限(服务端默认 32,1..=200) */
  const maxToolRounds = ref(32);
  /** 工具循环历史保留的最近完整轮数(服务端默认 4,1..=32) */
  const toolHistoryKeepRounds = ref(4);
  /** 工具循环历史 token 预算(服务端默认 16384;0 = 禁用预算闸门) */
  const toolHistoryBudgetTokens = ref(16384);
  /** 上下文压缩模式(off / manual / auto;服务端默认 off) */
  const compactionMode = ref<'off' | 'manual' | 'auto'>('off');
  /** 上下文压缩触发阈值(0.5..=0.95;服务端默认 0.8) */
  const compactionThreshold = ref(0.8);
  /** 压缩后保留的最近消息条数(2..=200;服务端默认 4,越界钳回默认) */
  const compactionKeepRecent = ref(4);
  /** snip 零成本裁剪的超长消息阈值(字节;0 = 禁用,上限 1MB;服务端默认 8192) */
  const compactionSnipBytes = ref(8192);
  /** 跨会话记忆蒸馏开关(服务端默认 false;关闭时蒸馏端点返回 400 带指引) */
  const memoryDistillEnabled = ref(false);
  /** 每次注入提示词的记忆条数上限(0..=50;服务端默认 8) */
  const memoryInjectLimit = ref(8);
  /** 记忆槽字符预算(0..=20000;0 = 不限制;服务端默认 2000):按精选排序累积到预算即停 */
  const memoryInjectCharBudget = ref(2000);
  /** 每角色记忆容量上限(0..=10000;0 = 不淘汰;服务端默认 200):超出后最低分条目置 selected=0 */
  const memoryMaxEntries = ref(200);
  /** 技能渐进披露开关(服务端默认 true):system 只注入「名称:用途」清单,正文按需 read */
  const skillProgressiveDisclosure = ref(true);
  /** 子智能体最大嵌套深度(1..=4;服务端默认 2) */
  const subagentMaxDepth = ref(2);
  /** 子智能体最大并发数(1..=16;服务端默认 6) */
  const subagentMaxConcurrency = ref(6);
  /** 子智能体结果最大字符数(500..=8000;服务端默认 2000,超出截断带尾注) */
  const subagentResultMaxChars = ref(2000);
  /** 回退快照(undo)开关(批次 6.1b;服务端默认 true):开时写工具执行前自动存档 */
  const undoEnabled = ref(true);
  /** MCP stdio 客户端总开关(批次 6.2;服务端默认 false;仅启动时装配,改后重启生效) */
  const mcpEnabled = ref(false);
  /** MCP 服务器列表(全量替换语义;服务端默认空) */
  const mcpServers = ref<api.McpServerConfig[]>([]);
  /** 授权模式三档(服务端默认 loose):strict=读/写/删都需授权;loose=读/写放行、删需授权;bypass=除系统路径写删外全放行 */
  const authorizationMode = ref<api.AuthorizationMode>('loose');
  /** 「始终需授权」清单(名单内工具在三档模式下都需授权) */
  const authorizationAlwaysRequired = ref<string[]>([]);
  /** 授权等待超时(秒;30..=1800) */
  const toolAuthorizationTimeoutSecs = ref(300);
  /** 任务模式工具策略 */
  const taskToolPolicy = ref<'all' | 'deny_dangerous' | 'allowlist'>('deny_dangerous');
  /** 任务模式工具白名单 */
  const taskToolAllowlist = ref<string[]>([]);

  // ===== Agent 设置(编辑区草稿,保存时随 patch 提交) =====
  const agentSystemPrompt = ref('');
  const searchEndpoint = ref('');
  /** mvu 变量状态注入位置(system / user_tail) */
  const mvuVarsPosition = ref<'system' | 'user_tail'>('system');
  /** 反思提示词(空 = 机械规则检查;非空 = 反思步骤调用 LLM 判定) */
  const reflectPrompt = ref('');
  /** 复杂模式预设尾部提示词(空 = 禁用;位置0 尾部) */
  const presetTailPrompt = ref('');
  /** 预设尾部提示词注入角色(user / assistant) */
  const presetTailRole = ref<'user' | 'assistant'>('user');
  /** 反思失败建议的可选补充说明(主体建议由引擎自动生成,注入位置0 内预设尾部之前;空 = 仅自动建议) */
  const reflectAdvicePrompt = ref('');
  /** 反思失败建议注入角色(user / assistant) */
  const reflectAdviceRole = ref<'user' | 'assistant'>('user');
  /** 执行者人设完整开关(R3a;服务端默认 false = 精简:仅 description+personality)。
   *  仅任务模式执行者人设注入生效;角色扮演模式下修改的是 task 覆盖层 None 时的沿用值 */
  const taskPersonaFull = ref(false);

  /** 任务模式是否继承提示词注入(2026-09-10 实跑修复;服务端默认 false = 隔离)。
   *  关闭时任务执行/汇总/追加轮不注入 prompt_floors.json(通常含角色扮演的
   *  「1200 字/第三人称/禁词表」等文章要求,会与任务目标冲突);开启恢复旧行为。 */
  const taskPromptInjectEnabled = ref(false);

  // ===== 提示词注入(简单模式 + 楼层系统;全局配置) =====
  const promptInject = ref<api.PromptInjectConfig | null>(null);
  // ===== 自定义 Agent 执行流程(custom 模式;全局配置) =====
  const agentFlowLibrary = ref<api.AgentFlowLibrary | null>(null);

  /** 加载运行期设置(API 连接 + 生成参数),仅回填本地默认值;按当前 appMode 读取对应合并值 */
  async function loadSettings(): Promise<void> {
    try {
      const s = await api.getSettings(useTaskStore().appMode);
      // 数值字段兜底默认值:后端异常响应(缺字段/null)不得污染 store 打崩渲染
      temperature.value = s.default_temperature ?? 0.8;
      topP.value = s.default_top_p ?? 0.9;
      maxTokens.value = s.default_max_tokens ?? 1024;
      maxContextTokens.value = s.max_context_tokens ?? 65536;
      maxToolRounds.value = s.max_tool_rounds ?? 32;
      toolHistoryKeepRounds.value = s.tool_history_keep_rounds ?? 4;
      toolHistoryBudgetTokens.value = s.tool_history_budget_tokens ?? 16384;
      agentSystemPrompt.value = s.agent_system_prompt ?? '';
      searchEndpoint.value = s.search_endpoint ?? '';
      mvuVarsPosition.value = (s.mvu_vars_position ?? 'system') as 'system' | 'user_tail';
      reflectPrompt.value = s.reflect_prompt ?? '';
      presetTailPrompt.value = s.preset_tail_prompt ?? '';
      presetTailRole.value = s.preset_tail_role === 'assistant' ? 'assistant' : 'user';
      reflectAdvicePrompt.value = s.reflect_advice_prompt ?? '';
      reflectAdviceRole.value = s.reflect_advice_role === 'assistant' ? 'assistant' : 'user';
      taskPersonaFull.value = s.task_persona_full ?? false;
      taskPromptInjectEnabled.value = s.task_prompt_inject_enabled ?? false;
      authorizationMode.value = readAuthorizationMode(s);
      authorizationAlwaysRequired.value = Array.isArray(s.bypass_blacklist) ? s.bypass_blacklist : [];
      toolAuthorizationTimeoutSecs.value = s.tool_authorization_timeout_secs ?? 300;
      taskToolPolicy.value = s.task_tool_policy ?? 'deny_dangerous';
      taskToolAllowlist.value = Array.isArray(s.task_tool_allowlist) ? s.task_tool_allowlist : [];
      const uiPrefs = useUiPrefsStore();
      uiPrefs.defaultRenderHtml = s.render_html ?? false;
      // 无角色卡记忆时,当前生效值跟随全局默认(角色卡记忆优先)
      uiPrefs.syncRenderHtmlToCurrent();
      compactionMode.value = (s.compaction_mode as 'off' | 'manual' | 'auto') ?? 'off';
      compactionThreshold.value = s.compaction_threshold ?? 0.8;
      compactionKeepRecent.value = s.compaction_keep_recent ?? 4;
      compactionSnipBytes.value = s.compaction_snip_bytes ?? 8192;
      memoryDistillEnabled.value = s.memory_distill_enabled ?? false;
      memoryInjectLimit.value = s.memory_inject_limit ?? 8;
      memoryInjectCharBudget.value = s.memory_inject_char_budget ?? 2000;
      memoryMaxEntries.value = s.memory_max_entries ?? 200;
      skillProgressiveDisclosure.value = s.skill_progressive_disclosure ?? true;
      subagentMaxDepth.value = s.subagent_max_depth ?? 2;
      subagentMaxConcurrency.value = s.subagent_max_concurrency ?? 6;
      subagentResultMaxChars.value = s.subagent_result_max_chars ?? 2000;
      undoEnabled.value = s.undo_enabled ?? true;
      mcpEnabled.value = s.mcp_enabled ?? false;
      mcpServers.value = Array.isArray(s.mcp_servers) ? s.mcp_servers : [];
      const modelConn = useModelConnStore();
      if (!modelConn.model) modelConn.model = s.model;
    } catch {
      /* 忽略 */
    }
  }

  /** 保存运行期设置(可部分字段;服务端持久化到 data/settings.json);按当前 appMode 写入对应覆盖层 */
  async function saveSettings(patch: api.RuntimeSettingsPatch): Promise<void> {
    const res = await api.saveSettings(patch, useTaskStore().appMode);
    const s = res.settings;
    // 数值字段兜底默认值(同 loadSettings)
    temperature.value = s.default_temperature ?? 0.8;
    topP.value = s.default_top_p ?? 0.9;
    maxTokens.value = s.default_max_tokens ?? 1024;
    maxContextTokens.value = s.max_context_tokens ?? 65536;
    maxToolRounds.value = s.max_tool_rounds ?? 32;
    toolHistoryKeepRounds.value = s.tool_history_keep_rounds ?? 4;
    toolHistoryBudgetTokens.value = s.tool_history_budget_tokens ?? 16384;
    useModelConnStore().model = s.model;
    agentSystemPrompt.value = s.agent_system_prompt ?? '';
    searchEndpoint.value = s.search_endpoint ?? '';
    mvuVarsPosition.value = (s.mvu_vars_position ?? 'system') as 'system' | 'user_tail';
    reflectPrompt.value = s.reflect_prompt ?? '';
    presetTailPrompt.value = s.preset_tail_prompt ?? '';
    presetTailRole.value = s.preset_tail_role === 'assistant' ? 'assistant' : 'user';
    reflectAdvicePrompt.value = s.reflect_advice_prompt ?? '';
    reflectAdviceRole.value = s.reflect_advice_role === 'assistant' ? 'assistant' : 'user';
    taskPersonaFull.value = s.task_persona_full ?? false;
    taskPromptInjectEnabled.value = s.task_prompt_inject_enabled ?? false;
    authorizationMode.value = readAuthorizationMode(s);
    authorizationAlwaysRequired.value = Array.isArray(s.bypass_blacklist) ? s.bypass_blacklist : [];
    toolAuthorizationTimeoutSecs.value = s.tool_authorization_timeout_secs ?? 300;
    taskToolPolicy.value = s.task_tool_policy ?? 'deny_dangerous';
    taskToolAllowlist.value = Array.isArray(s.task_tool_allowlist) ? s.task_tool_allowlist : [];
    const uiPrefs = useUiPrefsStore();
    uiPrefs.defaultRenderHtml = s.render_html ?? false;
    uiPrefs.syncRenderHtmlToCurrent();
    compactionMode.value = (s.compaction_mode as 'off' | 'manual' | 'auto') ?? 'off';
    compactionThreshold.value = s.compaction_threshold ?? 0.8;
    compactionKeepRecent.value = s.compaction_keep_recent ?? 4;
    compactionSnipBytes.value = s.compaction_snip_bytes ?? 8192;
    memoryDistillEnabled.value = s.memory_distill_enabled ?? false;
    memoryInjectLimit.value = s.memory_inject_limit ?? 8;
    memoryInjectCharBudget.value = s.memory_inject_char_budget ?? 2000;
    memoryMaxEntries.value = s.memory_max_entries ?? 200;
    skillProgressiveDisclosure.value = s.skill_progressive_disclosure ?? true;
    subagentMaxDepth.value = s.subagent_max_depth ?? 2;
    subagentMaxConcurrency.value = s.subagent_max_concurrency ?? 6;
    subagentResultMaxChars.value = s.subagent_result_max_chars ?? 2000;
    undoEnabled.value = s.undo_enabled ?? true;
    mcpEnabled.value = s.mcp_enabled ?? false;
    mcpServers.value = Array.isArray(s.mcp_servers) ? s.mcp_servers : [];
  }

  // 设置写入统一串行,避免 renderHtml 自动保存与设置面板保存交错回写旧响应。
  let settingsSaveQueue: Promise<void> = Promise.resolve();
  function queueSettingsSave(patch: api.RuntimeSettingsPatch): Promise<void> {
    const queued = settingsSaveQueue.then(() => saveSettings(patch));
    settingsSaveQueue = queued.catch(() => { /* 保持队列可继续使用 */ });
    return queued;
  }

  /** 切换授权模式并持久化(输入框底部三档开关;失败回滚本地值) */
  async function setAuthorizationMode(mode: api.AuthorizationMode): Promise<void> {
    const prev = authorizationMode.value;
    if (prev === mode) return;
    authorizationMode.value = mode;
    try {
      await queueSettingsSave({ authorization_mode: mode });
    } catch {
      authorizationMode.value = prev;
    }
  }

  // ===== 提示词注入 =====

  /** 加载注入配置(简单模式 + 楼层);失败保持 null,区块显示加载失败 */
  async function loadPromptInject(): Promise<void> {
    try {
      promptInject.value = await api.getPromptInject();
    } catch (e) {
      console.error('加载提示词注入配置失败', e);
    }
  }

  /** 全量保存注入配置(服务端持久化到 data/prompt_floors.json) */
  async function savePromptInjectConfig(config: api.PromptInjectConfig): Promise<void> {
    promptInject.value = await api.savePromptInject(config);
  }

  // ===== 自定义 Agent 执行流程(流程库:多流程 + 当前选择) =====

  /** 加载流程库;失败保持 null,设置区块显示加载失败 */
  async function loadAgentFlow(): Promise<void> {
    try {
      const { library } = await api.getAgentFlow();
      agentFlowLibrary.value = library;
    } catch (e) {
      console.error('加载执行流程配置失败', e);
    }
  }

  /** 保存(创建或更新)流程并设为当前选中(服务端校验,失败抛错由调用方提示) */
  async function saveAgentFlowConfig(config: api.AgentFlowConfig): Promise<void> {
    agentFlowLibrary.value = await api.saveAgentFlow(config);
  }

  /** 切换当前选中流程 */
  async function selectAgentFlow(id: string): Promise<void> {
    agentFlowLibrary.value = await api.selectAgentFlow(id);
  }

  /** 删除流程(删除当前流程时服务端回退到第一个) */
  async function deleteAgentFlow(id: string): Promise<void> {
    agentFlowLibrary.value = await api.deleteAgentFlow(id);
  }

  return {
    temperature,
    topP,
    maxTokens,
    maxContextTokens,
    maxToolRounds,
    toolHistoryKeepRounds,
    toolHistoryBudgetTokens,
    compactionMode,
    compactionThreshold,
    compactionKeepRecent,
    compactionSnipBytes,
    memoryDistillEnabled,
    memoryInjectLimit,
    memoryInjectCharBudget,
    memoryMaxEntries,
    skillProgressiveDisclosure,
    subagentMaxDepth,
    subagentMaxConcurrency,
    subagentResultMaxChars,
    undoEnabled,
    mcpEnabled,
    mcpServers,
    authorizationMode,
    authorizationAlwaysRequired,
    toolAuthorizationTimeoutSecs,
    taskToolPolicy,
    taskToolAllowlist,
    setAuthorizationMode,
    agentSystemPrompt,
    searchEndpoint,
    mvuVarsPosition,
    reflectPrompt,
    presetTailPrompt,
    presetTailRole,
    reflectAdvicePrompt,
    reflectAdviceRole,
    taskPersonaFull,
    taskPromptInjectEnabled,
    promptInject,
    agentFlowLibrary,
    loadSettings,
    saveSettings,
    queueSettingsSave,
    loadPromptInject,
    savePromptInjectConfig,
    loadAgentFlow,
    saveAgentFlowConfig,
    selectAgentFlow,
    deleteAgentFlow,
  };
});
