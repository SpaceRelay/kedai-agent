// 全局状态(Pinia setup store)
// 管理:角色列表、当前角色/会话、消息、Agent 执行状态、Token 统计、连接状态。
// 所有网络请求收敛到 api 模块,组件不再裸 fetch。

import { defineStore } from 'pinia';
import { computed, ref, watch } from 'vue';
// api 经聚合入口导入(旧 api.ts 已拆分为 api/ 目录);别名 api 保持全文件 api.X 引用不变
import * as api from './api';
import { emitMvuEvent } from './mvu/host';
import { parseUpdateVariable } from './mvu/parser';
// SSE 事件纯函数化 + mvu 状态纯函数(经 './api' 类型依赖)
import { idleAgent, reduceSseEvent } from './sseReducer';
import { applyMvuCommands, deepMergeVars, replayMvuVariables } from './mvu/mvuStore';
import type { MvuVars, MvuVariables } from './mvu/mvuStore';
import type { AgentActivity, UiMessage } from './sseReducer';
import { createChatStreamService } from './chatStreamService';
import {
  LocalScriptAuthorizationStore,
  hashRegexScripts,
  type ScriptAuthorizationGrant,
} from './scriptAuthorization';
import { saveExportFile } from './exportFile';
import { LocalRenderHtmlPreferenceStore } from './renderHtmlPreference';
import type { ApiEventLogEntry } from './devTools';

// 类型别名:保持 store 对外的类型导出(AgentActivity / UiMessage)不变
export type { AgentActivity, UiMessage };

const chatStream = createChatStreamService({
  streamChat: api.streamChat,
  stopChat: api.stopChat,
});

export const useAppStore = defineStore('app', () => {
  // ===== 状态 =====
  const characters = ref<api.CharacterRecord[]>([]);
  const currentCharacterId = ref<string | null>(null);
  const currentSessionId = ref<string | null>(null);
  const sessions = ref<api.SessionInfo[]>([]);
  const messages = ref<UiMessage[]>([]);
  const generating = ref(false);
  const agent = ref<AgentActivity>(idleAgent());
  const connStatus = ref<'ok' | 'fail' | 'unknown'>('unknown');
  const connMessage = ref('');
  const lastUsage = ref<api.TokenUsage | null>(null);
  const contextTokens = ref(0);
  const settingsOpen = ref(false);
  /** 提示词顺序管理面板开关 */
  const promptsOpen = ref(false);
  const agentDockOpen = ref(true);
  /** 右侧 Agent 面板开关(默认收起) */
  const agentPanelOpen = ref(false);
  const agentMode = ref<api.AgentMode>('fast');
  /** 授权模式:false=授权(高位操作需授权), true=放行(除黑名单外不弹授权) */
  const bypassMode = ref(false);
  /** 缓存命中率连续为 0 的次数(连续两次为 0 则隐藏命中率显示) */
  const cacheZeroStreak = ref(0);
  /** 当前会话累计 token(后端接口返回) */
  const sessionTotalTokens = ref(0);
  /** 全局累计 token(后端接口返回) */
  const globalTotalTokens = ref(0);
  /** 启动动画是否完成 */
  const splashDone = ref(false);
  const temperature = ref(0.8);
  const topP = ref(0.9);
  const maxTokens = ref(1024);
  /** 上下文窗口上限(token):历史超出后按时间裁剪(服务端执行;最低 64K,上限 1M) */
  const maxContextTokens = ref(65536);
  /** AGENT/CUSTOM 模式工具循环轮次上限(服务端默认 32,1..=200) */
  const maxToolRounds = ref(32);
  /** 上下文压缩模式(off / manual / auto;服务端默认 off) */
  const compactionMode = ref<'off' | 'manual' | 'auto'>('off');
  /** 上下文压缩触发阈值(0.5..=0.95;服务端默认 0.8) */
  const compactionThreshold = ref(0.8);
  const model = ref('');
  const models = ref<string[]>([]);
  const searchQuery = ref('');
  // ===== 世界书 =====
  const worldBooks = ref<api.WorldBookRecord[]>([]);
  const worldBooksOpen = ref(false);
  // ===== 快速回复(阶段四 4b) =====
  const quickReplies = ref<api.QuickReplyRecord[]>([]);
  const quickRepliesOpen = ref(false);
  // ===== 音频播放器(阶段五 5a) =====
  const audio = ref<api.AudioState | null>(null);
  const audioOpen = ref(false);
  /** 插件管理弹窗开关 */
  const pluginsOpen = ref(false);
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
  const scriptAuthorizationStore = new LocalScriptAuthorizationStore(localStorage);
  /** 当前角色脚本内容哈希；角色或脚本变化时重算。 */
  const currentScriptHash = ref('');
  /**
   * 授权变更信号:grant/revoke/删卡时递增,驱动依赖 localStorage 的
   * currentScriptAuthorized 等 computed 重算。scriptAuthorizationStore 本身
   * 不是响应式对象,若不引入此信号,授权后按钮/面板不会刷新。
   */
  const scriptAuthVersion = ref(0);
  /** 授权列表快照，供设置页查看与逐卡撤销。 */
  const scriptAuthorizations = ref<ScriptAuthorizationGrant[]>(scriptAuthorizationStore.list());
  /** mvu 变量树(stat_data/display_data),随历史回放与消息更新 */
  const mvuVariables = ref<MvuVars>({
    stat_data: {},
    display_data: {},
  });
  /** 当前角色 [InitVar] 初始变量(按条目 comment 保存原文,由 mvu 模块收集) */
  const initVarEntries = ref<Record<string, string>>({});
  /** 聊天记录面板开关 */
  const chatRecordsOpen = ref(false);
  // ===== 技能库 =====
  const skillsOpen = ref(false);
  const skills = ref<api.SkillRecord[]>([]);
  // ===== 契约编辑(P6 面板) =====
  const contractsOpen = ref(false);
  // ===== 用户脚本(ScriptTree,阶段三) =====
  const scriptsOpen = ref(false);
  // ===== 宏调试(阶段六 6b) =====
  const macrosOpen = ref(false);
  // ===== 事件监控(阶段六 6c):SSE 事件日志(前端采集,cap 500 丢最旧) =====
  const eventLog = ref<ApiEventLogEntry[]>([]);
  const eventsOpen = ref(false);
  // ===== 优化面板(阶段六 6d) =====
  const optimizeOpen = ref(false);
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
  // ===== 提示词注入(简单模式 + 楼层系统;全局配置) =====
  const promptInject = ref<api.PromptInjectConfig | null>(null);
  // ===== 自定义 Agent 执行流程(custom 模式;全局配置) =====
  const agentFlowLibrary = ref<api.AgentFlowLibrary | null>(null);

  // ===== 派生状态 =====
  const currentCharacter = computed(() =>
    characters.value.find((c) => c.id === currentCharacterId.value) ?? null,
  );
  /**
   * 当前角色全部开场(多开场切换用):主开场 first_mes + 备用开场 alternate_greetings,均去空。
   * 空数组 = 无开场;下标即 greeting_index(0=主开场,1..=备用)。
   */
  const currentGreetings = computed<string[]>(() => {
    const c = currentCharacter.value;
    if (!c) return [];
    const list: string[] = [];
    if (c.first_mes?.trim()) list.push(c.first_mes);
    for (const g of c.alternate_greetings ?? []) {
      if (g.trim()) list.push(g);
    }
    return list;
  });
  const currentCharacterName = computed(
    () => currentCharacter.value?.chara_name ?? currentCharacter.value?.name ?? '未选择角色',
  );
  const currentScriptAuthorized = computed(() => {
    const id = currentCharacterId.value;
    // 引用授权变更信号,确保 grant/revoke 后本 computed 重新求值
    void scriptAuthVersion.value;
    return !!id && !!currentScriptHash.value && scriptAuthorizationStore.isAuthorized(id, currentScriptHash.value);
  });
  /** 角色列表按搜索词过滤 */
  const filteredCharacters = computed(() => {
    const q = searchQuery.value.trim().toLowerCase();
    if (!q) return characters.value;
    return characters.value.filter((c) =>
      [c.chara_name, c.name, c.description].join(' ').toLowerCase().includes(q),
    );
  });

  // ===== 动作 =====
  async function loadCharacters(autoSelect = true): Promise<void> {
    try {
      characters.value = await api.listCharacters();
      if (currentCharacterId.value && !characters.value.some((c) => c.id === currentCharacterId.value)) {
        currentCharacterId.value = null;
        currentSessionId.value = null;
        sessions.value = [];
        messages.value = [];
      }
      if (autoSelect && !currentCharacterId.value && characters.value.length > 0) {
        await selectCharacter(characters.value[0].id);
      }
    } catch (e) {
      console.error('加载角色失败', e);
    }
  }

  async function selectCharacter(id: string): Promise<void> {
    currentCharacterId.value = id;
    // HTML 渲染开关按角色卡记忆:有记忆用记忆,无记忆回退全局默认
    syncRenderHtmlToCurrent();
    const selected = characters.value.find((c) => c.id === id);
    // 无条件计算脚本哈希:无脚本卡也得到合法哈希(空数组 SHA-256),使
    // currentScriptAuthorized 与 authorizeCurrentCharacterScripts 对无脚本卡同样生效,
    // 顶栏 JS 授权按钮常驻可授权/撤销;执行层因脚本数组为空永不执行,无安全影响。
    currentScriptHash.value = await hashRegexScripts(selected?.regex_scripts ?? []);
    currentSessionId.value = null;
    sessions.value = [];
    messages.value = [];
    lastUsage.value = null;
    agent.value = idleAgent();
    mvuVariables.value = { stat_data: {}, display_data: {} };
    // 加载该角色 [InitVar] 初始变量条目(mvu 初始化数据)
    try {
      initVarEntries.value = await api.fetchInitVars(id);
    } catch {
      initVarEntries.value = {};
    }
    try {
      await loadSessions(id);
      if (sessions.value.length > 0) {
        currentSessionId.value = sessions.value[0].id;
        await loadHistory(sessions.value[0].id);
      } else if (characters.value.find((c) => c.id === id)?.first_mes) {
        // 角色带开场白:自动新建会话,由后端注入开场白并显示
        await newSession();
      }
    } catch (e) {
      console.error('加载会话失败', e);
    }
  }

  async function loadSessions(characterId: string): Promise<void> {
    sessions.value = await api.listSessions(characterId);
  }

  /** 新建会话并切换过去(后端会注入角色开场白作为首条消息);greetingIndex=0 主开场,1..=备用开场 */
  async function newSession(greetingIndex = 0): Promise<void> {
    const cid = currentCharacterId.value;
    if (!cid) return;
    const session = await api.createSession(cid, undefined, greetingIndex);
    sessions.value.unshift(session);
    currentSessionId.value = session.id;
    lastUsage.value = null;
    agent.value = idleAgent();
    await loadHistory(session.id);
  }

  /** 会话内切换开场:清空当前会话消息后,按 greetingIndex 重新播种开场并刷新历史 */
  async function switchGreeting(greetingIndex: number): Promise<void> {
    const sid = currentSessionId.value;
    if (!sid) return;
    await api.regreetSession(sid, greetingIndex);
    messages.value = [];
    contextTokens.value = 0;
    lastUsage.value = null;
    await loadHistory(sid);
  }

  async function switchSession(id: string): Promise<void> {
    if (generating.value) return;
    currentSessionId.value = id;
    await loadHistory(id);
  }

  async function loadHistory(sessionId: string): Promise<void> {
    try {
      const msgs = await api.fetchHistory(sessionId);
      messages.value = msgs.map((m) => ({ ...m, streaming: false }));
      mvuVariables.value = replayMvuVariables(msgs);
      updateContextTokens(msgs);
      // 加载 Token 累计统计
      void loadTokenTotals(sessionId);
    } catch (e) {
      console.error('加载历史失败', e);
    }
  }

  /** 加载 Token 累计统计(会话 + 全局) */
  async function loadTokenTotals(sessionId?: string): Promise<void> {
    try {
      const sid = sessionId ?? currentSessionId.value;
      if (sid) {
        sessionTotalTokens.value = await api.getSessionTotalTokens(sid);
      }
      globalTotalTokens.value = await api.getGlobalTotalTokens();
    } catch {
      /* 忽略 */
    }
  }

  /** 消息完成时更新变量树并持久化快照(assistant 消息,解析 UpdateVariable) */
  async function applyMvuUpdate(m: { id: number; content: string; extra?: Record<string, unknown> }): Promise<void> {
    const parsed = parseUpdateVariable(m.content);
    if (parsed.commands.length === 0) return;
    mvuVariables.value = applyMvuCommands(mvuVariables.value, parsed.commands);
    // 兼容原版 MagVarUpdate:变量更新后触发 mag_variable_updated 事件
    emitMvuEvent('mag_variable_updated', {
      stat_data: mvuVariables.value.stat_data,
      display_data: mvuVariables.value.display_data,
    });
    // 持久化快照到该消息 extra.mvu
    if (currentSessionId.value && m.id > 0) {
      try {
        await api.saveMessageVariables(currentSessionId.value, m.id, {
          stat_data: mvuVariables.value.stat_data,
          display_data: mvuVariables.value.display_data,
        });
      } catch (e) {
        console.warn('保存变量快照失败', e);
      }
    }
  }

  async function updateContextTokens(msgs: Array<{ role: string; content: string }>): Promise<void> {
    try {
      contextTokens.value = await api.countTokens(msgs);
    } catch {
      /* 忽略 */
    }
  }

  async function uploadCharacter(file: File): Promise<api.CharacterRecord> {
    const record = await api.uploadCharacter(file);
    await loadCharacters(false);
    await selectCharacter(record.id);
    return record;
  }

  async function deleteCharacter(id: string): Promise<void> {
    await api.deleteCharacter(id);
    scriptAuthorizationStore.revoke(id);
    scriptAuthorizations.value = scriptAuthorizationStore.list();
    scriptAuthVersion.value += 1;
    // 一并清理该角色卡的 HTML 渲染开关记忆(随卡删除,不留孤儿数据)
    if (id in renderHtmlOverrides.value) {
      const next = { ...renderHtmlOverrides.value };
      delete next[id];
      renderHtmlOverrides.value = next;
      renderHtmlPreferenceStore.write(next);
    }
    await loadCharacters();
  }

  function authorizeCurrentCharacterScripts(): void {
    const id = currentCharacterId.value;
    if (!id || !currentScriptHash.value) return;
    scriptAuthorizationStore.grant(id, currentScriptHash.value);
    scriptAuthorizations.value = scriptAuthorizationStore.list();
    scriptAuthVersion.value += 1;
  }

  function revokeCharacterScripts(characterId: string): void {
    scriptAuthorizationStore.revoke(characterId);
    scriptAuthorizations.value = scriptAuthorizationStore.list();
    scriptAuthVersion.value += 1;
  }

  function isCharacterScriptAuthorized(characterId: string, scriptHash: string): boolean {
    return scriptAuthorizationStore.isAuthorized(characterId, scriptHash);
  }

  /** 更新角色提示词/开场白(description/first_mes/alternate_greetings),并同步本地列表 */
  async function updateCharacterPrompt(
    id: string,
    patch: { description?: string; first_mes?: string; alternate_greetings?: string[] },
  ): Promise<void> {
    const updated = await api.updateCharacter(id, patch);
    const idx = characters.value.findIndex((c) => c.id === id);
    if (idx !== -1) {
      characters.value[idx].description = updated.description;
      characters.value[idx].data_raw = updated.data_raw;
      if (updated.first_mes !== undefined) characters.value[idx].first_mes = updated.first_mes;
      if (updated.alternate_greetings !== undefined) {
        characters.value[idx].alternate_greetings = updated.alternate_greetings;
      }
    }
  }

  async function sendMessage(text: string): Promise<void> {
    const cid = currentCharacterId.value;
    if (!cid || generating.value) return;

    // 无会话时先新建(后端注入开场白,保证角色上下文完整)
    if (!currentSessionId.value) await newSession();

    messages.value.push({ id: -Date.now(), role: 'user', content: text, extra: {}, streaming: false });
    await startStream(text);
  }

  /** 发起流式生成(用户消息已就位时不重复 push,供「编辑后重发」复用) */
  async function startStream(
    text: string,
    opts: { resendMessageId?: number; regenerateAssistantId?: number } = {},
  ): Promise<void> {
    const cid = currentCharacterId.value;
    if (!cid || generating.value) return;
    if (!currentSessionId.value) await newSession();

    generating.value = true;
    agent.value = { ...idleAgent(), phase: 'planning', stepText: '计划中…' };

    chatStream.start(
      {
        session_id: currentSessionId.value ?? undefined,
        character_id: cid,
        message: text,
        agent_mode: agentMode.value,
        temperature: temperature.value,
        top_p: topP.value,
        max_tokens: maxTokens.value,
        resend_message_id: opts.resendMessageId,
        regenerate_assistant_id: opts.regenerateAssistantId,
      },
      onSseEvent,
    );
  }

  /**
   * 重发用户消息:可选新文本(编辑后重发)→ 截断该消息之后的所有消息 → 重新触发生成。
   * 不传 newText 时保持原内容直接重发。
   * 必须先刷新历史取落库后的真实 id:流式期间的临时消息是负 id,若直接用负 id 作
   * 截断锚点,服务端按 id>anchor 删除会把 anchor 及全部历史一并删掉,列表与数据库
   * 同时清空(表现为「聊天记录消失」)。
   */
  async function resendMessage(id: number, newText?: string): Promise<void> {
    const sid = currentSessionId.value;
    if (!sid || generating.value) return;

    // 1) 刷新历史,拿到该消息落库后的真实 id(流式临时消息 id 为负,未落库)
    await loadHistory(sid);
    const msg = messages.value.find((m) => m.id === id);
    if (!msg) {
      console.warn('重发目标消息不存在(刷新历史后仍未找到),已中止');
      return;
    }
    const anchorId = msg.id;
    if (anchorId <= 0) {
      // 刷新后仍未落库(如 loadHistory 失败):拒绝操作,绝不能拿负 id 截断
      console.warn('重发目标消息尚未落库,请稍后重试');
      return;
    }

    if (newText !== undefined && newText !== null && newText.trim() !== '') {
      const updated = await api.updateMessage(sid, anchorId, newText);
      const idx = messages.value.findIndex((m) => m.id === anchorId);
      if (idx >= 0) {
        messages.value[idx].content = updated.content;
        messages.value[idx].extra = updated.extra;
        delete messages.value[idx].content_display; // 同 updateMessage:清除陈旧宏展开文本
      }
    }

    const idx = messages.value.findIndex((m) => m.id === anchorId);
    if (idx < 0) return;
    const text = messages.value[idx].content;
    if (!text.trim()) return;

    // 丢弃该消息之后的所有上下文(前端 + 后端);anchor 为落库后的真实 id,恒为正
    await api.truncateMessages(sid, anchorId);
    // 变量树回滚:取 anchor 前最近一条 assistant 消息的 extra.mvu 快照推回后端
    // 覆盖会话树,新分支基于截断点的变量状态(而非旧分支的当前值);
    // 无快照则推空树,引擎下次请求时从 [InitVar] 重新初始化
    const kept = messages.value.slice(0, idx + 1);
    try {
      let snap: api.MvuSnapshot | undefined;
      for (let i = kept.length - 1; i >= 0; i--) {
        const m = kept[i];
        if (m.role === 'assistant') {
          snap = (m.extra as { mvu?: api.MvuSnapshot } | undefined)?.mvu;
          if (snap?.stat_data) break;
        }
      }
      await api.saveAssistantVars(sid, snap?.stat_data ?? {});
      mvuVariables.value = replayMvuVariables(kept);
    } catch (e) {
      console.warn('回滚变量树失败(仅影响变量状态,不影响重发)', e);
    }
    messages.value = kept;
    // 服务端验证锚点仍为当前会话最后一条 user 消息且正文一致。
    await startStream(text, { resendMessageId: anchorId });
  }

  /**
   * 生成新版本(阶段六 6f):对该 assistant 消息重新生成。
   * 原消息保留在后端(引擎收尾原地更新该行,旧内容并入 extra.swipes,id 稳定);
   * 前端清空其内容进入流式(sseReducer 的 token 事件追加到最后一条 assistant 消息,
   * 即本条),变量树回滚到该消息之前的最近 assistant 快照(复用 resendMessage 管线)。
   */
  async function regenerateMessage(id: number): Promise<void> {
    const sid = currentSessionId.value;
    if (!sid || generating.value) return;

    // 先刷新历史取落库后的真实 id(与 resendMessage 同理由:流式临时消息为负 id)
    await loadHistory(sid);
    const idx = messages.value.findIndex((m) => m.id === id);
    if (idx < 0) {
      console.warn('重生成目标消息不存在(刷新历史后仍未找到),已中止');
      return;
    }
    const msg = messages.value[idx];
    if (msg.role !== 'assistant' || id <= 0) {
      console.warn('重生成目标消息尚未落库或不是 assistant 消息,已中止');
      return;
    }
    // 服务端锚点要求目标为最后一条消息;其后若还有消息(异常状态)先截断
    if (idx !== messages.value.length - 1) {
      await api.truncateMessages(sid, id);
      messages.value = messages.value.slice(0, idx + 1);
    }
    // 取该消息之前最近一条 user 消息作为生成输入
    let userText = '';
    for (let i = idx - 1; i >= 0; i--) {
      const m = messages.value[i];
      if (m.role === 'user') {
        userText = m.content;
        break;
      }
    }
    if (!userText.trim()) {
      console.warn('重生成目标消息前没有可用的用户消息,已中止');
      return;
    }
    // 变量树回滚:取该消息之前最近一条 assistant 消息的 extra.mvu 快照推回后端
    // 覆盖会话树,新分支基于旧分支此点的变量状态(不含本条旧快照)
    const kept = messages.value.slice(0, idx);
    try {
      let snap: api.MvuSnapshot | undefined;
      for (let i = kept.length - 1; i >= 0; i--) {
        const m = kept[i];
        if (m.role === 'assistant') {
          snap = (m.extra as { mvu?: api.MvuSnapshot } | undefined)?.mvu;
          if (snap?.stat_data) break;
        }
      }
      await api.saveAssistantVars(sid, snap?.stat_data ?? {});
      mvuVariables.value = replayMvuVariables(kept);
    } catch (e) {
      console.warn('回滚变量树失败(仅影响变量状态,不影响重生成)', e);
    }
    // 清空旧内容进入流式:保留 id/extra(extra.swipes 为旧版本数组,后端收尾会追加新版本)
    messages.value[idx] = { ...msg, content: '', streaming: true };
    await startStream(userText, { regenerateAssistantId: id });
  }

  /**
   * 切换消息 swipe 版本(阶段六 6f):调后端切换 content 列并写入 extra.swipe_id,
   * 前端同步该消息显示内容,清除陈旧宏展开文本。
   */
  async function swipeMessage(id: number, swipeId: number): Promise<void> {
    const sid = currentSessionId.value;
    if (!sid || generating.value) return;
    try {
      const res = await api.swipeMessage(sid, id, swipeId);
      const idx = messages.value.findIndex((m) => m.id === id);
      if (idx >= 0) {
        const m = messages.value[idx];
        messages.value[idx] = { ...m, content: res.content, extra: { ...m.extra, swipe_id: res.swipe_id } };
        delete messages.value[idx].content_display;
      }
    } catch (e) {
      console.warn('切换 swipe 版本失败', e);
    }
  }

  function onSseEvent(event: api.SseEvent): void {
    // 事件日志采集(阶段六 6c):全部 SSE 事件(含本地合成事件,如 stop/interrupted)
    // 流经本入口,在此一次性采集;cap 500 丢最旧,带当前会话上下文便于面板过滤。
    eventLog.value.push({
      ts: Date.now(),
      session_id: currentSessionId.value ?? '',
      event,
    });
    if (eventLog.value.length > 500) eventLog.value.splice(0, eventLog.value.length - 500);
    // 事件语义处理在 sseReducer.ts(reduceSseEvent 纯函数);此处仅应用变更到响应式状态
    const changes = reduceSseEvent(
      {
        agent: agent.value,
        messages: messages.value,
        generating: generating.value,
        lastUsage: lastUsage.value,
        contextTokens: contextTokens.value,
        mvuVariables: mvuVariables.value,
      },
      event,
    );
    if (changes.generating !== undefined) generating.value = changes.generating;
    if (changes.lastUsage !== undefined) {
      lastUsage.value = changes.lastUsage;
      // 缓存命中率跟踪:连续两次为 0 则隐藏显示
      const u = changes.lastUsage;
      if (u && u.prompt_tokens > 0) {
        const hit = u.prompt_cache_hit_tokens ?? 0;
        if (hit === 0) {
          cacheZeroStreak.value += 1;
        } else {
          cacheZeroStreak.value = 0;
        }
      }
    }
    if (changes.contextTokens !== undefined) contextTokens.value = changes.contextTokens;
    if (changes.mvuVariables) mvuVariables.value = changes.mvuVariables;
    if (changes.applyMvuOn) void applyMvuUpdate(changes.applyMvuOn);
    if (changes.reloadHistory && currentSessionId.value) void loadHistory(currentSessionId.value);
    // 生成时自动展开右侧 Agent 面板(Deep/Agent/Custom 模式)
    if (generating.value && ['deep', 'agent', 'custom'].includes(agentMode.value)) {
      agentPanelOpen.value = true;
    }
  }

  function stop(): void {
    void chatStream.stop(currentSessionId.value)
      .catch((e) => console.warn('通知后端停止生成失败', e));
    onSseEvent({ type: 'interrupted' });
  }

  async function testConnection(): Promise<void> {
    try {
      const res = await api.testConnect();
      connStatus.value = res.ok ? 'ok' : 'fail';
      connMessage.value = res.message;
      // 连接测试返回的模型列表顺带填充 store.models(输入栏模型下拉数据源)
      if (res.models?.length) models.value = res.models;
    } catch (e) {
      connStatus.value = 'fail';
      connMessage.value = (e as Error).message;
    }
  }

  async function loadModel(): Promise<void> {
    try {
      model.value = await api.getModel();
    } catch {
      /* 忽略 */
    }
  }

  async function loadModels(): Promise<void> {
    try {
      models.value = await api.listModels();
    } catch {
      /* 忽略 */
    }
  }

  async function switchModel(m: string): Promise<boolean> {
    const res = await api.switchModel(m);
    model.value = res.model;
    return res.changed;
  }

  /** 加载运行期设置(API 连接 + 生成参数),仅回填本地默认值 */
  async function loadSettings(): Promise<void> {
    try {
      const s = await api.getSettings();
      temperature.value = s.default_temperature;
      topP.value = s.default_top_p;
      maxTokens.value = s.default_max_tokens;
      maxContextTokens.value = s.max_context_tokens;
      maxToolRounds.value = s.max_tool_rounds ?? 32;
      agentSystemPrompt.value = s.agent_system_prompt ?? '';
      searchEndpoint.value = s.search_endpoint ?? '';
      mvuVarsPosition.value = (s.mvu_vars_position ?? 'system') as 'system' | 'user_tail';
      reflectPrompt.value = s.reflect_prompt ?? '';
      presetTailPrompt.value = s.preset_tail_prompt ?? '';
      presetTailRole.value = s.preset_tail_role === 'assistant' ? 'assistant' : 'user';
      reflectAdvicePrompt.value = s.reflect_advice_prompt ?? '';
      reflectAdviceRole.value = s.reflect_advice_role === 'assistant' ? 'assistant' : 'user';
      bypassMode.value = s.bypass_mode ?? false;
      defaultRenderHtml.value = s.render_html ?? false;
      // 无角色卡记忆时,当前生效值跟随全局默认(角色卡记忆优先)
      syncRenderHtmlToCurrent();
      compactionMode.value = (s.compaction_mode as 'off' | 'manual' | 'auto') ?? 'off';
      compactionThreshold.value = s.compaction_threshold ?? 0.8;
      if (!model.value) model.value = s.model;
    } catch {
      /* 忽略 */
    }
  }

  /** 保存运行期设置(可部分字段;服务端持久化到 data/settings.json) */
  async function saveSettings(patch: api.RuntimeSettingsPatch): Promise<void> {
    const res = await api.saveSettings(patch);
    const s = res.settings;
    temperature.value = s.default_temperature;
    topP.value = s.default_top_p;
    maxTokens.value = s.default_max_tokens;
    maxContextTokens.value = s.max_context_tokens;
    maxToolRounds.value = s.max_tool_rounds ?? 32;
    model.value = s.model;
    agentSystemPrompt.value = s.agent_system_prompt ?? '';
    searchEndpoint.value = s.search_endpoint ?? '';
    mvuVarsPosition.value = (s.mvu_vars_position ?? 'system') as 'system' | 'user_tail';
    reflectPrompt.value = s.reflect_prompt ?? '';
    presetTailPrompt.value = s.preset_tail_prompt ?? '';
    presetTailRole.value = s.preset_tail_role === 'assistant' ? 'assistant' : 'user';
    reflectAdvicePrompt.value = s.reflect_advice_prompt ?? '';
    reflectAdviceRole.value = s.reflect_advice_role === 'assistant' ? 'assistant' : 'user';
    bypassMode.value = s.bypass_mode ?? false;
    defaultRenderHtml.value = s.render_html ?? false;
    syncRenderHtmlToCurrent();
    compactionMode.value = (s.compaction_mode as 'off' | 'manual' | 'auto') ?? 'off';
    compactionThreshold.value = s.compaction_threshold ?? 0.8;
  }

  // 手动压缩当前会话历史(阶段借鉴 harness):调用后端摘要,成功后刷新历史展示。
  // 模式为 off 时后端拒绝;历史不足时返回 compacted=false(前端给出对应提示)。
  async function compactCurrentChat(): Promise<{ compacted: boolean }> {
    const sid = currentSessionId.value;
    if (!sid) {
      throw new Error('当前无会话,无法压缩');
    }
    const res = await api.compactChat(sid);
    if (res.compacted) {
      await loadHistory(sid);
    }
    return res;
  }

  // 撤销压缩(阶段借鉴 harness):清除摘要行恢复完整原文历史,成功后刷新历史展示。
  async function clearCurrentCompaction(): Promise<{ cleared: boolean }> {
    const sid = currentSessionId.value;
    if (!sid) {
      throw new Error('当前无会话,无法恢复');
    }
    const res = await api.clearCompactChat(sid);
    if (res.cleared) {
      await loadHistory(sid);
    }
    return res;
  }

  // 设置写入统一串行,避免 renderHtml 自动保存与设置面板保存交错回写旧响应。
  let settingsSaveQueue: Promise<void> = Promise.resolve();
  function queueSettingsSave(patch: api.RuntimeSettingsPatch): Promise<void> {
    const queued = settingsSaveQueue.then(() => saveSettings(patch));
    settingsSaveQueue = queued.catch(() => { /* 保持队列可继续使用 */ });
    return queued;
  }

  /**
   * 按当前角色卡同步 HTML 渲染开关生效值:有角色卡记忆用记忆,否则用全局默认。
   * 程序性赋值走 restoring 闩锁,不触发持久化写入(避免为默认值凭空生成记忆)。
   */
  function syncRenderHtmlToCurrent(): void {
    const cid = currentCharacterId.value;
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
      const cid = currentCharacterId.value;
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

  /** 删除一条本地消息(同步后端)。负 id 为未落库的临时气泡(如中断残留),从未入库,
   *  删除接口按 id 必然命中 0 行返回 404;正 id 删除失败(如网络错误)也不阻塞本地移除,
   *  最终以服务端历史为准。 */
  async function removeMessage(id: number): Promise<void> {
    const sid = currentSessionId.value;
    if (!sid) return;
    if (id > 0) {
      try {
        await api.deleteMessage(sid, id);
      } catch (e) {
        console.warn('删除服务端消息失败(本地仍移除,刷新后以服务端为准)', e);
      }
    }
    messages.value = messages.value.filter((m) => m.id !== id);
  }

  /** 编辑一条消息内容 */
  async function updateMessage(id: number, content: string): Promise<void> {
    const sid = currentSessionId.value;
    if (!sid) return;
    const updated = await api.updateMessage(sid, id, content);
    const idx = messages.value.findIndex((m) => m.id === id);
    if (idx !== -1) {
      messages.value[idx].content = updated.content;
      messages.value[idx].extra = updated.extra;
      // 清除旧的宏展开显示文本:PUT 端点不重新生成 content_display,
      // 保留旧值会导致气泡继续显示编辑前的文本(下次 loadHistory 才会刷新)
      delete messages.value[idx].content_display;
    }
  }

  /** 清空当前会话 */
  async function clearCurrentChat(): Promise<void> {
    const sid = currentSessionId.value;
    if (!sid) return;
    await api.clearMessages(sid);
    messages.value = [];
    contextTokens.value = 0;
  }

  /**
   * 导出当前会话(保存 JSON),返回导出文件名。
   * 桌面版弹出系统保存对话框由用户选择导出位置;浏览器回退常规下载。
   * 用户取消对话框时返回 null(调用方不提示成功/失败);无会话时抛错。
   */
  async function exportCurrentChat(): Promise<string | null> {
    const sid = currentSessionId.value;
    if (!sid) throw new Error('没有可导出的会话');
    const data = await api.exportChat(sid);
    const fileName = `kedai-export-${new Date().toISOString().replace(/[:.]/g, '-')}.json`;
    const saved = await saveExportFile(fileName, JSON.stringify(data, null, 2));
    return saved ? fileName : null;
  }

  // ===== 世界书 =====

  async function loadWorldBooks(): Promise<void> {
    try {
      worldBooks.value = await api.listWorldBooks();
    } catch (e) {
      console.error('加载世界书失败', e);
    }
  }

  async function uploadWorldBook(file: File, characterId?: string): Promise<api.WorldBookRecord> {
    const record = await api.uploadWorldBook(file, characterId);
    await loadWorldBooks();
    return record;
  }

  async function updateWorldBook(id: string, patch: { enabled?: boolean; character_id?: string; name?: string }): Promise<void> {
    const updated = await api.updateWorldBook(id, patch);
    const idx = worldBooks.value.findIndex((b) => b.id === id);
    if (idx !== -1) worldBooks.value[idx] = updated;
  }

  async function deleteWorldBook(id: string): Promise<void> {
    await api.deleteWorldBook(id);
    await loadWorldBooks();
  }

  // ===== 快速回复(阶段四 4b) =====

  async function loadQuickReplies(): Promise<void> {
    try {
      quickReplies.value = await api.listQuickReplies(true);
    } catch (e) {
      console.error('加载快速回复失败', e);
    }
  }

  // ===== 音频播放器(阶段五 5a) =====

  async function loadAudio(): Promise<void> {
    try {
      audio.value = await api.getAudio();
    } catch (e) {
      console.error('加载音频配置失败', e);
    }
  }

  /** 保存单通道设置(部分字段合并);成功后回填全量状态 */
  async function saveAudioSettings(
    type: api.AudioChannelType,
    settings: Partial<api.AudioChannelSettings>,
  ): Promise<void> {
    try {
      audio.value = await api.updateAudioSettings(type, settings);
    } catch (e) {
      console.error('音频设置保存失败', e);
      throw e;
    }
  }

  /** 保存单通道播放列表;成功后回填全量状态 */
  async function saveAudioPlaylist(
    type: api.AudioChannelType,
    tracks: api.AudioTrack[],
  ): Promise<void> {
    try {
      audio.value = await api.updateAudioPlaylist(type, tracks);
    } catch (e) {
      console.error('音频播放列表保存失败', e);
      throw e;
    }
  }

  // ===== 技能库 =====

  async function loadSkills(): Promise<void> {
    try {
      skills.value = await api.listSkills();
    } catch (e) {
      console.error('加载技能失败', e);
    }
  }

  /** 从 JSON 文件导入技能(支持单对象 / 数组 / {skills:[...]} 包壳) */
  async function importSkillsFile(file: File): Promise<number> {
    const text = await file.text();
    const parsed: unknown = JSON.parse(text);
    let items: api.SkillImportItem[];
    if (Array.isArray(parsed)) {
      items = parsed as api.SkillImportItem[];
    } else if (parsed && typeof parsed === 'object' && Array.isArray((parsed as { skills?: unknown }).skills)) {
      items = (parsed as { skills: api.SkillImportItem[] }).skills;
    } else {
      items = [parsed as api.SkillImportItem];
    }
    const res = await api.importSkills(items);
    await loadSkills();
    return res.imported;
  }

  /** 切换技能启用状态 */
  async function toggleSkill(s: api.SkillRecord): Promise<void> {
    await api.updateSkill(s.id, { enabled: !s.enabled });
    await loadSkills();
  }

  /** 删除技能 */
  async function removeSkill(id: string): Promise<void> {
    await api.deleteSkill(id);
    await loadSkills();
  }

  return {
    // 状态
    characters,
    currentCharacterId,
    currentSessionId,
    sessions,
    messages,
    generating,
    agent,
    connStatus,
    connMessage,
    lastUsage,
    contextTokens,
    settingsOpen,
    promptsOpen,
    agentDockOpen,
    agentPanelOpen,
    agentMode,
    bypassMode,
    cacheZeroStreak,
    sessionTotalTokens,
    globalTotalTokens,
    splashDone,
    temperature,
    topP,
    maxTokens,
    maxContextTokens,
    maxToolRounds,
    compactionMode,
    compactionThreshold,
    model,
    models,
    searchQuery,
    // 世界书
    worldBooks,
    worldBooksOpen,
    // 快速回复(阶段四 4b)
    quickReplies,
    quickRepliesOpen,
    // 音频播放器(阶段五 5a)
    audio,
    audioOpen,
    // 插件
    pluginsOpen,
    // 技能库
    skillsOpen,
    skills,
    // 契约编辑(P6 面板)
    contractsOpen,
    // 用户脚本(阶段三)
    scriptsOpen,
    // 宏调试(阶段六 6b)
    macrosOpen,
    // 事件监控(阶段六 6c)
    eventLog,
    eventsOpen,
    // 优化面板(阶段六 6d)
    optimizeOpen,
    // Agent 设置草稿
    agentSystemPrompt,
    searchEndpoint,
    mvuVarsPosition,
    reflectPrompt,
    presetTailPrompt,
    presetTailRole,
    reflectAdvicePrompt,
    reflectAdviceRole,
    // 提示词注入
    promptInject,
    // 自定义执行流程(流程库)
    agentFlowLibrary,
    // 界面
    renderHtml,
    currentScriptHash,
    currentScriptAuthorized,
    scriptAuthorizations,
    chatRecordsOpen,
    // mvu 变量
    mvuVariables,
    initVarEntries,
    // 派生
    currentCharacter,
    currentGreetings,
    currentCharacterName,
    filteredCharacters,
    // 动作
    loadCharacters,
    selectCharacter,
    loadSessions,
    newSession,
    switchGreeting,
    switchSession,
    loadHistory,
    loadTokenTotals,
    updateContextTokens,
    uploadCharacter,
    deleteCharacter,
    authorizeCurrentCharacterScripts,
    revokeCharacterScripts,
    isCharacterScriptAuthorized,
    updateCharacterPrompt,
    sendMessage,
    onSseEvent,
    stop,
    testConnection,
    loadModel,
    loadModels,
    switchModel,
    loadSettings,
    saveSettings,
    compactCurrentChat,
    clearCurrentCompaction,
    loadPromptInject,
    savePromptInjectConfig,
    loadAgentFlow,
    saveAgentFlowConfig,
    selectAgentFlow,
    deleteAgentFlow,
    removeMessage,
    updateMessage,
    resendMessage,
    regenerateMessage,
    swipeMessage,
    clearCurrentChat,
    exportCurrentChat,
    // 世界书动作
    loadWorldBooks,
    uploadWorldBook,
    updateWorldBook,
    deleteWorldBook,
    // 快速回复动作(阶段四 4b)
    loadQuickReplies,
    // 音频播放器动作(阶段五 5a)
    loadAudio,
    saveAudioSettings,
    saveAudioPlaylist,
    // 技能库动作
    loadSkills,
    importSkillsFile,
    toggleSkill,
    removeSkill,
  };
});
