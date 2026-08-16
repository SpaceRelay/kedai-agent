// 沙箱脚本生成的回归测试。
// 背景:sandboxScript 用模板字面量拼接内联脚本,模板字面量会消化 `\d` / `\s` / `\/`
// 这类反斜杠序列(JS 对未知转义只保留后一个字符),导致生成的正则字面量变成
// `/^d+$/` 与 `/...[sS]*?</UpdateVariable>/gi` —— 后者提前闭合正则,整段脚本
// 抛 SyntaxError: Invalid regular expression flags,任何角色卡脚本都无法执行,
// 状态栏永远停在卡片自带的「加载中」静态文案。故此处锁定「生成物必须可解析」。
import { describe, expect, it } from 'vitest';
import vm from 'node:vm';
import { sandboxScript } from './characterScriptSandbox';

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

  it('沙箱提供内存 localStorage shim(作者脚本读协议状态不抛错)', () => {
    const out = sandboxScript('x();', 'nonce-ls', { stat_data: {}, display_data: {} });
    expect(out).toContain('__kdMemoryStorage');
    expect(out).toContain('getItem: function');
    expect(out).not.toContain('localStorage=undefined');
    expect(() => new vm.Script(out)).not.toThrow();
  });

  it('沙箱提供 toastr stub(作者脚本 window.parent.toastr 桥接失败时仍可用)', () => {
    const out = sandboxScript('toastr.info("hi");', 'nonce-tr', { stat_data: {}, display_data: {} });
    expect(out).toContain('toastr');
    expect(out).toContain('info:');
    expect(() => new vm.Script(out)).not.toThrow();
  });
});
