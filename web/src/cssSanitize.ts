// cssSanitize.ts — CSS 声明级清洗与容器作用域化(从 render.ts 抽出,供静态渲染管道
// 与沙箱运行期 <style> 通道共用;sandbox/sanitize.ts 的 sanitizeScriptHtmlWithStyles 依赖本模块)
//
// 管线分工:sanitizeScopedCss 做声明级清洗(剥 expression()/@import/危险 url),
// scopeCss 做作用域化(选择器加 [data-kd-scope] 祖先前缀、keyframes 重命名),
// applyKeyframeRename 把 keyframes 映射回写到引用处(animation/animation-name)。

/**
 * 样式清洗(renderHtml 逐卡信任模型下的保真版)。
 *
 * renderHtml 是用户逐卡主动开启的渲染开关(与 JS 授权同属信任模型),角色卡界面
 * (悬浮球/弹窗/全屏遮罩/状态栏定位)依赖 position/z-index/url() 等声明,过度清洗
 * 会让卡片「只有文字没有界面」(wuwa 卡 bug)。放行:position/z-index/inset/四向
 * 偏移/pointer-events/cursor/content、vw/vh 视口单位、https 与 data:image 的 url()。
 * 仍禁:behavior/binding(IE 脚本绑定)、expression()、@import、javascript:,
 * 以及非白名单的 url()(http 混合内容、data: 文档、file: 本地读取)。
 */
export function sanitizeScopedCss(css: string): string {
  const cleanBlocks = (input: string, keyframe = false): string => splitCssBlocks(input)
    .map(({ head, inner, isAt }) => {
      const name = head.trim();
      if (isAt) {
        if (/^@(?:-webkit-)?keyframes\s+[\w-]+$/i.test(name)) {
          return `${name}{${cleanBlocks(inner, true)}}`;
        }
        return '';
      }
      const body = sanitizeCssDeclarations(inner);
      if (!body) return '';
      if (keyframe && !/^(?:from|to|(?:\d{1,3}(?:\.\d+)?)%)(?:\s*,\s*(?:from|to|(?:\d{1,3}(?:\.\d+)?)%))*$/i.test(name)) return '';
      return `${name} { ${body} }`;
    })
    .filter(Boolean)
    .join('\n');

  // 注释必须先剥:注释内若含 {} 会破坏 splitCssBlocks 的顶层块切分
  return cleanBlocks(stripCssComments(css).replace(/<\/style/gi, ''));
}

/**
 * 剥离 CSS 注释(引号感知)。作者卡常把说明写在声明之间:
 *   `background-color: #fce8e6;\n/* 信纸质感 *\/\nbackground-image: radial-gradient(…)`
 * 注释文本会被并入下一条声明的属性名,导致该声明整条被清洗丢弃(赛马娘卡纸纹背景
 * 与 .select-item 的 gap 就是这样丢的)。这里把注释替换为一个空格再走声明切分。
 *
 * 引号内的 `/*` 不清:url("https://a/*.png") 这类合法值不能被误伤;反斜杠转义按
 * CSS 语义跳过下一个字符,避免 `content: "\\"` 这类写法提前结束引号态。
 */
export function stripCssComments(css: string): string {
  if (!css.includes('/*')) return css;
  let out = '';
  let quote: string | null = null;
  for (let i = 0; i < css.length; i++) {
    const ch = css[i];
    if (quote) {
      out += ch;
      if (ch === '\\') {
        if (i + 1 < css.length) out += css[++i];
      } else if (ch === quote) {
        quote = null;
      }
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      out += ch;
      continue;
    }
    if (ch === '/' && css[i + 1] === '*') {
      const end = css.indexOf('*/', i + 2);
      // 未闭合注释:按 CSS 容错语义吞到末尾
      i = end === -1 ? css.length : end + 1;
      out += ' ';
      continue;
    }
    out += ch;
  }
  return out;
}

/** url() 逐条白名单:仅 https 与 data:image(外链图/内联图);无 url() 直接放行 */
export function cssUrlsSafe(value: string): boolean {
  const re = /url\s*\(\s*(['"]?)([^'")]*)\1\s*\)/gi;
  let m: RegExpExecArray | null;
  while ((m = re.exec(value)) !== null) {
    const u = m[2].trim();
    if (!/^https:\/\//i.test(u) && !/^data:image\//i.test(u)) return false;
  }
  return true;
}

/**
 * CSS 声明清洗(规则体与 style 属性共用):
 * 先剥注释(否则注释会被并入下一条声明的属性名,整条声明被丢弃),
 * 再剥 behavior/binding、expression()/@import/javascript:、非白名单 url();
 * 属性名限小写字母/连字符或 -- 自定义属性。
 */
export function sanitizeCssDeclarations(input: string): string {
  const deniedProperties = new Set(['behavior', 'binding', '-moz-binding']);
  const unsafeValue = /expression\s*\(|@import|javascript:/i;
  return splitCssDeclarations(stripCssComments(input))
    .map((declaration) => {
      const colon = declaration.indexOf(':');
      if (colon <= 0) return '';
      const property = declaration.slice(0, colon).trim().toLowerCase();
      const value = declaration.slice(colon + 1).trim();
      if (!/^--[\w-]+$|^[a-z-]+$/.test(property)) return '';
      if (deniedProperties.has(property) || unsafeValue.test(value) || /<\/style/i.test(value)) return '';
      if (!cssUrlsSafe(value)) return '';
      return `${property}: ${value}`;
    })
    .filter(Boolean)
    .join('; ');
}

/** style 属性清洗(声明级,与规则体同一管线;供 sanitizeVisibleHtml 的 transformTags 用) */
export function sanitizeStyleAttribute(style: string): string {
  return sanitizeCssDeclarations(style);
}

function splitCssDeclarations(input: string): string[] {
  const declarations: string[] = [];
  let start = 0;
  let quote: string | null = null;
  let parentheses = 0;
  for (let index = 0; index <= input.length; index++) {
    const char = input[index];
    if (quote) {
      if (char === '\\') index++;
      else if (char === quote) quote = null;
    } else if (char === '"' || char === "'") quote = char;
    else if (char === '(') parentheses++;
    else if (char === ')') parentheses = Math.max(0, parentheses - 1);
    else if ((char === ';' || index === input.length) && parentheses === 0) {
      declarations.push(input.slice(start, index).trim());
      start = index + 1;
    }
  }
  return declarations;
}

/**
 * 单条选择器作用域化。作者 CSS 按整文档思维常以前导 body/html 为根(如
 * `body.theme-blue .tide-card`,wuwa 状态栏 35 处)——作用域容器就是「文档根」:
 * 前导 body/html 重写为容器自身,紧邻 body 的类/伪类/属性选择器落到容器上
 * (与沙箱 $('body') → 容器的特判对称;$('body').addClass('theme-blue') 因而生效)。
 */
function scopeSelector(s: string, attr: string): string {
  if (!s || s.startsWith(attr)) return s;
  const m = /^(?:html(?![\w-])\s+)?body(?![\w-])((?:[.:#][\w-]+(?:\([^)]*\))?|\[[^\]]*\])*)([\s\S]*)$/.exec(s);
  if (m) return `${attr}${m[1] ?? ''}${m[2] ?? ''}`;
  return `${attr} ${s}`;
}

/**
 * CSS 作用域化:每条规则加祖先前缀 `[data-kd-scope="scopeId"]`。
 * - 普通规则:选择器加前缀(已是前缀则跳过;前导 body/html 重写为容器,见 scopeSelector)
 * - @media/@supports:内部规则递归加前缀
 * - @keyframes:名字重命名(前缀 + 原哈希),并记录映射供引用替换
 * - @font-face/@import 等:原样保留
 */
export function scopeCss(css: string, scopeId: string): { css: string; keyframes: Map<string, string> } {
  const attr = `[data-kd-scope="${scopeId}"]`;
  const kfMap = new Map<string, string>();
  const out: string[] = [];

  // 按顶层块切分:selector{...} / @rule{...} / 顶层声明
  const blocks = splitCssBlocks(css);
  for (const block of blocks) {
    const { head, inner, isAt } = block;
    if (isAt) {
      const name = head.trim();
      if (/^@(media|supports|container|layer)\b/i.test(name)) {
        const sub = scopeCss(inner, scopeId);
        sub.keyframes.forEach((v, k) => kfMap.set(k, v));
        out.push(`${name}{${sub.css}}`);
      } else if (/^@keyframes\b/i.test(name)) {
        // 重命名 keyframes 名字
        const orig = name.replace(/^@keyframes\s+/i, '').trim();
        const renamed = `kd-${scopeId}-${orig}`;
        kfMap.set(orig, renamed);
        out.push(`@keyframes ${renamed}{${inner}}`);
      } else {
        // @font-face / @import / @page / @charset 等原样
        out.push(inner ? `${name}{${inner}}` : name);
      }
    } else {
      const sel = head.trim();
      if (!sel) continue;
      const scopedSel = sel
        .split(',')
        .map((s) => scopeSelector(s.trim(), attr))
        .join(', ');
      out.push(`${scopedSel} {${inner}}`);
    }
  }
  return { css: out.join('\n'), keyframes: kfMap };
}

/** 应用 keyframes 重命名到 CSS 文本(animation/animation-name 中的名字)。
 *  负向断言排除已带前缀的 keyframes 定义名,避免双重前缀。 */
export function applyKeyframeRename(css: string, map: Map<string, string>): string {
  if (map.size === 0) return css;
  let s = css;
  for (const [orig, renamed] of map) {
    const re = new RegExp(`(?<![\\w-])${escapeRegex(orig)}(?![\\w-])`, 'g');
    s = s.replace(re, renamed);
  }
  return s;
}

function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** 切分 CSS 为顶层块:[{head, inner, isAt}] */
function splitCssBlocks(css: string): Array<{ head: string; inner: string; isAt: boolean }> {
  const blocks: Array<{ head: string; inner: string; isAt: boolean }> = [];
  let i = 0;
  const n = css.length;
  while (i < n) {
    // 跳过注释与空白
    if (css.startsWith('/*', i)) {
      const end = css.indexOf('*/', i + 2);
      i = end < 0 ? n : end + 2;
      continue;
    }
    if (/\s/.test(css[i])) {
      i++;
      continue;
    }
    // 收集 head(直到 '{' 或 ';',跳过引号内的分号)
    let headStart = i;
    let j = i;
    let headQuote: string | null = null;
    while (j < n) {
      const ch = css[j];
      if (headQuote) {
        if (ch === headQuote) headQuote = null;
      } else if (ch === '"' || ch === "'") {
        headQuote = ch;
      } else if (ch === '{' || ch === ';') {
        break;
      }
      j++;
    }
    if (j >= n) break;
    if (css[j] === ';') {
      // 顶层声明(如 @import 分号结尾)或空规则,原样保留
      blocks.push({ head: css.slice(headStart, j + 1), inner: '', isAt: css[headStart] === '@' });
      i = j + 1;
      continue;
    }
    // 有 '{':扫描匹配的 '}'(注意字符串)
    let depth = 1;
    let k = j + 1;
    let quote: string | null = null;
    while (k < n && depth > 0) {
      const ch = css[k];
      if (quote) {
        if (ch === quote) quote = null;
      } else if (ch === '"' || ch === "'") {
        quote = ch;
      } else if (ch === '{') {
        depth++;
      } else if (ch === '}') {
        depth--;
      }
      k++;
    }
    blocks.push({
      head: css.slice(headStart, j),
      inner: css.slice(j + 1, depth === 0 ? k - 1 : k),
      isAt: css[headStart] === '@',
    });
    i = depth === 0 ? k : k + 1;
  }
  return blocks;
}
