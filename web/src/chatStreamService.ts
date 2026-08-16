import type { ChatStreamPayload, SseEvent, SseHandler } from './api';

interface StreamController {
  abort(): void;
}

export interface ChatStreamDependencies {
  streamChat(payload: ChatStreamPayload, onEvent: SseHandler): StreamController;
  stopChat(sessionId: string): Promise<{ ok: boolean }>;
}

export interface ChatStreamService {
  start(payload: ChatStreamPayload, onEvent: SseHandler): void;
  stop(sessionId?: string | null): Promise<void>;
  isActive(): boolean;
}

/**
 * 管理单条聊天流的浏览器控制器、后端停止通知和终态释放。
 * store 只负责业务状态，不再持有 AbortController 生命周期。
 */
export function createChatStreamService(deps: ChatStreamDependencies): ChatStreamService {
  let activeController: StreamController | null = null;

  function start(payload: ChatStreamPayload, onEvent: SseHandler): void {
    activeController?.abort();
    let startedController: StreamController | null = null;
    const handleEvent = (event: SseEvent): void => {
      onEvent(event);
      if ((event.type === 'finish' || event.type === 'interrupted')
        && activeController === startedController) {
        activeController = null;
      }
    };
    startedController = deps.streamChat(payload, handleEvent);
    activeController = startedController;
  }

  async function stop(sessionId?: string | null): Promise<void> {
    // 先发起后端通知，再立即中止本地流；同步抛错也必须执行 abort。
    const controller = activeController;
    activeController = null;
    let notification: Promise<{ ok: boolean }>;
    try {
      notification = sessionId ? deps.stopChat(sessionId) : Promise.resolve({ ok: true });
    } finally {
      controller?.abort();
    }
    await notification;
  }

  return {
    start,
    stop,
    isActive: () => activeController !== null,
  };
}
