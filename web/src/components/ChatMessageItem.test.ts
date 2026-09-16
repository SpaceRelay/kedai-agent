// @vitest-environment jsdom
// ChatMessageItem 组件测试(批次 G.4 补测)。
//
// 背景:本组件是消息渲染的核心(282 行——markdown / 状态栏 / swipe / 资源卡全在此),
// 却一直没有测试;仓库现有组件测试多为「字符串匹配式弱断言」。本文件按行为断言:
// 角色分流(owner/assistant/plain)、markdown 渲染、状态栏注入与隐藏、
// swipe 角标、流式光标、以及批次 G.1 引入的「流式渲染节流」语义。
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';

const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() {
    return memStorage.size;
  },
});

vi.mock('../api', async (importOriginal) => {
  const orig = await importOriginal<typeof import('../api')>();
  return { ...orig, listModels: vi.fn().mockResolvedValue([]) };
});

import ChatMessageItem from './ChatMessageItem.vue';
import { useAppStore } from '../store';
import type { UiMessage } from '../sseReducer';
import type { RegexScript } from '../api';

/** 构造一条消息(默认 assistant 角色,渲染走 markdown 分支) */
function msg(over: Partial<UiMessage> = {}): UiMessage {
  return {
    id: 1,
    role: 'assistant',
    content: '你好',
    extra: {},
    ...over,
  } as UiMessage;
}

/** 挂载组件(统一默认 props;各用例按需覆盖) */
function mountItem(m: UiMessage, over: Record<string, unknown> = {}) {
  return mount(ChatMessageItem, {
    props: {
      m,
      editing: false,
      avatarUrl: null,
      characterName: '测试角色',
      renderHtml: false,
      scripts: [],
      depth: 0,
      scriptHash: 'h0',
      ...over,
    },
  });
}

describe('ChatMessageItem', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    memStorage.clear();
    void useAppStore();
  });

  it('user 角色渲染为右侧用户气泡,不走 markdown 容器', () => {
    const w = mountItem(msg({ role: 'user', content: '用户输入' }));
    expect(w.find('.sv-msg.user').exists()).toBe(true);
    expect(w.find('.sv-msg.assistant').exists()).toBe(false);
    expect(w.text()).toContain('用户输入');
  });

  it('assistant 角色渲染 markdown 容器,并显示角色名', () => {
    const w = mountItem(msg({ content: '**加粗**' }));
    expect(w.find('.sv-msg.assistant').exists()).toBe(true);
    expect(w.find('.sv-msg-name').text()).toBe('测试角色');
    // markdown 已渲染为 <strong>(而非原样输出 **)
    expect(w.find('.sv-msg-bubble.sv-msg-md').html()).toContain('<strong>');
  });

  it('content_display 优先于 content(服务端宏展开结果)', () => {
    const w = mountItem(msg({ content: '{{char}}原文', content_display: '展开后的文本' }));
    expect(w.text()).toContain('展开后的文本');
    expect(w.text()).not.toContain('{{char}}');
  });

  it('流式中显示光标类,非流式显示操作按钮', () => {
    const streaming = mountItem(msg({ streaming: true }));
    expect(streaming.find('.sv-stream-cursor').exists()).toBe(true);

    const done = mountItem(msg({ streaming: false }));
    expect(done.find('.sv-stream-cursor').exists()).toBe(false);
    expect(done.find('.sv-msg-actions').exists()).toBe(true);
  });

  it('swipe 多版本时显示角标,单版本不显示', () => {
    const multi = mountItem(
      msg({ extra: { swipes: ['a', 'b', 'c'], swipe_id: 1 } }),
    );
    expect(multi.find('.sv-swipe-btn').exists()).toBe(true);
    expect(multi.text()).toContain('2/3');

    const single = mountItem(msg({ extra: { swipes: ['only'], swipe_id: 0 } }));
    expect(single.find('.sv-swipe-btn').exists()).toBe(false);
  });

  it('状态栏文本注入渲染文本(无状态栏脚本时以纯文本气泡展示)', () => {
    // extra.status_bar 会被 buildMessageRenderText 拼入渲染文本
    const w = mountItem(msg({ content: '正文', extra: { status_bar: 'HP: 100' } }));
    expect(w.text()).toContain('HP: 100');
  });

  it('extra.truncated 时显示截断提示(可观测性问题①);未标记则不显示', () => {
    const cut = mountItem(msg({ content: '半截正文', extra: { truncated: true } }));
    expect(cut.find('.sv-trunc-note').exists()).toBe(true);
    expect(cut.find('.sv-badge.trunc').exists()).toBe(true);
    expect(cut.text()).toContain('截断');

    const normal = mountItem(msg({ content: '完整正文' }));
    expect(normal.find('.sv-trunc-note').exists()).toBe(false);
  });

  it('状态栏占位符脚本存在且 HTML 渲染开启时,隐藏纯文本状态栏气泡', () => {
    // 按 RegexScript 的真实字段构造(字段名:script_name / find_regex / replace_string;
    // 判定见 render.ts hasStatusPlaceholderScript:enabled + replace_string + find_regex 含占位符)
    const scripts: RegexScript[] = [
      {
        id: 's1',
        script_name: '状态栏',
        find_regex: '<StatusPlaceHolderImpl/>',
        replace_string: '<div>HP</div>',
        markdown_only: false,
        enabled: true,
      },
    ];
    const w = mountItem(msg({ content: '正文', extra: { status_bar: 'HP: 100' } }), {
      scripts,
      renderHtml: true,
    });
    // 状态栏由 HTML 卡片承载 → 不再重复出现在文本气泡里
    expect(w.text()).not.toContain('HP: 100');
  });

  it('流式渲染节流:多帧内 content 连改只增加一次渲染节拍(批次 G.1)', async () => {
    // node 环境下无 requestAnimationFrame,组件退化为 setTimeout(16ms)。
    // 断言:同一帧内到达的多个 token 只触发一次重算——即「节流」的语义,
    // 而非「每个 token 立即重渲染」。
    vi.useFakeTimers();
    try {
      const m = msg({ content: 'a', streaming: true });
      const w = mountItem({ ...m } as UiMessage);
      const before = w.find('.sv-msg-bubble').html();

      // 连续追加(模拟逐 token)
      (w.props('m') as UiMessage).content += 'b';
      (w.props('m') as UiMessage).content += 'c';
      await w.vm.$nextTick();

      // 未到 rAF 节拍:渲染仍为旧值(说明被合并/延迟,不是逐 token 立即渲染)
      expect(w.find('.sv-msg-bubble').html()).toBe(before);

      // 推进到节拍:一次性反映全部累积内容(不丢字)
      vi.advanceTimersByTime(20);
      await w.vm.$nextTick();
      expect(w.text()).toContain('abc');
    } finally {
      vi.useRealTimers();
    }
  });

  it('非流式消息不受节流影响:content 变化立即渲染', async () => {
    const m = msg({ content: 'x', streaming: false });
    const w = mountItem({ ...m } as UiMessage);
    (w.props('m') as UiMessage).content = 'xyz';
    await w.vm.$nextTick();
    expect(w.text()).toContain('xyz');
  });
});
