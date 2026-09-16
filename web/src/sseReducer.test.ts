import { describe, expect, it } from 'vitest';
import { idleAgent, reduceSseEvent, swipeCount, swipeIndex } from './sseReducer';
import type { SseStateSlice } from './sseReducer';

/** 全零 token 用量(finish 事件必带;测试只关心截断标记,数值无关) */
function zeroUsage() {
  return {
    prompt_tokens: 0,
    completion_tokens: 0,
    total_tokens: 0,
    context_tokens: 0,
    prompt_cache_hit_tokens: 0,
    prompt_cache_miss_tokens: 0,
  };
}

function state(): SseStateSlice {
  return {
    agent: idleAgent(),
    messages: [],
    generating: true,
    lastUsage: null,
    contextTokens: 0,
    mvuVariables: { stat_data: {}, display_data: {} },
  };
}

describe('reduceSseEvent', () => {
  it('tool_call/tool_result 透传 renderKind 到 toolCalls', () => {
    const current = state();
    reduceSseEvent(current, { type: 'tool_call', name: 'search', input: { q: 'x' }, call_id: 'c1', render_kind: 'search' });
    expect(current.agent.toolCalls[0]).toMatchObject({ name: 'search', renderKind: 'search' });

    reduceSseEvent(current, { type: 'tool_result', name: 'search', output: { results: [] }, call_id: 'c1', render_kind: 'search' });
    expect(current.agent.toolCalls[0]).toMatchObject({ renderKind: 'search', status: 'done' });
  });

  it('tool_result 未带 render_kind 时保留调用阶段已有的 renderKind', () => {
    const current = state();
    reduceSseEvent(current, { type: 'tool_call', name: 'read', input: {}, call_id: 'c1', render_kind: 'read' });
    // 旧服务端 tool_result 事件无 render_kind
    reduceSseEvent(current, { type: 'tool_result', name: 'read', output: '内容', call_id: 'c1' });
    expect(current.agent.toolCalls[0].renderKind).toBe('read');
  });

  it('授权事件按 call_id 标记对应的同名工具，不误改其他调用', () => {
    const current = state();
    reduceSseEvent(current, { type: 'tool_call', name: 'write', input: { path: 'a' }, call_id: 'call-a' });
    reduceSseEvent(current, { type: 'tool_call', name: 'write', input: { path: 'b' }, call_id: 'call-b' });

    reduceSseEvent(current, {
      type: 'tool_authorization_required',
      name: 'write',
      call_id: 'call-a',
      run_id: 'run-a',
      risk: 'dangerous',
      reason: '需要写入文件',
    });

    expect(current.agent.toolCalls[0]).toMatchObject({
      callId: 'call-a',
      runId: 'run-a',
      status: 'authorization_required',
      risk: 'dangerous',
      reason: '需要写入文件',
    });
    expect(current.agent.toolCalls[1]).toMatchObject({ callId: 'call-b', status: 'running' });
    expect(current.agent.pendingTool).toEqual({ name: 'write', input: { path: 'b' } });
  });

  it('interrupted 结束生成并削除未落库的临时草稿气泡', () => {
    const current = state();
    current.messages.push({ id: -1, role: 'assistant', content: '部分内容', extra: {}, streaming: true });

    const changes = reduceSseEvent(current, { type: 'interrupted' });

    expect(changes.generating).toBe(false);
    expect(current.agent).toMatchObject({ phase: 'interrupted', stepText: '已中断' });
    // 临时草稿(负 id)从未落库,服务端中断不持久化任何消息:必须削除,
    // 否则残留为「最后一条无法删除的草稿」(负 id 无法经删除接口移除)。
    expect(current.messages.length).toBe(0);
  });

  it('interrupted 保留已落库(正 id)消息,仅关闭流式状态', () => {
    const current = state();
    current.messages.push({ id: 42, role: 'assistant', content: '已落库内容', extra: {}, streaming: true });

    reduceSseEvent(current, { type: 'interrupted' });

    expect(current.messages.length).toBe(1);
    expect(current.messages[0].streaming).toBe(false);
  });

  it('finish 落最终正文、结束生成并请求刷新历史', () => {
    const current = state();
    current.messages.push({ id: -1, role: 'assistant', content: '部分', extra: {}, streaming: true });
    const usage = {
      prompt_tokens: 10,
      completion_tokens: 5,
      total_tokens: 15,
      context_tokens: 12,
      prompt_cache_hit_tokens: 3,
      prompt_cache_miss_tokens: 12,
    };

    const changes = reduceSseEvent(current, { type: 'finish', content: '最终正文', usage });

    expect(changes).toMatchObject({
      generating: false,
      lastUsage: usage,
      contextTokens: 12,
      reloadHistory: true,
    });
    expect(current.messages[0]).toMatchObject({ content: '最终正文', streaming: false });
  });

  it('finish_reason=length 时给消息打截断标记(可观测性问题①);stop 不打', () => {
    // 截断:正文是半截,extra.truncated 置位供气泡展示「截断」提示
    const truncatedState = state();
    truncatedState.messages.push({ id: -1, role: 'assistant', content: '半截', extra: {}, streaming: true });
    reduceSseEvent(truncatedState, {
      type: 'finish',
      content: '半截正文',
      usage: zeroUsage(),
      finish_reason: 'length',
    });
    expect(truncatedState.messages[0].extra.truncated).toBe(true);

    // 正常收尾:不得误标截断
    const normalState = state();
    normalState.messages.push({ id: -1, role: 'assistant', content: '部分', extra: {}, streaming: true });
    reduceSseEvent(normalState, {
      type: 'finish',
      content: '完整正文',
      usage: zeroUsage(),
      finish_reason: 'stop',
    });
    expect(normalState.messages[0].extra.truncated).toBeUndefined();

    // 未下发 finish_reason(旧客户端形态):同样不标
    const unknownState = state();
    unknownState.messages.push({ id: -1, role: 'assistant', content: '部分', extra: {}, streaming: true });
    reduceSseEvent(unknownState, { type: 'finish', content: '正文', usage: zeroUsage() });
    expect(unknownState.messages[0].extra.truncated).toBeUndefined();
  });

  it('截断标记不覆盖既有 extra 字段(status_bar 等需保留)', () => {
    const current = state();
    current.messages.push({
      id: -1,
      role: 'assistant',
      content: '半截',
      extra: { status_bar: 'HP 10/10' },
      streaming: true,
    });
    reduceSseEvent(current, {
      type: 'finish',
      content: '半截正文',
      usage: zeroUsage(),
      finish_reason: 'length',
    });
    expect(current.messages[0].extra).toMatchObject({
      status_bar: 'HP 10/10',
      truncated: true,
    });
  });

  it('error 终态结束生成、进入 error 阶段并清理未落库临时草稿(含部分内容)', () => {
    const current = state();
    current.messages.push({ id: -1, role: 'assistant', content: '写到一半的内容', extra: {}, streaming: true });

    const changes = reduceSseEvent(current, {
      type: 'error',
      code: 'request_timeout',
      message: '上游连接超时 504',
      retryable: true,
    });

    expect(changes).toMatchObject({ generating: false });
    // 无残留的用户临时气泡时不请求刷新历史
    expect(changes.reloadHistory).toBe(false);
    expect(current.agent).toMatchObject({ phase: 'error', stepText: '执行出错' });
    // 错误终态下服务端无落库:部分内容的临时草稿也必须削除(无法删除的幽灵气泡)
    expect(current.messages.length).toBe(0);
    // 推理链记录错误
    expect(current.agent.chain.some((c) => c.text.includes('上游连接超时 504'))).toBe(true);
  });

  it('error 终态保留已落库(有内容)的消息,只关闭流式状态', () => {
    const current = state();
    current.messages.push({ id: 42, role: 'assistant', content: '已落库内容', extra: {}, streaming: true });

    reduceSseEvent(current, { type: 'error', code: 'upstream_error', message: '连接失败', retryable: true });

    expect(current.messages.length).toBe(1);
    expect(current.messages[0].streaming).toBe(false);
  });

  it('interrupted 削除末尾未落库的用户临时气泡并请求刷新历史', () => {
    // 发送消息时前端先 push 负 id 用户气泡,服务端同步落库为正 id;中断终态不刷新历史,
    // 该气泡会残留成「删不掉的草稿」(负 id 无法经删除接口移除)。
    const current = state();
    current.messages.push({ id: -100, role: 'user', content: '被中断的消息', extra: {}, streaming: false });

    const changes = reduceSseEvent(current, { type: 'interrupted' });

    expect(changes.generating).toBe(false);
    expect(changes.reloadHistory).toBe(true);
    expect(current.messages.length).toBe(0);
  });

  it('interrupted 削除用户临时气泡并保留其前已落库消息', () => {
    const current = state();
    current.messages.push({ id: 7, role: 'user', content: '已落库消息', extra: {}, streaming: false });
    current.messages.push({ id: -100, role: 'user', content: '被中断的临时气泡', extra: {}, streaming: false });

    const changes = reduceSseEvent(current, { type: 'interrupted' });

    expect(changes.reloadHistory).toBe(true);
    expect(current.messages.map((m) => m.id)).toEqual([7]);
  });

  it('error 终态削除末尾未落库的用户临时气泡并请求刷新历史', () => {
    const current = state();
    current.messages.push({ id: -100, role: 'user', content: '出错前发送的消息', extra: {}, streaming: false });

    const changes = reduceSseEvent(current, {
      type: 'error',
      code: 'request_timeout',
      message: '上游连接超时 504',
      retryable: true,
    });

    expect(changes).toMatchObject({ generating: false, reloadHistory: true });
    expect(current.messages.length).toBe(0);
  });

  it('task 事件不产生任何状态变更(仅供事件监控消费)', () => {
    const current = state();
    current.messages.push({ id: 7, role: 'assistant', content: '既有消息', extra: {}, streaming: false });
    const before = JSON.stringify(current);

    const changes = reduceSseEvent(current, {
      type: 'task',
      task_id: 't1',
      title: '写一首诗',
      status: 'running',
      detail: '计划步骤 1/3',
    });

    expect(changes).toEqual({});
    expect(JSON.stringify(current)).toBe(before);
  });
});

describe('swipe 版本纯函数(阶段六 6f)', () => {
  it('无 swipes(单版本)返回 -1 / 0,隐藏切换 UI', () => {
    expect(swipeIndex({})).toBe(-1);
    expect(swipeCount({})).toBe(0);
  });

  it('多版本时返回激活索引与总数', () => {
    const extra = {
      swipe_id: 1,
      swipes: [
        { swipe_id: 0, content: '旧', ts: 1 },
        { swipe_id: 1, content: '新', ts: 2 },
      ],
    };
    expect(swipeIndex(extra)).toBe(1);
    expect(swipeCount(extra)).toBe(2);
  });

  it('swipe_id 缺失(历史消息兼容)回退 -1', () => {
    expect(swipeIndex({ swipes: [{ swipe_id: 0, content: 'a', ts: 1 }] })).toBe(-1);
  });
});
