// 弹窗注册表元测试(批次 5.2):MODAL_FLAGS 单点定义的三方一致性。
//
// 单点定义的价值在于「漏改必红」,故本文件把三边都拉来做契约校验:
//   ① web/src/modals.ts 的 MODALS 注册表(真源);
//   ② uiPrefs store 暴露的弹窗开关(逐个 ref 声明:源码文本解析 + 运行期键检查,
//      解析契约是「弹窗开关均以 `const xOpen = ref(false)` 声明」);
//   ③ App.vue 的渲染覆盖(必须由注册表 v-for 派生,且不得残留按 flag 硬编码的
//      v-if="store.<flag>")。
// 任何一边漏改(新增/重命名/删除弹窗只改了一两处),本文件必有断言失败。
// 另:tools/check-arch.mjs 的「组件直改 state」弹窗白名单同样由 modals.ts 派生
// (见该脚本 readModalFlags),此处不重复校验。

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { MODALS, MODAL_FLAGS } from './modals';
import { useUiPrefsStore } from './stores/uiPrefs';
// 读源码文本做契约校验(仓库既有先例:mvu/parser.contract.test.ts);?raw 由 vite 提供
import appSource from './App.vue?raw';
import uiPrefsSource from './stores/uiPrefs.ts?raw';

// node 环境无 localStorage(uiPrefs 初始化即访问),补内存桩(同 uiPrefs.test.ts)
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

beforeEach(() => {
  memStorage.clear();
  setActivePinia(createPinia());
});

/**
 * uiPrefs 里「非弹窗」的 Open 后缀 ref(false) 白名单(抽屉/面板等)。
 * uiPrefs 出现新的此类开关时必须显式归类:要么加进本白名单,要么进 modals.ts 注册表——
 * 不允许含糊地两不管(否则下一条集合相等断言会失败,提示需要归类)。
 */
const NON_MODAL_OPEN_REFS = ['agentPanelOpen', 'sidebarOpen', 'audioOpen'];

/** 从 uiPrefs.ts 源码解析弹窗开关 ref 名。 */
function parseUiPrefsModalFlags(): string[] {
  const all = [...uiPrefsSource.matchAll(/const\s+([A-Za-z_$][\w$]*Open)\s*=\s*ref\(false\)/g)]
    .map((m) => m[1]);
  return all.filter((f) => !NON_MODAL_OPEN_REFS.includes(f));
}

describe('弹窗注册表自身一致性', () => {
  it('flag 无重复', () => {
    const flags = MODALS.map((m) => m.flag);
    expect(new Set(flags).size).toBe(flags.length);
  });

  it('label 非空且无首尾空白(懒加载失败提示条依赖)', () => {
    for (const m of MODALS) {
      expect(m.label.trim().length, `${m.flag} 的 label 不能为空`).toBeGreaterThan(0);
      expect(m.label, `${m.flag} 的 label 不应带首尾空白`).toBe(m.label.trim());
    }
  });

  it('每条的组件都是 defineAsyncComponent 产物(真·懒加载,而非直接 import)', () => {
    for (const m of MODALS) {
      // lazyModal 内部走 defineAsyncComponent:产物带 __asyncLoader,可据此区分
      // 「声明成懒加载」与「被误改成静态 import」——比 toBeTruthy 更紧,后者恒真。
      // Vue 未在公开类型里导出该内部字段,故经 in 收窄而非断言。
      const comp: object = m.component;
      expect('__asyncLoader' in comp, `${m.flag} 的组件不是异步组件(懒加载失效)`).toBe(true);
    }
  });

  it('MODAL_FLAGS 与 MODALS 声明顺序逐一对应', () => {
    expect([...MODAL_FLAGS]).toEqual(MODALS.map((m) => m.flag));
  });
});

describe('registry ↔ uiPrefs 双向一致', () => {
  it('uiPrefs 源码里的弹窗开关集合与注册表完全相同(两个方向都会红)', () => {
    expect([...parseUiPrefsModalFlags()].sort()).toEqual([...MODAL_FLAGS].sort());
  });

  it('uiPrefs store 运行期暴露每个 flag 且初值为布尔 false(声明与 return 都齐)', () => {
    const uiPrefs = useUiPrefsStore();
    for (const flag of MODAL_FLAGS) {
      expect(uiPrefs[flag as keyof typeof uiPrefs], `uiPrefs 未暴露 ${flag}`).toBe(false);
    }
  });
});

describe('App.vue 渲染与单点表一致', () => {
  it('弹窗渲染由 MODALS v-for 派生(过渡与开关判断同源)', () => {
    expect(appSource).toContain("from './modals'");
    expect(appSource).toContain('v-for="modal in MODALS"');
    expect(appSource).toContain(':is="modal.component"');
    expect(appSource).toContain('v-if="isModalOpen(modal.flag)"');
  });

  it('Android 返回键顺序取 MODAL_FLAGS,不再手抄一份 flag 数组', () => {
    expect(appSource).toContain('MODAL_FLAGS');
    expect(appSource).not.toContain('const MODAL_FLAGS');
  });

  it('不残留按 flag 硬编码的弹窗 v-if="store.<flag>"', () => {
    for (const flag of MODAL_FLAGS) {
      expect(appSource, `${flag} 仍被硬编码渲染`).not.toContain(`v-if="store.${flag}"`);
    }
  });
});
