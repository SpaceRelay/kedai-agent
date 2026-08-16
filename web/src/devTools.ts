// 开发者工具纯函数(阶段六 6c 事件监控):类型映射 / 过滤 / 载荷截断。
// 与 UI 解耦,便于单元测试;DevToolsModal 与 store 事件采集共用。
import type { SseEvent } from './api';

/** 事件日志条目(store 采集,cap 500 丢最旧) */
export interface ApiEventLogEntry {
  /** 采集时间戳(ms) */
  ts: number;
  /** 事件发生时所在会话(空 = 无会话上下文) */
  session_id: string;
  event: SseEvent;
}

/** 事件类型 → 中文标签(9 类全覆盖;未知类型回退原样) */
export const EVENT_TYPE_LABELS: Record<string, string> = {
  token: '文本流',
  step: '步骤',
  tool_call: '工具调用',
  tool_authorization_required: '工具待授权',
  tool_result: '工具结果',
  vars: '变量更新',
  interrupted: '中断',
  error: '错误',
  finish: '完成',
};

/** 事件类型中文标签;未知类型回退原样(便于看到新类型而非吞掉) */
export function eventTypeLabel(type: string): string {
  return EVENT_TYPE_LABELS[type] ?? type;
}

/** 全部事件类型清单(面板过滤下拉用;保持稳定顺序) */
export const EVENT_TYPES: string[] = Object.keys(EVENT_TYPE_LABELS);

/** 过滤事件日志:类型(空/'all' = 全部)+ 会话(空 = 全部会话) */
export function filterEventLog(
  entries: ApiEventLogEntry[],
  type: string,
  sessionId: string,
): ApiEventLogEntry[] {
  return entries.filter(
    (e) =>
      (type === '' || type === 'all' || e.event.type === type) &&
      (sessionId === '' || e.session_id === sessionId),
  );
}

/** 载荷 JSON 截断展示:超过 maxLen 截断并追加省略号(长载荷如 finish.content 折叠) */
export function truncatePayload(text: string, maxLen = 300): string {
  if (text.length <= maxLen) return text;
  return `${text.slice(0, maxLen)}…`;
}
