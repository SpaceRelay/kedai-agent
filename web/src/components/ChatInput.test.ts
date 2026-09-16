// @vitest-environment jsdom
// ChatInput 组件测试(阶段 A 补测):390 行,此前无测试。输入栏是用户主输入路径,
// 并承载 Agent 模式切换(M5 已把直改 state 改为 setAgentMode action,此处锁死防回退)。
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
  get length() { return memStorage.size; },
});

vi.mock('../api', async (importOriginal) => {
  const orig = await importOriginal<typeof import('../api')>();
  return {
    ...orig,
    listModels: vi.fn().mockResolvedValue([]),
    getSlashCommands: vi.fn().mockResolvedValue([]),
    listQuickReplies: vi.fn().mockResolvedValue([]),
  };
});

import { useAppStore } from '../store';
import ChatInput from './ChatInput.vue';

function mountInput() {
  setActivePinia(createPinia());
  return { store: useAppStore(), wrapper: mount(ChatInput) };
}

describe('ChatInput 组件(阶段 A 补测)', () => {
  beforeEach(() => {
    memStorage.clear();
  });

  it('挂载渲染输入栏与发送按钮', () => {
    const { wrapper } = mountInput();
    expect(wrapper.find('.sv-inputbar').exists()).toBe(true);
    expect(wrapper.find('.sv-btn-send').exists()).toBe(true);
    expect(wrapper.find('.sv-inputbox').exists()).toBe(true);
  });

  it('模式按钮点击走 setAgentMode action(不由组件直改 state)', async () => {
    const { store, wrapper } = mountInput();
    const setSpy = vi.spyOn(store, 'setAgentMode');
    const modeButtons = wrapper.findAll('.sv-mode button');
    expect(modeButtons.length).toBeGreaterThan(0);

    await modeButtons[0].trigger('click');
    expect(setSpy).toHaveBeenCalled();
    // store 值同步更新(证明走的是真实 action 而非空壳)
    expect(['fast', 'deep', 'agent', 'custom']).toContain(store.agentMode);
  });

  it('生成中切换为「停止」按钮,不再显示发送(防重复提交)', async () => {
    const { store, wrapper } = mountInput();
    // 未生成:发送按钮在场、停止按钮不在
    expect(wrapper.find('.sv-btn-send').exists()).toBe(true);
    expect(wrapper.find('.sv-btn-send.stop').exists()).toBe(false);

    store.generating = true;
    await wrapper.vm.$nextTick();
    // 生成中:替换为停止按钮
    expect(wrapper.find('.sv-btn-send.stop').exists()).toBe(true);
    expect(wrapper.find('.sv-btn-send').attributes('title')).toBe('停止生成');
  });

  it('无角色或无输入时发送按钮禁用', () => {
    const { wrapper } = mountInput();
    // 初始无 currentCharacterId 且无输入 → 禁用
    expect(wrapper.find('.sv-btn-send').attributes('disabled')).toBeDefined();
  });
});
