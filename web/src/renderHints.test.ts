// renderHints.test.ts — 顶栏两条引导条可见性真值表
//
// 背景(实测):赛马娘卡的界面显示正常但点击全部失效,根因是 HTML 渲染开了、JS 尚未授权。
// 旧实现只在「HTML 未开」时提示,该状态没有任何线索 → 用户以为卡坏了。
import { describe, expect, it } from 'vitest';
import {
  hasRenderableScripts,
  shouldShowJsAuthHint,
  shouldShowRenderHint,
  type RenderHintState,
} from './renderHints';

function state(overrides: Partial<RenderHintState> = {}): RenderHintState {
  return {
    characterId: 'c1',
    renderHtml: false,
    scriptAuthorized: false,
    hasRenderable: true,
    renderHintDismissed: false,
    jsHintDismissed: false,
    ...overrides,
  };
}

describe('hasRenderableScripts', () => {
  it('有启用且带非空替换体的脚本 → true', () => {
    expect(hasRenderableScripts([{ enabled: true, replace_string: '<div>x</div>' }])).toBe(true);
  });

  it('禁用 / 空替换体 / 只有空白的替换体 → false', () => {
    expect(hasRenderableScripts([{ enabled: false, replace_string: '<div>x</div>' }])).toBe(false);
    expect(hasRenderableScripts([{ enabled: true, replace_string: '' }])).toBe(false);
    expect(hasRenderableScripts([{ enabled: true, replace_string: '   ' }])).toBe(false);
  });

  it('enabled 缺省视为启用(兼容精简形态)', () => {
    expect(hasRenderableScripts([{ replace_string: 'x' }])).toBe(true);
  });
});

describe('HTML 渲染提示 shouldShowRenderHint', () => {
  it('HTML 未开 + 有可渲染脚本 → 显示', () => {
    expect(shouldShowRenderHint(state({ renderHtml: false }))).toBe(true);
  });

  it('HTML 已开 → 不显示', () => {
    expect(shouldShowRenderHint(state({ renderHtml: true }))).toBe(false);
  });

  it('无角色 / 卡无可渲染脚本 → 不显示', () => {
    expect(shouldShowRenderHint(state({ characterId: null }))).toBe(false);
    expect(shouldShowRenderHint(state({ hasRenderable: false }))).toBe(false);
  });

  it('已按角色关闭 → 不显示', () => {
    expect(shouldShowRenderHint(state({ renderHintDismissed: true }))).toBe(false);
  });
});

describe('JS 授权提示 shouldShowJsAuthHint(实测缺口)', () => {
  it('HTML 已开 + 未授权 → 显示(界面显示但点击全失效的场景)', () => {
    expect(shouldShowJsAuthHint(state({ renderHtml: true, scriptAuthorized: false }))).toBe(true);
  });

  it('HTML 已开 + 已授权 → 不显示', () => {
    expect(shouldShowJsAuthHint(state({ renderHtml: true, scriptAuthorized: true }))).toBe(false);
  });

  it('HTML 未开 → 不显示(先由渲染提示引导,避免两条同时出现)', () => {
    expect(shouldShowJsAuthHint(state({ renderHtml: false, scriptAuthorized: false }))).toBe(false);
  });

  it('无角色 / 无可渲染脚本 → 不显示', () => {
    expect(shouldShowJsAuthHint(state({ characterId: null, renderHtml: true }))).toBe(false);
    expect(shouldShowJsAuthHint(state({ hasRenderable: false, renderHtml: true }))).toBe(false);
  });

  it('两条提示的关闭状态互相独立', () => {
    const jsDismissed = state({ renderHtml: false, jsHintDismissed: true });
    expect(shouldShowRenderHint(jsDismissed)).toBe(true);
    const htmlDismissed = state({ renderHtml: true, renderHintDismissed: true });
    expect(shouldShowJsAuthHint(htmlDismissed)).toBe(true);
  });

  it('同一角色关闭 JS 提示后不再显示', () => {
    expect(shouldShowJsAuthHint(state({ renderHtml: true, jsHintDismissed: true }))).toBe(false);
  });
});
