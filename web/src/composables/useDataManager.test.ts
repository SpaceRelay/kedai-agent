import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useAppStore } from '../store';
import { useDataManager, useGenerationParams } from './useDataManager';

// useDataManager / useGenerationParams 装配测试(批次 6.1b undo_enabled):
// 装载(getSettings → store.undoEnabled)与保存(保存 patch 含 undo_enabled)两条链路。
// mock 范式参照 stores/task.test.ts(../api 整体 mock + hoisted 句柄)。

// node 环境无 localStorage(子 store 初始化即访问),补内存桩
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

/** mock 控制句柄 */
const h = vi.hoisted(() => ({
  /** getSettings 返回的设置体(装载链路) */
  settingsBody: {} as Record<string, unknown>,
  /** saveSettings 收到的 patch 序列(保存链路断言用) */
  savedPatches: [] as Array<Record<string, unknown>>,
  /** true 时 saveSettings 抛错(保存失败回滚场景) */
  saveFail: false,
}));

vi.mock('../api', async (importOriginal) => {
  const orig = await importOriginal<typeof import('../api')>();
  return {
    ...orig,
    getSettings: vi.fn(async () => h.settingsBody),
    saveSettings: vi.fn(async (patch: Record<string, unknown>) => {
      h.savedPatches.push(patch);
      if (h.saveFail) throw new Error('服务端错误,请查看日志');
      return { settings: h.settingsBody };
    }),
  };
});

beforeEach(() => {
  memStorage.clear();
  setActivePinia(createPinia());
  h.settingsBody = {};
  h.savedPatches = [];
  h.saveFail = false;
});

describe('undo_enabled 装配(批次 6.1b)', () => {
  it('装载:loadSettings 回填 undo_enabled;保存参数 patch 携带 undo_enabled', async () => {
    const store = useAppStore();
    // 缺省字段回退默认 true(后端默认开)
    await store.loadSettings();
    expect(store.undoEnabled).toBe(true);

    // 服务端返回 false 时回填 false
    h.settingsBody = { undo_enabled: false };
    await store.loadSettings();
    expect(store.undoEnabled).toBe(false);

    // 生成参数保存:patch 携带当前 undo_enabled
    const { saveParamsNow } = useGenerationParams();
    await saveParamsNow();
    const last = h.savedPatches.at(-1);
    expect(last).toBeDefined();
    expect(last!.undo_enabled, '保存 patch 应携带当前 undo_enabled').toBe(false);
  });

  it('数据管理开关:saveUndoEnabled 立即保存;失败回滚本地开关', async () => {
    const store = useAppStore();
    const { undoEnabled, undoMsg, saveUndoEnabled } = useDataManager();

    // 关闭开关 → 保存 patch 只含 undo_enabled: false
    // (mock 响应体同步携带 undo_enabled: false:saveSettings 会用响应回填 store)
    h.settingsBody = { undo_enabled: false };
    undoEnabled.value = false;
    await saveUndoEnabled();
    expect(h.savedPatches.at(-1)).toEqual({ undo_enabled: false });
    expect(undoMsg.value).toContain('已关闭');
    expect(store.undoEnabled, '保存成功不回滚').toBe(false);

    // 保存失败 → 本地开关回滚,提示失败
    h.saveFail = true;
    undoEnabled.value = true;
    await saveUndoEnabled();
    expect(undoEnabled.value, '保存失败应回滚本地开关').toBe(false);
    expect(undoMsg.value).toContain('保存失败');
  });
});
