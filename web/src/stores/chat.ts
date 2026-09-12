// 聊天 store:会话/消息/流式生成(SSE)/swipe/重发/重生成、Token 统计、mvu 变量树、
// SSE 事件日志采集、会话压缩与聊天导入导出。
// 从 store.ts 按领域拆分。eventLog 留在本 store:SSE 事件采集是聊天流基础设施
// (任务事件的合成上报也经 onSseEvent 入口)。跨 store 引用(currentCharacterId /
// 生成参数 / agentPanelOpen)均在动作或回调运行时解析,setup 阶段不实例化其他 store。
import { defineStore } from 'pinia';
import { ref } from 'vue';
import * as api from '../api';
import { emitMvuEvent } from '../mvu/host';
import { parseUpdateVariable } from '../mvu/parser';
import { broadcastCardEvent, broadcastMvuUpdate } from '../characterScriptSandbox';
// SSE 事件纯函数化 + mvu 状态纯函数(经 '../api' 类型依赖)
import { idleAgent, reduceSseEvent } from '../sseReducer';
import { applyMvuCommands, replayMvuVariables } from '../mvu/mvuStore';
import type { MvuVars } from '../mvu/mvuStore';
import type { AgentActivity, UiMessage } from '../sseReducer';
import { createChatStreamService } from '../chatStreamService';
import { saveExportFile } from '../exportFile';
import { writeLastSessionId } from '../lastPosition';
import type { ApiEventLogEntry } from '../devTools';
import { useCharacterStore } from './character';
import { useGenSettingsStore } from './genSettings';
import { useUiPrefsStore } from './uiPrefs';

const chatStream = createChatStreamService({
  streamChat: api.streamChat,
  stopChat: api.stopChat,
});

export const useChatStore = defineStore('app.chat', () => {
  // ===== 状态 =====
  const currentSessionId = ref<string | null>(null);
  const sessions = ref<api.SessionInfo[]>([]);
  const messages = ref<UiMessage[]>([]);
  const generating = ref(false);
  const agent = ref<AgentActivity>(idleAgent());
  const lastUsage = ref<api.TokenUsage | null>(null);
  const contextTokens = ref(0);
  const agentMode = ref<api.AgentMode>('fast');
  /** 缓存命中率连续为 0 的次数(连续两次为 0 则隐藏命中率显示) */
  const cacheZeroStreak = ref(0);
  /** 当前会话累计 token(后端接口返回) */
  const sessionTotalTokens = ref(0);
  /** 全局累计 token(后端接口返回) */
  const globalTotalTokens = ref(0);
  /** mvu 变量树(stat_data/display_data),随历史回放与消息更新 */
  const mvuVariables = ref<MvuVars>({
    stat_data: {},
    display_data: {},
  });
  /** 当前角色 [InitVar] 初始变量(按条目 comment 保存原文,由 mvu 模块收集) */
  const initVarEntries = ref<Record<string, string>>({});
  /** SSE 事件日志(前端采集,cap 500 丢最旧;事件监控面板数据源) */
  const eventLog = ref<ApiEventLogEntry[]>([]);

  // ===== 会话管理 =====
  async function loadSessions(characterId: string): Promise<void> {
    sessions.value = await api.listSessions(characterId);
  }

  /** 新建会话并切换过去(后端会注入角色开场白作为首条消息);greetingIndex=0 主开场,1..=备用开场 */
  async function newSession(greetingIndex = 0): Promise<void> {
    const cid = useCharacterStore().currentCharacterId;
    if (!cid) return;
    const session = await api.createSession(cid, undefined, greetingIndex);
    sessions.value.unshift(session);
    currentSessionId.value = session.id;
    writeLastSessionId(cid, session.id);
    lastUsage.value = null;
    agent.value = idleAgent();
    // 新会话恢复自动展开语义
    useUiPrefsStore().resetAgentPanelAutoSuppress();
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
    // 新会话恢复自动展开语义(上一会话的「不再自动弹」不跨越会话边界)
    useUiPrefsStore().resetAgentPanelAutoSuppress();
    const cid = useCharacterStore().currentCharacterId;
    if (cid) writeLastSessionId(cid, id);
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
      // 恢复该会话的 Agent 记录(实跑问题 4:此前 agent 纯内存,切会话/重启即丢;
      // 后端 tool_calls 已持久化,这里按会话读回工具调用列表)
      void restoreAgentTrace(sessionId);
    } catch (e) {
      console.error('加载历史失败', e);
      // 历史加载失败必须可见:静默失败 + sendMessage 自动新建会话会把旧会话「藏」起来,
      // 表现为聊天记录丢失
      useUiPrefsStore().dataLoadError = `加载聊天记录失败:${(e as Error).message ?? e}`;
    }
  }

  /**
   * 恢复角色扮演模式的 Agent 记录(只读):读 agent_sessions + tool_calls,
   * 把工具调用列表回填到 agent.value(状态置 done,历史记录不会处于 running)。
   * 无记录 / 请求失败时保持空闲态,不阻断聊天主流程。
   */
  async function restoreAgentTrace(sessionId: string): Promise<void> {
    try {
      const trace = await api.fetchAgentTrace(sessionId);
      // 会话已切换:迟到的响应不得覆盖当前会话(loadHistory 并发可重入)
      if (currentSessionId.value !== sessionId) return;
      if (!trace || trace.tool_calls.length === 0) {
        agent.value = idleAgent();
        return;
      }
      const base = idleAgent();
      base.phase = trace.state === 'idle' ? 'finished' : trace.state;
      base.toolCalls = trace.tool_calls.map((c) => ({
        name: c.name,
        input: c.input,
        output: c.output,
        status: 'done' as const,
      }));
      agent.value = base;
    } catch {
      // 静默:Agent 记录是辅助展示,失败保持空闲态
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
    if (parsed.commands.length === 0 && !parsed.initvar) return;
    // initvar 仅在变量树为空时播种(开场初始化),随后按序应用 _.set/JSONPatch 命令
    mvuVariables.value = applyMvuCommands(mvuVariables.value, parsed.commands, parsed.initvar);
    // 兼容原版 MagVarUpdate:变量更新后触发 mag_variable_updated 事件
    emitMvuEvent('mag_variable_updated', {
      stat_data: mvuVariables.value.stat_data,
      display_data: mvuVariables.value.display_data,
    });
    // 广播给存活的角色卡脚本沙箱(wuwa 状态栏 eventOn 订阅随之重渲染)
    broadcastMvuUpdate(mvuVariables.value);
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

  // ===== 发送与流式生成 =====
  async function sendMessage(text: string): Promise<void> {
    const cid = useCharacterStore().currentCharacterId;
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
    const cid = useCharacterStore().currentCharacterId;
    if (!cid || generating.value) return;
    if (!currentSessionId.value) await newSession();

    generating.value = true;
    agent.value = { ...idleAgent(), phase: 'planning', stepText: '计划中…' };
    // 生成开始时自动展开右侧 Agent 面板(仅 Deep/Agent/Custom 模式,且用户本轮未主动收起过)。
    // 只在这一次置位;生成期间的 SSE 事件不再干预面板开合(详见 AgentPanel.showPanel)。
    if (['deep', 'agent', 'custom'].includes(agentMode.value)) {
      useUiPrefsStore().autoOpenAgentPanel();
    }

    const genSettings = useGenSettingsStore();
    chatStream.start(
      {
        session_id: currentSessionId.value ?? undefined,
        character_id: cid,
        message: text,
        agent_mode: agentMode.value,
        temperature: genSettings.temperature,
        top_p: genSettings.topP,
        max_tokens: genSettings.maxTokens,
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
      broadcastMvuUpdate(mvuVariables.value);
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
      broadcastMvuUpdate(mvuVariables.value);
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
        // 卡级脚本事件流:广播 swipe 切换,payload 为楼层序号(0-based 数组下标,
        // 对齐酒馆 MESSAGE_SWIPED 的 message_id 语义——舰娘卡「随开场白切换世界书」
        // 脚本判定 messageId===0 即第一条开场白)。切会话/历史加载后的状态漂移由
        // 脚本自带的轮询兜底,事件只保证即时性。
        broadcastCardEvent('message_swiped', idx);
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
      // 任务模式合成事件不隶属聊天会话:空 session_id(面板会话过滤对 task 类型始终放行)
      session_id: event.type === 'task' ? '' : (currentSessionId.value ?? ''),
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
  }

  function stop(): void {
    void chatStream.stop(currentSessionId.value)
      .catch((e) => console.warn('通知后端停止生成失败', e));
    onSseEvent({ type: 'interrupted' });
  }

  // ===== 会话压缩(阶段借鉴 harness) =====

  // 手动压缩当前会话历史:调用后端摘要,成功后刷新历史展示。
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

  // 撤销压缩:清除摘要行恢复完整原文历史,成功后刷新历史展示。
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

  // ===== 消息编辑与清空 =====

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
    // 清空即回到空会话状态:恢复自动展开语义
    useUiPrefsStore().resetAgentPanelAutoSuppress();
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

  return {
    currentSessionId,
    sessions,
    messages,
    generating,
    agent,
    lastUsage,
    contextTokens,
    agentMode,
    cacheZeroStreak,
    sessionTotalTokens,
    globalTotalTokens,
    mvuVariables,
    initVarEntries,
    eventLog,
    loadSessions,
    newSession,
    switchGreeting,
    switchSession,
    loadHistory,
    loadTokenTotals,
    updateContextTokens,
    sendMessage,
    startStream,
    resendMessage,
    regenerateMessage,
    swipeMessage,
    onSseEvent,
    stop,
    compactCurrentChat,
    clearCurrentCompaction,
    removeMessage,
    updateMessage,
    clearCurrentChat,
    exportCurrentChat,
  };
});
