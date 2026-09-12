import { describe, it, expect } from 'vitest';
import {
  scopeCss,
  applyKeyframeRename,
  buildScopedScriptHtml,
  renderScopedScripts,
  expandReplaceRefs,
  expandDisplayMacros,
  hasStatusPlaceholderScript,
  buildMessageRenderText,
  sanitizeScopedCss,
  sanitizeStyleAttribute,
  extractBodyLoadUrl,
  buildRemoteResourceHtml,
  stableFrameNonce,
  demoteInlineHandlers,
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
  it('渲染保真:position/z-index/视口单位等布局声明放行(renderHtml 逐卡信任模型)', () => {
    // 角色卡界面(悬浮球/弹窗/全屏遮罩)依赖这些声明;整段剥掉会让卡片「只有文字没有界面」
    const css = '.x{position:fixed;z-index:999;inset:0;top:0;width:100vw;height:100vh;pointer-events:none;cursor:pointer;content:"x";color:red}';
    const safe = sanitizeScopedCss(css);
    expect(safe).toContain('position: fixed');
    expect(safe).toContain('z-index: 999');
    expect(safe).toContain('top: 0');
    expect(safe).toContain('width: 100vw');
    expect(safe).toContain('pointer-events: none');
    expect(safe).toContain('cursor: pointer');
    expect(safe).toContain('content: "x"');
    expect(safe).toContain('color: red');
  });

  it('仍剥脚本绑定/expression/javascript: 与非白名单 url()', () => {
    const safe = sanitizeScopedCss(
      '.a{behavior:url(x.htc);-moz-binding:url(x.xml#b);width:expression(alert(1));background:url(javascript:alert(1))}' +
      '.b{background:url(http://evil.test/x.png)}' + // http 混合内容不放行
      '.c{background:url(https://cdn.test/x.png);mask:url("data:image/svg+xml;base64,AA")}' +
      '.d{background:url(data:text/html;base64,PGh0bWw)}', // 非 image 的 data: 不放行
    );
    expect(safe).not.toMatch(/behavior|-moz-binding|expression|javascript:/i);
    expect(safe).not.toContain('http://evil.test');
    expect(safe).not.toContain('data:text/html');
    expect(safe).toContain('https://cdn.test/x.png');
    expect(safe).toContain('data:image/svg+xml');
  });

  it('拒绝外部资源和非白名单 at-rule', () => {
    const safe = sanitizeScopedCss('@import "x"; @font-face{x:y} @media(max-width:1px){.x{color:red}} @keyframes k{from{opacity:0}}');
    expect(safe).not.toMatch(/@import|@font-face|@media/i);
    expect(safe).toContain('@keyframes k');
  });
});

describe('CSS 注释处理(回归:注释吞掉紧随声明)', () => {
  it('注释写在声明之间时,两侧声明都保留(赛马娘卡 body 纸纹背景)', () => {
    // 作者卡常把说明写在声明之间;旧实现把注释文本并入下一条的属性名 → 整条声明被丢弃
    const css =
      'margin: 0; background-color: #fce8e6; /* 信纸质感 */ background-image: radial-gradient(circle at top left, rgba(255,255,255,0.8) 0%, transparent 100%); font-family: sans-serif;';
    const safe = sanitizeScopedCss(`body{${css}}`);
    expect(safe).toContain('background-color: #fce8e6');
    expect(safe).toContain('background-image: radial-gradient');
    expect(safe).toContain('font-family: sans-serif');
    expect(safe).not.toContain('/*');
  });

  it('注释在属性名前(.select-item 的 gap / .description-box 的 padding)', () => {
    const safe = sanitizeScopedCss(
      '.select-item{display:flex; /* 竖向排列更清爽 */ gap:8px}' +
        '.description-box{border-left:4px solid #f6a192; /* 温柔粉橘边框 */ padding:15px 20px}',
    );
    expect(safe).toContain('gap: 8px');
    expect(safe).toContain('padding: 15px 20px');
  });

  it('注释含花括号时不破坏顶层块切分', () => {
    const safe = sanitizeScopedCss('/* 说明 { 花括号 } */ .a{color:red} .b{color:blue}');
    expect(safe).toContain('.a { color: red }');
    expect(safe).toContain('.b { color: blue }');
  });

  it('引号内的 /* 不被当注释(合法 url 值)', () => {
    const safe = sanitizeScopedCss('.a{background:url("https://a/x/*.png")}');
    expect(safe).toContain('https://a/x/*.png');
  });

  it('无其他声明的纯注释规则被丢弃,不留空壳', () => {
    expect(sanitizeScopedCss('.a{/* 只有注释 */}')).toBe('');
  });

  it('剥注释后安全拦截不回退', () => {
    const safe = sanitizeScopedCss(
      '.a{ /* 说明 */ behavior:url(x.htc)}' +
        '.b{ /* 说明 */ width:expression(alert(1))}' +
        '.c{ /* 说明 */ background:url(javascript:alert(1))}' +
        '.d{ /* 说明 */ background:url(http://evil.test/x.png)}',
    );
    expect(safe).not.toMatch(/behavior|expression|javascript:|evil\.test/i);
  });

  it('style 属性清洗同样剥注释(sanitizeStyleAttribute 共用管线)', () => {
    const safe = sanitizeStyleAttribute('color: red; /* 说明 */ padding: 4px');
    expect(safe).toContain('color: red');
    expect(safe).toContain('padding: 4px');
    expect(safe).not.toContain('/*');
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
    // 真实事件处理器属性降级为 data-kd-on*(不在宿主文档执行);javascript: 链接 / 内联脚本清除
    expect(r.html).not.toMatch(/javascript:|<script/i);
    expect(r.html).not.toMatch(/\sonclick=/i);
    expect(r.html).toContain('data-kd-onclick=');
    // https 图片保留(WuWa 开场 Logo 等外链图依赖此能力)
    expect(r.html).toContain('https://evil.test/a.png');
    expect(r.scripts).toEqual(['steal()']);
  });

  it('危险样式能力被清理:https url 与 position 放行,@import 仍剥除', () => {
    const s = '<style>@import url(https://evil.test/a.css); .a{background:url(https://evil.test/x);position:fixed}</style><div class="a">正文</div>';
    const r = buildScopedScriptHtml(s, 'm3');
    expect(r.html).toContain('正文');
    expect(r.html).not.toMatch(/@import/i);
    // renderHtml 逐卡信任模型:https 外链图与定位声明保留(界面保真所需)
    expect(r.html).toContain('url(https://evil.test/x)');
    expect(r.html).toContain('position: fixed');
  });

  it('style 属性保留且值过声明级清洗(布局保留,危险声明剥除)', () => {
    const s = '<div style="position:fixed;top:10px;behavior:url(x.htc);background:url(http://evil.test/x)">样式文本</div>';
    const r = buildScopedScriptHtml(s, 'm4');
    expect(r.html).toContain('style=');
    // sanitize-html 会重新序列化 style 值(空格可能被规范化),按声明存在性断言
    expect(r.html).toMatch(/position:\s*fixed/);
    expect(r.html).toMatch(/top:\s*10px/);
    expect(r.html).not.toMatch(/behavior|http:\/\/evil\.test/i);
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
        // 显示渲染类脚本(非隐藏剥除类),必填字段补 false
        markdown_only: false,
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

  it('楼层深度过滤:min_depth 脚本对浅层消息不生效(wuwa 主开场空白回归)', () => {
    // wuwa 卡真实结构:「删除远楼层开场标记」(minDepth=2,空替换串)清理历史楼层的
    // 开场占位符;「鸣潮开场」把当前楼层的占位符渲染成创建界面。深度丢失会让删除
    // 脚本误删当前开场,渲染脚本无匹配 → 空白气泡。
    const scripts: RegexScript[] = [
      {
        id: '1',
        script_name: '删除远楼层开场标记',
        find_regex: '\\[角色创建与故事开场\\]',
        replace_string: '',
        markdown_only: true,
        enabled: true,
        min_depth: 2,
      },
      {
        id: '2',
        script_name: '鸣潮开场',
        find_regex: '\\[角色创建与故事开场\\]',
        replace_string: '<div class="wuwa-panel">创建界面</div>',
        markdown_only: true,
        enabled: true,
      },
    ];
    const MSG = '[角色创建与故事开场]';
    // 当前开场(depth 0/1):删除脚本不适用,渲染脚本命中出界面
    const d0 = renderScopedScripts(MSG, scripts, 'm1', 0);
    expect(d0).not.toBeNull();
    expect(d0!.html).toContain('创建界面');
    const d1 = renderScopedScripts(MSG, scripts, 'm1', 1);
    expect(d1).not.toBeNull();
    // 远楼层(depth >= 2):删除脚本生效,占位符清除后无命中返回 null
    expect(renderScopedScripts(MSG, scripts, 'm1', 2)).toBeNull();
    // max_depth:仅对近楼层生效
    const nearOnly: RegexScript[] = [
      { ...scripts[1], id: '3', max_depth: 1 },
    ];
    expect(renderScopedScripts(MSG, nearOnly, 'm1', 0)).not.toBeNull();
    expect(renderScopedScripts(MSG, nearOnly, 'm1', 2)).toBeNull();
    // 不传 depth:不过滤(兼容旧调用)
    expect(renderScopedScripts(MSG, nearOnly, 'm1')).not.toBeNull();
  });

  it('UpdateVariable 块先剥离再渲染', () => {
    const scripts: RegexScript[] = [
      {
        id: '1',
        script_name: 'x',
        find_regex: 'PH',
        replace_string: '<b>hit</b>',
        enabled: true,
        markdown_only: false,
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
      markdown_only: false,
    };
    const ui: RegexScript = {
      id: '9', script_name: 'ui',
      find_regex: '<StatusPlaceHolderImpl/>',
      replace_string: '<head><style>.ui-x{color:red}</style></head><body><div class="ui-x">手机界面</div><script>window.__ui = 1</script></body>',
      enabled: true,
      markdown_only: false,
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
      markdown_only: false,
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
  it('展开 $1 捕获组与 {{match}}/$0 全匹配(ST 语义)', () => {
    const args = ['12', '12', 0, '前文 12 后文'];
    expect(expandReplaceRefs('<b>$1</b>', args)).toBe('<b>12</b>');
    expect(expandReplaceRefs('{{match}}', args)).toBe('12');
    expect(expandReplaceRefs('[ $0 ]', args)).toBe('[ 12 ]');
  });

  it('不存在的捕获组展开为空串', () => {
    const args = ['x', 'x', 0, 'x'];
    expect(expandReplaceRefs('$2|$9', args)).toBe('|');
  });

  it('展开 $<name> 命名捕获组', () => {
    const args = ['ab', 'a', 'b', 0, 'ab', { first: 'a', second: 'b' }];
    expect(expandReplaceRefs('$<second>$<first>', args)).toBe('ba');
    expect(expandReplaceRefs('$<missing>', args)).toBe('');
  });

  it('$$、$&、$`、$\' 一律字面保留(ST 函数替换语义:非 $n/$<name> 不展开)', () => {
    // wuwa 卡 safeReplace 的模板字面量 `(.*)$\` 此前被 $` 展开吞掉反引号,整个脚本解析失败
    const args = ['12', '12', 2, 'ab12cd'];
    expect(expandReplaceRefs('$$', args)).toBe('$$');
    expect(expandReplaceRefs('$&', args)).toBe('$&');
    expect(expandReplaceRefs('$`', args)).toBe('$`');
    expect(expandReplaceRefs("$'", args)).toBe("$'");
    expect(expandReplaceRefs('new RegExp(`(^\\s*${k}\\s*:)(.*)$`, \'m\')', args)).toBe(
      'new RegExp(`(^\\s*${k}\\s*:)(.*)$`, \'m\')',
    );
  });
});

describe('expandDisplayMacros / 脚本替换串身份宏', () => {
  it('展开 {{user}}/{{char}} 及别名,大小写不敏感', () => {
    const out = expandDisplayMacros('{{user}}对{{CHAR}}说({{user_name}}/{{character_name}})', {
      charName: '守岸人',
      userName: '漂泊者',
    });
    expect(out).toBe('漂泊者对守岸人说(漂泊者/守岸人)');
  });

  it('userName 缺省为「用户」(与后端 content_display 展开一致)', () => {
    expect(expandDisplayMacros('{{user}}档案', { charName: '守岸人' })).toBe('用户档案');
  });

  it('renderScopedScripts:替换串注入的宏在渲染前展开(含 <script> 源码内)', () => {
    // wuwa 状态栏:替换串里 JS 源码含 name: '{{user}}'(sex-monitor),HTML 含 {{user}}档案
    const scripts: RegexScript[] = [
      {
        id: 'st',
        script_name: '[状态栏]MVU浪潮状态栏',
        find_regex: '/<StatusPlaceHolderImpl\\/>/',
        replace_string:
          '<div class="bar">{{user}}档案</div><script>const n = \'{{user}}\';</script>',
        markdown_only: false,
        enabled: true,
      },
    ];
    const out = renderScopedScripts('正文<StatusPlaceHolderImpl/>', scripts, 'm1', 0, {
      charName: '守岸人',
      userName: '漂泊者',
    });
    expect(out).not.toBeNull();
    expect(out!.html).toContain('漂泊者档案');
    expect(out!.html).not.toContain('{{user}}');
    // 脚本块源码内的 '{{user}}' 同样被展开(ST substituteParams 作用于脚本替换后全文)
    expect(out!.blocks[0].scripts.join('\n')).toContain("const n = '漂泊者';");
  });

  it('renderScopedScripts:macros 缺省时保持原样(兼容旧调用)', () => {
    const scripts: RegexScript[] = [
      {
        id: 'st',
        script_name: 'x',
        find_regex: '/<StatusPlaceHolderImpl\\/>/',
        replace_string: '<div>{{user}}档案</div>',
        markdown_only: false,
        enabled: true,
      },
    ];
    const out = renderScopedScripts('正文<StatusPlaceHolderImpl/>', scripts, 'm1', 0);
    expect(out!.html).toContain('{{user}}档案');
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
    expect(html).toContain('sandbox="allow-scripts allow-popups allow-forms allow-modals"');
    expect(html).toContain('referrerpolicy="no-referrer"');
    expect(html).toContain('在新窗口打开');
    // 宽屏切换按钮:作者页游戏界面为全屏设计,沙箱 iframe 的撑满样式进不了宿主
    // 文档,由宿主侧整卡 fixed 撑满补足(点击委托在 ChatWindow)
    expect(html).toContain('data-kd-resource-wide="1"');
    expect(html).not.toContain('data-kd-wide="1"');
    // 不直接用 src 直嵌(作者服务器可能禁止 iframe 嵌入)
    expect(html).not.toContain('src="https://files.yuzuki-rii.xyz');
  });

  it('nonce 按 (url+seed) 稳定派生:同一消息重渲染产出相同 HTML,iframe 不被重建', () => {
    // 回归:早期 nonce 用 Date.now+Math.random,消息 computed 每次重算都产出不同
    // iframe src → v-html 变化 → iframe 重建,资源页(吸血鬼卡 1.25MB)反复重新下载
    const url = 'https://files.yuzuki-rii.xyz/bby/v2.1.html';
    const a = buildRemoteResourceHtml(url, 'm42');
    const b = buildRemoteResourceHtml(url, 'm42');
    expect(a).toBe(b);
    // 不同消息(seed 不同)nonce 必须不同(ready/boot 双向认证按 nonce 隔离)
    const c = buildRemoteResourceHtml(url, 'm43');
    expect(c).not.toBe(a);
    expect(stableFrameNonce(`${url}m42`)).toBe(stableFrameNonce(`${url}m42`));
    expect(stableFrameNonce('a')).not.toBe(stableFrameNonce('b'));
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
    // 真实事件处理器属性不存在(降级为 data-kd-on*,宿主文档无 inline 执行)
    expect(r.html).not.toMatch(/\sonclick=|\sonchange=|\soninput=/i);
    expect(r.html).toContain('data-kd-onclick=');
  });

  it('事件处理器降级为 data-kd-on*(真实 on* 属性不存活),禁用/勾选状态保留', () => {
    const s = '<input type="checkbox" id="agree" onchange="send()" disabled><select onchange="go()"><option selected>选一</option></select>';
    const r = buildScopedScriptHtml(s, 'form2');
    expect(r.html).not.toMatch(/\sonchange=|\sonclick=|\soninput=/i);
    expect(r.html).toContain('data-kd-onchange=');
    expect(r.html).toContain('disabled');
    expect(r.html).toContain('selected');
  });
});

describe('scopeCss(body/html 根选择器重写)', () => {
  // wuwa 状态栏等作者 CSS 按整文档思维写 `body.theme-blue .x`(实测 35 处):
  // 直接加祖先前缀会变成 `[scope] body.x` 永不命中,状态栏大面积掉样式。
  // 作用域容器即「文档根」,前导 body/html 须重写为容器自身。
  it('body 开头的选择器重写为作用域容器(类链落到容器)', () => {
    const { css } = scopeCss('body.theme-blue .tide-card { color: red; }', 'sc1');
    expect(css).toContain('[data-kd-scope="sc1"].theme-blue .tide-card');
    expect(css).not.toContain(' body.theme-blue');
  });

  it('裸 body 与 html body 前缀映射为容器;body 后代/子代保持组合器', () => {
    const { css } = scopeCss('body { margin: 0; } html body > .x { top: 0; } body .y { left: 0; }', 'sc2');
    expect(css).toContain('[data-kd-scope="sc2"] {');
    expect(css).toContain('[data-kd-scope="sc2"] > .x');
    expect(css).toContain('[data-kd-scope="sc2"] .y');
  });

  it('非 body 选择器与普通单词(如 bodyshop)不受影响', () => {
    const { css } = scopeCss('.card { color: blue; } bodyshop { top: 1px; }', 'sc3');
    expect(css).toContain('[data-kd-scope="sc3"] .card');
    expect(css).toContain('[data-kd-scope="sc3"] bodyshop');
  });

  it('@media 内的 body 选择器同样重写', () => {
    const { css } = scopeCss('@media (max-width: 600px) { body.dark .p { color: #fff; } }', 'sc4');
    expect(css).toContain('[data-kd-scope="sc4"].dark .p');
  });
});

describe('demoteInlineHandlers(inline 事件降级桥)', () => {
  // wuwa 状态栏 72 处 onclick(switchTab 等):sanitizeVisibleHtml 会剥真实 on* 属性,
  // 降级为 data-kd-on* 后可存活,宿主绑监听转发沙箱求值(沙箱 CSP 含 unsafe-eval)。
  it('on* 属性改写为 data-kd-on*,单双引号与裸值均覆盖', () => {
    const html = demoteInlineHandlers(
      `<button onclick="switchTab('a',this)">x</button><div onmouseover='h()'>y</div><span oninput=g()>z</span>`,
    );
    expect(html).toContain(`data-kd-onclick="switchTab('a',this)"`);
    expect(html).toContain('data-kd-onmouseover="h()"');
    expect(html).toContain('data-kd-oninput="g()"');
    expect(html).not.toContain(' onclick=');
  });

  it('属性内的引号转义为 &quot;,避免截断属性串', () => {
    const html = demoteInlineHandlers(`<b onclick="x(&quot;v&quot;)">t</b>`);
    expect(html).toContain('data-kd-onclick="x(&quot;v&quot;)"');
  });

  it('文本中的 on 字样(非属性)不误伤', () => {
    const html = demoteInlineHandlers(`<p>click the button</p><div class="onion">t</div>`);
    expect(html).toContain('<div class="onion">');
    expect(html).not.toContain('data-kd-');
  });

  it('经 buildScopedScriptHtml 管线后 data-kd-on* 存活过白名单', () => {
    const { html } = buildScopedScriptHtml(
      '```html\n<div><button onclick="go(this)">go</button></div>\n```',
      'dm1',
    );
    expect(html).toContain('data-kd-onclick="go(this)"');
    expect(html).not.toContain(' onclick=');
  });

  it('赛马娘卡形态:select onchange / input oninput / button onclick 全部降级存活,尾部脚本被提取', () => {
    // 实景:该卡首楼 HTML 由 regex 替换串给出(含内联事件 + 尾部 <script> 定义
    // updateDictDesc/submitCreation)。此处锁定入口链:三类事件降级为 data-kd-on* 且
    // 过白名单存活,脚本正文被 extractScripts 收入 scripts[](供沙箱求值)。
    const cardHtml = [
      '```html',
      '<!DOCTYPE html><html><head><style>#scroll-container{position:relative}</style></head><body>',
      '<select id="identity-select" onchange="updateDictDesc(\'identity\', this.value)">',
      '<option value="">—</option><option value="trainer1">训练员</option></select>',
      '<input type="text" id="identity-custom-input" oninput="updateDictDesc(\'identity\', \'custom\')">',
      '<div class="description-box" id="identity-desc-box">欢迎</div>',
      '<button class="start-btn" onclick="submitCreation()">开始你的特雷森物语</button>',
      '<script>function submitCreation(){return getSelectedOrCustomText("identity-select","identity-custom-input");}</script>',
      '</body></html>',
      '```',
    ].join('\n');
    const scoped = buildScopedScriptHtml(cardHtml, 'uma1');
    expect(scoped.html).toContain('data-kd-onchange=');
    expect(scoped.html).toContain('data-kd-oninput=');
    expect(scoped.html).toContain('data-kd-onclick="submitCreation()"');
    // 真实 on* 属性一律不存活(宿主文档绝不执行卡内 inline 代码)
    expect(scoped.html).not.toMatch(/\son(change|input|click)=/);
    // 表单控件与 id 保留(脚本经 getElementById/#id 定位)
    expect(scoped.html).toContain('id="identity-select"');
    expect(scoped.html).toContain('id="identity-desc-box"');
    // 尾部脚本被提取进 scripts[](沙箱执行面)
    expect(scoped.scripts.join('\n')).toContain('function submitCreation()');
  });
});
