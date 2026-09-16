// SSE 流式聊天(streamChat)
import { BASE, apiErrorMessage, authorizedFetch, request } from './client';
import { createSseFrameParser } from './sseParser';
import { pumpSseFrames, readErrorParts } from './stream';
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
  return { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, context_tokens: 0, prompt_cache_hit_tokens: 0, prompt_cache_miss_tokens: 0 };
}

export interface SseParser {
  push(chunk: Uint8Array): void;
  /** 喂入**已解出的 data 文本**(与 push 二选一):供共享读循环 stream.ts 复用同一解析器 */
  pushData(data: string): void;
  finish(): boolean;
}

/**
 * 聊天流 SSE 解析器:帧解析委托 sseParser.ts 共享层(兼容 CRLF/LF、跨 chunk 分隔符和
 * UTF-8,多 data 行按规范以换行拼接),本层只保留聊天专属语义:
 * - data 文本 JSON.parse 为 SseEvent 后回调(单个坏事件不阻断后续);
 * - 终态判定:finish / interrupted / error(error 也是服务端错误终态)收到后,
 *   finish() 返回 true,streamChat 不再补合成 finish。
 */
export function createSseParser(onEvent: SseHandler): SseParser {
  let terminalReceived = false;

  /** 单帧 data 文本 → 聊天事件(坏帧跳过,不阻断后续);终态在此登记 */
  const handleData = (data: string): void => {
    try {
      const event = JSON.parse(data) as SseEvent;
      onEvent(event);
      // error 也是终态事件(服务端错误终态):收到后不再补合成 finish
      if (event.type === 'finish' || event.type === 'interrupted' || event.type === 'error') terminalReceived = true;
    } catch {
      // 单个坏事件不应阻断后续事件。
    }
  };

  const frames = createSseFrameParser(handleData);

  return {
    push: (chunk) => frames.push(chunk),
    pushData: handleData,
    finish: () => {
      frames.finish();
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
        // 失败必须发 finish(空 content),否则 store 的 generating 永远不复位,UI 卡在生成中。
        // 错误口径与 request() 一致:按结构化 code 分类(见 client.ts apiErrorMessage),
        // 错误体解析与 tasks 流共用 stream.ts 的单点实现。
        const { status, code, detail } = await readErrorParts(res);
        onEvent({ type: 'step', step: '请求失败', detail: apiErrorMessage(status, code, detail) });
        onEvent({ type: 'finish', usage: emptyUsage(), content: '' });
        return;
      }
      // 读循环与帧解析走共享底座(stream.ts),终态判定仍由本层的 createSseParser 负责
      const parser = createSseParser(onEvent);
      await pumpSseFrames(res, (data) => parser.pushData(data));
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
