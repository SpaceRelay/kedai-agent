// 沙箱脚本生成的回归测试。
// 背景:sandboxScript 用模板字面量拼接内联脚本,模板字面量会消化 `\d` / `\s` / `\/`
// 这类反斜杠序列(JS 对未知转义只保留后一个字符),导致生成的正则字面量变成
// `/^d+$/` 与 `/...[sS]*?</UpdateVariable>/gi` —— 后者提前闭合正则,整段脚本
// 抛 SyntaxError: Invalid regular expression flags,任何角色卡脚本都无法执行,
// 状态栏永远停在卡片自带的「加载中」静态文案。故此处锁定「生成物必须可解析」。
import { describe, expect, it } from 'vitest';
// @types/node 未安装(tsconfig types 仅 vite/client),node:vm 无类型声明;
// 运行时 vitest 走 node 环境可正常解析。此处压类型错误而非装依赖;
// 若日后安装 @types/node,本行会因「未使用的 expect-error」报错提醒删除。
// @ts-expect-error -- node:vm 缺少类型声明(见上)
import vm from 'node:vm';
import { sandboxAttributes, sandboxScript } from './characterScriptSandbox';

describe('sandboxScript', () => {
  const script = sandboxScript('const x = 1;', 'nonce-1', {
    stat_data: {},
    display_data: {},
  });

  it('生成的脚本体必须是合法 JavaScript', () => {
    expect(() => new vm.Script(script)).not.toThrow();
  });

  it('正则字面量的反斜杠必须保留到生成物中', () => {
    // 数字索引判定:模板转义丢失会退化成 /^d+$/
    expect(script).toContain(String.raw`/^\d+$/`);
    // UpdateVariable 剥离:`\/` 丢失会提前闭合正则
    expect(script).toContain(String.raw`[\s\S]*?<\/UpdateVariable>`);
    expect(script).not.toContain('[sS]*?');
  });

  it('注入的用户代码与变量可用', () => {
    const withVars = sandboxScript('populate();', 'nonce-2', {
      stat_data: { 世界: { 时间: ['14:30', '初始'] } },
      display_data: {},
    });
    expect(() => new vm.Script(withVars)).not.toThrow();
    expect(withVars).toContain('populate();');
    expect(withVars).toContain('14:30');
  });

  it('</script> 与 < 被转义,不破坏注入', () => {
    const escaped = sandboxScript('const s = "</script>";', 'nonce-3', {
      stat_data: { a: ['<b>', ''] },
      display_data: {},
    });
    expect(escaped).toContain(String.raw`<\/script`);
    expect(escaped).toContain('\\u003c');
    expect(() => new vm.Script(escaped)).not.toThrow();
  });

  it('jq 子集提供开场界面脚本所需方法(prop/trigger/append/focus/slideDown/slideUp/is/outerHeight/find/attr/remove/closest/empty)', () => {
    // WuWa 开场脚本依赖这些方法驱动协议弹窗、表单交互与悬浮球;
    // 缺失时脚本首行调用即抛 TypeError,协议层/悬浮球永不显示。
    const out = sandboxScript('x();', 'nonce-jq', { stat_data: {}, display_data: {} });
    for (const method of [
      "coll.prop=", "coll.trigger=", "coll.append=", "coll.focus=",
      "coll.slideDown=", "coll.slideUp=", "coll.is=", "coll.outerHeight=",
      "coll.find=", "coll.attr=", "coll.remove=", "coll.closest=", "coll.empty=", "coll.off=",
    ]) {
      expect(out).toContain(method);
    }
    expect(() => new vm.Script(out)).not.toThrow();
  });

  it('沙箱 localStorage 持久化到宿主、sessionStorage 保持会话级且两者独立(协议状态互不污染)', () => {
    const out = sandboxScript('x();', 'nonce-ls', { stat_data: {}, display_data: {} }, {}, { kk: 'vv' });
    expect(out).toContain('__kdMemoryStorage');
    expect(out).toContain('getItem: function');
    expect(out).not.toContain('localStorage=undefined');
    // localStorage 带宿主持久化桥:boot 注入已存快照,写操作同步 local-storage 消息
    expect(out).toContain('__kdSavedLocal=JSON.parse("{\\"kk\\":\\"vv\\"}")');
    expect(out).toContain('__kdMakeStorage(__kdSavedLocal,true)');
    // 回归:此前 localStorage/sessionStorage 共用同一内存对象
    expect(out).toContain('__kdSessionStorage=__kdMakeStorage(null,false)');
    expect(out).toContain('sessionStorage=__kdSessionStorage');
    expect(out).not.toContain('sessionStorage=__kdMemoryStorage');
    expect(() => new vm.Script(out)).not.toThrow();
  });

  it('沙箱 iframe 允许模态(allow-modals:作者脚本的 alert/confirm 不被浏览器拒)', () => {
    expect(sandboxAttributes().sandbox).toContain('allow-modals');
  });

  it('getter 从宿主状态镜像读真值(开场协议勾选/确认按钮场景)', async () => {
    // 协议脚本模式:勾选复选框 → 点确认 → 回调里读 $('#agree').prop('checked') 与按钮文本。
    // 早期 getter 全是假值桩(永远 false/''),协议确认永远读不到勾选 → 开场流程卡死。
    const user = [
      "$('#btn').on('click', function(){",
      "  const agreed=$('#agree').prop('checked');",
      "  const label=$(this).text();",
      "  warn('agreed='+agreed+';label='+label);",
      "});",
    ].join('\n');
    const nonce = 'nonce-mirror';
    const script = sandboxScript(user, nonce, { stat_data: {}, display_data: {} });
    const messages: Array<Record<string, unknown>> = [];
    const listeners: Array<(ev: { data: unknown }) => void> = [];
    const sandboxGlobal = {
      parent: { postMessage: (m: Record<string, unknown>) => void messages.push(m) },
      addEventListener: (_type: string, fn: (ev: { data: unknown }) => void) => void listeners.push(fn),
      console,
      structuredClone: (v: unknown) => JSON.parse(JSON.stringify(v)),
      queueMicrotask,
      Promise,
      // 共享桥注入/diff 读写 window(真实浏览器沙箱必有),harness 补齐对齐
      window: {},
    };
    vm.createContext(sandboxGlobal);
    new vm.Script(script).runInContext(sandboxGlobal);
    const dispatch = (m: Record<string, unknown>): void => {
      for (const fn of listeners) fn({ data: m });
    };
    // 主流程:on() 绑定入队 → flush 发出 batch → 宿主 ack(无镜像)
    await new Promise((r) => setTimeout(r, 10));
    for (const m of messages.filter((x) => x.type === 'batch')) {
      dispatch({ channel: 'kedai-character-script-v1', nonce, type: 'rpc-result', id: m.id, ok: true, value: true });
    }
    await new Promise((r) => setTimeout(r, 10));
    // 宿主派发点击:回包携带事件目标 state(按钮文本)与表单控件镜像(#agree 已勾选)
    dispatch({
      channel: 'kedai-character-script-v1',
      nonce,
      type: 'jq-event',
      jqId: 1,
      event: { type: 'click' },
      target: { kind: 'target', id: 1, data: {}, state: { text: '确认', val: '', checked: false, disabled: false, classes: [] } },
      mirror: { controls: [{ id: 'agree', tag: 'input', val: '', checked: true, disabled: false }] },
    });
    await new Promise((r) => setTimeout(r, 10));
    const warn = messages.find(
      (m) => m.type === 'warn' && typeof m.message === 'string' && m.message.includes('agreed='),
    );
    expect(warn?.message).toContain('agreed=true');
    expect(warn?.message).toContain('label=确认');
  });

  it('未命中镜像的选择器 getter 会 enqueue probe(下一批回包后可读到真值)', async () => {
    const user = "$('#later').prop('checked');"; // getter miss → probe 入队
    const nonce = 'nonce-probe';
    const script = sandboxScript(user, nonce, { stat_data: {}, display_data: {} });
    const messages: Array<Record<string, unknown>> = [];
    const listeners: Array<(ev: { data: unknown }) => void> = [];
    const sandboxGlobal = {
      parent: { postMessage: (m: Record<string, unknown>) => void messages.push(m) },
      addEventListener: (_type: string, fn: (ev: { data: unknown }) => void) => void listeners.push(fn),
      console,
      structuredClone: (v: unknown) => JSON.parse(JSON.stringify(v)),
      queueMicrotask,
      Promise,
      // 共享桥注入/diff 读写 window(真实浏览器沙箱必有),harness 补齐对齐
      window: {},
    };
    vm.createContext(sandboxGlobal);
    new vm.Script(script).runInContext(sandboxGlobal);
    await new Promise((r) => setTimeout(r, 10));
    const batch = messages.find((m) => m.type === 'batch');
    const ops = (batch?.ops ?? []) as Array<{ method?: string; ref?: { value?: string } }>;
    expect(ops.some((op) => op.method === 'probe' && op.ref?.value === '#later')).toBe(true);
  });

  it('沙箱提供 toastr stub(作者脚本 window.parent.toastr 桥接失败时仍可用)', () => {
    const out = sandboxScript('toastr.info("hi");', 'nonce-tr', { stat_data: {}, display_data: {} });
    expect(out).toContain('toastr');
    expect(out).toContain('info:');
    expect(() => new vm.Script(out)).not.toThrow();
  });
});

describe('sandboxScript(wuwa 状态栏兼容面)', () => {
  // wuwa MVU 浪潮状态栏(161KB)实测依赖链:init 里 $('.nav-btn').first()、
  // toggleClass、$('<div id="opt-list">') 创建元素、eventOn(Mvu.events…)、
  // getVariables({type:'global'}) 设置持久化。任一缺失 → init 抛 TypeError →
  // 宿主 finish(error) 拆除沙箱,用户看到「状态栏加载失败」。
  const out = sandboxScript('x();', 'nonce-wuwa', { stat_data: {}, display_data: {} }, { statusBarSettings: { isDarkMode: true } });

  it('jq 子集补齐 toggleClass/first/last/eq/click/each/length/数字索引', () => {
    for (const marker of [
      'coll.toggleClass=', 'coll.first=', 'coll.last=', 'coll.eq=',
      'coll.click=', 'coll.each=',
      "Object.defineProperty(coll,'length'",
    ]) {
      expect(out).toContain(marker);
    }
    expect(() => new vm.Script(out)).not.toThrow();
  });

  it('$("<div>") 创建判定正则的反斜杠必须保留(模板字面量会消化 \s)', () => {
    // 退化为 /^s</ 会把以 s 开头的选择器误判为创建语法
    expect(out).toContain(String.raw`/^\s*</`);
  });

  it('window 挂载 eventOn/Mvu.events/变量 API(状态栏检查 window.eventOn && window.Mvu 才订阅更新)', () => {
    for (const marker of [
      'globalThis.eventOn=eventOn', 'globalThis.Mvu=Mvu',
      'globalThis.getVariables=getVariables', 'globalThis.replaceVariables=replaceVariables',
      "VARIABLE_UPDATE_ENDED:'mag_variable_updated'",
    ]) {
      expect(out).toContain(marker);
    }
    expect(() => new vm.Script(out)).not.toThrow();
  });

  it('宿主 mvu-event 广播在沙箱内合并变量并触发本地订阅', async () => {
    const user = [
      "let seen=null;",
      "eventOn(Mvu.events.VARIABLE_UPDATE_ENDED,(vars)=>{seen=vars&&vars.stat_data?vars.stat_data:null;});",
      "setTimeout(()=>{warn('seen='+JSON.stringify(seen));},30);",
    ].join('\n');
    const nonce = 'nonce-mvu';
    const script = sandboxScript(user, nonce, { stat_data: { 旧: 1 }, display_data: {} }, {});
    const messages: Array<Record<string, unknown>> = [];
    const listeners: Array<(ev: { data: unknown }) => void> = [];
    const sandboxGlobal = {
      parent: { postMessage: (m: Record<string, unknown>) => void messages.push(m) },
      addEventListener: (_type: string, fn: (ev: { data: unknown }) => void) => void listeners.push(fn),
      console,
      structuredClone: (v: unknown) => JSON.parse(JSON.stringify(v)),
      queueMicrotask,
      Promise,
      setTimeout,
      // 共享桥注入/diff 读写 window(真实浏览器沙箱必有),harness 补齐对齐
      window: {},
    };
    vm.createContext(sandboxGlobal);
    new vm.Script(script).runInContext(sandboxGlobal);
    // 宿主广播变量更新
    for (const fn of listeners) {
      fn({
        data: {
          channel: 'kedai-character-script-v1',
          nonce,
          type: 'mvu-event',
          event: 'mag_variable_updated',
          payload: { stat_data: { 新值: 42 }, display_data: {} },
          variables: { stat_data: { 新值: 42 }, display_data: {} },
        },
      });
    }
    await new Promise((r) => setTimeout(r, 60));
    const warn = messages.find(
      (m) => m.type === 'warn' && typeof m.message === 'string' && m.message.includes('seen='),
    );
    expect(warn?.message).toContain('新值');
    expect(warn?.message).toContain('42');
  });

  it('getVariables({type:"global"}) 读启动下发的全局快照;replaceVariables 触发 global-save RPC', async () => {
    const user = [
      "const g=getVariables({type:'global'});",
      "warn('dark='+JSON.stringify(g.statusBarSettings&&g.statusBarSettings.isDarkMode));",
      "replaceVariables({statusBarSettings:{isDarkMode:false}},{type:'global'}).then(()=>{",
      "  warn('after='+JSON.stringify(getVariables({type:'global'}).statusBarSettings.isDarkMode));",
      "});",
    ].join('\n');
    const nonce = 'nonce-globals';
    const script = sandboxScript(user, nonce, { stat_data: {}, display_data: {} }, { statusBarSettings: { isDarkMode: true } });
    const messages: Array<Record<string, unknown>> = [];
    const listeners: Array<(ev: { data: unknown }) => void> = [];
    const sandboxGlobal = {
      parent: { postMessage: (m: Record<string, unknown>) => void messages.push(m) },
      addEventListener: (_type: string, fn: (ev: { data: unknown }) => void) => void listeners.push(fn),
      console,
      structuredClone: (v: unknown) => JSON.parse(JSON.stringify(v)),
      queueMicrotask,
      Promise,
      setTimeout,
      // 共享桥注入/diff 读写 window(真实浏览器沙箱必有),harness 补齐对齐
      window: {},
    };
    vm.createContext(sandboxGlobal);
    new vm.Script(script).runInContext(sandboxGlobal);
    await new Promise((r) => setTimeout(r, 20));
    // 应答 global-save rpc
    for (const m of messages.filter((x) => x.type === 'rpc')) {
      for (const fn of listeners) {
        fn({ data: { channel: 'kedai-character-script-v1', nonce, type: 'rpc-result', id: m.id, ok: true, value: true } });
      }
    }
    await new Promise((r) => setTimeout(r, 20));
    const warns = messages.filter((m) => m.type === 'warn').map((m) => String(m.message));
    expect(warns.some((w) => w.includes('dark=true'))).toBe(true);
    expect(warns.some((w) => w.includes('after=false'))).toBe(true);
    expect(messages.some((m) => m.type === 'rpc' && m.op === 'global-save')).toBe(true);
  });

  it('事件回调异常仅告警,不再拆沙箱(此前 send(error) 会触发宿主拆除)', () => {
    // jq-event 分支的 catch 须 send warn 而非 error
    const jqBranch = out.slice(out.indexOf("m.type==='jq-event'"), out.indexOf("m.type==='mvu-event'"));
    expect(jqBranch).toContain("send('warn'");
    expect(jqBranch).not.toContain("send('error'");
  });
});

describe('sandboxScript(inline 事件降级桥沙箱半)', () => {
  it('inline-event 消息用 new Function 求值作者代码,this 为状态门面(classList/value/dataset)', async () => {
    const nonce = 'nonce-inline';
    const script = sandboxScript('', nonce, { stat_data: {}, display_data: {} });
    const messages: Array<Record<string, unknown>> = [];
    const listeners: Array<(ev: { data: unknown }) => void> = [];
    const sandboxGlobal = {
      parent: { postMessage: (m: Record<string, unknown>) => void messages.push(m) },
      addEventListener: (_type: string, fn: (ev: { data: unknown }) => void) => void listeners.push(fn),
      console,
      structuredClone: (v: unknown) => JSON.parse(JSON.stringify(v)),
      queueMicrotask,
      Promise,
      setTimeout,
      // 共享桥注入/diff 读写 window(真实浏览器沙箱必有),harness 补齐对齐
      window: {},
    };
    vm.createContext(sandboxGlobal);
    // new Function 处理器的全局必须是 vm Realm 自身(与浏览器沙箱一致:只能看到
    // globalThis 上挂载的全局,看不到 boot 脚本的 const 闭包变量)
    (sandboxGlobal as Record<string, unknown>).Function = vm.runInContext('Function', sandboxGlobal);
    new vm.Script(script).runInContext(sandboxGlobal);
    for (const fn of listeners) {
      fn({
        data: {
          channel: 'kedai-character-script-v1',
          nonce,
          type: 'inline-event',
          code: "globalThis.__kdInline=this.classList.contains('ready')+'|'+this.value+'|'+this.dataset.page+'|'+event.type;event.preventDefault();",
          event: { type: 'click' },
          target: {
            kind: 'target',
            id: 1,
            data: { page: 'opt' },
            state: { classes: ['ready'], val: 'hello' },
          },
        },
      });
    }
    await new Promise((r) => setTimeout(r, 20));
    expect((sandboxGlobal as Record<string, unknown>).__kdInline).toBe('true|hello|opt|click');
    expect(messages.some((m) => m.type === 'error')).toBe(false);
  });

  it('inline 代码语法错误只告警不拆沙箱', async () => {
    const nonce = 'nonce-inline-bad';
    const script = sandboxScript('', nonce, { stat_data: {}, display_data: {} });
    const messages: Array<Record<string, unknown>> = [];
    const listeners: Array<(ev: { data: unknown }) => void> = [];
    const sandboxGlobal = {
      parent: { postMessage: (m: Record<string, unknown>) => void messages.push(m) },
      addEventListener: (_type: string, fn: (ev: { data: unknown }) => void) => void listeners.push(fn),
      console,
      structuredClone: (v: unknown) => JSON.parse(JSON.stringify(v)),
      queueMicrotask,
      Promise,
      setTimeout,
      Function,
      // 共享桥注入/diff 读写 window(真实浏览器沙箱必有),harness 补齐对齐
      window: {},
    };
    vm.createContext(sandboxGlobal);
    new vm.Script(script).runInContext(sandboxGlobal);
    for (const fn of listeners) {
      fn({ data: { channel: 'kedai-character-script-v1', nonce, type: 'inline-event', code: 'if({', event: { type: 'click' }, target: { kind: 'target', id: 1 } } });
    }
    await new Promise((r) => setTimeout(r, 20));
    // 消息级沙箱经闭包桥 eval 求值:语法错误在调用时抛出 → 归入「回调出错」;
    // 无桥(卡级)时 new Function 同步抛 → 「解析失败」。两种都只告警、不拆沙箱。
    expect(
      messages.some((m) => m.type === 'warn' && String(m.message).includes('内联事件')),
    ).toBe(true);
    expect(messages.some((m) => m.type === 'error')).toBe(false);
  });
});

describe('沙箱协议流支持(wuwa 开场协议:滚动解锁/定位/localStorage)', () => {
  /** 与浏览器沙箱等价的 vm 沙箱:window/document 提供最小几何桩(模板会覆写其访问器) */
  function makeSandbox(nonce: string, script: string) {
    const messages: Array<Record<string, unknown>> = [];
    const listeners: Array<(ev: { data: unknown }) => void> = [];
    const sandboxGlobal: Record<string, unknown> = {
      parent: { postMessage: (m: Record<string, unknown>) => void messages.push(m) },
      addEventListener: (_type: string, fn: (ev: { data: unknown }) => void) => void listeners.push(fn),
      console,
      structuredClone: (v: unknown) => JSON.parse(JSON.stringify(v)),
      queueMicrotask,
      Promise,
      setTimeout,
      window: {},
      document: { body: {}, documentElement: {} },
    };
    vm.createContext(sandboxGlobal);
    (sandboxGlobal as Record<string, unknown>).Function = vm.runInContext('Function', sandboxGlobal as never);
    new vm.Script(script).runInContext(sandboxGlobal as never);
    const dispatch = (m: Record<string, unknown>): void => {
      for (const fn of listeners) fn({ data: m });
    };
    return { messages, dispatch, sandboxGlobal };
  }

  it('几何镜像:window.innerHeight/frameElement/document.body 映射宿主容器,geometry 消息实时更新', async () => {
    const nonce = 'nonce-geo';
    const script = sandboxScript('', nonce, { stat_data: {}, display_data: {} }, {}, {}, {
      top: 63, left: 260, width: 1017, height: 2190, scrollHeight: 2190, scrollWidth: 1017, hostH: 720, hostW: 1280,
    });
    const { dispatch, sandboxGlobal } = makeSandbox(nonce, script);
    const read = (expr: string): unknown => vm.runInContext(expr, sandboxGlobal as never);
    // boot 注入的初始几何
    expect(read('window.innerHeight')).toBe(2190);
    expect(read('window.innerWidth')).toBe(1017);
    expect(read('window.frameElement.getBoundingClientRect().top')).toBe(63);
    expect(read('window.frameElement.getBoundingClientRect().height')).toBe(2190);
    // 父窗口门面:innerHeight 是宿主视口而非容器高度(作者 positionOverlay 的 parentH)
    expect(read('window.parent.innerHeight')).toBe(720);
    expect(read('document.body.getBoundingClientRect().top')).toBe(0);
    expect(read('document.body.scrollHeight')).toBe(2190);
    expect(read('document.documentElement.scrollWidth')).toBe(1017);
    // 聊天滚动后宿主推送新几何:容器顶部滚出视口
    dispatch({
      channel: 'kedai-character-script-v1', nonce, type: 'geometry',
      geo: { top: -1639, left: 260, width: 1017, height: 2190, scrollHeight: 2190, scrollWidth: 1017, hostH: 720, hostW: 1280 },
    });
    expect(read('window.frameElement.getBoundingClientRect().top')).toBe(-1639);
    expect(read('window.innerHeight')).toBe(2190);
    expect(read('window.parent.innerHeight')).toBe(720);
    await new Promise((r) => setTimeout(r, 5));
  });

  it('localStorage 写入同步 local-storage 消息,boot 注入的已存快照可同步读', async () => {
    const nonce = 'nonce-ls2';
    const user = [
      "warn('accepted='+localStorage.getItem('ww_agreement_accepted_v1'));",
      "localStorage.setItem('ww_agreement_accepted_v1','1');",
      "localStorage.setItem('k','v');",
      "localStorage.removeItem('k');",
      "sessionStorage.setItem('s','1');", // sessionStorage 不应产生同步消息
    ].join('\n');
    const script = sandboxScript(user, nonce, { stat_data: {}, display_data: {} }, {}, { ww_agreement_accepted_v1: '0' });
    const { messages } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const warn = messages.find((m) => m.type === 'warn' && String(m.message).startsWith('accepted='));
    expect(warn?.message).toBe('accepted=0');
    const ops = messages.filter((m) => m.type === 'local-storage');
    expect(ops).toContainEqual(expect.objectContaining({ op: 'set', key: 'ww_agreement_accepted_v1', value: '1' }));
    expect(ops).toContainEqual(expect.objectContaining({ op: 'set', key: 'k', value: 'v' }));
    expect(ops).toContainEqual(expect.objectContaining({ op: 'remove', key: 'k' }));
    expect(ops.some((m) => m.key === 's')).toBe(false);
  });

  it('事件回包的 target.state 按 id 合入镜像,$card[0] 直读滚动度量(checkBottom 场景)', async () => {
    const nonce = 'nonce-scroll';
    // wuwa 协议卡 checkBottom 模式:scroll/wheel 回调里读 $card[0] 的滚动度量判断到底
    const user = [
      "$('#ww-agreement-card').on('scroll wheel', function(){",
      "  const el=$('#ww-agreement-card')[0];",
      "  const atBottom=el.scrollHeight<=el.clientHeight+2||el.scrollTop+el.clientHeight>=el.scrollHeight-8;",
      "  warn('bottom='+atBottom+'|'+el.scrollTop+'/'+el.clientHeight+'/'+el.scrollHeight);",
      "});",
    ].join('\n');
    const script = sandboxScript(user, nonce, { stat_data: {}, display_data: {} });
    const { messages, dispatch } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    // on 入队的 batch 先 ack(镜像无滚动度量,模拟 boot 时卡片未滚动)
    for (const m of messages.filter((x) => x.type === 'batch')) {
      dispatch({ channel: 'kedai-character-script-v1', nonce, type: 'rpc-result', id: m.id, ok: true, value: true });
    }
    await new Promise((r) => setTimeout(r, 10));
    // 宿主派发 wheel 事件:target.state 带事件当时的滚动真值(已滚到底 183+380>=564-8)
    dispatch({
      channel: 'kedai-character-script-v1', nonce, type: 'jq-event', jqId: 1, event: { type: 'wheel' },
      target: { kind: 'target', id: 1, data: {}, state: { id: 'ww-agreement-card', classes: [], scrollTop: 183, clientHeight: 380, scrollHeight: 564 } },
      mirror: { controls: [] },
    });
    await new Promise((r) => setTimeout(r, 10));
    const warn = messages.find((m) => m.type === 'warn' && String(m.message).startsWith('bottom='));
    expect(warn?.message).toBe('bottom=true|183/380/564');
  });

  it('outerHeight 从镜像 offsetHeight 读真值(positionOverlay 的 overlayH)', async () => {
    const nonce = 'nonce-oh';
    // 读取放在事件回调里:回调触发时镜像已随 batch ack 到达,outerHeight 须读到真值
    const user = [
      "$('#ww-agreement-overlay').on('click', function(){",
      "  warn('oh='+$('#ww-agreement-overlay').outerHeight());",
      "});",
    ].join('\n');
    const script = sandboxScript(user, nonce, { stat_data: {}, display_data: {} });
    const { messages, dispatch } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    // ack 携带选择器镜像(on 批的 buildStateMirror 会采 #ww-agreement-overlay)
    for (const m of messages.filter((x) => x.type === 'batch')) {
      dispatch({
        channel: 'kedai-character-script-v1', nonce, type: 'rpc-result', id: m.id, ok: true, value: true,
        mirror: { selectors: { '#ww-agreement-overlay': { n: 1, offsetHeight: 486, classes: [] } }, controls: [] },
      });
    }
    await new Promise((r) => setTimeout(r, 10));
    dispatch({
      channel: 'kedai-character-script-v1', nonce, type: 'jq-event', jqId: 1, event: { type: 'click' },
      target: { kind: 'target', id: 1, data: {}, state: { classes: [] } }, mirror: { controls: [] },
    });
    await new Promise((r) => setTimeout(r, 10));
    const warn = messages.find((m) => m.type === 'warn' && String(m.message).startsWith('oh='));
    expect(warn?.message).toBe('oh=486');
  });

  it('变量门面返回裸值视图([值,原因] 叶子解包,wuwa 状态栏直接属性读)', async () => {
    // wuwa 浪潮状态栏按裸值读:String(u.是否是漂泊者)==='true'、u.性别||'男'、
    // safe(sd.当前时间) 直接进 HTML。门面若给 [v,'初始'] 数组,时间显示带「,初始」尾巴、
    // 布尔判断全部错位。叶子 [x, 原因字符串] 解包为 x;真实数组值经包裹后同样还原。
    const nonce = 'nonce-unwrap';
    const user = [
      'const sd = getAllVariables().stat_data;',
      "warn('time=' + sd.当前时间 + ';drifter=' + String(sd.主角信息.是否是漂泊者) + ';log=' + JSON.stringify(sd.女性角色.守岸人.私密资料.性爱日志));",
      "warn('mvu=' + Mvu.getMvuData().stat_data.当前时间 + ';gv=' + Mvu.getVariables().stat_data.主角信息.是否是漂泊者);",
    ].join('\n');
    const script = sandboxScript(user, nonce, {
      stat_data: {
        当前时间: ['第1年 10月13日 周日 14:00', '初始'],
        主角信息: { 是否是漂泊者: [true, '初始'] },
        女性角色: { 守岸人: { 私密资料: { 性爱日志: [[], '初始'] } } },
      },
      display_data: {},
    });
    const { messages } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const w = messages.filter((m) => m.type === 'warn').map((m) => String(m.message));
    expect(w.some((x) => x.includes('time=第1年 10月13日 周日 14:00;drifter=true;log=[]'))).toBe(true);
    expect(w.some((x) => x.includes('mvu=第1年 10月13日 周日 14:00;gv=true'))).toBe(true);
  });

  it("attr('id') 复合类选择器经反向索引回查(wuwa render 的 activeTabId)", async () => {
    // wuwa 状态栏 render():$('.tab-page').removeClass('active') + $('#page-user').addClass('active')
    // 之后读 $('.tab-page.active').attr('id') 决定渲染哪个 tab 页;attr 读恒 '' 时
    // doRenderTab 永不执行,所有 tab 页停在骨架 '--'。
    // 真实时序:init 到 render() 全是 microtask,任何 batch ack(postMessage 宏任务)都来不及,
    // 故 id 轻量表必须随 boot 注入,addClass 的本地补丁合入同一 state 对象。
    const nonce = 'nonce-attr';
    const user = [
      "$('.tab-page').removeClass('active');",
      "$('#page-user').addClass('active');",
      "warn('tab=' + $('.tab-page.active').attr('id'));",
    ].join('\n');
    const script = sandboxScript(
      user,
      nonce,
      { stat_data: {}, display_data: {} },
      {},
      {},
      undefined,
      { '#page-user': { id: 'page-user', classes: ['tab-page'], n: 1 } },
    );
    const { messages } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const warn = messages.find((m) => m.type === 'warn' && String(m.message).startsWith('tab='));
    expect(warn?.message).toBe('tab=page-user');
  });
});

describe('sandboxScript(卡级脚本兼容面:tavern_events/TavernHelper 世界书/消息)', () => {
  const out = sandboxScript('x();', 'nonce-card', { stat_data: {}, display_data: {} }, {}, {}, undefined, {}, '主世界书');

  it('tavern_events 常量定义并挂载 globalThis(TDZ 安全:挂在 const 定义之后)', () => {
    expect(out).toContain("MESSAGE_SWIPED:'message_swiped'");
    expect(out).toContain('globalThis.tavern_events=tavern_events');
    // 挂载必须出现在 const tavern_events 定义之后,否则 TDZ ReferenceError
    expect(out.indexOf('globalThis.tavern_events=tavern_events')).toBeGreaterThan(
      out.indexOf('const tavern_events='),
    );
    expect(() => new vm.Script(out)).not.toThrow();
  });

  it('TavernHelper 扩展方法:事件/世界书/聊天消息', () => {
    for (const marker of [
      'errorCatched:errorCatched', 'eventOn:eventOn', 'eventOff:eventOff', 'eventEmit:eventEmit',
      'getCurrentCharPrimaryLorebook:function()', 'getLorebookEntries:function(name)',
      'setLorebookEntries:function(name,list)', 'getChatMessages:function(id,opts)',
    ]) {
      expect(out).toContain(marker);
    }
    expect(() => new vm.Script(out)).not.toThrow();
  });

  it('主世界书名随 boot 注入(__KD_LOREBOOK__),缺省为 null', () => {
    expect(out).toContain('const __KD_LOREBOOK__="主世界书";');
    const bare = sandboxScript('x();', 'nonce-bare', { stat_data: {}, display_data: {} });
    expect(bare).toContain('const __KD_LOREBOOK__=null;');
  });

  /** 与浏览器沙箱等价的 vm 沙箱(与上方 makeSandbox 同款,本 describe 自包含) */
  function makeCardSandbox(nonce: string, script: string) {
    const messages: Array<Record<string, unknown>> = [];
    const listeners: Array<(ev: { data: unknown }) => void> = [];
    const sandboxGlobal: Record<string, unknown> = {
      parent: { postMessage: (m: Record<string, unknown>) => void messages.push(m) },
      addEventListener: (_type: string, fn: (ev: { data: unknown }) => void) => void listeners.push(fn),
      console,
      structuredClone: (v: unknown) => JSON.parse(JSON.stringify(v)),
      queueMicrotask,
      Promise,
      setTimeout,
      window: {},
      document: { body: {}, documentElement: {} },
    };
    vm.createContext(sandboxGlobal);
    (sandboxGlobal as Record<string, unknown>).Function = vm.runInContext('Function', sandboxGlobal as never);
    new vm.Script(script).runInContext(sandboxGlobal as never);
    const dispatch = (m: Record<string, unknown>): void => {
      for (const fn of listeners) fn({ data: m });
    };
    return { messages, dispatch };
  }

  it('宿主 card-event 广播经事件总线派发到 eventOn 订阅者(舰娘卡 MESSAGE_SWIPED 链路)', async () => {
    const nonce = 'nonce-swipe';
    const user = [
      "eventOn(tavern_events.MESSAGE_SWIPED,(id)=>{warn('swiped='+id);});",
    ].join('\n');
    const script = sandboxScript(user, nonce, { stat_data: {}, display_data: {} });
    const { messages, dispatch } = makeCardSandbox(nonce, script);
    dispatch({ channel: 'kedai-character-script-v1', nonce, type: 'card-event', name: 'message_swiped', payload: 0 });
    await new Promise((r) => setTimeout(r, 20));
    const warn = messages.find((m) => m.type === 'warn' && String(m.message).startsWith('swiped='));
    expect(warn?.message).toBe('swiped=0');
  });

  it('getLorebookEntries 名称不符直接 resolve null 且不发 RPC;相符走 lorebook-entries RPC', async () => {
    const nonce = 'nonce-lb';
    const user = [
      "TavernHelper.getLorebookEntries('别的书').then((v)=>warn('mismatch='+JSON.stringify(v)));",
      "TavernHelper.getLorebookEntries('主书').then((v)=>warn('match='+JSON.stringify(v)));",
      "TavernHelper.setLorebookEntries('别的书',[]).then((v)=>warn('setmis='+JSON.stringify(v)));",
    ].join('\n');
    const script = sandboxScript(user, nonce, { stat_data: {}, display_data: {} }, {}, {}, undefined, {}, '主书');
    const { messages, dispatch } = makeCardSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    // 只有名称相符的那一路发出 RPC(过滤启动时的 audio-snapshot 预拉取)
    const rpcs = messages.filter((m) => m.type === 'rpc' && m.op !== 'audio-snapshot');
    expect(rpcs.length).toBe(1);
    expect(rpcs[0].op).toBe('lorebook-entries');
    // 应答该 RPC
    dispatch({
      channel: 'kedai-character-script-v1', nonce, type: 'rpc-result', id: rpcs[0].id,
      ok: true, value: [{ uid: 99, comment: '条目', enabled: true }],
    });
    await new Promise((r) => setTimeout(r, 10));
    const warns = messages.filter((m) => m.type === 'warn').map((m) => String(m.message));
    expect(warns.some((w) => w === 'mismatch=null')).toBe(true);
    expect(warns.some((w) => w === 'setmis=false')).toBe(true);
    expect(warns.some((w) => w.includes('match=[{"uid":99'))).toBe(true);
  });

  it('getChatMessages 走 chat-messages RPC(带楼层序号与 include_swipe 选项)', async () => {
    const nonce = 'nonce-chatmsg';
    const user = [
      "TavernHelper.getChatMessages(0,{include_swipe:true}).then((v)=>warn('msgs='+JSON.stringify(v)));",
    ].join('\n');
    const script = sandboxScript(user, nonce, { stat_data: {}, display_data: {} });
    const { messages, dispatch } = makeCardSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const rpc = messages.find((m) => m.type === 'rpc' && m.op === 'chat-messages');
    expect(rpc).toBeTruthy();
    expect((rpc?.args as unknown[])[0]).toBe(0);
    dispatch({
      channel: 'kedai-character-script-v1', nonce, type: 'rpc-result', id: rpc?.id,
      ok: true, value: [{ role: 'assistant', content: '开场', swipes: ['开场'], swipe_id: 0 }],
    });
    await new Promise((r) => setTimeout(r, 10));
    const warn = messages.find((m) => m.type === 'warn' && String(m.message).startsWith('msgs='));
    expect(String(warn?.message)).toContain('swipe_id":0');
  });
});

describe('sandboxScript(角色卡悬浮窗兼容面:head 映射/css 对象/attr 对象/draggable)', () => {
  /** 与浏览器沙箱等价的 vm 沙箱(document/window 最小桩;window 是真实对象引用) */
  function makeSandbox(nonce: string, script: string) {
    const messages: Array<Record<string, unknown>> = [];
    const listeners: Array<(ev: { data: unknown }) => void> = [];
    const sandboxGlobal: Record<string, unknown> = {
      parent: { postMessage: (m: Record<string, unknown>) => void messages.push(m) },
      addEventListener: (_type: string, fn: (ev: { data: unknown }) => void) => void listeners.push(fn),
      console,
      structuredClone: (v: unknown) => JSON.parse(JSON.stringify(v)),
      queueMicrotask,
      Promise,
      setTimeout,
      window: {},
      document: { body: {}, documentElement: {} },
      navigator: {},
    };
    vm.createContext(sandboxGlobal);
    (sandboxGlobal as Record<string, unknown>).Function = vm.runInContext('Function', sandboxGlobal as never);
    new vm.Script(script).runInContext(sandboxGlobal as never);
    const dispatch = (m: Record<string, unknown>): void => {
      for (const fn of listeners) fn({ data: m });
    };
    return { messages, dispatch, sandboxGlobal };
  }

  const batchOps = (messages: Array<Record<string, unknown>>): Array<Record<string, unknown>> =>
    messages.filter((m) => m.type === 'batch').flatMap((m) => (m.ops as Array<Record<string, unknown>>) ?? []);

  it("$('head').append('<style>…') 入队 batch op 且 ref 值为 head", async () => {
    // 根因 2:$('head') 在 queryScoped 里 0 匹配导致 <style> 注入静默失败
    const nonce = 'nonce-head';
    const script = sandboxScript(
      "$('head').append('<style id=\"style_x\">.a{color:red}</style>');",
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const op = batchOps(messages).find((o) => o.method === 'append');
    expect(op?.ref).toEqual({ kind: 'selector', value: 'head' });
    expect(String((op?.args as unknown[])[0])).toContain('style_x');
  });

  it("$('<div>').attr('id','x').css({position:'fixed',zIndex:9}) 的 created spec 含 attrs.id 与 css 键值", async () => {
    // 根因 2 的 jqCreated 半:此前 attr/css 是 no-op,悬浮球连 id 都没设上
    const nonce = 'nonce-created';
    const script = sandboxScript(
      "$('body').append($('<div>').attr('id','x').css({position:'fixed',zIndex:9}));",
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const op = batchOps(messages).find((o) => o.method === 'appendCreated');
    const spec = (op?.args as Array<Record<string, unknown>>)[0];
    expect(spec.attrs).toEqual({ id: 'x' });
    expect(spec.css).toEqual({ position: 'fixed', zIndex: '9' });
  });

  it('.css({a:1,b:2}) 对象形式入队两个 css op(此前被当 getter 吞掉)', async () => {
    const nonce = 'nonce-css-obj';
    const script = sandboxScript(
      "$('#fx').css({position:'fixed',zIndex:9999});",
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const cssOps = batchOps(messages).filter((o) => o.method === 'css');
    expect(cssOps).toHaveLength(2);
    expect(cssOps.map((o) => o.args)).toEqual(
      expect.arrayContaining([['position', 'fixed'], ['zIndex', 9999]]),
    );
  });

  it('.draggable({start,drag,stop,containment}) 入队 draggable op 且回调 jqId 为数字', async () => {
    const nonce = 'nonce-drag';
    const script = sandboxScript(
      "$('#fx-floating-ball').draggable({containment:'window',distance:3,start:function(){},drag:function(){},stop:function(){}});",
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const op = batchOps(messages).find((o) => o.method === 'draggable');
    expect(op?.ref).toEqual({ kind: 'selector', value: '#fx-floating-ball' });
    const wire = (op?.args as Array<Record<string, unknown>>)[0];
    expect(wire.containment).toBe('window');
    expect(wire.distance).toBe(3);
    expect(typeof wire.start).toBe('number');
    expect(typeof wire.drag).toBe('number');
    expect(typeof wire.stop).toBe('number');
  });

  it("$(window).on('unload',fn) 识别为 window 引用(此前对象形式 jq(window) → ref=null 静默失效)", async () => {
    const nonce = 'nonce-win';
    const script = sandboxScript(
      "$(window).on('unload',function(){});",
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const op = batchOps(messages).find((o) => o.method === 'on');
    expect(op?.ref).toEqual({ kind: 'window' });
    expect((op?.args as unknown[])[0]).toBe('unload');
  });

  it("实跑问题 7 R3:三参委托写法 on(evt, selector, fn) 的 selector 作为第 3 参数入队,回调登记可用", async () => {
    const nonce = 'nonce-deleg';
    const script = sandboxScript(
      "$(document).on('click.myNS','.modal-btn',function(){warn('hit');});",
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages, dispatch } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const op = batchOps(messages).find((o) => o.method === 'on');
    expect(op).toBeTruthy();
    expect(op?.ref).toEqual({ kind: 'document' });
    const args = op?.args as unknown[];
    // 事件名原样入队(命名空间剥离在宿主 applyJq 侧统一做,见 dom-rpc.test.ts)
    expect(args[0]).toBe('click.myNS');
    // 第 3 参数是委托选择器(旧实现把函数当参数,宿主收不到有效事件名)
    expect(args[2]).toBe('.modal-btn');
    expect(typeof args[1]).toBe('number');
    // 回发该 jqId 的事件:回调应执行(旧实现把 selector 当回调,fn.call 抛 TypeError 被吞)
    dispatch({
      channel: 'kedai-character-script-v1',
      nonce,
      type: 'jq-event',
      jqId: args[1] as number,
      event: { type: 'click' },
      target: { kind: 'target', id: 1, data: {}, state: {} },
      mirror: { controls: [] },
    });
    await new Promise((r) => setTimeout(r, 10));
    expect(messages.some((m) => m.type === 'warn' && String(m.message).includes('hit'))).toBe(true);
  });

  it('实跑问题 7 R4:剪贴板桥把 navigator.clipboard.writeText 转发为 clipboard-write RPC', async () => {
    const nonce = 'nonce-clip';
    const script = sandboxScript(
      "navigator.clipboard.writeText('提示词正文');",
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const rpc = messages.find((m) => m.type === 'rpc' && m.op === 'clipboard-write');
    expect(rpc).toBeTruthy();
    expect((rpc?.args as unknown[])[0]).toBe('提示词正文');
  });

  it('剪贴板 RPC 失败时沙箱侧 Promise reject,作者脚本走 catch 分支(不再谎报成功)', async () => {
    const nonce = 'nonce-clip-fail';
    const script = sandboxScript(
      "navigator.clipboard.writeText('提示词').then(function(){warn('then-hit')}).catch(function(e){warn('catch-hit:'+e.message)});",
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages, dispatch } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const rpc = messages.find((m) => m.type === 'rpc' && m.op === 'clipboard-write');
    expect(rpc).toBeTruthy();
    // 宿主剪贴板失败 → rpc-result(ok:false);沙箱侧据此 reject
    dispatch({
      channel: 'kedai-character-script-v1',
      nonce,
      type: 'rpc-result',
      id: rpc?.id,
      ok: false,
      error: '宿主页面不提供剪贴板写入',
    });
    await new Promise((r) => setTimeout(r, 10));
    const warns = messages.filter((m) => m.type === 'warn').map((m) => String(m.message));
    expect(warns.some((w) => w.includes('catch-hit'))).toBe(true);
    expect(warns.some((w) => w.includes('then-hit'))).toBe(false);
    // 单次失败只上报 warn,不拆沙箱
    expect(messages.some((m) => m.type === 'error')).toBe(false);
  });

  it('实跑问题 7 R4:execCommand("copy") 桥接后不再走原生(无选区也不抛错)', async () => {
    const nonce = 'nonce-execcopy';
    const script = sandboxScript(
      "document.execCommand('copy');",
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const out = sandboxScript('x();', 'nonce-execcopy-gen', { stat_data: {}, display_data: {} });
    expect(out).toContain('__kdExec');
    expect(() => new vm.Script(out)).not.toThrow();
    const { messages } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    expect(messages.some((m) => m.type === 'error')).toBe(false);
  });

  it("draggable('destroy') 入队 destroy op,data('ui-draggable') 随应用翻转", async () => {
    const nonce = 'nonce-drag-destroy';
    const script = sandboxScript(
      "$('#fx').draggable({});warn('has='+$('#fx').data('ui-draggable'));$('#fx').draggable('destroy');warn('after='+$('#fx').data('ui-draggable'));",
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 10));
    const ops = batchOps(messages).filter((o) => o.method === 'draggable');
    expect(ops[0]?.args).toEqual([expect.objectContaining({ start: 0, drag: 0, stop: 0 })]);
    expect(ops[1]?.args).toEqual(['destroy']);
    const warns = messages.filter((m) => m.type === 'warn').map((m) => String(m.message));
    expect(warns).toContain('has=true');
    expect(warns).toContain('after=false');
  });
});

describe('sandboxScript(多段单 realm:逐 <script> 注入共享 window)', () => {
  // boot 模板多段路径把每段包成独立 <script> 由 document.head.appendChild 注入;
  // vm 无真实 DOM,harness 的 appendChild 同步执行段文本(runInContext 同一 context →
  // 与真实浏览器共享 window/全局词法环境一致);段错误(语法/运行时)捕获后派发
  // window error 事件(对齐真实 HTML 逐 <script> 语义:该段作废,后续段照常执行),
  // 沙箱 error 监听器据此上报带段名的 warn。
  function makeSegmentSandbox(nonce: string, script: string) {
    const messages: Array<Record<string, unknown>> = [];
    const listeners: Array<{ type: string; fn: (e: Record<string, unknown>) => void }> = [];
    const fire = (type: string, e: Record<string, unknown>): void => {
      for (const l of listeners) {
        if (l.type !== type) continue;
        try {
          l.fn(e);
        } catch {
          /* 监听器异常不扩散 */
        }
      }
    };
    const sandboxGlobal: Record<string, unknown> = {
      parent: { postMessage: (m: Record<string, unknown>) => void messages.push(m) },
      addEventListener: (type: string, fn: (e: Record<string, unknown>) => void) => void listeners.push({ type, fn }),
      console,
      structuredClone: (v: unknown) => JSON.parse(JSON.stringify(v)),
      queueMicrotask,
      Promise,
      setTimeout,
      window: {},
      document: {
        body: {},
        documentElement: {},
        createElement: () => ({ textContent: '' }),
        // appendChild 捕获段错误并派发 error(该段作废不拖垮其余段)
        head: {
          appendChild: (node: { textContent?: string }) => {
            const code = node?.textContent ?? '';
            if (!code.trim()) return;
            try {
              new vm.Script(code).runInContext(sandboxGlobal as never);
            } catch (error) {
              fire('error', {
                type: 'error',
                error,
                message: error instanceof Error ? error.message : String(error),
              });
            }
          },
        },
      },
    };
    vm.createContext(sandboxGlobal as never);
    (sandboxGlobal as Record<string, unknown>).Function = vm.runInContext('Function', sandboxGlobal as never);
    new vm.Script(script).runInContext(sandboxGlobal as never);
    return { messages };
  }

  it('段 1 挂 window 全局,段 2 同 realm 可读并写宿主可见结果(共享 window/全局词法环境)', async () => {
    const nonce = 'nonce-seg-share';
    const script = sandboxScript(
      [
        { name: '剧情逻辑', code: 'window.SharedInfo={mark:7};' },
        { name: '界面脚本', code: "warn('got='+window.SharedInfo.mark);" },
      ],
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages } = makeSegmentSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 30));
    const warn = messages.find((m) => m.type === 'warn' && String(m.message).startsWith('got='));
    expect(warn?.message).toBe('got=7');
    expect(messages.some((m) => m.type === 'done')).toBe(true);
    expect(messages.some((m) => m.type === 'error')).toBe(false);
  });

  it('单段语法错误只废该段:已执行段副作用保留,整体仍 done 不炸', async () => {
    const nonce = 'nonce-seg-bad';
    const script = sandboxScript(
      [
        { name: '段1', code: "warn('one-ran');" },
        { name: '段2', code: 'const = syntax error;' },
      ],
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages } = makeSegmentSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 30));
    const warns = messages.filter((m) => m.type === 'warn').map((m) => String(m.message));
    expect(warns.some((w) => w.includes('one-ran'))).toBe(true);
    // 语法错误段整段不解析(含段首 __kdScriptName 注入),error 上报存在即可
    expect(warns.length).toBeGreaterThanOrEqual(2);
    expect(messages.some((m) => m.type === 'done')).toBe(true);
    expect(messages.some((m) => m.type === 'error')).toBe(false);
  });

  it('段运行期 throw 不阻断后续段,错误上报带段名', async () => {
    const nonce = 'nonce-seg-throw';
    const script = sandboxScript(
      [
        { name: '剧情逻辑', code: "throw new Error('boom-seg1');" },
        { name: '界面脚本', code: "warn('two-ran');" },
      ],
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages } = makeSegmentSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 30));
    const warns = messages.filter((m) => m.type === 'warn').map((m) => String(m.message));
    expect(warns.some((w) => w.includes('two-ran'))).toBe(true);
    expect(warns.some((w) => w.includes('[卡脚本 剧情逻辑]') && w.includes('boom-seg1'))).toBe(true);
    expect(messages.some((m) => m.type === 'done')).toBe(true);
    expect(messages.some((m) => m.type === 'error')).toBe(false);
  });
});

describe('sandboxScript(跨 realm 共享全局桥:注入/diff 上报/update 更新)', () => {
  /** 与浏览器沙箱等价的 vm 沙箱(共享桥代码读写 window:{} 与消息监听) */
  function makeSandbox(nonce: string, script: string) {
    const messages: Array<Record<string, unknown>> = [];
    const listeners: Array<(ev: { data: unknown }) => void> = [];
    const sandboxGlobal: Record<string, unknown> = {
      parent: { postMessage: (m: Record<string, unknown>) => void messages.push(m) },
      addEventListener: (_type: string, fn: (ev: { data: unknown }) => void) => void listeners.push(fn),
      console,
      structuredClone: (v: unknown) => JSON.parse(JSON.stringify(v)),
      queueMicrotask,
      Promise,
      setTimeout,
      window: {},
      document: { body: {}, documentElement: {} },
    };
    vm.createContext(sandboxGlobal as never);
    (sandboxGlobal as Record<string, unknown>).Function = vm.runInContext('Function', sandboxGlobal as never);
    new vm.Script(script).runInContext(sandboxGlobal as never);
    const dispatch = (m: Record<string, unknown>): void => {
      for (const fn of listeners) fn({ data: m });
    };
    return { messages, dispatch, sandboxGlobal };
  }

  it('沙箱内 window 新增纯数据全局 → done 前宿主收到 shared-publish(卡级挂载 → 宿主快照)', async () => {
    const nonce = 'nonce-share-pub';
    const script = sandboxScript(
      'window.WuWaShared={ready:true,story:"S"};',
      nonce,
      { stat_data: {}, display_data: {} },
    );
    const { messages } = makeSandbox(nonce, script);
    await new Promise((r) => setTimeout(r, 20));
    const pub = messages.find((m) => m.type === 'shared-publish');
    expect(pub).toBeTruthy();
    expect((pub?.globals as { WuWaShared?: unknown })?.WuWaShared).toEqual({ ready: true, story: 'S' });
  });

  it('boot 注入的 sharedGlobals 沙箱内 window 同步可读(消息级 realm 降级读源)', async () => {
    const nonce = 'nonce-share-inj';
    const script = sandboxScript('', nonce, { stat_data: {}, display_data: {} }, {}, {}, undefined, {}, null, {
      WuWaShared: { story: 'S' },
    });
    const { sandboxGlobal } = makeSandbox(nonce, script);
    expect(vm.runInContext('window.WuWaShared', sandboxGlobal as never)).toEqual({ story: 'S' });
  });

  it('shared-update 后 window 全局更新并入基线(宿主实时推送注入)', async () => {
    const nonce = 'nonce-share-upd';
    const script = sandboxScript('', nonce, { stat_data: {}, display_data: {} });
    const { dispatch, sandboxGlobal } = makeSandbox(nonce, script);
    dispatch({
      channel: 'kedai-character-script-v1',
      nonce,
      type: 'shared-update',
      globals: { LiveInfo: { n: 2 } },
    });
    await new Promise((r) => setTimeout(r, 10));
    expect(vm.runInContext('window.LiveInfo', sandboxGlobal as never)).toEqual({ n: 2 });
  });
});
