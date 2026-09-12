import { describe, expect, it } from 'vitest';
import {
  createVirtualListState,
  ESTIMATED_HEIGHTS,
  ESTIMATED_HEIGHT_FALLBACK,
} from './useVirtualMessages';

// 虚拟滚动核心状态单测(纯逻辑,无 DOM;IO 胶水在 useVirtualMessages 内,浏览器侧行为)
// 覆盖:启用阈值、尾部常驻、占位高度(实测缓存/角色估算/兜底)、进出缓冲区切换、pin 与重置。

describe('createVirtualListState', () => {
  it('消息数不超过阈值时全部真实挂载(短会话零开销)', () => {
    const s = createVirtualListState({ threshold: 80, tailKeep: 30 });
    expect(s.isEnabled(80)).toBe(false);
    expect(s.isEnabled(81)).toBe(true);
    // 阈值边界(=80)不启用:任意下标均激活
    for (let i = 0; i < 80; i++) {
      expect(s.isActive(i + 1, i, 80)).toBe(true);
    }
  });

  it('超阈值后仅尾部 tailKeep 条默认激活,头部未上报可见的不挂载', () => {
    const s = createVirtualListState({ threshold: 80, tailKeep: 30 });
    const count = 200;
    // 尾部 30 条(下标 170..199)常驻真实挂载
    expect(s.isActive(200, 199, count)).toBe(true);
    expect(s.isActive(171, 170, count)).toBe(true);
    // 尾部窗口之外(下标 169 及以前)未进入过视口 → 占位
    expect(s.isActive(170, 169, count)).toBe(false);
    expect(s.isActive(1, 0, count)).toBe(false);
  });

  it('markVisible 进入缓冲区后挂载;markHidden 记录实测高度后降级占位', () => {
    const s = createVirtualListState({ threshold: 10, tailKeep: 5 });
    const count = 100;
    expect(s.isActive(42, 41, count)).toBe(false);
    s.markVisible(42);
    expect(s.isActive(42, 41, count)).toBe(true);
    // 离开缓冲区:实测高度入缓存,行降级为占位
    s.markHidden(42, 233);
    expect(s.isActive(42, 41, count)).toBe(false);
    expect(s.placeholderHeight(42, 'assistant')).toBe(233);
  });

  it('占位高度:未渲染过的按角色估算,未知角色走兜底', () => {
    const s = createVirtualListState();
    expect(s.placeholderHeight(1, 'user')).toBe(ESTIMATED_HEIGHTS.user);
    expect(s.placeholderHeight(2, 'assistant')).toBe(ESTIMATED_HEIGHTS.assistant);
    expect(s.placeholderHeight(3, 'system')).toBe(ESTIMATED_HEIGHTS.system);
    expect(s.placeholderHeight(4, 'unknown-role')).toBe(ESTIMATED_HEIGHT_FALLBACK);
  });

  it('实测高度优先于角色估算;markHidden 忽略非正高度', () => {
    const s = createVirtualListState({ threshold: 10, tailKeep: 5 });
    s.markVisible(7);
    s.markHidden(7, 0); // 元素尚未排版完成:不写缓存,但仍降级占位
    expect(s.isActive(7, 6, 100)).toBe(false);
    expect(s.placeholderHeight(7, 'user')).toBe(ESTIMATED_HEIGHTS.user);
    s.markVisible(7);
    s.markHidden(7, 120);
    expect(s.placeholderHeight(7, 'user')).toBe(120);
  });

  it('pinnedId(编辑中的消息)强制真实挂载,与可见集无关', () => {
    const s = createVirtualListState({ threshold: 10, tailKeep: 5 });
    const count = 100;
    expect(s.isActive(9, 8, count, 9)).toBe(true);
    // 其他行不受 pin 影响
    expect(s.isActive(8, 7, count, 9)).toBe(false);
    // pin 解除后按常规规则判定
    expect(s.isActive(9, 8, count, null)).toBe(false);
  });

  it('列表长度变化时尾部窗口跟随(新消息进入尾部即真实挂载)', () => {
    const s = createVirtualListState({ threshold: 80, tailKeep: 30 });
    // 200 条时窗口为下标 170..199;追加一条(201 条)后窗口滑动为 171..200:
    // 原下标 170 退出常驻窗口(未上报可见 → 占位),新消息(下标 200)常驻真实挂载
    expect(s.isActive(171, 170, 200)).toBe(true);
    expect(s.isActive(171, 170, 201)).toBe(false);
    expect(s.isActive(202, 200, 201)).toBe(true);
  });

  it('reset 清空可见集与高度缓存(会话切换)', () => {
    const s = createVirtualListState({ threshold: 10, tailKeep: 5 });
    s.markVisible(42);
    s.markHidden(42, 233);
    s.markVisible(43);
    s.reset();
    expect(s.visible.size).toBe(0);
    expect(s.heights.size).toBe(0);
    expect(s.isActive(43, 42, 100)).toBe(false);
    expect(s.placeholderHeight(42, 'assistant')).toBe(ESTIMATED_HEIGHTS.assistant);
  });
});
