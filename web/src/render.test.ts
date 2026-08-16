import { describe, it, expect } from 'vitest';
import {
  scopeCss,
  applyKeyframeRename,
  buildScopedScriptHtml,
  renderScopedScripts,
  expandReplaceRefs,
  hasStatusPlaceholderScript,
  buildMessageRenderText,
  sanitizeScopedCss,
  extractBodyLoadUrl,
  buildRemoteResourceHtml,
  buildScopedScriptHtml,
} from './render';
import type { RegexScript } from './api';

describe('scopeCss', () => {
  it('普通规则加前缀', () => {
    const { css } = scopeCss('.a { color: red } .b, .c { x: 1 }', 's1');
    expect(css).toContain('[data-kd-scope="s1"] .a { color: red }');
    expect(css).toContain('[data-kd-scope="s1"] .b, [data-kd-scope="s1"] .c { x: 1 }');
  });

  it('keyframes 重命名并映射', () => {
    const { css, keyframes } = scopeCss('@keyframes pulse-glow { 0% { opacity: 0 } }', 's2');
    expect(keyframes.get('pulse-glow')).toBe('kd-s2-pulse-glow');
    expect(css).toContain('@keyframes kd-s2-pulse-glow');
  });

  it('嵌套 @media 内部规则加前缀', () => {
    const { css } = scopeCss('@media (max-width: 600px) { .x { color: blue } }', 's3');
    expect(css).toContain('[data-kd-scope="s3"] .x');
  });

  it('@font-face 原样保留', () => {
    const { css } = scopeCss("@font-face { font-family: 'X'; src: url(x) }", 's4');
    expect(css).toContain('@font-face');
    expect(css).toContain('url(x)');
  });
});

describe('applyKeyframeRename', () => {
  it('替换 animation 中的名字', () => {
    const map = new Map([['pulse-glow', 'kd-s-pulse-glow']]);
    const out = applyKeyframeRename('.card { animation: pulse-glow 4s infinite }', map);
    expect(out).toContain('animation: kd-s-pulse-glow 4s infinite');
  });

  it('不重复替换已带前缀的 keyframes 定义名', () => {
    const map = new Map([['pulse-glow', 'kd-s-pulse-glow']]);
    const css = '@keyframes kd-s-pulse-glow { from { opacity: 0 } } .card { animation: pulse-glow 1s }';
    const out = applyKeyframeRename(css, map);
    expect(out).toContain('@keyframes kd-s-pulse-glow');
    expect(out).not.toContain('kd-s-kd-s-pulse-glow');
    expect(out).toContain('animation: kd-s-pulse-glow 1s');
  });

  it('@import 内部分号不被切分', () => {
    const { css } = scopeCss(
      "@import url('https://x/font.css?wght@300;400&family=Noto'); .a { color: red }",
      's5',
    );
    expect(css).toContain("@import url('https://x/font.css?wght@300;400&family=Noto');");
    expect(css).toContain('[data-kd-scope="s5"] .a');
  });
});

describe('sanitizeScopedCss', () => {
  it('结构化移除覆盖页面与交互劫持声明', () => {
    const css = '.x{position:fixed;position:sticky;z-index:999;inset:0;top:0;width:100vw;height:100vh;pointer-events:none;color:red}';
    const safe = sanitizeScopedCss(css);
    expect(safe).toContain('color: red');
    expect(safe).not.toMatch(/position|z-index|inset|top|100vw|100vh|pointer-events/i);
  });

  it('拒绝外部资源和非白名单 at-rule', () => {
    const safe = sanitizeScopedCss('@import "x"; @font-face{x:y} @media(max-width:1px){.x{color:red}} @keyframes k{from{opacity:0}}');
    expect(safe).not.toMatch(/@import|@font-face|@media/i);
    expect(safe).toContain('@keyframes k');
  });
});

describe('buildScopedScriptHtml', () => {
  const script = '```html\n<style>\n.a { color: red }\n@keyframes k { from{opacity:0} to{opacity:1} }\n</style>\n<div class="a">Hi</div>\n<script>window.__x = 1;</script>\n```';

  it('生成作用域容器 + 脚本源码 + 稳定 scopeId', () => {
    const a = buildScopedScriptHtml(script, 'm1');
    const b = buildScopedScriptHtml(script, 'm1');
    expect(a.scopeId).toBe(b.scopeId); // 稳定
    expect(a.html).toContain(`data-kd-scope="k${'m1'}"`);
    expect(a.html).toContain('[data-kd-scope=');
    expect(a.html).toContain('>Hi</div>');
    expect(a.scripts.length).toBe(1);
    expect(a.scripts[0]).toContain('window.__x = 1');
    // 脚本不含 import
    expect(a.scripts[0]).not.toContain('import');
  });

  it('危险 HTML 被清理，但安全 HTML 在未授权 JavaScript 时仍可显示', () => {
    const s = '<style>.a{color:red}</style><div class="a" onclick="steal()"><a href="javascript:steal()">安全文本</a><img src="https://evil.test/a.png"><script>steal()</script></div>';
    const r = buildScopedScriptHtml(s, 'm2');
    expect(r.html).toContain('安全文本');
    // 事件处理器 / javascript: 链接 / 内联脚本一律清除
    expect(r.html).not.toMatch(/onclick|javascript:|<script/i);
    // https 图片保留(WuWa 开场 Logo 等外链图依赖此能力)
    expect(r.html).toContain('https://evil.test/a.png');
    expect(r.scripts).toEqual(['steal()']);
  });

  it('危险样式能力被清理', () => {
    const s = '<style>@import url(https://evil.test/a.css); .a{background:url(https://evil.test/x);position:fixed}</style><div class="a">正文</div>';
    const r = buildScopedScriptHtml(s, 'm3');
    expect(r.html).toContain('正文');
    expect(r.html).not.toMatch(/@import|url\s*\(|position\s*:\s*fixed/i);
  });
});

describe('renderScopedScripts', () => {
  it('命中脚本返回 html+blocks,未命中返回 null', () => {
    const scripts: RegexScript[] = [
      {
        id: '1',
        script_name: '状态栏',
        find_regex: '<StatusPlaceHolderImpl/>',
        replace_string: '<div class="sb">状态</div>',
        enabled: true,
      },
    ];
    const r = renderScopedScripts('前文 <StatusPlaceHolderImpl/> 后文', scripts, 'm9');
    expect(r).not.toBeNull();
    expect(r!.html).toContain('前文');
    expect(r!.html).toContain('状态');
    expect(r!.blocks).toHaveLength(1);

    const none = renderScopedScripts('无命中', scripts, 'm9');
    expect(none).toBeNull();
  });

  it('UpdateVariable 块先剥离再渲染', () => {
    const scripts: RegexScript[] = [
      {
        id: '1',
        script_name: 'x',
        find_regex: 'PH',
        replace_string: '<b>hit</b>',
        enabled: true,
      },
    ];
    const text = '<UpdateVariable>_.set("a",1,2)</UpdateVariable> PH';
    const r = renderScopedScripts(text, scripts, 'm9');
    expect(r!.html).not.toContain('UpdateVariable');
    expect(r!.html).not.toContain('_.set');
    expect(r!.html).toContain('hit');
  });

  it('状态栏渲染脚本优先于包裹脚本(碧蓝卡场景:ui 手机界面不被「状态栏代码块」吞掉)', () => {
    // 模拟碧蓝卡脚本链:包裹脚本(把占位符包回围栏)排在渲染脚本(整页界面)前
    const wrap: RegexScript = {
      id: '8', script_name: '状态栏代码块',
      find_regex: '<StatusPlaceHolderImpl\\/>',
      replace_string: '```\n<StatusPlaceHolderImpl/>\n```',
      enabled: true,
    };
    const ui: RegexScript = {
      id: '9', script_name: 'ui',
      find_regex: '<StatusPlaceHolderImpl/>',
      replace_string: '<head><style>.ui-x{color:red}</style></head><body><div class="ui-x">手机界面</div><script>window.__ui = 1</script></body>',
      enabled: true,
    };
    const r = renderScopedScripts('正文 <StatusPlaceHolderImpl/>', [wrap, ui], 'bl');
    expect(r).not.toBeNull();
    // ui 脚本应命中并渲染界面(而不是被包裹脚本先吞掉)
    expect(r!.html).toContain('手机界面');
    expect(r!.blocks.some((b) => b.scripts.some((s) => s.includes('window.__ui')))).toBe(true);
    // 包裹脚本不应命中(占位符已被渲染脚本消费),不得残留围栏
    expect(r!.html).not.toContain('```');
    expect(r!.html).not.toContain('数据');
  });

  it('包裹脚本替换值被清洗后为空 → 不产生空容器', () => {
    // 只有「状态栏代码块」脚本:替换值为围栏包占位符,围栏内标签被白名单丢弃 → 空
    const wrap: RegexScript = {
      id: '8', script_name: '状态栏代码块',
      find_regex: '<StatusPlaceHolderImpl/>',
      replace_string: '```\n<StatusPlaceHolderImpl/>\n```',
      enabled: true,
    };
    const r = renderScopedScripts('前文 <StatusPlaceHolderImpl/>', [wrap], 'bl2');
    // 正文仍渲染(前文),但不得出现空的 scoped 容器或围栏残留
    expect(r!.html).toContain('前文');
    expect(r!.html).not.toContain('data-kd-scope');
    expect(r!.html).not.toContain('```');
    expect(r!.blocks).toHaveLength(0);
  });

  it('隐藏类脚本(markdown_only + 空替换)先剥除 thinking/剧情等块,只留开场剧情', () => {
    const hideThinking: RegexScript = {
      id: '0', script_name: '隐藏thinking', markdown_only: true,
      find_regex: '<thinking>([\\s\\S]*)</thinking>',
      replace_string: '', enabled: true,
    };
    const hidePlot: RegexScript = {
      id: '2', script_name: '隐藏剧情', markdown_only: true,
      find_regex: '<now_plot>([\\s\\S]*)</now_plot>',
      replace_string: '', enabled: true,
    };
    const ui: RegexScript = {
      id: '9', script_name: 'ui', markdown_only: true,
      find_regex: '<StatusPlaceHolderImpl/>',
      replace_string: '<div class="ui-x">手机界面</div>',
      enabled: true,
    };
    const text = '<thinking>给AI看的计划</thinking>\n开场剧情内容 <StatusPlaceHolderImpl/>\n<now_plot>隐藏的剧情指导</now_plot>';
    const r = renderScopedScripts(text, [hideThinking, hidePlot, ui], 'hide');
    // 开场剧情保留,思考块/剧情指导被剥除,界面占位符被 ui 脚本渲染
    expect(r!.html).toContain('开场剧情内容');
    expect(r!.html).toContain('手机界面');
    expect(r!.html).not.toContain('给AI看的计划');
    expect(r!.html).not.toContain('隐藏的剧情指导');
    expect(r!.html).not.toContain('<thinking');
    expect(r!.html).not.toContain('<now_plot');
    // 纯隐藏块场景(无渲染脚本命中)→ null(调用方走 markdown + stripHiddenPlaceholders 路径)
    const r2 = renderScopedScripts('<thinking>x</thinking> 正文', [hideThinking], 'hide2');
    expect(r2).toBeNull();
  });

  it('markdown_only=false 的隐藏脚本(对AI隐藏状态栏)不剥占位符,渲染脚本仍命中', () => {
    const hideForAi: RegexScript = {
      id: '7', script_name: '对AI隐藏状态栏', markdown_only: false,
      find_regex: '<StatusPlaceHolderImpl/>',
      replace_string: '', enabled: true,
    };
    const ui: RegexScript = {
      id: '9', script_name: 'ui', markdown_only: true,
      find_regex: '<StatusPlaceHolderImpl/>',
      replace_string: '<div class="ui-x">手机界面</div>',
      enabled: true,
    };
    const r = renderScopedScripts('正文 <StatusPlaceHolderImpl/>', [hideForAi, ui], 'ph');
    // markdown_only=false 的隐藏脚本不应剥掉占位符;渲染脚本仍渲染界面
    expect(r!.html).toContain('手机界面');
    expect(r!.html).not.toContain('StatusPlaceHolderImpl');
  });
});

describe('expandReplaceRefs', () => {
  it('展开 $1 捕获组与 $& 全匹配', () => {
    const args = ['12', '12', 0, '前文 12 后文'];
    expect(expandReplaceRefs('<b>$1</b>', args)).toBe('<b>12</b>');
    expect(expandReplaceRefs('[ $& ]', args)).toBe('[ 12 ]');
  });

  it('不存在的捕获组展开为空串', () => {
    const args = ['x', 'x', 0, 'x'];
    expect(expandReplaceRefs('$2|$9', args)).toBe('|');
  });

  it('$$ 为字面量 $, $` 与 $\' 为前后文本', () => {
    const args = ['12', '12', 2, 'ab12cd'];
    expect(expandReplaceRefs('$$', args)).toBe('$');
    expect(expandReplaceRefs('$`', args)).toBe('ab');
    expect(expandReplaceRefs("$'", args)).toBe('cd');
  });
});

describe('hasStatusPlaceholderScript', () => {
  const st: RegexScript = {
    id: '1',
    script_name: '状态栏',
    find_regex: '<StatusPlaceHolderImpl/>',
    replace_string: '<div class="sb">卡</div>',
    markdown_only: true,
    enabled: true,
  };

  it('存在启用且有替换串的占位符脚本 → true', () => {
    expect(hasStatusPlaceholderScript([st])).toBe(true);
  });

  it('禁用 / 空替换串 / 不匹配占位符 → false', () => {
    expect(hasStatusPlaceholderScript([{ ...st, enabled: false }])).toBe(false);
    expect(hasStatusPlaceholderScript([{ ...st, replace_string: '' }])).toBe(false);
    expect(hasStatusPlaceholderScript([{ ...st, find_regex: '其他' }])).toBe(false);
    expect(hasStatusPlaceholderScript([])).toBe(false);
  });

  it('大小写不敏感匹配占位符', () => {
    expect(hasStatusPlaceholderScript([{ ...st, find_regex: '<statusplaceholderimpl/>' }])).toBe(true);
  });
});

describe('buildMessageRenderText', () => {
  it('有状态栏 + 有状态栏脚本 → 正文后追加占位符(供脚本命中渲染 HTML 卡片)', () => {
    const out = buildMessageRenderText('正文', '好感度 0→3', true);
    expect(out).toBe('正文\n<StatusPlaceHolderImpl/>');
  });

  it('无状态栏或角色卡无状态栏脚本 → 原样返回正文', () => {
    expect(buildMessageRenderText('正文', null, true)).toBe('正文');
    expect(buildMessageRenderText('正文', '好感度 0→3', false)).toBe('正文');
    expect(buildMessageRenderText('正文', '', true)).toBe('正文');
  });

  it('正文已含占位符(备用开场自带界面)时不再追加,避免双卡片重复渲染', () => {
    const out = buildMessageRenderText('正文 <StatusPlaceHolderImpl/>', '好感度 0→3', true);
    expect(out).toBe('正文 <StatusPlaceHolderImpl/>');
  });
});

describe('extractBodyLoadUrl / buildRemoteResourceHtml', () => {
  it("提取 $('body').load('https://…') → URL(吸血鬼卡 first_mes 形态)", () => {
    const t = "```text\r\n<body>\r\n<h1>提示</h1>\r\n<script>  $('body').load('https://files.yuzuki-rii.xyz/bby/v2.1.html')\r\n</script>\r\n</body>\r\n```";
    expect(extractBodyLoadUrl(t)).toBe('https://files.yuzuki-rii.xyz/bby/v2.1.html');
  });

  it('双引号与空白变体也能提取', () => {
    expect(extractBodyLoadUrl('<script> $("body") .load( "https://a.b/c" ) </script>')).toBe('https://a.b/c');
  });

  it('非 https / 非 load 模式 → null', () => {
    expect(extractBodyLoadUrl("<script>$('body').load('http://a.b/c')</script>")).toBeNull();
    expect(extractBodyLoadUrl("<script>$('body').load('javascript:alert(1)')</script>")).toBeNull();
    expect(extractBodyLoadUrl('普通消息无资源界面')).toBeNull();
    expect(extractBodyLoadUrl("$('body').html('https://a.b')")).toBeNull();
  });

  it('资源卡片 HTML 含沙箱 iframe 与新窗口链接,URL 经转义存储', () => {
    const html = buildRemoteResourceHtml('https://files.yuzuki-rii.xyz/bby/v2.1.html', 'test');
    expect(html).toContain('class="sv-resource-card"');
    expect(html).toContain('data-kd-resource-url="' + encodeURIComponent('https://files.yuzuki-rii.xyz/bby/v2.1.html') + '"');
    expect(html).toContain('data-kd-resource-frame="1"');
    expect(html).toContain('sandbox="allow-scripts allow-popups allow-forms"');
    expect(html).toContain('referrerpolicy="no-referrer"');
    expect(html).toContain('在新窗口打开');
    // 不直接用 src 直嵌(作者服务器可能禁止 iframe 嵌入)
    expect(html).not.toContain('src="https://files.yuzuki-rii.xyz');
  });

  it('资源卡片 iframe 加载宿主文档且不 lazy(投递时序依赖 iframe 尽快就绪)', () => {
    const html = buildRemoteResourceHtml('https://files.yuzuki-rii.xyz/bby/v2.1.html', 't2');
    expect(html).toContain('src="/resource-frame.html#');
    expect(html).not.toContain('loading="lazy"');
  });
});

describe('sanitizeVisibleHtml(表单控件保留)', () => {
  it('保留开场界面的表单控件与状态属性(WuWa 开场:协议勾选/身份/版本/区域)', () => {
    const html = '<form><label for="a">身份</label>' +
      '<input type="text" id="a" name="custom" placeholder="自定义身份" class="wuwa-input">' +
      '<input type="checkbox" id="agree" checked disabled>' +
      '<select id="ver" class="wuwa-select"><option value="v1" selected>版本一</option><option value="v2">版本二</option></select>' +
      '<textarea id="plot" name="plot" placeholder="补充剧情" rows="3"></textarea>' +
      '<button type="button" onclick="steal()">确认</button></form>';
    const r = buildScopedScriptHtml(`<style>.wuwa-input{color:red}</style>${html}`, 'form1');
    expect(r.html).toContain('type="text"');
    expect(r.html).toContain('placeholder="自定义身份"');
    expect(r.html).toContain('type="checkbox"');
    expect(r.html).toContain('<select');
    expect(r.html).toContain('<option');
    expect(r.html).toContain('<textarea');
    expect(r.html).toContain('>版本一</option>');
    expect(r.html).toContain('<label');
    // 事件处理器仍被剥离(安全不降级)
    expect(r.html).not.toMatch(/onclick|onchange|oninput/i);
  });

  it('剥离开场 HTML 的事件处理器但保留禁用/勾选状态', () => {
    const s = '<input type="checkbox" id="agree" onchange="send()" disabled><select onchange="go()"><option selected>选一</option></select>';
    const r = buildScopedScriptHtml(s, 'form2');
    expect(r.html).not.toMatch(/onchange|onclick|oninput/i);
    expect(r.html).toContain('disabled');
    expect(r.html).toContain('selected');
  });
});
