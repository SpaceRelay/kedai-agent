import { describe, expect, it, vi } from 'vitest';
import { ScriptRunner } from './scriptRunner';
import {
  executeSandboxedCharacterScript,
  sandboxAttributes,
  sandboxScript,
  type SandboxEnvironment,
} from './characterScriptSandbox';

function rootWithScope(scopeId: string): HTMLElement {
  const container = {} as HTMLElement;
  return {
    querySelector: (selector: string) => selector.includes(scopeId) ? container : null,
  } as unknown as HTMLElement;
}

function rootWithReplaceableScope(scopeId: string): { root: HTMLElement; replaceContainer: () => HTMLElement } {
  let container = {} as HTMLElement;
  return {
    root: {
      querySelector: (selector: string) => selector.includes(scopeId) ? container : null,
    } as unknown as HTMLElement,
    replaceContainer: () => {
      container = {} as HTMLElement;
      return container;
    },
  };
}

const context = {
  characterId: 'character-a',
  scriptHash: 'hash-a',
  isAuthorized: (characterId: string, hash: string) => characterId === 'character-a' && hash === 'hash-a',
  initVarEntries: {},
  mvuVariables: { stat_data: {}, display_data: {} },
};

function sandboxHarness() {
  let listener: ((event: MessageEvent) => void) | undefined;
  let iframe: HTMLIFrameElement;
  const postMessage = vi.fn();
  const childWindow = { postMessage } as unknown as Window;
  const environment: SandboxEnvironment = {
    document: {
      createElement: () => {
        iframe = {
          contentWindow: childWindow,
          setAttribute: vi.fn(),
          style: {},
          remove: vi.fn(),
        } as unknown as HTMLIFrameElement;
        return iframe;
      },
      body: { appendChild: vi.fn() } as unknown as HTMLElement,
    },
    window: {
      addEventListener: (_type, callback) => { listener = callback as (event: MessageEvent) => void; },
      removeEventListener: vi.fn(),
    },
    timeoutMs: 100,
  };
  return {
    environment,
    childWindow,
    postMessage,
    iframe: () => iframe!,
    dispatch: (data: unknown, source: MessageEventSource | null = null) => listener?.({ data, source } as MessageEvent),
    /** 从 iframe URL fragment 读取 nonce,模拟 bootstrap 带回认证 ready。 */
    boot: () => {
      const nonce = new URL(iframe.src, 'https://kedai.invalid').hash.slice(1);
      listener?.({ data: { channel: 'kedai-character-script-v1', nonce, type: 'ready' }, source: null } as MessageEvent);
      return postMessage.mock.calls
        .map((call) => call[0] as { type?: string; nonce?: string; script?: string })
        .find((message) => message.type === 'boot');
    },
  };
}

describe('ScriptRunner 强制授权与去重', () => {
  it('未授权时不进入脚本执行入口', async () => {
    const execute = vi.fn();
    const runner = new ScriptRunner(execute);
    await runner.runMessageScripts(rootWithScope('s1'), 1, [{ scopeId: 's1', scripts: ['work()'] }], {
      ...context,
      isAuthorized: () => false,
    });
    expect(execute).not.toHaveBeenCalled();
  });

  it('临时流消息不执行，落库消息同一角色与哈希只执行一次', async () => {
    const execute = vi.fn().mockResolvedValue(undefined);
    const runner = new ScriptRunner(execute);
    const block = [{ scopeId: 's1', scripts: ['work()'] }];
    const root = rootWithScope('s1');
    await runner.runMessageScripts(root, -1, block, context);
    await runner.runMessageScripts(root, 42, block, context);
    await runner.runMessageScripts(root, 42, block, context);
    expect(execute).toHaveBeenCalledTimes(1);
  });

  it('状态栏容器被 Vue 重建后,同一消息会对新容器重新执行', async () => {
    const execute = vi.fn().mockResolvedValue(undefined);
    const runner = new ScriptRunner(execute);
    const block = [{ scopeId: 's1', scripts: ['work()'] }];
    const { root, replaceContainer } = rootWithReplaceableScope('s1');

    await runner.runMessageScripts(root, 42, block, context);
    const replacement = replaceContainer();
    await runner.runMessageScripts(root, 42, block, context);

    expect(execute).toHaveBeenCalledTimes(2);
    expect(execute.mock.calls[1][1].container).toBe(replacement);
  });

  it('执行器失败时传播错误且同一消息可以重试', async () => {
    const execute = vi.fn()
      .mockRejectedValueOnce(new Error('首次失败'))
      .mockResolvedValueOnce(undefined);
    const runner = new ScriptRunner(execute);
    const block = [{ scopeId: 's1', scripts: ['work()'] }];
    const root = rootWithScope('s1');

    await expect(runner.runMessageScripts(root, 43, block, context)).rejects.toThrow('首次失败');
    await expect(runner.runMessageScripts(root, 43, block, context)).resolves.toBeUndefined();
    expect(execute).toHaveBeenCalledTimes(2);
  });

  it('容器缺失时抛错,容器出现后同一消息可以重试', async () => {
    const container = {} as HTMLElement;
    let mounted = false;
    const root = {
      querySelector: () => mounted ? container : null,
    } as unknown as HTMLElement;
    const execute = vi.fn().mockResolvedValue(undefined);
    const runner = new ScriptRunner(execute);
    const block = [{ scopeId: 's1', scripts: ['work()'] }];

    await expect(runner.runMessageScripts(root, 44, block, context)).rejects.toThrow('未找到状态栏脚本容器');
    mounted = true;
    await expect(runner.runMessageScripts(root, 44, block, context)).resolves.toBeUndefined();
    expect(execute).toHaveBeenCalledTimes(1);
  });

  it('同一 executionKey 并发调用共享执行,失败清除后可以重试', async () => {
    let rejectFirst!: (error: Error) => void;
    const firstExecution = new Promise<unknown>((_resolve, reject) => { rejectFirst = reject; });
    const execute = vi.fn()
      .mockReturnValueOnce(firstExecution)
      .mockResolvedValueOnce(undefined);
    const runner = new ScriptRunner(execute);
    const block = [{ scopeId: 's1', scripts: ['work()'] }];
    const root = rootWithScope('s1');

    const first = runner.runMessageScripts(root, 45, block, context);
    const concurrent = runner.runMessageScripts(root, 45, block, context);
    expect(execute).toHaveBeenCalledTimes(1);
    rejectFirst(new Error('并发执行失败'));
    await expect(first).rejects.toThrow('并发执行失败');
    await expect(concurrent).rejects.toThrow('并发执行失败');

    await expect(runner.runMessageScripts(root, 45, block, context)).resolves.toBeUndefined();
    expect(execute).toHaveBeenCalledTimes(2);
  });

  it('sandbox iframe 不获得同源权限', () => {
    expect(sandboxAttributes().sandbox).toBe('allow-scripts');
    expect(sandboxAttributes().sandbox).not.toContain('allow-same-origin');
  });

  // CSP 断言已移除:沙箱 CSP 现在由服务端 /sandbox.html 的响应头下发
  // (server-rs security::sandbox_headers),不再是前端 JS 生成物的一部分。
  it('沙箱脚本屏蔽网络全局、localStorage 用内存 shim(作者脚本读写不抛错),且协议不传 token', () => {
    const script = sandboxScript(
      'Dom.setText(".x", String(({}).constructor.constructor("return document")()))',
      'nonce-only',
      { stat_data: {}, display_data: {} },
    );
    // 网络全局仍屏蔽
    expect(script).toContain('fetch=undefined');
    expect(script).toContain('indexedDB=undefined');
    // localStorage 是内存 shim:作者脚本(如 WuWa 协议状态)可读写,但不落盘、与宿主隔离
    expect(script).toContain('__kdMemoryStorage');
    expect(script).not.toContain('localStorage=undefined');
    expect(script).not.toMatch(/apiToken|authorization|bearer/i);
    expect(script).toContain('constructor.constructor');
  });

  it('等待异步 ready 回调和带 id 的 batch ack 后才发送 done', () => {
    const script = sandboxScript('$(async()=>{ await work(); });', 'nonce-only', {
      stat_data: {}, display_data: {},
    });
    expect(script).toContain('await Promise.resolve(fn())');
    expect(script).toMatch(/send\('batch',\{id,ops\}\)/);
    expect(script).not.toContain("rpc('batch',batch)");
    expect(script).not.toContain('args[0]');
    expect(script).not.toContain('setTimeout(() => finish(), 150)');
  });

  it('iframe 接收响应不依赖 parent source,并以 pending id 匹配 rpc-result', () => {
    const script = sandboxScript('', 'nonce-only', { stat_data: {}, display_data: {} });
    expect(script).not.toContain('event.source!==parent');
    expect(script).toContain('m.channel!==CHANNEL||m.nonce!==NONCE');
    expect(script).toContain('const p=pending.get(m.id);if(!p)return');
  });

  it('沙箱文档以 fragment nonce 认证 ready 后宿主才投递 boot', () => {
    const harness = sandboxHarness();
    void executeSandboxedCharacterScript('populate();', {
      container: { querySelector: vi.fn() } as unknown as HTMLElement,
      variables: { stat_data: {}, display_data: {} },
    }, harness.environment);
    // iframe 走 src 加载服务端文档,fragment 不会进入 HTTP 请求且不需要 allow-same-origin。
    expect(harness.iframe().src).toMatch(/^\/sandbox\.html#[0-9a-f-]+$/i);
    expect(harness.iframe().srcdoc).toBeUndefined();

    harness.dispatch({ channel: 'kedai-character-script-v1', type: 'ready' });
    expect(harness.postMessage).not.toHaveBeenCalled();
    const boot = harness.boot();
    expect(boot?.script).toContain('populate();');
    expect(boot?.nonce).toBeTruthy();
  });

  it('按最终 boot 的 UTF-8 字节数拒绝超限脚本(上限 1MB)', async () => {
    const harness = sandboxHarness();
    // 1.2MB 脚本(角色卡状态栏脚本通常几十 KB,1MB 上限覆盖正常卡而防内存压力)
    const execution = executeSandboxedCharacterScript('a'.repeat(1_200_000), {
      container: { querySelector: vi.fn() } as unknown as HTMLElement,
      variables: { stat_data: {}, display_data: {} },
    }, harness.environment);

    harness.boot();
    await expect(execution).rejects.toThrow('角色卡脚本启动消息过大');
    expect(harness.postMessage).not.toHaveBeenCalled();
  });

  it('接受 WebView2 source wrapper,但仍严格校验 channel 与 nonce', async () => {
    const harness = sandboxHarness();
    const execution = executeSandboxedCharacterScript('', {
      container: { querySelector: vi.fn() } as unknown as HTMLElement,
      variables: { stat_data: {}, display_data: {} },
    }, harness.environment);
    // nonce 只存在于执行闭包内,唯一观察点是宿主投递给沙箱的 boot 消息
    const parsedNonce = harness.boot()?.nonce;
    expect(parsedNonce).toBeTruthy();

    harness.dispatch({ channel: 'other', nonce: parsedNonce, type: 'done' }, {} as Window);
    harness.dispatch({ channel: 'kedai-character-script-v1', nonce: 'wrong', type: 'done' }, {} as Window);
    let settled = false;
    void execution.finally(() => { settled = true; });
    await Promise.resolve();
    expect(settled).toBe(false);

    harness.dispatch({ channel: 'kedai-character-script-v1', nonce: parsedNonce, type: 'done' }, {} as Window);
    const cleanup = await execution;
    expect(cleanup).toBeTypeOf('function');
    cleanup();
  });

  it('沙箱源码支持 :first/:last 与受控事件 target 句柄', () => {
    const script = sandboxScript("$('.tab:first'); $('.tab:last').on('click', function(){ $(this).data('tab'); $(this).addClass('active'); });", 'nonce-only', {
      stat_data: {}, display_data: {},
    });
    expect(script).toContain("kind:'target'");
    expect(script).toContain('coll.data=function');
    expect(script).toContain(':first');
    expect(script).toContain(':last');
  });

  it('脚本执行完成后保留事件回调,调用 cleanup 才移除监听器与 iframe', async () => {
    const execute = vi.fn().mockResolvedValue(() => undefined);
    const runner = new ScriptRunner(execute);
    const block = [{ scopeId: 's1', scripts: ['work()'] }];
    const root = rootWithScope('s1');

    await runner.runMessageScripts(root, 46, block, context);
    expect(execute).toHaveBeenCalledTimes(1);
    const cleanup = runner.cleanup();
    expect(cleanup).toBe(1);
    expect(runner.cleanup()).toBe(0);
  });
});
