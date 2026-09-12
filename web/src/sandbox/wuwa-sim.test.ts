// wuwa-sim.test.ts — WuWa 悬浮窗脚本形态端到端回归(沙箱 jq 子集 → RPC 序列)。
// 复刻三个悬浮窗脚本的关键调用序列,锁定「不再 TypeError、RPC 序列完整」:
//   th-5 created 悬浮球(attr id + css 对象 + html + draggable)、
//   th-3/th-4 html 字符串 append + 按 id draggable + $(window).on(unload)、
//   css 对象形式/字符串 getter/is(':hidden')/toggle/animate 并存。
// 真实浏览器布局与拖拽手感仍需实卡复验,此处只保证脚本调用链不中断、参数不丢。
import { describe, expect, it } from 'vitest';
// @ts-expect-error -- node:vm 缺少类型声明
import vm from 'node:vm';
import { sandboxScript } from '../characterScriptSandbox';

interface Harness {
  messages: Array<Record<string, unknown>>;
  run: (code: string) => Promise<Array<{ method?: string; ref?: { kind?: string; value?: string }; args?: unknown[] }>>;
}

function makeHarness(nonce: string): Harness {
  const messages: Array<Record<string, unknown>> = [];
  const listeners: Array<(ev: { data: unknown }) => void> = [];
  const sandboxWindow: Record<string, unknown> = {};
  const sandboxGlobal = {
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
    document: { head: { appendChild: () => {} } },
  };
  vm.createContext(sandboxGlobal);
  const run = async (
    code: string,
  ): Promise<Array<{ method?: string; ref?: { kind?: string; value?: string }; args?: unknown[] }>> => {
    const script = sandboxScript(code, nonce, { stat_data: {}, display_data: {} });
    new vm.Script(script).runInContext(sandboxGlobal);
    await new Promise((r) => setTimeout(r, 15));
    // 宿主 ack 每个 batch(镜像为空即可,本例只验证 op 序列)
    for (const m of messages.filter((x) => x.type === 'batch')) {
      for (const fn of listeners) {
        fn({
          data: { channel: 'kedai-character-script-v1', nonce, type: 'rpc-result', id: m.id, ok: true, value: true },
        });
      }
    }
    await new Promise((r) => setTimeout(r, 15));
    return messages
      .filter((m) => m.type === 'batch')
      .flatMap((m) => (m.ops ?? []) as Array<{ method?: string; ref?: { kind?: string; value?: string }; args?: unknown[] }>);
  };
  return { messages, run };
}

describe('WuWa 悬浮窗脚本形态端到端模拟', () => {
  it('th-5 形态:created 悬浮球(attr id + css 对象 + html + draggable)全程无异常且 op 完整', async () => {
    const h = makeHarness('sim-th5');
    const code = [
      "const $ball = $('<div>').attr('id','fx-floating-ball').html('<i class=\"x\"></i>').css({position:'fixed',zIndex:9999,width:'52px',height:'52px'});",
      "$ball.draggable({ cursor:'move', distance:3, start:function(e,ui){}, drag:function(e,ui){}, stop:function(e,ui){} });",
      "$('body').append($ball);",
      "$('head').append('<style id=\"fx-style\">#fx-floating-ball{position:fixed}</style>');",
    ].join('\n');
    const ops = await h.run(code);
    // 不应出现 error 上送
    expect(h.messages.filter((m) => m.type === 'error')).toHaveLength(0);
    // created 落 DOM:appendCreated 带 spec(含 attrs.id / css / draggable)
    const appendCreated = ops.find((o) => o.method === 'appendCreated');
    expect(appendCreated).toBeTruthy();
    const spec = appendCreated?.args?.[0] as {
      attrs?: Record<string, string>;
      css?: Record<string, string>;
      draggable?: { cursor?: string; distance?: number };
    };
    expect(spec.attrs?.id).toBe('fx-floating-ball');
    expect(spec.css?.position).toBe('fixed');
    expect(spec.css?.zIndex).toBe('9999');
    expect(spec.draggable?.cursor).toBe('move');
    expect(spec.draggable?.distance).toBe(3);
    // $('head') 样式注入:append op 的 ref 值必须是 head(此前 0 匹配静默丢失)
    const headAppend = ops.find(
      (o) => o.method === 'append' && o.ref?.kind === 'selector' && o.ref?.value === 'head',
    );
    expect(headAppend).toBeTruthy();
    expect(String(headAppend?.args?.[0] ?? '')).toContain('fx-floating-ball');
  });

  it('th-3/th-4 形态:html 字符串 append 后按 id 选择器 draggable + $(window).on(unload)', async () => {
    const h = makeHarness('sim-th3');
    const code = [
      "$('body').append('<div id=\"wuwa-story-ui\" style=\"position:fixed\">x</div>');",
      "$('#wuwa-story-ui').draggable({ handle:'.head', containment:'window', start:function(e,ui){}, stop:function(e,ui){} });",
      "$(window).on('unload', function(){});",
      "$(window).on('pagehide', function(){});",
    ].join('\n');
    const ops = await h.run(code);
    expect(h.messages.filter((m) => m.type === 'error')).toHaveLength(0);
    const drag = ops.find((o) => o.method === 'draggable' && o.ref?.value === '#wuwa-story-ui');
    expect(drag).toBeTruthy();
    expect((drag?.args?.[0] as { containment?: string })?.containment).toBe('window');
    // $(window) 事件:ref.kind 为 window(此前 ref=null 静默失效)
    const winOn = ops.filter((o) => o.method === 'on' && o.ref?.kind === 'window');
    expect(winOn.length).toBe(2);
  });

  it('css 对象形式与字符串 getter 并存,is(:hidden)/toggle 不抛错', async () => {
    const h = makeHarness('sim-css');
    const code = [
      "$('#p').css({display:'none',position:'fixed'});",
      "const d = $('#p').css('display');",
      "$('#p').toggle();",
      "if($('#p').is(':hidden')){}",
      "$('#p').animate({scrollTop:100},200);",
    ].join('\n');
    await h.run(code);
    expect(h.messages.filter((m) => m.type === 'error')).toHaveLength(0);
    const cssOps = h.messages
      .filter((m) => m.type === 'batch')
      .flatMap((m) => (m.ops ?? []) as Array<{ method?: string; args?: unknown[] }>)
      .filter((o) => o.method === 'css');
    // 对象形式逐键入队:display 与 position 各一条
    expect(cssOps.some((o) => o.args?.[0] === 'display')).toBe(true);
    expect(cssOps.some((o) => o.args?.[0] === 'position')).toBe(true);
  });
});
