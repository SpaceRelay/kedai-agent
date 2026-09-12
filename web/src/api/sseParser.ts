// 共享 SSE 解析层:字节流 → 事件帧 data 文本(纯解析,不含任何业务终态/重连语义)。
// chat.ts(聊天流,「必发终态」)与 tasks.ts(任务事件流,长连接退避重连)共用本层,
// 各自的终态合成 / 事件过滤 / 重连策略保留在调用方。
//
// 解析规则(两端历史一致):
// - TextDecoder 流式解码,跨 chunk 的 UTF-8 多字节字符不撕裂;
// - 分帧兼容三种空行:\r\n\r\n | \n\n | \r\r;
// - 帧内多 data 行按 SSE 规范以 '\n' 拼接,单 data 行前缀后最多剥一个空格;
// - 无 data 帧(KeepAlive 注释行等)跳过;EOF 时无空行结尾的残留帧照常派发。

/** 帧解析器句柄:push 喂字节,finish 在流结束时冲刷(返回 void,终态判定归调用方) */
export interface SseFrameParser {
  push(chunk: Uint8Array): void;
  finish(): void;
}

/**
 * 创建增量 SSE 帧解析器。
 * @param onData 每个完整事件帧的拼接后 data 文本(未 JSON.parse,解析与容错归调用方)
 */
export function createSseFrameParser(onData: (data: string) => void): SseFrameParser {
  const decoder = new TextDecoder();
  let buffer = '';

  /** 派发一个完整帧:提取并拼接 data 行;无 data 帧静默跳过 */
  const dispatch = (block: string): void => {
    const data = block
      .split(/\r\n|\n|\r/)
      .filter((line) => line.startsWith('data:'))
      .map((line) => line.slice(5).replace(/^ /, ''))
      .join('\n');
    if (!data) return;
    onData(data);
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
    },
  };
}
