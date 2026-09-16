// storeBridge:六条桥的「注册 / 降级」配对测试(批次 5.1)。
//
// 桥是模块级单例(注册后进程内长期有效),故每个用例用 vi.resetModules() 取全新
// 模块实例,避免用例之间互相串注册状态。

import { beforeEach, describe, expect, it, vi } from 'vitest';

type BridgeModule = typeof import('./storeBridge');

/** 取一份未注册过任何实现的桥模块(等价于「对应 store 从未初始化」)。 */
async function freshBridge(): Promise<BridgeModule> {
  vi.resetModules();
  return import('./storeBridge');
}

beforeEach(() => {
  vi.restoreAllMocks();
});

describe('characterId 桥:注册取值 / 未注册降级 null', () => {
  it('注册后 currentCharacterIdValue 返回注册闭包的当下值', async () => {
    const bridge = await freshBridge();
    let current = 'char-1';
    bridge.registerCharacterIdProvider(() => current);
    expect(bridge.currentCharacterIdValue()).toBe('char-1');
    // 闭包是「读值函数」:注册后源状态变化仍能读到最新值
    current = 'char-2';
    expect(bridge.currentCharacterIdValue()).toBe('char-2');
  });

  it('未注册时返回 null(等同「未选角色」,不抛错)', async () => {
    const bridge = await freshBridge();
    expect(bridge.currentCharacterIdValue()).toBeNull();
  });
});

describe('settingsSaver 桥:注册转调 / 未注册 reject', () => {
  it('注册后 queueSettingsSave 把 patch 原样转给注册函数', async () => {
    const bridge = await freshBridge();
    const saver = vi.fn(async () => {});
    bridge.registerSettingsSaver(saver);
    await bridge.queueSettingsSave({ default_temperature: 0.5 });
    expect(saver).toHaveBeenCalledTimes(1);
    expect(saver).toHaveBeenCalledWith({ default_temperature: 0.5 });
  });

  it('未注册时返回 reject(调用方走「保存失败回滚」既有分支)', async () => {
    const bridge = await freshBridge();
    await expect(bridge.queueSettingsSave({})).rejects.toThrow('设置服务尚未就绪');
  });
});

describe('settingsLoader 桥:注册转调 / 未注册 no-op', () => {
  it('注册后 reloadSettings 同步转调注册函数', async () => {
    const bridge = await freshBridge();
    const loader = vi.fn(async () => {});
    bridge.registerSettingsLoader(loader);
    bridge.reloadSettings();
    expect(loader).toHaveBeenCalledTimes(1);
  });

  it('未注册时 reloadSettings 静默 no-op(不抛错)', async () => {
    const bridge = await freshBridge();
    expect(() => bridge.reloadSettings()).not.toThrow();
  });
});

describe('renderHtmlDefault 桥:注册通知 / 未注册 no-op', () => {
  it('注册后 notifyRenderHtmlDefault 把默认值转给注册 sink', async () => {
    const bridge = await freshBridge();
    const sink = vi.fn();
    bridge.registerRenderHtmlDefaultSink(sink);
    bridge.notifyRenderHtmlDefault(true);
    expect(sink).toHaveBeenCalledTimes(1);
    expect(sink).toHaveBeenCalledWith(true);
  });

  it('未注册时 notifyRenderHtmlDefault 静默 no-op(仅影响展示值)', async () => {
    const bridge = await freshBridge();
    expect(() => bridge.notifyRenderHtmlDefault(false)).not.toThrow();
  });
});

describe('chatEvent 桥:注册上报 / 未注册 no-op', () => {
  it('注册后 reportChatEvent 把事件原样转给注册 sink', async () => {
    const bridge = await freshBridge();
    const sink = vi.fn();
    bridge.registerChatEventSink(sink);
    const ev: { type: 'interrupted' } = { type: 'interrupted' };
    bridge.reportChatEvent(ev);
    expect(sink).toHaveBeenCalledTimes(1);
    expect(sink).toHaveBeenCalledWith(ev);
  });

  it('未注册时 reportChatEvent 静默 no-op', async () => {
    const bridge = await freshBridge();
    expect(() => bridge.reportChatEvent({ type: 'interrupted' })).not.toThrow();
  });
});

describe('uiPrefs 桥:注册取能力 / 未注册安全空实现', () => {
  it('注册后 uiPrefsBridge 返回注册对象(动作与读值同源)', async () => {
    const bridge = await freshBridge();
    let open = true;
    const collapse = vi.fn();
    const autoOpen = vi.fn();
    bridge.registerUiPrefsSink({
      collapseAgentPanel: collapse,
      autoOpenAgentPanel: autoOpen,
      isCallTraceOpen: () => open,
    });
    const sink = bridge.uiPrefsBridge();
    sink.collapseAgentPanel();
    sink.autoOpenAgentPanel();
    expect(collapse).toHaveBeenCalledTimes(1);
    expect(autoOpen).toHaveBeenCalledTimes(1);
    expect(sink.isCallTraceOpen()).toBe(true);
    open = false;
    expect(sink.isCallTraceOpen()).toBe(false);
  });

  it('未注册时返回安全空实现:方法不崩、isCallTraceOpen 恒 false', async () => {
    const bridge = await freshBridge();
    const sink = bridge.uiPrefsBridge();
    expect(() => sink.collapseAgentPanel()).not.toThrow();
    expect(() => sink.autoOpenAgentPanel()).not.toThrow();
    expect(sink.isCallTraceOpen()).toBe(false);
  });
});

describe('owner 注册诊断', () => {
  it('同一 owner 重复注册为幂等覆盖,不告警', async () => {
    const bridge = await freshBridge();
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    bridge.storeBridges.characterId.register('character', () => 'a');
    bridge.storeBridges.characterId.register('character', () => 'b');
    expect(warn).not.toHaveBeenCalled();
    expect(bridge.currentCharacterIdValue()).toBe('b');
  });

  it('不同 owner 覆盖同一桥时告警,且以后一次注册为准', async () => {
    const bridge = await freshBridge();
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    bridge.storeBridges.characterId.register('character', () => 'a');
    bridge.storeBridges.characterId.register('冒名者', () => 'b');
    expect(warn).toHaveBeenCalledTimes(1);
    expect(warn.mock.calls[0][0]).toContain('characterId');
    expect(warn.mock.calls[0][0]).toContain('character');
    expect(warn.mock.calls[0][0]).toContain('冒名者');
    expect(bridge.currentCharacterIdValue()).toBe('b');
  });
});
