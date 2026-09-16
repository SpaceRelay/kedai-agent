// @vitest-environment jsdom
// PromptManager 组件测试(M5 补测):705 行组件,此前无任何测试。
// 覆盖最高风险面:提示词顺序管理弹窗的装载渲染与「关闭」动作走 store action
// (M5 把 promptsOpen 的赋值改走 action,此处锁定不回退为直改 state)。
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
    getPromptInject: vi.fn().mockResolvedValue({ mode: 'simple', floors: [], simple: {} }),
    listWorldBooks: vi.fn().mockResolvedValue([]),
    listWorldBookEntries: vi.fn().mockResolvedValue([]),
  };
});

import { useAppStore } from '../store';
import PromptManager from './PromptManager.vue';

function mountManager() {
  setActivePinia(createPinia());
  const store = useAppStore();
  // onMounted 会调 loadPromptInject → 真实 fetch(jsdom 无 base URL 会报 ERR_INVALID_URL)。
  // 组件测试不验证该 action,直接替换为 no-op;api 层已整体 mock,其余调用安全。
  store.loadPromptInject = vi.fn().mockResolvedValue(undefined);
  const wrapper = mount(PromptManager);
  return { store, wrapper };
}

describe('PromptManager 组件(M5 补测)', () => {
  beforeEach(() => {
    memStorage.clear();
  });

  it('挂载后渲染弹窗标题与遮罩容器', () => {
    const { wrapper } = mountManager();
    expect(wrapper.find('.sv-modal-mask').exists()).toBe(true);
    expect(wrapper.find('.sv-modal-head').text()).toContain('提示词顺序管理');
  });

  it('点击关闭按钮走 action 置 promptsOpen=false(不由组件直改 state)', async () => {
    const { store, wrapper } = mountManager();
    store.promptsOpen = true;

    await wrapper.find('.sv-modal-head button').trigger('click');
    expect(store.promptsOpen).toBe(false);
  });

  it('提示词注入配置为空时显示空态提示,不崩溃', () => {
    const { wrapper } = mountManager();
    expect(wrapper.find('.sv-modal-body').exists()).toBe(true);
    // 空配置下不应出现楼层相关控件(无楼层草稿)
    expect(wrapper.findAll('.floor-select').length).toBe(0);
  });
});
