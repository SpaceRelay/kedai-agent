import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useUiPrefsStore } from './uiPrefs';

// uiPrefs:Agent 面板「自动展开一次,收起后不再弹」语义。
// 回归点:autoOpenAgentPanel 在用户主动收起后必须失效,复位后恢复。

// node 环境无 localStorage(store 初始化即访问),补内存桩
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

beforeEach(() => {
  memStorage.clear();
  setActivePinia(createPinia());
});

describe('uiPrefs Agent 面板开合语义', () => {
  it('默认收起,autoOpenAgentPanel 在未抑制时展开', () => {
    const store = useUiPrefsStore();
    expect(store.agentPanelOpen).toBe(false);
    store.autoOpenAgentPanel();
    expect(store.agentPanelOpen).toBe(true);
  });

  it('用户收起后,自动展开失效(收起后不再弹)', () => {
    const store = useUiPrefsStore();
    store.autoOpenAgentPanel();
    expect(store.agentPanelOpen).toBe(true);

    store.collapseAgentPanel();
    expect(store.agentPanelOpen).toBe(false);
    expect(store.agentPanelAutoSuppressed).toBe(true);

    // 后续每次生成开始的自动展开都被抑制
    store.autoOpenAgentPanel();
    expect(store.agentPanelOpen).toBe(false);
  });

  it('用户显式展开会清除抑制,下一次生成仍可自动展开', () => {
    const store = useUiPrefsStore();
    store.collapseAgentPanel();
    expect(store.agentPanelAutoSuppressed).toBe(true);

    store.openAgentPanel();
    expect(store.agentPanelOpen).toBe(true);
    expect(store.agentPanelAutoSuppressed).toBe(false);

    store.collapseAgentPanel();
    store.openAgentPanel(); // 再次显式展开
    store.collapseAgentPanel();
    store.resetAgentPanelAutoSuppress(); // 新会话复位
    store.autoOpenAgentPanel();
    expect(store.agentPanelOpen).toBe(true);
  });

  it('toggleAgentPanel 依据当前态开合,收起时记抑制', () => {
    const store = useUiPrefsStore();
    store.toggleAgentPanel();
    expect(store.agentPanelOpen).toBe(true);
    expect(store.agentPanelAutoSuppressed).toBe(false);

    store.toggleAgentPanel();
    expect(store.agentPanelOpen).toBe(false);
    expect(store.agentPanelAutoSuppressed).toBe(true);
  });
});
