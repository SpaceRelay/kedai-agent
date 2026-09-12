import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

// node 环境无 localStorage,store 初始化即访问(appMode / 渲染记忆等),补内存桩
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

import { useAgentPromptEditor } from './useAgentPromptEditor';
import { useAppStore } from '../store';
import * as api from '../api';

// 只桩掉预览请求,其余 api 原样(store 门面引用众多,不能整体替身)
vi.spyOn(api, 'getPromptPreview').mockResolvedValue({ ok: true, note: '', layers: [] });

describe('useAgentPromptEditor', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    vi.mocked(api.getPromptPreview).mockClear();
  });

  it('loadPromptPreview 把当前 appMode 传给预览接口', async () => {
    const store = useAppStore();
    store.appMode = 'task';
    const ed = useAgentPromptEditor();
    await ed.loadPromptPreview();
    expect(api.getPromptPreview).toHaveBeenCalledWith(undefined, undefined, 'task');
  });

  it('appMode 变更后再次加载传新模式(预览随模式切换的数据面)', async () => {
    const store = useAppStore();
    store.appMode = 'roleplay';
    const ed = useAgentPromptEditor();
    await ed.loadPromptPreview();
    expect(api.getPromptPreview).toHaveBeenLastCalledWith(undefined, undefined, 'roleplay');
    store.appMode = 'task';
    await ed.loadPromptPreview();
    expect(api.getPromptPreview).toHaveBeenLastCalledWith(undefined, undefined, 'task');
  });
});
