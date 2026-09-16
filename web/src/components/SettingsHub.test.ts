// @vitest-environment jsdom
// SettingsHub 组件测试(阶段 A 补测):332 行,设置中心两层导航,此前无测试。
// 覆盖:导航区渲染、一级域切换、二级项点击切分区、以及 task 模式下的分区过滤
// (角色扮演专属项被隐藏;「执行流程」必须可见——IFW-3 入口错位回归)。
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { nextTick } from 'vue';
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
  return { ...orig, listModels: vi.fn().mockResolvedValue([]), getSettings: vi.fn().mockResolvedValue({}) };
});
vi.mock('../api/health', () => ({
  health: vi.fn().mockResolvedValue({ ok: true, version: '0.0.0-test' }),
}));

// 屏蔽分区懒加载(本用例只验证「设置中心导航 + 任务模式下分区过滤」,与分区内部无关)。
//
// 必要性(2026-09-16 实测):SettingsModal 的 9 个分区经 lazyModal→defineAsyncComponent
// 动态 import,「点击域」会触发加载。测试随即结束、import 仍在飞行,环境拆除后该 Promise
// 才 reject → lazyModal 的 onError 重试一次 → 二次失败触发 console.error(报错文案与
// uiPrefs.modalLoadError 写入)→ 该 console 调用在 worker 关闭时呈 pending(vitest 报
// EnvironmentTeardownError: Closing rpc while "onUserConsoleLog" was pending)→ **退出码 1**,
// 而断言其实全绿。它只在全量套件下复现(单文件跑 10 次 0 失败;加入第 89 个测试文件后
// 触发率约 4/24),因为能否命中取决于 vitest 的 worker 调度——属测试生命周期缺陷而非产品缺陷。
// 屏蔽懒加载后本文件不再产生任何挂起的动态 import,判定确定。
vi.mock('../asyncModal', () => ({
  lazyModal: () => ({ name: 'StubSettingsSection', render: () => null }),
}));

import { useAppStore } from '../store';
import SettingsHub from './SettingsHub.vue';

function mountHub() {
  setActivePinia(createPinia());
  return { store: useAppStore(), wrapper: mount(SettingsHub) };
}

describe('SettingsHub 组件(阶段 A 补测)', () => {
  beforeEach(() => {
    memStorage.clear();
  });

  it('挂载渲染设置中心容器与左侧导航', () => {
    const { wrapper } = mountHub();
    expect(wrapper.find('.sv-modal-mask').exists()).toBe(true);
    expect(wrapper.find('.sv-hub-nav').exists()).toBe(true);
    expect(wrapper.findAll('.sv-hub-nav-item').length).toBeGreaterThan(0);
  });

  it('一级域按钮带序号渲染(01/02…)', () => {
    const { wrapper } = mountHub();
    expect(wrapper.find('.sv-hub-nav-idx').text()).toBe('01');
  });

  it('点击一级域后该域呈 active 态', async () => {
    const { wrapper } = mountHub();
    const items = wrapper.findAll('.sv-hub-nav-item');
    expect(items.length).toBeGreaterThan(1);

    await items[1].trigger('click');
    expect(items[1].classes()).toContain('active');
  });

  it('点击二级 section 项切换右侧内容区(active 态转移)', async () => {
    const { wrapper } = mountHub();
    const subs = wrapper.findAll('.hub-sub-item');
    expect(subs.length).toBeGreaterThan(0);
    await subs[0].trigger('click');
    // section 项点击后应有一个二级项为 active
    const activeCount = wrapper.findAll('.hub-sub-item.active').length;
    expect(activeCount).toBe(1);
  });

  it('任务模式下「执行流程」入口可见(IFW-3 回归)', async () => {
    const { store, wrapper } = mountHub();
    store.appMode = 'task';
    await nextTick();

    // 「对话与提示词」域在任务模式下整域隐藏,故一级域下标与角色扮演模式不同,按文案定位
    const agentDomain = wrapper
      .findAll('.sv-hub-nav-item')
      .find((b) => b.text().includes('Agent 与任务'));
    expect(agentDomain).toBeTruthy();
    await agentDomain?.trigger('click');

    const labels = wrapper.findAll('.hub-sub-item').map((it) => it.text());
    expect(labels).toContain('执行流程');
  });

  it('任务模式下角色扮演专属域仍整体隐藏(防过度放行)', async () => {
    const { store, wrapper } = mountHub();
    store.appMode = 'task';
    await nextTick();

    // 「对话与提示词」只含 prompt/preset,两者都属角色扮演专属 → 整域被过滤
    const navLabels = wrapper.findAll('.sv-hub-nav-item').map((b) => b.text());
    expect(navLabels.some((t) => t.includes('对话与提示词'))).toBe(false);
    expect(navLabels.some((t) => t.includes('Agent 与任务'))).toBe(true);
  });
});
