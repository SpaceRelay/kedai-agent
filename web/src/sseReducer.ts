// SSE 事件 → 状态变更(纯函数化:就地更新传入的状态片段,返回值字段变更与副作用钩子,store 负责应用)
// 事件语义与原 store.onSseEvent 完全一致,不改变任何对外行为。
import type { SseEvent, TokenUsage } from './api';
import type { MvuVars } from './mvu/mvuStore';

export interface AgentActivity {
  /** 当前执行阶段:planning | executing | tool_call | reflecting | finished | interrupted | error | idle */
  phase: string;
  stepText: string;
  detail: string;
  /** 推理链(完整步骤记录) */
  chain: Array<{ at: number; text: string; detail?: string }>;
  /** 工具调用预告 */
  pendingTool: { name: string; input: unknown } | null;
  /** 完整工具调用列表(AGENT 模式多轮) */
  toolCalls: Array<{
    name: string;
    input: unknown;
    output?: unknown;
    status: 'running' | 'done' | 'error' | 'authorization_required';
    callId?: string;
    runId?: string;
    risk?: 'safe' | 'sensitive' | 'dangerous';
    reason?: string;
    /** 工具展示意图(render intent),缺省 undefined → 前端回退 JSON 直出 */
    renderKind?: string;
  }>;
  /** 自定义流程(custom 模式)步骤进度;其余模式为 null */
  flowProgress: { index: number; total: number; name: string } | null;
}

/** 初始 Agent 活动状态(多处重置复用) */
export function idleAgent(): AgentActivity {
  return {
    phase: 'idle',
    stepText: '',
    detail: '',
    chain: [],
    pendingTool: null,
    toolCalls: [],
    flowProgress: null,
  };
}

/** 界面消息(带前端临时字段) */
export interface UiMessage {
  id: number;
  role: 'user' | 'assistant' | 'system';
  content: string;
  /** 服务端宏展开后的显示文本(GET history 生成);编辑后本地清除,避免陈旧展示 */
  content_display?: string;
  extra: Record<string, unknown>;
  /** 流式渲染中的部分内容 */
  streaming?: boolean;
}

/** swipe 版本数组元素:{ swipe_id, content, ts } */
export interface SwipeEntry {
  swipe_id: number;
  content: string;
  ts: number;
}

/** 当前激活版本索引;无 swipes(单版本)时返回 -1 */
export function swipeIndex(extra: Record<string, unknown>): number {
  const idx = extra.swipe_id;
  return typeof idx === 'number' && Number.isInteger(idx) ? idx : -1;
}

/** 版本总数;无 swipes 时为 0 */
export function swipeCount(extra: Record<string, unknown>): number {
  const arr = extra.swipes;
  return Array.isArray(arr) ? arr.length : 0;
}

/** 传入 reducer 的可变状态片段(agent / messages 就地更新,保持响应式引用) */
export interface SseStateSlice {
  agent: AgentActivity;
  messages: UiMessage[];
  generating: boolean;
  lastUsage: TokenUsage | null;
  contextTokens: number;
  mvuVariables: MvuVars;
}

/** reducer 返回的变更描述(值类型字段 + finish 侧效应钩子) */
export interface SseStateChanges {
  generating?: boolean;
  lastUsage?: TokenUsage | null;
  contextTokens?: number;
  mvuVariables?: MvuVars;
  /** finish 且非空正文:需要解析变量块的消息(store 负责调用 applyMvuUpdate) */
  applyMvuOn?: UiMessage | null;
  /** finish 事件应刷新历史(会话存在时) */
  reloadHistory?: boolean;
}

export function reduceSseEvent(state: SseStateSlice, event: SseEvent): SseStateChanges {
  const a = state.agent;
  switch (event.type) {
    case 'step':
      a.stepText = event.step;
      a.detail = event.detail ?? '';
      if (event.step === '计划中…') a.chain = [];
      a.chain.push({ at: Date.now(), text: event.step, detail: event.detail });
      // 反思重试:清空上一条流式 assistant 消息,避免两次生成内容拼接显示
      if (event.step === '重新生成') {
        const last = state.messages[state.messages.length - 1];
        if (last && last.role === 'assistant' && last.id < 0) last.content = '';
      }
      // 自定义流程进度(custom 模式):按步骤名展示「x/y」
      if (event.index !== undefined && event.total !== undefined) {
        a.flowProgress = { index: event.index, total: event.total, name: event.detail ?? '' };
      }
      if (event.step === '计划中…') a.phase = 'planning';
      else if (event.step.includes('反思')) a.phase = 'reflecting';
      else if (event.step.includes('生成')) a.phase = 'executing';
      break;
    case 'tool_call':
      a.pendingTool = { name: event.name, input: event.input };
      a.toolCalls.push({ name: event.name, input: event.input, status: 'running', callId: event.call_id, renderKind: event.render_kind });
      a.phase = 'tool_call';
      a.chain.push({ at: Date.now(), text: `调用工具 ${event.name}`, detail: JSON.stringify(event.input) });
      break;
    case 'tool_authorization_required':
      for (let i = a.toolCalls.length - 1; i >= 0; i--) {
        const t = a.toolCalls[i];
        if ((event.call_id ? t.callId === event.call_id : t.name === event.name) && t.status === 'running') {
          t.status = 'authorization_required';
          t.runId = event.run_id;
          t.risk = event.risk;
          t.reason = event.reason;
          break;
        }
      }
      const pending = [...a.toolCalls].reverse().find((t) => t.status === 'running');
      a.pendingTool = pending ? { name: pending.name, input: pending.input } : null;
      a.chain.push({ at: Date.now(), text: `工具 ${event.name} 等待授权`, detail: event.reason });
      break;
    case 'tool_result':
      a.pendingTool = null;
      // call_id 优先保证并行/同名工具结果稳定配对；旧服务端事件仍回退到同名逆序匹配。
      for (let i = a.toolCalls.length - 1; i >= 0; i--) {
        const t = a.toolCalls[i];
        if ((event.call_id ? t.callId === event.call_id : t.name === event.name)
          && (t.status === 'running' || t.status === 'authorization_required')) {
          t.output = event.output;
          t.status = event.output && typeof event.output === 'object' && 'error' in event.output ? 'error' : 'done';
          if (event.render_kind !== undefined) t.renderKind = event.render_kind;
          break;
        }
      }
      a.chain.push({ at: Date.now(), text: `工具 ${event.name} 返回`, detail: JSON.stringify(event.output) });
      break;
    case 'token': {
      const last = state.messages[state.messages.length - 1];
      if (!last || last.role !== 'assistant') {
        state.messages.push({ id: -Date.now(), role: 'assistant', content: event.text, extra: {}, streaming: true });
      } else {
        last.content += event.text;
        last.streaming = true;
      }
      break;
    }
    case 'interrupted': {
      a.phase = 'interrupted';
      a.stepText = '已中断';
      a.chain.push({ at: Date.now(), text: '生成被中断' });
      // 中断时服务端不落库任何消息:直接削除最后的临时草稿气泡(负 id 未持久化,
      // 刷新后不会出现,却因负 id 无法经删除接口移除——「最后一条草稿删不掉」)。
      // 仅当最后消息已落库(id>0)时才保留并关闭流式状态。
      let droppedUserDraft = false;
      const last = state.messages[state.messages.length - 1];
      if (last && last.role === 'assistant') {
        if (last.id < 0) {
          state.messages.pop();
        } else {
          last.streaming = false;
        }
      }
      // 发送消息时前端先以负 id 临时气泡展示,服务端同步落库为正 id;中断终态
      // 不刷新历史,该临时气泡会残留成「删不掉的草稿」(负 id 无法经删除接口移除)。
      // 末尾仍是未落库的用户气泡(如「编辑后重发」中断)时一并削除,并请求刷新历史,
      // 让服务端正 id 版本回填。
      const tail = state.messages[state.messages.length - 1];
      if (tail && tail.role === 'user' && tail.id < 0) {
        state.messages.pop();
        droppedUserDraft = true;
      }
      return { generating: false, reloadHistory: droppedUserDraft };
    }
    case 'error': {
      // 顶层生成错误终态:模型/上游失败(取代旧「空 finish 伪装成功」)。复位生成态,
      // 清理无内容的临时 assistant 气泡,展示错误信息。
      a.phase = 'error';
      a.stepText = '执行出错';
      a.chain.push({ at: Date.now(), text: `执行出错:${event.message}`, detail: event.code });
      const last = state.messages[state.messages.length - 1];
      if (last && last.role === 'assistant') {
        if (last.id < 0) {
          // 未落库的临时草稿:错误终态下服务端没有落库,一律削除(含部分内容),
          // 避免「最后一条草稿无法删除」的幽灵气泡残留。
          state.messages.pop();
        } else {
          last.streaming = false;
        }
      }
      // 同 interrupted:削除残留的未落库用户临时气泡并请求刷新历史回填正 id
      let droppedUserDraft = false;
      const tail = state.messages[state.messages.length - 1];
      if (tail && tail.role === 'user' && tail.id < 0) {
        state.messages.pop();
        droppedUserDraft = true;
      }
      return { generating: false, reloadHistory: droppedUserDraft };
    }
    case 'vars':
      // 酒馆助手变量树同步(后端权威:已应用 <UpdateVariable> 补丁);
      // display_data 用 stat_data 镜像(后端不维护"老->新"轨迹)
      return {
        mvuVariables: {
          stat_data: event.stat_data,
          display_data: { ...event.stat_data },
        },
      };
    case 'finish': {
      const changes: SseStateChanges = {
        generating: false,
        lastUsage: event.usage,
        contextTokens: event.usage.context_tokens,
        reloadHistory: true,
      };
      a.phase = 'finished';
      a.stepText = '完成';
      // 结束流式消息:非空正文落最终内容并解析变量块;空正文(纯变量更新消息)
      // 也结束流式状态——变量树已由 vars 事件更新,落库快照由后端保证,
      // 随后 loadHistory 回放不会再把变量树回滚到更新前。
      const last = state.messages[state.messages.length - 1];
      if (last && last.role === 'assistant') {
        last.content = event.content;
        last.streaming = false;
        // 错误/空回复(HTTP 错误、网络错误、服务端 error 的空 finish):无内容即无落库,
        // 清理临时空消息,避免幽灵空气泡残留
        if (!event.content && last.id < 0) {
          state.messages.pop();
          changes.applyMvuOn = null;
        } else if (event.content) {
          // 解析 UpdateVariable 块 → 更新变量树 + 持久化快照
          changes.applyMvuOn = last;
        }
      }
      // 服务端已在 Finish 事件发出前落库 assistant 消息,此处 loadHistory 无竞态(由 store 应用)
      return changes;
    }
  }
  // 兜底(事件已穷尽,此处仅为满足 strict 下 noImplicitReturns)
  return {};
}
