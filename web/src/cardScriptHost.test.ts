// cardScriptHost 测试:RPC 扩展(世界书读写白名单/聊天消息视图)、ESM 跳过、
// 长驻沙箱键控生命周期。世界书 API 与沙箱执行器均 mock,只验宿主编排逻辑。
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('./api/worldbooks', () => ({
  getCharacterWorldEntries: vi.fn(),
  saveCharacterWorldEntries: vi.fn(),
}));
vi.mock('./characterScriptSandbox', () => ({
  executeSandboxedCharacterScript: vi.fn(),
}));

import { getCharacterWorldEntries, saveCharacterWorldEntries } from './api/worldbooks';
import { executeSandboxedCharacterScript } from './characterScriptSandbox';
import {
  cleanupCardScriptSandbox,
  ensureCardScriptSandbox,
  isEsmScript,
  makeCardScriptRpcExtensions,
  type CardChatMessage,
} from './cardScriptHost';
import type { WorldBookEntry } from './api/types';

const mockedGet = getCharacterWorldEntries as unknown as ReturnType<typeof vi.fn>;
const mockedSave = saveCharacterWorldEntries as unknown as ReturnType<typeof vi.fn>;
const mockedExec = executeSandboxedCharacterScript as unknown as ReturnType<typeof vi.fn>;

function entry(id: number, enabled: boolean, extra: Partial<WorldBookEntry> = {}): WorldBookEntry {
  return {
    id,
    comment: `条目${id}`,
    keys: [`k${id}`],
    constant: false,
    enabled,
    content: `内容${id}`,
    ...extra,
  };
}

const CONTAINER = {} as HTMLElement;

beforeEach(() => {
  cleanupCardScriptSandbox();
  vi.clearAllMocks();
});

describe('makeCardScriptRpcExtensions(lorebook-entries)', () => {
  it('名称与主世界书不符返回 null 且不拉取(对齐资源帧 shim 语义)', async () => {
    const rpc = makeCardScriptRpcExtensions({
      characterId: 'c1',
      lorebookName: '主书',
      readMessages: () => [],
    });
    const res = await rpc('lorebook-entries', ['别的书']);
    expect(res).toEqual({ handled: true, value: null });
    expect(mockedGet).not.toHaveBeenCalled();
  });

  it('名称相符返回酒馆助手条目形态(uid/key 双写,uid 来自 kedai id)', async () => {
    mockedGet.mockResolvedValue([entry(99, true, { keys_secondary: ['s'], depth: 2 })]);
    const rpc = makeCardScriptRpcExtensions({
      characterId: 'c1',
      lorebookName: '主书',
      readMessages: () => [],
    });
    const res = await rpc('lorebook-entries', ['主书']);
    expect(res.handled).toBe(true);
    const list = res.value as Array<Record<string, unknown>>;
    expect(list).toHaveLength(1);
    expect(list[0].uid).toBe(99);
    expect(list[0].comment).toBe('条目99');
    expect(list[0].enabled).toBe(true);
    expect(list[0].key).toEqual(['k99']);
    expect(list[0].keys).toEqual(['k99']);
    expect(list[0].keysecondary).toEqual(['s']);
    expect(mockedGet).toHaveBeenCalledWith('c1');
  });

  it('无内嵌世界书(lorebookName=null)一律返回 null', async () => {
    const rpc = makeCardScriptRpcExtensions({
      characterId: 'c1',
      lorebookName: null,
      readMessages: () => [],
    });
    expect(await rpc('lorebook-entries', ['任意'])).toEqual({ handled: true, value: null });
    expect(mockedGet).not.toHaveBeenCalled();
  });
});

describe('makeCardScriptRpcExtensions(lorebook-set:uid+enabled 白名单)', () => {
  it('只应用 {uid, enabled},其余字段(content/keys)不进入回写', async () => {
    mockedGet.mockResolvedValue([entry(10, true), entry(11, true)]);
    mockedSave.mockImplementation(async (_id: string, entries: WorldBookEntry[]) => entries);
    const rpc = makeCardScriptRpcExtensions({
      characterId: 'c1',
      lorebookName: '主书',
      readMessages: () => [],
    });
    const res = await rpc('lorebook-set', [
      '主书',
      [
        { uid: 10, enabled: false, content: '篡改提示词', keys: ['hack'] },
        { uid: 11, enabled: true },
        { uid: 999, enabled: false }, // 不存在的 uid:忽略
        { uid: 'abc', enabled: false }, // 非法 uid:忽略
        { enabled: false }, // 缺 uid:忽略
      ],
    ]);
    expect(res.handled).toBe(true);
    expect((res.value as { updated: number }).updated).toBe(1);
    expect(mockedSave).toHaveBeenCalledTimes(1);
    const written = mockedSave.mock.calls[0][1] as WorldBookEntry[];
    const e10 = written.find((e) => e.id === 10)!;
    expect(e10.enabled).toBe(false);
    expect(e10.content).toBe('内容10'); // 白名单外字段保持原值
    expect(e10.keys).toEqual(['k10']);
  });

  it('无实际变化时不回写(省一次全量 PUT)', async () => {
    mockedGet.mockResolvedValue([entry(10, false)]);
    const rpc = makeCardScriptRpcExtensions({
      characterId: 'c1',
      lorebookName: '主书',
      readMessages: () => [],
    });
    const res = await rpc('lorebook-set', ['主书', [{ uid: 10, enabled: false }]]);
    expect((res.value as { updated: number }).updated).toBe(0);
    expect(mockedSave).not.toHaveBeenCalled();
  });

  it('名称不符返回 false 且不读写', async () => {
    const rpc = makeCardScriptRpcExtensions({
      characterId: 'c1',
      lorebookName: '主书',
      readMessages: () => [],
    });
    expect(await rpc('lorebook-set', ['别的书', [{ uid: 10, enabled: false }]])).toEqual({
      handled: true,
      value: false,
    });
    expect(mockedGet).not.toHaveBeenCalled();
    expect(mockedSave).not.toHaveBeenCalled();
  });
});

describe('makeCardScriptRpcExtensions(chat-messages:楼层序号/swipe 视图)', () => {
  const MSGS: CardChatMessage[] = [
    {
      role: 'assistant',
      content: '开场v1',
      extra: {
        swipes: [
          { swipe_id: 0, content: '开场v0', ts: 1 },
          { swipe_id: 1, content: '开场v1', ts: 2 },
        ],
        swipe_id: 1,
      },
    },
    { role: 'user', content: '你好', extra: {} },
  ];

  function makeRpc(): ReturnType<typeof makeCardScriptRpcExtensions> {
    return makeCardScriptRpcExtensions({ characterId: 'c1', lorebookName: null, readMessages: () => MSGS });
  }

  it('id>=0 按 0-based 楼层下标;swipes 降维为文本数组、swipe_id 透传', async () => {
    const res = await makeRpc()('chat-messages', [0, { include_swipe: true }]);
    const list = res.value as Array<Record<string, unknown>>;
    expect(list).toHaveLength(1);
    expect(list[0].content).toBe('开场v1');
    expect(list[0].swipes).toEqual(['开场v0', '开场v1']);
    expect(list[0].swipe_id).toBe(1);
  });

  it('负数从末尾数(-1=最后一条);无版本数据退化为 [content]/0', async () => {
    const res = await makeRpc()('chat-messages', [-1, {}]);
    const list = res.value as Array<Record<string, unknown>>;
    expect(list[0].role).toBe('user');
    expect(list[0].swipes).toEqual(['你好']);
    expect(list[0].swipe_id).toBe(0);
  });

  it('越界/非法 id 返回空数组', async () => {
    expect((await makeRpc()('chat-messages', [5, {}])).value).toEqual([]);
    expect((await makeRpc()('chat-messages', [-3, {}])).value).toEqual([]);
    expect((await makeRpc()('chat-messages', ['abc', {}])).value).toEqual([]);
  });

  it('未知 op 返回 handled=false(回退内置分发)', async () => {
    expect(await makeRpc()('audio-snapshot', [])).toEqual({ handled: false });
  });
});

describe('isEsmScript(沙箱只支持经典脚本,ESM 卡级脚本跳过)', () => {
  it('顶层 import/export 判定为 ESM', () => {
    expect(isEsmScript("import { registerMvuSchema } from 'https://x.js';")).toBe(true);
    expect(isEsmScript('export const Schema = z.object({});')).toBe(true);
    expect(isEsmScript('  import x from "y";')).toBe(true);
  });

  it('经典脚本(含字符串里的 import 字样)不误判', () => {
    expect(isEsmScript("const s = 'import 很多'; eventOn('x',()=>{});")).toBe(false);
    expect(isEsmScript('$(() => { onScriptLoad(); });')).toBe(false);
    expect(isEsmScript('')).toBe(false);
  });
});

describe('ensureCardScriptSandbox(键控生命周期)', () => {
  const SCRIPT_A = { id: 'a', name: 'A', content: 'eventOn("x",()=>{});', enabled: true };
  const SCRIPT_B = { id: 'b', name: 'B', content: 'eventOn("y",()=>{});', enabled: true };

  function opts(over: Partial<Parameters<typeof ensureCardScriptSandbox>[0]> = {}) {
    return {
      characterId: 'c1',
      scriptHash: 'h1',
      scripts: [SCRIPT_A],
      container: CONTAINER,
      lorebookName: '主书',
      readMessages: () => [],
      ...over,
    };
  }

  it('同键重复 ensure 是 no-op;键变化销毁旧沙箱重建', async () => {
    const cleanupA = vi.fn();
    const cleanupB = vi.fn();
    mockedExec.mockResolvedValueOnce(cleanupA).mockResolvedValueOnce(cleanupB);
    await ensureCardScriptSandbox(opts());
    await ensureCardScriptSandbox(opts());
    expect(mockedExec).toHaveBeenCalledTimes(1);
    expect(cleanupA).not.toHaveBeenCalled();
    await ensureCardScriptSandbox(opts({ scriptHash: 'h2' }));
    expect(mockedExec).toHaveBeenCalledTimes(2);
    expect(cleanupA).toHaveBeenCalledTimes(1);
  });

  it('cleanup 销毁沙箱且幂等(多脚本合并单 realm:一次执行一个 cleanup)', async () => {
    const cleanup = vi.fn();
    mockedExec.mockResolvedValueOnce(cleanup);
    await ensureCardScriptSandbox(opts({ scripts: [SCRIPT_A, SCRIPT_B] }));
    expect(mockedExec).toHaveBeenCalledTimes(1);
    cleanupCardScriptSandbox();
    expect(cleanup).toHaveBeenCalledTimes(1);
    cleanupCardScriptSandbox();
    expect(cleanup).toHaveBeenCalledTimes(1);
  });

  it('多脚本合并进单个 realm:一次调用,segments 按序携带全部段(名+正文)', async () => {
    mockedExec.mockResolvedValue(vi.fn());
    await ensureCardScriptSandbox(opts({ scripts: [SCRIPT_A, SCRIPT_B] }));
    expect(mockedExec).toHaveBeenCalledTimes(1);
    // 第一个参数从单段字符串变为段数组(沙箱侧逐 <script> 注入,共享 window)
    const segments = mockedExec.mock.calls[0][0] as Array<{ name: string; code: string }>;
    expect(segments).toEqual([
      { name: 'A', code: SCRIPT_A.content },
      { name: 'B', code: SCRIPT_B.content },
    ]);
  });

  it('启动前给容器设 data-kd-scope(运行期 <style> 作用域锚点),characterId 净化为合法 CSS 标识符', async () => {
    mockedExec.mockResolvedValue(vi.fn());
    const setAttribute = vi.fn();
    const container = { setAttribute } as unknown as HTMLElement;
    await ensureCardScriptSandbox(opts({ characterId: 'c/1:xx', container }));
    expect(setAttribute).toHaveBeenCalledWith('data-kd-scope', 'card-c-1-xx');
  });

  it('ESM 卡级脚本跳过执行(其余照常),空执行面不占用键', async () => {
    const esm = { id: 'z', name: 'Zod', content: "import { x } from 'https://y.js';", enabled: true };
    await ensureCardScriptSandbox(opts({ scripts: [esm] }));
    expect(mockedExec).not.toHaveBeenCalled();
    // 同键再 ensure 不带 ESM 之外的脚本仍是 no-op(没有泄漏的占位键)
    await ensureCardScriptSandbox(opts({ scripts: [SCRIPT_A] }));
    expect(mockedExec).toHaveBeenCalledTimes(1);
  });

  it('沙箱上下文携带 lorebookName 与 rpcExtensions(世界书 RPC 经扩展点)', async () => {
    mockedExec.mockResolvedValue(vi.fn());
    await ensureCardScriptSandbox(opts());
    const ctx = mockedExec.mock.calls[0][1] as {
      lorebookName?: string | null;
      rpcExtensions?: unknown;
      characterId?: string;
    };
    expect(ctx.lorebookName).toBe('主书');
    expect(ctx.characterId).toBe('c1');
    expect(typeof ctx.rpcExtensions).toBe('function');
  });
});

describe('ensureOverlayRoot / cleanupCardScriptSandbox(卡级覆层根)', () => {
  /** 最小 DOM 桩:querySelector 只认覆层根选择器,记录 append/remove */
  function makeContainer() {
    let root: Record<string, unknown> | null = null;
    const created: Array<Record<string, unknown>> = [];
    const container = {
      attrs: {} as Record<string, string>,
      setAttribute(name: string, value: string) {
        this.attrs[name] = value;
      },
      removeAttribute(name: string) {
        delete this.attrs[name];
      },
      querySelector(sel: string) {
        return sel === '[data-kd-overlay-root]' ? root : null;
      },
      appendChild(node: Record<string, unknown>) {
        root = node;
        created.push(node);
        return node;
      },
      ownerDocument: {
        createElement() {
          const el: Record<string, unknown> = {
            attrs: {} as Record<string, string>,
            style: { cssText: '' },
            setAttribute(name: string, value: string) {
              (this.attrs as Record<string, string>)[name] = value;
            },
            remove() {
              if (root === el) root = null;
            },
          };
          return el;
        },
      },
    };
    return { container, getRoot: () => root, created };
  }

  it('ensureInner 创建覆层根(data-kd-overlay-root + data-kd-scope),容器与根同名作用域', async () => {
    mockedExec.mockResolvedValue(vi.fn());
    const { container, getRoot } = makeContainer();
    await ensureCardScriptSandbox({
      characterId: 'c1',
      scriptHash: 'h1',
      scripts: [{ id: 'a', name: 'A', content: 'x();', enabled: true }],
      container: container as unknown as HTMLElement,
      lorebookName: null,
      readMessages: () => [],
    });
    const root = getRoot() as { attrs: Record<string, string>; style: { cssText: string } };
    expect(root).toBeTruthy();
    expect(root.attrs['data-kd-overlay-root']).toBe('');
    expect(root.attrs['data-kd-scope']).toBe('card-c1');
    expect(container.attrs['data-kd-scope']).toBe('card-c1');
    // 根自身不拦截指针,子节点由 dom-rpc 打注入标记时补 auto
    expect(root.style.cssText).toContain('pointer-events:none');
    expect(root.style.cssText).toContain('position:fixed');
  });

  it('cleanup 移除覆层根与容器 data-kd-scope(切卡/撤授权/卸载三路径共用)', async () => {
    mockedExec.mockResolvedValue(vi.fn());
    const { container, getRoot } = makeContainer();
    await ensureCardScriptSandbox({
      characterId: 'c1',
      scriptHash: 'h1',
      scripts: [{ id: 'a', name: 'A', content: 'x();', enabled: true }],
      container: container as unknown as HTMLElement,
      lorebookName: null,
      readMessages: () => [],
    });
    expect(getRoot()).toBeTruthy();
    cleanupCardScriptSandbox();
    expect(getRoot()).toBeNull();
    expect(container.attrs['data-kd-scope']).toBeUndefined();
  });

  it('同键 ensure 不重复创建覆层根(幂等)', async () => {
    mockedExec.mockResolvedValue(vi.fn());
    const { container, created } = makeContainer();
    const base = {
      characterId: 'c1',
      scriptHash: 'h1',
      scripts: [{ id: 'a', name: 'A', content: 'x();', enabled: true }],
      container: container as unknown as HTMLElement,
      lorebookName: null,
      readMessages: () => [],
    };
    await ensureCardScriptSandbox(base);
    await ensureCardScriptSandbox(base);
    expect(created).toHaveLength(1);
  });
});
