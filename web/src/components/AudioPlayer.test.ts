// @vitest-environment jsdom
// AudioPlayer 组件测试(阶段 A 补测):513 行,双通道音频面板,此前无测试。
// 覆盖:折叠态、通道 tab 渲染与切换、通道标签文案。
// 组件用原生 <audio> 单实例,jsdom 不下真实媒体栈(play() 未实现),故只断言结构与状态。
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
    getAudioState: vi.fn().mockResolvedValue({ bgm: {}, ambient: {} }),
    listAudioTracks: vi.fn().mockResolvedValue([]),
  };
});

import { useAppStore } from '../store';
import AudioPlayer from './AudioPlayer.vue';

async function mountPlayer() {
  setActivePinia(createPinia());
  const store = useAppStore();
  store.loadAudio = vi.fn().mockResolvedValue(undefined) as never;
  const wrapper = mount(AudioPlayer);
  await Promise.resolve();
  return { store, wrapper };
}

describe('AudioPlayer 组件(阶段 A 补测)', () => {
  beforeEach(() => {
    memStorage.clear();
  });

  it('挂载渲染播放器面板与标题', async () => {
    const { wrapper } = await mountPlayer();
    expect(wrapper.find('.sv-audio-panel').exists()).toBe(true);
    expect(wrapper.find('.sv-audio-title').text()).toContain('播放器');
  });

  it('渲染 BGM / 音效双通道 tab', async () => {
    const { wrapper } = await mountPlayer();
    const tabs = wrapper.findAll('.sv-audio-tab');
    expect(tabs.length).toBe(2);
    expect(tabs[0].text()).toContain('BGM');
    expect(tabs[1].text()).toContain('音效');
  });

  it('默认激活 BGM 通道,点击音效后切换 active 态', async () => {
    const { wrapper } = await mountPlayer();
    const tabs = wrapper.findAll('.sv-audio-tab');
    expect(tabs[0].classes()).toContain('active');

    await tabs[1].trigger('click');
    const after = wrapper.findAll('.sv-audio-tab');
    expect(after[1].classes()).toContain('active');
    expect(after[0].classes()).not.toContain('active');
  });

  it('点击折叠按钮后通道控件收起(仅留标题栏)', async () => {
    const { wrapper } = await mountPlayer();
    expect(wrapper.find('.sv-audio-tabs').exists()).toBe(true);

    await wrapper.find('.sv-audio-mini').trigger('click');
    expect(wrapper.find('.sv-audio-tabs').exists()).toBe(false);
  });
});
