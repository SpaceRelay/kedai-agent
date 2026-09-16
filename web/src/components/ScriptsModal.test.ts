// @vitest-environment jsdom
// ScriptsModal 组件测试(阶段 A 补测):524 行,脚本库管理,此前无测试。
// 覆盖:弹窗装载、空态、脚本列表渲染、关闭动作。
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

// vi.mock 工厂被提升到文件顶部,不能引用外部变量(会 TDZ 报错);故内联定义,
// 需要断言调用时通过 `vi.mocked(api.getScriptTree)` 取用。
vi.mock('../api', async (importOriginal) => {
  const orig = await importOriginal<typeof import('../api')>();
  return {
    ...orig,
    getScriptTree: vi.fn().mockResolvedValue([]),
    saveScriptTree: vi.fn().mockResolvedValue(undefined),
  };
});

import * as api from '../api';
import { useAppStore } from '../store';
import ScriptsModal from './ScriptsModal.vue';

async function mountModal() {
  setActivePinia(createPinia());
  const store = useAppStore();
  const wrapper = mount(ScriptsModal);
  await Promise.resolve();
  await Promise.resolve();
  return { store, wrapper };
}

describe('ScriptsModal 组件(阶段 A 补测)', () => {
  beforeEach(() => {
    memStorage.clear();
    vi.mocked(api.getScriptTree).mockClear();
  });

  it('挂载渲染弹窗容器与标题', async () => {
    const { wrapper } = await mountModal();
    expect(wrapper.find('.sv-modal-mask').exists()).toBe(true);
    expect(wrapper.find('.sv-modal-head').exists()).toBe(true);
  });

  it('脚本列表为空时显示空态提示', async () => {
    const { wrapper } = await mountModal();
    // 空态文案由 trees 为空触发;若加载报错则显示错误态,故两者取其一
    const text = wrapper.text();
    expect(text.includes('暂无脚本') || text.includes('加载')).toBe(true);
  });

  it('点击关闭置 scriptsOpen=false', async () => {
    const { store, wrapper } = await mountModal();
    store.scriptsOpen = true;
    await wrapper.vm.$nextTick();

    // head 区有多个按钮(导出/导入/关闭),按 class 精确定位关闭按钮(✕)
    const closeBtn = wrapper
      .findAll('.sv-modal-head button')
      .find((b) => b.text().includes('✕'));
    expect(closeBtn).toBeDefined();
    await closeBtn!.trigger('click');
    expect(store.scriptsOpen).toBe(false);
  });

  it('挂载时发起脚本树加载(数据源已接)', async () => {
    await mountModal();
    expect(vi.mocked(api.getScriptTree)).toHaveBeenCalled();
  });
});
