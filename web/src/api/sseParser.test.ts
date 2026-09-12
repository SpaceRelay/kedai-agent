import { describe, expect, it, vi } from 'vitest';
import { createSseFrameParser } from './sseParser';

// sseParser.ts 共享帧解析层直测:字节流 → data 文本帧。
// 业务语义(聊天终态判定 / 任务事件过滤)在 chat.test.ts / tasks.test.ts 各自覆盖,此处只锁帧解析契约。

const bytes = (text: string): Uint8Array => new TextEncoder().encode(text);

describe('createSseFrameParser(共享 SSE 帧解析层)', () => {
  it('三种分帧符(CRLF/LF/CR)与跨 chunk 分隔符均正确切帧', () => {
    const frames: string[] = [];
    const parser = createSseFrameParser((data) => frames.push(data));
    const source = bytes('data: a\r\n\r\ndata: b\n\ndata: c\r\r');

    parser.push(source.slice(0, 11)); // 切在 CRLFCRLF 中间
    parser.push(source.slice(11));

    expect(frames).toEqual(['a', 'b', 'c']);
  });

  it('多 data 行按规范以换行拼接;data: 后至多剥一个空格', () => {
    const frames: string[] = [];
    const parser = createSseFrameParser((data) => frames.push(data));
    parser.push(bytes('data: 第一行\ndata:第二行\ndata:  三\n\n'));

    expect(frames).toEqual(['第一行\n第二行\n 三']);
  });

  it('无 data 帧(KeepAlive 注释行)跳过;EOF 时无空行结尾的残留帧照常派发', () => {
    const onData = vi.fn();
    const parser = createSseFrameParser(onData);
    parser.push(bytes(': keep-alive\r\n\r\ndata: 尾帧无空行结尾'));

    expect(onData).not.toHaveBeenCalled();
    parser.finish();
    expect(onData).toHaveBeenCalledOnce();
    expect(onData).toHaveBeenCalledWith('尾帧无空行结尾');
  });

  it('跨 chunk 拆分的 UTF-8 多字节字符不撕裂', () => {
    const frames: string[] = [];
    const parser = createSseFrameParser((data) => frames.push(data));
    const source = bytes('data: 你好\n\n');
    // 「你」三字节:切在其内部,流式解码必须跨 chunk 拼接
    const cut = 'data: '.length + 1;
    parser.push(source.slice(0, cut));
    parser.push(source.slice(cut));
    parser.finish();

    expect(frames).toEqual(['你好']);
  });
});
