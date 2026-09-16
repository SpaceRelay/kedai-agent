// @vitest-environment jsdom
// 全局错误兜底(计划批次 5.1 / 遗留「项 10」):三条路径各自上报、去重与截断、卸载后不再上报。
// jsdom 环境:需要一个真实 window 来验证 addEventListener/removeEventListener 的成对行为。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { describeError, installGlobalErrorHandlers } from './globalErrorHandlers';

/** 可注入的时间源:去重窗口是时间相关的,用可控时钟避免真的 wait 3s */
function fakeClock(start = 0) {
  let t = start;
  return { now: () => t, advance: (ms: number) => (t += ms) };
}

/** 直接派发原生事件(比构造 ErrorEvent 更贴近真实 addEventListener 路径) */
function dispatchErrorEvent(message: string, error?: unknown): void {
  const ev = new Event('error') as Event & { error?: unknown; message?: string };
  ev.message = message;
  if (error !== undefined) ev.error = error;
  window.dispatchEvent(ev);
}

function dispatchRejection(reason: unknown): void {
  const ev = new Event('unhandledrejection') as Event & { reason?: unknown };
  ev.reason = reason;
  window.dispatchEvent(ev);
}

/**
 * 每个用例注册的卸载函数。集中在此处卸载而非用例末尾调用:
 * 用例中途断言失败时,末尾的 uninstall() 不会执行,监听会残留到下一个用例
 * (window 在同一测试文件内共享),导致后续用例收到双份回调、出现与断言无关的失败。
 * 放在 afterEach 里则无论用例如何结束都会清理。
 */
const uninstallers: Array<() => void> = [];

/** 安装兜底并登记卸载(用例内不要自己调返回的卸载函数,除非用例本身在验证卸载) */
function install(opts: Parameters<typeof installGlobalErrorHandlers>[0]): () => void {
  const uninstall = installGlobalErrorHandlers(opts);
  uninstallers.push(uninstall);
  return uninstall;
}

afterEach(() => {
  while (uninstallers.length) uninstallers.pop()!();
  vi.restoreAllMocks();
});

// 被测模块有意在 console.error 留痕;多数用例只断言 onError 回调,故默认静音,
// 需要断言 console 的用例内自行 spyOn(restoreAllMocks 会恢复)。
let consoleSpy: ReturnType<typeof vi.spyOn>;
beforeEach(() => {
  consoleSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
});
afterEach(() => {
  consoleSpy.mockRestore();
});

describe('全局错误兜底(批次 5.1)', () => {
  it('三条路径各自上报:Vue errorHandler / unhandledrejection / window error', () => {
    const onError = vi.fn();
    const app = { config: {} as { errorHandler?: (e: unknown, i: unknown, info: string) => void } };
    install({ onError, app, now: () => 0 });

    // ① Vue 组件内同步异常
    app.config.errorHandler!(new Error('组件渲染炸了'), null, 'render');
    expect(onError).toHaveBeenLastCalledWith('组件渲染炸了');

    // ② 未处理的 Promise 拒绝
    dispatchRejection(new Error('异步请求失败'));
    expect(onError).toHaveBeenLastCalledWith('异步请求失败');

    // ③ 组件外的未捕获同步异常(e.error 优先)
    dispatchErrorEvent('ignored', new Error('定时器里炸了'));
    expect(onError).toHaveBeenLastCalledWith('定时器里炸了');
    // e.error 缺失时退回 e.message
    dispatchErrorEvent('裸错误消息');
    expect(onError).toHaveBeenLastCalledWith('裸错误消息');

    expect(onError).toHaveBeenCalledTimes(4);
  });

  it('同一消息在去重窗口内只上报一次,窗口过后可再次上报', () => {
    const onError = vi.fn();
    const clock = fakeClock();
    install({ onError, now: clock.now });

    // 渲染路径上的异常会每帧重复抛出:三次相同消息只报一次
    dispatchRejection(new Error('每帧都炸'));
    dispatchRejection(new Error('每帧都炸'));
    dispatchRejection(new Error('每帧都炸'));
    expect(onError).toHaveBeenCalledTimes(1);

    // 窗口(3000ms)内仍去重
    clock.advance(2999);
    dispatchRejection(new Error('每帧都炸'));
    expect(onError).toHaveBeenCalledTimes(1);

    // 越过窗口后可再次上报(否则横幅关掉后再出同类错误就永远静默了)
    clock.advance(2);
    dispatchRejection(new Error('每帧都炸'));
    expect(onError).toHaveBeenCalledTimes(2);

    // 不同消息互不影响
    dispatchRejection(new Error('另一条错误'));
    expect(onError).toHaveBeenCalledTimes(3);
  });

  it('超长消息被截断(不把整段内容铺进横幅)', () => {
    const onError = vi.fn();
    install({ onError, now: () => 0 });

    const long = 'x'.repeat(1000);
    dispatchRejection(new Error(long));
    const shown = onError.mock.calls[0][0] as string;
    expect(shown.length).toBeLessThanOrEqual(301); // 300 + 省略号
    expect(shown.endsWith('…')).toBe(true);
    expect(shown.startsWith('xxx')).toBe(true);
  });

  it('卸载后三条路径都不再上报(测试隔离与热更新场景不残留监听)', () => {
    const onError = vi.fn();
    const app = { config: {} as { errorHandler?: (e: unknown, i: unknown, info: string) => void } };
    const uninstall = install({ onError, app, now: () => 0 });

    onError.mockClear();
    uninstall();

    dispatchRejection(new Error('卸载后的拒绝'));
    dispatchErrorEvent('卸载后的错误');
    expect(app.config.errorHandler, 'errorHandler 应复位').toBeUndefined();
    expect(onError).not.toHaveBeenCalled();
  });

  it('describeError 归一化各类抛出值(跨 realm Error / 字符串 / 对象 / 空值)', () => {
    expect(describeError(new Error('普通'))).toBe('普通');
    // 跨 realm(iframe)的 Error 不是本 realm 实例,但有 message
    expect(describeError({ message: '跨 realm' })).toBe('跨 realm');
    expect(describeError('字符串抛出')).toBe('字符串抛出');
    expect(describeError(null)).toBe('null');
    expect(describeError(new Error(''))).toBe('Error'); // message 为空退回 name
    expect(describeError(new Error('   '))).toBe('未知错误'); // 纯空白视为空
  });

  it('上报通道自身抛错时不外泄(避免与 unhandledrejection 形成递归)', () => {
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const onError = vi.fn(() => {
      throw new Error('横幅写入失败');
    });
    install({ onError, now: () => 0 });

    expect(() => dispatchRejection(new Error('原始错误'))).not.toThrow();
    expect(onError).toHaveBeenCalledTimes(1);
    // 两次 console.error:一条原始异常留痕,一条上报失败留痕
    expect(spy).toHaveBeenCalledTimes(2);
  });
});
