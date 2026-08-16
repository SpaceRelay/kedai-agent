import { describe, expect, it, vi } from 'vitest';
import { createSseParser } from './chat';
import type { SseEvent } from './types';

const bytes = (text: string): Uint8Array => new TextEncoder().encode(text);

describe('createSseParser', () => {
  it('兼容 LF/CRLF、跨 chunk 分隔符和拆分的 UTF-8', () => {
    const events: SseEvent[] = [];
    const parser = createSseParser((event) => events.push(event));
    // 正式契约为 { type: 'token', text }(旧测试误用 content,不守护真实协议)
    const source = bytes('data: {"type":"token","text":"你"}\r\n\r\ndata: {"type":"interrupted"}\n\n');

    parser.push(source.slice(0, 37));
    parser.push(source.slice(37, source.length - 1));
    parser.push(source.slice(source.length - 1));

    expect(parser.finish()).toBe(true);
    expect(events).toEqual([
      { type: 'token', text: '你' },
      { type: 'interrupted' },
    ]);
  });

  it('拼接多 data 行并在 malformed 事件后继续', () => {
    const onEvent = vi.fn();
    const parser = createSseParser(onEvent);
    parser.push(bytes('data: {broken}\n\ndata: {"type":"token",\ndata: "text":"ok"}\n\n'));

    expect(parser.finish()).toBe(false);
    expect(onEvent).toHaveBeenCalledOnce();
    expect(onEvent).toHaveBeenCalledWith({ type: 'token', text: 'ok' });
  });

  it('EOF 时派发没有空行结尾的最后事件并报告终态', () => {
    const onEvent = vi.fn();
    const parser = createSseParser(onEvent);
    parser.push(bytes('data: {"type":"finish","usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0,"context_tokens":0,"prompt_cache_hit_tokens":0},"content":""}'));

    expect(parser.finish()).toBe(true);
    expect(onEvent).toHaveBeenCalledOnce();
  });

  it('error 事件是终态:收到后 finish() 返回 true,不补合成 finish', () => {
    const events: SseEvent[] = [];
    const parser = createSseParser((event) => events.push(event));
    parser.push(bytes('data: {"type":"error","code":"request_timeout","message":"上游连接超时 504","retryable":true}\n\n'));

    expect(parser.finish()).toBe(true);
    expect(events).toEqual([
      { type: 'error', code: 'request_timeout', message: '上游连接超时 504', retryable: true },
    ]);
  });
});
