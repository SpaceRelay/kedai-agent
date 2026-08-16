import { describe, expect, it, vi } from 'vitest';
import { createChatStreamService } from './chatStreamService';
import type { SseEvent } from './api';

describe('createChatStreamService', () => {
  it('stop 会先通知后端并中止当前浏览器流', async () => {
    const abort = vi.fn();
    const stopChat = vi.fn().mockResolvedValue({ ok: true });
    const service = createChatStreamService({
      streamChat: vi.fn().mockReturnValue({ abort }),
      stopChat,
    });

    service.start({
      session_id: 'session-1',
      character_id: 'character-1',
      message: '你好',
      agent_mode: 'fast',
    }, vi.fn());
    await service.stop('session-1');

    expect(stopChat).toHaveBeenCalledOnce();
    expect(stopChat).toHaveBeenCalledWith('session-1');
    expect(abort).toHaveBeenCalledOnce();
    expect(service.isActive()).toBe(false);
  });

  it('收到终态事件后释放控制器，后续 stop 不重复中止旧流', async () => {
    const abort = vi.fn();
    let emit: ((event: SseEvent) => void) | undefined;
    const onEvent = vi.fn();
    const service = createChatStreamService({
      streamChat: vi.fn((_payload, handler) => {
        emit = handler;
        return { abort };
      }),
      stopChat: vi.fn().mockResolvedValue({ ok: true }),
    });

    service.start({ message: '你好', agent_mode: 'fast' }, onEvent);
    emit?.({ type: 'interrupted' });
    await service.stop('session-1');

    expect(onEvent).toHaveBeenCalledWith({ type: 'interrupted' });
    expect(abort).not.toHaveBeenCalled();
    expect(service.isActive()).toBe(false);
  });

  it('stopChat 同步抛错时仍中止当前浏览器流', async () => {
    const abort = vi.fn();
    const service = createChatStreamService({
      streamChat: vi.fn().mockReturnValue({ abort }),
      stopChat: vi.fn(() => { throw new Error('同步失败'); }),
    });
    service.start({ message: '你好', agent_mode: 'fast' }, vi.fn());

    await expect(service.stop('session-1')).rejects.toThrow('同步失败');
    expect(abort).toHaveBeenCalledOnce();
    expect(service.isActive()).toBe(false);
  });

  it('旧流迟到终态不会清理当前新流', async () => {
    const handlers: Array<(event: SseEvent) => void> = [];
    const controllers = [{ abort: vi.fn() }, { abort: vi.fn() }];
    const service = createChatStreamService({
      streamChat: vi.fn((_payload, handler) => {
        handlers.push(handler);
        return controllers[handlers.length - 1];
      }),
      stopChat: vi.fn().mockResolvedValue({ ok: true }),
    });

    service.start({ message: '旧', agent_mode: 'fast' }, vi.fn());
    service.start({ message: '新', agent_mode: 'fast' }, vi.fn());
    handlers[0]({ type: 'interrupted' });

    expect(service.isActive()).toBe(true);
    await service.stop('session-1');
    expect(controllers[1].abort).toHaveBeenCalledOnce();
  });
});
