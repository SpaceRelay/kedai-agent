import { describe, expect, it, vi } from 'vitest';
import { executeCurrentMessageScripts } from './chatMessageScriptScheduler';

const assistantMessage = {
  id: 42,
  role: 'assistant' as const,
  content: '正文',
  extra: {},
};

function createHarness(authorized = true) {
  const root = {} as HTMLElement;
  const runMessageScripts = vi.fn().mockResolvedValue(undefined);
  const options = {
    renderHtml: true,
    authorized,
    scripts: [{ id: 'script-1' }],
    messages: [assistantMessage],
    characterId: 'character-a',
    scriptHash: 'hash-a',
    initVarEntries: {},
    mvuVariables: { stat_data: {}, display_data: {} },
    isAuthorized: vi.fn(() => authorized),
    // 返回类型与 src options 契约对齐(() => HTMLElement | null):
    // 不标注时 vi.fn(() => root) 推导为 () => HTMLElement,测试覆写返回 null 会报错
    getScrollArea: vi.fn((): HTMLElement | null => root),
    afterRender: vi.fn().mockResolvedValue(undefined),
    renderText: vi.fn(() => '渲染正文'),
    depthOf: vi.fn((_m: unknown, i: number, total: number) => total - 1 - i),
    renderScripts: vi.fn(() => ({ blocks: [{ scopeId: 'scope-a', scripts: ['work()'] }] })),
    runMessageScripts,
  };
  return { options, root, runMessageScripts };
}

describe('executeCurrentMessageScripts', () => {
  it('首次挂载时已有 assistant 消息也会执行脚本', async () => {
    const { options, root, runMessageScripts } = createHarness();

    await executeCurrentMessageScripts(options);

    expect(options.afterRender).toHaveBeenCalledOnce();
    expect(runMessageScripts).toHaveBeenCalledOnce();
    expect(runMessageScripts).toHaveBeenCalledWith(
      root,
      42,
      [{ scopeId: 'scope-a', scripts: ['work()'] }],
      expect.objectContaining({ characterId: 'character-a', scriptHash: 'hash-a' }),
    );
    // 实跑问题 7 R2:调度器把真实楼层深度传给 renderScripts(与渲染侧同口径)
    expect(options.renderScripts).toHaveBeenCalledWith('渲染正文', options.scripts, 'msg-42', 0);
  });

  it('未授权时跳过,授权 false -> true 后补执行已有消息', async () => {
    const { options, runMessageScripts } = createHarness(false);

    await executeCurrentMessageScripts(options);
    expect(runMessageScripts).not.toHaveBeenCalled();

    options.authorized = true;
    options.isAuthorized = vi.fn(() => true);
    await executeCurrentMessageScripts(options);

    expect(runMessageScripts).toHaveBeenCalledOnce();
  });

  it('渲染后滚动容器仍不存在时安全跳过', async () => {
    const { options, runMessageScripts } = createHarness();
    options.getScrollArea = vi.fn(() => null);

    await executeCurrentMessageScripts(options);

    expect(runMessageScripts).not.toHaveBeenCalled();
  });
});
