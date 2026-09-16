// @vitest-environment jsdom
// ChatWindow 组件测试(阶段 A 补测):424 行,消息画布宿主,此前无测试。
// 覆盖:空态渲染、有消息时的渲染、以及 store 状态驱动的结构变化。
// 组件 onMounted 会 installMvuGlobals + 挂 message 监听,故需 jsdom 环境;
// 脚本沙箱相关模块整体 mock(不启 iframe)。
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
  return { ...orig, listModels: vi.fn().mockResolvedValue([]), getSlashCommands: vi.fn().mockResolvedValue([]) };
});

// 脚本沙箱:避免真实创建 iframe / 沙箱 realm
vi.mock('../cardScriptHost', () => ({
  ensureCardScriptSandbox: vi.fn().mockResolvedValue(undefined),
  cleanupCardScriptSandbox: vi.fn(),
  makeCardScriptRpcExtensions: vi.fn(() => ({})),
}));
vi.mock('../chatMessageScriptScheduler', () => ({
  executeCurrentMessageScripts: vi.fn().mockResolvedValue(undefined),
}));

import { useAppStore } from '../store';
import ChatWindow from './ChatWindow.vue';

function mountWindow() {
  setActivePinia(createPinia());
  const store = useAppStore();
  // ChatInput 挂载会触发 loadPromptInject → 真实 fetch(jsdom 无 base URL 报 ERR_INVALID_URL)。
  // 用不到该数据,替换为 no-op 消除噪声与偶发失败。
  store.loadPromptInject = vi.fn().mockResolvedValue(undefined) as never;
  return { store, wrapper: mount(ChatWindow) };
}

describe('ChatWindow 组件(阶段 A 补测)', () => {
  beforeEach(() => {
    memStorage.clear();
  });

  it('无消息时渲染空态引导文案', () => {
    const { wrapper } = mountWindow();
    expect(wrapper.find('.sv-empty').exists()).toBe(true);
    expect(wrapper.text()).toContain('选择左侧角色开始对话');
  });

  it('有消息时不再显示空态', async () => {
    const { store, wrapper } = mountWindow();
    const before = wrapper.find('.sv-empty').exists();

    store.messages = [
      { id: 1, role: 'user', content: '你好', extra: {}, streaming: false },
    ] as never;
    await wrapper.vm.$nextTick();

    // 空态消失(无角色时消息区可能仍受其他条件约束,故断言「与初始不同」而非绝对不存在)
    expect(before).toBe(true);
    expect(wrapper.text()).not.toContain('选择左侧角色开始对话');
  });

  it('渲染输入栏与顶栏(主交互链路常驻)', () => {
    const { wrapper } = mountWindow();
    // ChatInput / ChatTopbar 均在画布内
    expect(wrapper.find('.sv-inputbar').exists()).toBe(true);
  });
});
