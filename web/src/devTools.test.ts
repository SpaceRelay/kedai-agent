import { describe, expect, it } from 'vitest';
import {
  EVENT_TYPES,
  EVENT_TYPE_LABELS,
  eventTypeLabel,
  filterEventLog,
  truncatePayload,
  type ApiEventLogEntry,
} from './devTools';
import type { SseEvent } from './api';

function entry(ts: number, event: SseEvent, sessionId = 's1'): ApiEventLogEntry {
  return { ts, session_id: sessionId, event };
}

describe('事件类型标签映射(6c)', () => {
  it('9 类事件全覆盖', () => {
    expect(EVENT_TYPES).toHaveLength(9);
    expect(EVENT_TYPE_LABELS).toEqual({
      token: '文本流',
      step: '步骤',
      tool_call: '工具调用',
      tool_authorization_required: '工具待授权',
      tool_result: '工具结果',
      vars: '变量更新',
      interrupted: '中断',
      error: '错误',
      finish: '完成',
    });
  });

  it('已知类型返回中文标签,未知类型回退原样', () => {
    expect(eventTypeLabel('token')).toBe('文本流');
    expect(eventTypeLabel('finish')).toBe('完成');
    expect(eventTypeLabel('mystery_event')).toBe('mystery_event');
  });
});

describe('事件过滤(6c)', () => {
  const log: ApiEventLogEntry[] = [
    entry(1, { type: 'token', text: 'a' }, 's1'),
    entry(2, { type: 'step', step: '计划', detail: 'x' }, 's1'),
    entry(3, { type: 'token', text: 'b' }, 's2'),
    entry(4, { type: 'error', code: 'E', message: 'm', retryable: false }, 's1'),
  ];

  it('全部(空/all)返回原列表', () => {
    expect(filterEventLog(log, '', '')).toHaveLength(4);
    expect(filterEventLog(log, 'all', '')).toHaveLength(4);
  });

  it('按类型过滤', () => {
    const got = filterEventLog(log, 'token', '');
    expect(got.map((e) => e.event)).toEqual([
      { type: 'token', text: 'a' },
      { type: 'token', text: 'b' },
    ]);
  });

  it('按会话过滤', () => {
    const got = filterEventLog(log, '', 's2');
    expect(got).toHaveLength(1);
    expect(got[0].event.type).toBe('token');
  });

  it('类型 + 会话组合过滤', () => {
    const got = filterEventLog(log, 'token', 's1');
    expect(got).toHaveLength(1);
    expect((got[0].event as { text: string }).text).toBe('a');
  });

  it('空列表返回空', () => {
    expect(filterEventLog([], 'all', '')).toHaveLength(0);
  });
});

describe('载荷截断(6c)', () => {
  it('短文本原样', () => {
    expect(truncatePayload('你好')).toBe('你好');
  });

  it('长文本截断并追加省略号', () => {
    const long = 'x'.repeat(500);
    const got = truncatePayload(long, 100);
    expect(got).toHaveLength(101);
    expect(got.endsWith('…')).toBe(true);
    expect(got.slice(0, 100)).toBe('x'.repeat(100));
  });

  it('恰好等于上限不截断', () => {
    expect(truncatePayload('y'.repeat(300), 300)).toBe('y'.repeat(300));
  });
});
