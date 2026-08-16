// SSE 流式聊天(streamChat)
import { BASE, authorizedFetch, request } from './client';
import type { AgentMode, SseEvent, TokenUsage } from './types';

export type SseHandler = (event: SseEvent) => void;

export interface ChatStreamPayload {
  session_id?: string;
  character_id?: string;
  message: string;
  agent_mode: AgentMode;
  temperature?: number;
  top_p?: number;
  max_tokens?: number;
  /** 重发锚点:服务端严格验证为当前会话最后一条且正文一致的 user 消息。 */
  resend_message_id?: number;
  /** 重生成锚点(阶段六 6f):必须是当前会话最后一条 assistant 消息,生成新版本原地更新该消息。 */
  regenerate_assistant_id?: number;
  /** 手动压缩标记:true 时本轮生成前对较早历史做摘要压缩(仅 compaction_mode=manual 时生效)。 */
  compact?: boolean;
}

/** 通知后端中止指定会话的生成任务。 */
export function stopChat(sessionId: string): Promise<{ ok: boolean }> {
  return request('/chat/stop', {
    method: 'POST',
    body: JSON.stringify({ session_id: sessionId }),
  });
}

/** 手动压缩会话历史(阶段借鉴 harness):对较早对话做摘要,原文保留可恢复。 */
export function compactChat(sessionId: string): Promise<{ ok: boolean; compacted: boolean }> {
  return request('/chat/compact', {
    method: 'POST',
    body: JSON.stringify({ session_id: sessionId }),
  });
}

/** 撤销压缩,恢复完整原文历史(阶段借鉴 harness):清除摘要行,原文从未删除。 */
export function clearCompactChat(sessionId: string): Promise<{ ok: boolean; cleared: boolean }> {
  return request('/chat/compact/clear', {
    method: 'POST',
    body: JSON.stringify({ session_id: sessionId }),
  });
}

/** 空 usage(错误分支兜底:保证 finish 事件结构完整,store 能安全复位) */
function emptyUsage(): TokenUsage {
  return { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, context_tokens: 0, prompt_cache_hit_tokens: 0 };
}

export interface SseParser {
  push(chunk: Uint8Array): void;
  finish(): boolean;
}

/** 增量 SSE 解析器:兼容 CRLF/LF、跨 chunk 分隔符和 UTF-8，多 data 行按规范以换行拼接。 */
export function createSseParser(onEvent: SseHandler): SseParser {
  const decoder = new TextDecoder();
  let buffer = '';
  let terminalReceived = false;

  const dispatch = (block: string): void => {
    const data = block
      .split(/\r\n|\n|\r/)
      .filter((line) => line.startsWith('data:'))
      .map((line) => line.slice(5).replace(/^ /, ''))
      .join('\n');
    if (!data) return;
    try {
      const event = JSON.parse(data) as SseEvent;
      onEvent(event);
      // error 也是终态事件(服务端错误终态):收到后不再补合成 finish
      if (event.type === 'finish' || event.type === 'interrupted' || event.type === 'error') terminalReceived = true;
    } catch {
      // 单个坏事件不应阻断后续事件。
    }
  };

  const drain = (atEof = false): void => {
    const separator = /\r\n\r\n|\n\n|\r\r/;
    let match = separator.exec(buffer);
    while (match) {
      dispatch(buffer.slice(0, match.index));
      buffer = buffer.slice(match.index + match[0].length);
      match = separator.exec(buffer);
    }
    if (atEof && buffer) {
      dispatch(buffer);
      buffer = '';
    }
  };

  return {
    push(chunk) {
      buffer += decoder.decode(chunk, { stream: true });
      drain();
    },
    finish() {
      buffer += decoder.decode();
      drain(true);
      return terminalReceived;
    },
  };
}

/** 发起 Agent 聊天流;返回用于中断的 AbortController */
export function streamChat(
  payload: ChatStreamPayload,
  onEvent: SseHandler,
): AbortController {
  const controller = new AbortController();
  void (async () => {
    try {
      const res = await authorizedFetch(`${BASE}/chat/send`, {
        method: 'POST',
        body: JSON.stringify(payload),
        signal: controller.signal,
      });
      if (!res.ok || !res.body) {
        const body = (await res.json().catch(() => ({}))) as { error?: string };
        // 失败必须发 finish(空 content),否则 store 的 generating 永远不复位,UI 卡在生成中
        onEvent({ type: 'step', step: '请求失败', detail: body.error ?? `HTTP ${res.status}` });
        onEvent({ type: 'finish', usage: emptyUsage(), content: '' });
        return;
      }
      const reader = res.body.getReader();
      const parser = createSseParser(onEvent);
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        parser.push(value);
      }
      if (!parser.finish()) {
        onEvent({ type: 'step', step: '连接中断', detail: '流在收到终态事件前结束' });
        onEvent({ type: 'finish', usage: emptyUsage(), content: '' });
      }
    } catch (e) {
      // 网络异常同样必须发 finish 复位 generating(AbortError 是用户主动停止,由 interrupted 语义处理)
      if ((e as Error).name !== 'AbortError') {
        onEvent({ type: 'step', step: '网络错误', detail: (e as Error).message });
        onEvent({ type: 'finish', usage: emptyUsage(), content: '' });
      }
    }
  })();
  return controller;
}
