// uma-creation.test.ts — 赛马娘卡(Pretty Derby)首楼界面三类交互的沙箱端到端回归。
//
// 实景:该卡 first_mes 只有占位符 <MN>,首楼 HTML 由 regex 脚本「开局美化」替换而来;
// 界面交互全部用内联事件属性(onchange/oninput/onclick)+ 脚本内 function 声明 +
// 裸 document.getElementById/navigator.clipboard/alert,不用 jQuery/TavernHelper。
// 修复前两处断点使三类交互全失效:
//   1) 消息级脚本被包进 async IIFE 成闭包局部,inline 事件在全局域用 new Function 求值
//      → submitCreation/updateDictDesc is not defined(ReferenceError,仅 warn);
//   2) 卡函数体内裸 document.getElementById 指向沙箱空文档 → null,读值/写 innerHTML 全落空。
// 本测试用卡内真实脚本正文在 vm 沙箱复刻:选身份 → 更新描述 → 提交 → 剪贴板 RPC → alert。
import { describe, expect, it } from 'vitest';
// @ts-expect-error -- node:vm 缺少类型声明
import vm from 'node:vm';
import { sandboxScript } from '../characterScriptSandbox';

/** 卡内 <script> 正文(取自角色卡「开局美化」regex 替换串,保持原始形态) */
const CARD_SCRIPT = `
const dict = {
  identity: { trainer1: "最王道的选择呢!" },
  uma: { empty: "懒狗作者没有给这个写介绍文本哦。" },
  start: { randomly: "谁知道会发生什么?" },
  world: { none: "普普通通的王道剧本也不错。" }
};
function updateDictDesc(cat, val) {
  const box = document.getElementById(cat + '-desc-box');
  const customContainer = document.getElementById(cat + '-custom-container');
  let customVal = "";
  if (customContainer) { customVal = document.getElementById(cat + '-custom-input').value.trim(); }
  if (val === 'custom') {
    customContainer.style.display = 'block';
    box.innerHTML = customVal !== "" ? customVal : "让我看看你的选择吧。";
  } else {
    if (customContainer) customContainer.style.display = 'none';
    if (dict[cat] && dict[cat][val]) box.innerHTML = dict[cat][val];
  }
}
function getSelectedOrCustomText(selectId, customInputId) {
  const selectElem = document.getElementById(selectId);
  if (!selectElem || selectElem.selectedIndex === -1 || selectElem.value === "") return "未知";
  if (selectElem.value === "custom" && customInputId) {
    return document.getElementById(customInputId).value || "未填写";
  }
  return selectElem.options[selectElem.selectedIndex].text;
}
function submitCreation() {
  const identity = getSelectedOrCustomText('identity-select', 'identity-custom-input');
  const uma = getSelectedOrCustomText('uma-select', 'uma-custom-input');
  const start = getSelectedOrCustomText('start-select', 'start-custom-input');
  const world = getSelectedOrCustomText('world-select', 'world-custom-input');
  const characterPrompt = '[身份]' + identity + '\\n[马娘]' + uma + '\\n[场景]' + start + '\\n[IF]' + world;
  if (navigator.clipboard) {
    navigator.clipboard.writeText(characterPrompt).then(() => {
      alert("设定已锁定,正在发放特雷森通行证!");
    }).catch(err => { alert("哎呀,启动失败了?"); });
  }
}
`;

interface BatchOp {
  method?: string;
  ref?: { kind?: string; value?: string };
  args?: unknown[];
}

interface Harness {
  alerts: string[];
  warnings: string[];
  errors: string[];
  /** 向沙箱投递一条 inline-event(模拟宿主 data-kd-on* 降级桥触发) */
  fireInline: (code: string, targetId: string, state: Record<string, unknown>) => void;
  /** 取已发出的 batch 操作(取前自动 ack 并让微任务链推进) */
  ops: () => BatchOp[];
  /** 宿主 ack 当前所有 batch + rpc,并让沙箱 Promise 链跑完 */
  settle: () => Promise<void>;
}

/**
 * 控件快照:与宿主 gatherControls 形状一致(只含 input/select/textarea/button,
 * 不含 div —— 见 dom-rpc.ts 的 querySelectorAll('input, select, textarea, button'))。
 * 这一点很关键:卡脚本里 document.getElementById('identity-desc-box') 这类 div 查询
 * 只能命中宿主经 gatherIdMap 下发的 id 轻量表(含 classes),拿不到 html/css;
 * 夹具若给 div 造出 html/css 字段,就会掩盖真实数据来源(此前夹具与生产不一致)。
 */
const CONTROLS = [
  { id: 'identity-select', tag: 'select', val: 'trainer1', selectedIndex: 1, options: [{ value: '', text: '— 请选择你的职称 —' }, { value: 'trainer1', text: '训练员' }] },
  { id: 'identity-custom-input', tag: 'input', val: '' },
  { id: 'uma-select', tag: 'select', val: 'empty', selectedIndex: 2, options: [{ value: '', text: '—' }, { value: 'custom', text: '自定义' }, { value: 'empty', text: '暂时留空' }] },
  { id: 'uma-custom-input', tag: 'input', val: '' },
  { id: 'start-select', tag: 'select', val: 'randomly', selectedIndex: 1, options: [{ value: '', text: '—' }, { value: 'randomly', text: '随机开局' }] },
  { id: 'start-custom-input', tag: 'input', val: '' },
  { id: 'world-select', tag: 'select', val: 'none', selectedIndex: 1, options: [{ value: '', text: '—' }, { value: 'none', text: '无' }] },
  { id: 'world-custom-input', tag: 'input', val: '' },
];

/** id 轻量表(宿主 gatherIdMap 形状:只含 id/classes/n),div 与容器类元素只在这里存在 */
const ID_MAP = {
  '#identity-select': { id: 'identity-select', classes: [], n: 1 },
  '#identity-custom-input': { id: 'identity-custom-input', classes: [], n: 1 },
  '#identity-custom-container': { id: 'identity-custom-container', classes: ['custom-container'], n: 1 },
  '#identity-desc-box': { id: 'identity-desc-box', classes: ['description-box'], n: 1 },
  '#uma-select': { id: 'uma-select', classes: [], n: 1 },
  '#uma-custom-input': { id: 'uma-custom-input', classes: [], n: 1 },
  '#uma-custom-container': { id: 'uma-custom-container', classes: ['custom-container'], n: 1 },
  '#uma-desc-box': { id: 'uma-desc-box', classes: ['description-box'], n: 1 },
  '#start-select': { id: 'start-select', classes: [], n: 1 },
  '#start-custom-input': { id: 'start-custom-input', classes: [], n: 1 },
  '#start-custom-container': { id: 'start-custom-container', classes: ['custom-container'], n: 1 },
  '#start-desc-box': { id: 'start-desc-box', classes: ['description-box'], n: 1 },
  '#world-select': { id: 'world-select', classes: [], n: 1 },
  '#world-custom-input': { id: 'world-custom-input', classes: [], n: 1 },
  '#world-custom-container': { id: 'world-custom-container', classes: ['custom-container'], n: 1 },
  '#world-desc-box': { id: 'world-desc-box', classes: ['description-box'], n: 1 },
};

function makeHarness(nonce: string): Harness {
  const messages: Array<Record<string, unknown>> = [];
  const listeners: Array<(ev: { data: unknown }) => void> = [];
  const alerts: string[] = [];
  const warnings: string[] = [];
  const errors: string[] = [];
  const sandboxWindow: Record<string, unknown> = {};
  const documentStub: Record<string, unknown> = {
    // 沙箱空文档:原生 getElementById 恒 null(修复前卡脚本拿到的就是这个)
    getElementById: () => null,
    createElement: () => ({ style: {}, setAttribute: () => {}, appendChild: () => {}, select: () => {} }),
    body: { appendChild: () => {}, removeChild: () => {} },
    head: { appendChild: () => {} },
    documentElement: {},
    execCommand: () => false,
  };
  const sandboxGlobal: Record<string, unknown> = {
    parent: { postMessage: (m: Record<string, unknown>) => void messages.push(m) },
    addEventListener: (_t: string, fn: (ev: { data: unknown }) => void) => void listeners.push(fn),
    console,
    structuredClone: (v: unknown) => JSON.parse(JSON.stringify(v)),
    queueMicrotask,
    Promise,
    setTimeout,
    clearTimeout,
    setInterval: () => 0,
    clearInterval: () => {},
    window: sandboxWindow,
    document: documentStub,
    navigator: {},
    alert: (m: string) => void alerts.push(String(m)),
  };
  vm.createContext(sandboxGlobal);

  const script = sandboxScript(
    CARD_SCRIPT,
    nonce,
    { stat_data: {}, display_data: {} },
    {},
    {},
    undefined,
    ID_MAP,
    null,
    {},
    CONTROLS,
  );
  new vm.Script(script).runInContext(sandboxGlobal);

  const ack = (): void => {
    for (const m of messages.filter((x) => x.type === 'batch')) {
      for (const fn of listeners) {
        fn({
          data: {
            channel: 'kedai-character-script-v1',
            nonce,
            type: 'rpc-result',
            id: m.id,
            ok: true,
            value: true,
          },
        });
      }
    }
    for (const m of messages.filter((x) => x.type === 'warn')) warnings.push(String(m.message));
    for (const m of messages.filter((x) => x.type === 'error')) errors.push(String(m.message));
  };
  // 待 ack 的 batch 应只 ack 一次:按 id 去重,避免重复解析同一批 rpc 造成计数噪声
  const acked = new Set<number>();
  const ackOnce = (): void => {
    // batch 与 rpc 共用 id 序号空间,统一 ack(rpc-result 同时 resolve 待决 Promise)
    for (const m of messages.filter((x) => x.type === 'batch' || x.type === 'rpc')) {
      const id = Number(m.id);
      if (!Number.isFinite(id) || acked.has(id)) continue;
      acked.add(id);
      for (const fn of listeners) {
        fn({
          data: { channel: 'kedai-character-script-v1', nonce, type: 'rpc-result', id, ok: true, value: true },
        });
      }
    }
    for (const m of messages.filter((x) => x.type === 'warn')) {
      const s = String(m.message);
      if (!warnings.includes(s)) warnings.push(s);
    }
    for (const m of messages.filter((x) => x.type === 'error')) {
      const s = String(m.message);
      if (!errors.includes(s)) errors.push(s);
    }
  };
  // 沙箱启动即有一批 boot 后续微任务;settle 反复推进直到稳定
  const settle = async (): Promise<void> => {
    for (let i = 0; i < 6; i++) {
      ackOnce();
      await new Promise((r) => setTimeout(r, 8));
    }
    ackOnce();
  };
  const bootP = settle();

  return {
    alerts,
    warnings,
    errors,
    ops: () => {
      const batch = messages
        .filter((m) => m.type === 'batch')
        .flatMap((m) => (m.ops ?? []) as BatchOp[]);
      // rpc 形态:send('rpc',{id,op,args}) → 归一为 {method,args} 便于统一断言
      const rpcOps = messages
        .filter((m) => m.type === 'rpc')
        .map((m) => ({ method: String(m.op ?? ''), args: (m.args ?? []) as unknown[] }));
      return [...batch, ...rpcOps];
    },
    settle: async () => {
      await bootP;
      await settle();
    },
    fireInline: (code, targetId, state) => {
      // 按宿主 inline-event 消息形状投递(this 门面拿到 target.state)
      const payload = {
        channel: 'kedai-character-script-v1',
        nonce,
        type: 'inline-event',
        code,
        event: { type: 'click' },
        target: { kind: 'target', id: Number(targetId), data: {}, state },
        mirror: { controls: CONTROLS },
      };
      for (const fn of listeners) fn({ data: payload });
    },
  };
}

describe('赛马娘卡首楼交互(内联事件 + 裸 document 查询)', () => {
  it('inline 事件能调用 IIFE 内声明的函数(闭包桥,不再 ReferenceError)', async () => {
    const h = makeHarness('uma-1');
    await h.settle();
    h.fireInline("updateDictDesc('identity','trainer1')", '1', { id: 'identity-select', val: 'trainer1' });
    await h.settle();
    const batchOps = h.ops();
    const htmlOp = batchOps.find((o) => o.method === 'html' && o.ref?.value === '#identity-desc-box');
    expect(htmlOp).toBeTruthy();
    expect(String(htmlOp?.args?.[0] ?? '')).toContain('最王道的选择呢!');
    const cssOp = batchOps.find(
      (o) => o.method === 'css' && o.ref?.value === '#identity-custom-container' && o.args?.[0] === 'display',
    );
    expect(cssOp?.args?.[1]).toBe('none');
    expect(h.errors).toEqual([]);
    expect(h.warnings.filter((w) => w.includes('not defined'))).toEqual([]);
  });

  it('submitCreation:读 select 真值拼提示词,经剪贴板桥写入并 alert', async () => {
    const h = makeHarness('uma-2');
    await h.settle();
    h.fireInline('submitCreation()', '99', { id: 'start-btn' });
    await h.settle();
    const batchOps = h.ops();
    const clip = batchOps.find((o) => o.method === 'clipboard-write');
    expect(clip, '剪贴板应经宿主 RPC 桥写入而非沙箱内静默失败').toBeTruthy();
    const text = String(clip?.args?.[0] ?? '');
    // 门面按 selectedIndex 读 options[i].text 得到真实选项文案(非 value 回退)
    expect(text).toContain('[身份]训练员');
    expect(text).toContain('[马娘]暂时留空');
    expect(text).toContain('[场景]随机开局');
    expect(text).toContain('[IF]无');
    // 剪贴板 RPC 回包后 alert 弹窗被调用(弹窗事件链路闭环)
    expect(h.alerts.join('|')).toContain('设定已锁定');
    expect(h.errors).toEqual([]);
  });

  it('回归:未命中镜像的 id 走原生空值分支,沙箱不整体死亡', async () => {
    // 未命中镜像的 id 走原生(空文档 null);卡函数无守卫时会抛,但只应 warn,不得 error 上送
    const h = makeHarness('uma-3');
    await h.settle();
    h.fireInline("updateDictDesc('nope','x')", '2', { id: 'nope' });
    await h.settle();
    expect(h.errors).toEqual([]);
  });

  it('div 门面数据源是 gatherIdMap(只带 id/classes):可读 class,可写并乐观回读', async () => {
    // 真实宿主 gatherControls 只采集 input/select/textarea/button,div 不在其中;
    // div 门面完全靠 gatherIdMap 的 id 轻量表提供。此用例锁住该真实来源,
    // 避免夹具再次给 div 造出 html/css 字段而掩盖问题。
    const h = makeHarness('uma-4');
    await h.settle();
    h.fireInline(
      "var b=document.getElementById('identity-desc-box');" +
        "alert('cls='+b.className+'|initHtml=['+b.innerHTML+']');" +
        "b.innerHTML='写入值';" +
        "alert('after='+b.innerHTML);",
      '3',
      { id: 'identity-desc-box' },
    );
    await h.settle();
    const out = h.alerts.join('\n');
    expect(out).toContain('cls=description-box'); // classes 来自 idMap
    expect(out).toContain('initHtml=[]'); // idMap 无 html 字段 → 初始读为空
    expect(out).toContain('after=写入值'); // 写后同一同步段乐观回读
    // 写入经 DOM 白名单 batch 回写宿主真实元素
    const htmlOp = h.ops().find((o) => o.method === 'html' && o.ref?.value === '#identity-desc-box');
    expect(htmlOp?.args?.[0]).toBe('写入值');
    expect(h.errors).toEqual([]);
  });
});
