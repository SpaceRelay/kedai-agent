// @vitest-environment jsdom
// Sidebar 组件测试(阶段 A 补测):585 行,此前无测试。侧栏承载角色列表 / 任务列表 /
// 会话入口三类主路径,验证空态、列表渲染与「收起」动作不回退为直改 state。
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
    listCharacters: vi.fn().mockResolvedValue([]),
    listTasks: vi.fn().mockResolvedValue([]),
    listSessions: vi.fn().mockResolvedValue([]),
  };
});

import { useAppStore } from '../store';
import Sidebar from './Sidebar.vue';

function mountSidebar() {
  setActivePinia(createPinia());
  return { store: useAppStore(), wrapper: mount(Sidebar) };
}

describe('Sidebar 组件(阶段 A 补测)', () => {
  beforeEach(() => {
    memStorage.clear();
  });

  it('挂载渲染侧栏容器与品牌区', () => {
    const { wrapper } = mountSidebar();
    expect(wrapper.find('.sv-sidebar').exists()).toBe(true);
    expect(wrapper.find('.sv-side-head').exists()).toBe(true);
  });

  it('角色模式且无角色时显示「暂无角色」空态', () => {
    const { wrapper } = mountSidebar();
    expect(wrapper.text()).toContain('暂无角色');
  });

  it('任务模式且无任务时显示任务空态文案', () => {
    const { store, wrapper } = mountSidebar();
    store.appMode = 'task';
    // appMode 变化后需等待渲染
    return wrapper.vm.$nextTick().then(() => {
      expect(wrapper.text()).toContain('暂无任务');
    });
  });

  it('有角色时按列表渲染角色项,不再显示空态', () => {
    const { store } = mountSidebar();
    store.characters = [
      { id: 'c1', chara_name: '绫波', name: '绫波' },
      { id: 'c2', chara_name: '明日香', name: '明日香' },
    ] as never;
    const wrapper = mount(Sidebar);
    expect(wrapper.text()).not.toContain('暂无角色');
    expect(wrapper.text()).toContain('绫波');
    expect(wrapper.text()).toContain('明日香');
  });

  it('侧栏 open 类随 store.sidebarOpen 变化(移动端抽屉)', async () => {
    const { store, wrapper } = mountSidebar();
    expect(wrapper.find('.sv-sidebar').classes()).not.toContain('open');
    store.sidebarOpen = true;
    await wrapper.vm.$nextTick();
    expect(wrapper.find('.sv-sidebar').classes()).toContain('open');
  });
});
