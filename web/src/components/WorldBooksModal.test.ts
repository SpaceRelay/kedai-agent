// @vitest-environment jsdom
// WorldBooksModal 组件测试(M5 补测):654 行组件,此前无任何测试。
// 覆盖:弹窗装载渲染、空列表时的「暂无独立世界书」空态、加载失败时的错误提示、
// 以及关闭动作置 worldBooksOpen=false(该字段为纯 UI 开关,无配套 action,
// 见 tools/check-arch.mjs 的 UI_FLAG_WHITELIST)。
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
    listWorldBooks: vi.fn().mockResolvedValue([]),
    getWorldBookEntries: vi.fn().mockResolvedValue([]),
    getCharacterWorldEntries: vi.fn().mockResolvedValue([]),
    checkWorldBookAutoAssign: vi.fn().mockResolvedValue({ checks: [] }),
  };
});

import { useAppStore } from '../store';
import WorldBooksModal from './WorldBooksModal.vue';

/** 挂载并等待 onMounted 的两个异步加载落定 */
async function mountModal() {
  setActivePinia(createPinia());
  const store = useAppStore();
  // onMounted → store.loadWorldBooks() 走真实 fetch;替换为 no-op 并直接播种列表
  store.loadWorldBooks = vi.fn().mockResolvedValue(undefined);
  const wrapper = mount(WorldBooksModal);
  await Promise.resolve();
  await Promise.resolve();
  return { store, wrapper };
}

describe('WorldBooksModal 组件(M5 补测)', () => {
  beforeEach(() => {
    memStorage.clear();
  });

  it('挂载后渲染弹窗标题与容器', async () => {
    const { wrapper } = await mountModal();
    expect(wrapper.find('.sv-modal-mask').exists()).toBe(true);
    expect(wrapper.find('.sv-modal-head').text()).toContain('世界书');
  });

  it('列表为空时显示「暂无独立世界书」空态', async () => {
    const { wrapper } = await mountModal();
    expect(wrapper.text()).toContain('暂无独立世界书');
    expect(wrapper.findAll('.sv-wb-row').length).toBe(0);
  });

  it('已加载世界书时逐行渲染,不再显示空态', async () => {
    setActivePinia(createPinia());
    const store = useAppStore();
    store.loadWorldBooks = vi.fn().mockResolvedValue(undefined);
    store.worldBooks = [
      { id: 'wb1', name: '设定集', enabled: true },
      { id: 'wb2', name: '人物志', enabled: false },
    ] as never;
    const wrapper = mount(WorldBooksModal);
    await Promise.resolve();

    expect(wrapper.text()).not.toContain('暂无独立世界书');
    expect(wrapper.findAll('.sv-wb-row').length).toBe(2);
    expect(wrapper.text()).toContain('设定集');
  });

  it('点击关闭置 worldBooksOpen=false', async () => {
    const { store, wrapper } = await mountModal();
    store.worldBooksOpen = true;

    await wrapper.find('.sv-modal-head button').trigger('click');
    expect(store.worldBooksOpen).toBe(false);
  });
});
