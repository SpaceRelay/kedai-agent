// 消息 HTML 渲染工具
// 流程:先在原始文本上把角色卡正则脚本(regex_scripts)命中的片段替换为唯一占位符,
//       再对整段做 HTML 转义(LLM 输出不可信,防注入),最后把占位符还原为脚本自带的
//       replaceString(作者可信 HTML)。
// 排版修复:v1 无脑剥离 <script> 且注入 <style> 为全局作用域,导致状态栏脚本不执行、
//           @keyframes 等命名污染整个页面。本版:
//           - 样式作用域化:每条规则加祖先前缀(等效 @scope 降级),keyframes 重命名
//           - 脚本受控执行:提取 <script> 源码(剥 import),由调用方在容器挂载后执行
//           - <UpdateVariable> 块由 mvu 解析器剥离,不进入渲染
import sanitizeHtml from 'sanitize-html';
import type { RegexScript } from './api';
import { parseUpdateVariable } from './mvu/parser';
import { renderMarkdown } from './markdown';

/** HTML 转义(防注入) */
export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/** JS 风格正则拆分为 (pattern, flags);非 /.../ 形式按普通字符串处理 */
export function splitJsRegex(input: string): { pattern: string; flags: string } | null {
  const t = input.trim();
  if (t.length >= 2 && t.startsWith('/')) {
    const m = /^\/([\s\S]*)\/([gimsuy]*)$/.exec(t);
    if (m) return { pattern: m[1], flags: m[2] };
  }
  return { pattern: t, flags: '' };
}

/**
 * 展开 replace_string 中的捕获组引用,与 String.prototype.replace 语义一致:
 * `$&` 匹配全文、`$1`..`$n` 捕获组、`$$` 字面量 $、`` $` `` 匹配前文本、`$'` 匹配后文本。
 * 函数式 replace 回调的返回值不会做 $ 展开,角色卡脚本 find:/...(\d+).../ replace:'<b>$1</b>'
 * 会原样输出 $1——这里补上标准展开。args 为回调原始参数(match, ...groups, offset, input)。
 */
export function expandReplaceRefs(replaceString: string, args: unknown[]): string {
  const match = args[0] as string;
  const offset = args[args.length - 2] as number;
  const input = args[args.length - 1] as string;
  // 捕获组个数 = 总参数 - 3(match + offset + input);$n 超出范围按空串处理(JS 原生语义)
  const groupCount = args.length - 3;
  return replaceString.replace(/\$\$|\$(\d+)|\$&|\$`|\$'/g, (m, num: string | undefined) => {
    if (num !== undefined) {
      const i = Number(num);
      if (i === 0) return match;
      return i <= groupCount ? ((args[i] as string | undefined) ?? '') : '';
    }
    switch (m) {
      case '$$':
        return '$';
      case '$&':
        return match;
      case '$`':
        return input.slice(0, offset);
      case "$'":
        return input.slice(offset + match.length);
      default:
        return m;
    }
  });
}

/** 提取 <script> 源码(剥掉 import 语句,避免加载外部依赖) */
export function extractScripts(s: string): string[] {
  const out: string[] = [];
  const re = /<script\b[^>]*>([\s\S]*?)<\/script\s*>/gi;
  let m: RegExpExecArray | null;
  while ((m = re.exec(s)) !== null) {
    let code = m[1].trim();
    if (!code) continue;
    // 剥 import '...'; 语句(单/双引号)
    code = code.replace(/\bimport\s+(['"][^'"]+['"])\s*;?\s*/g, '');
    // 剥 import { x } from '...' 与 import x from '...'
    code = code.replace(
      /\bimport\s+(?:\{[\s\S]*?\}\s*from\s*)?(?:[\w$]+\s*,\s*)?\{?[\s\S]*?\}?\s*from\s*['"][^'"]+['"]\s*;?/g,
      '',
    );
    // 剥动态 import('...')
    code = code.replace(/\bimport\s*\(\s*['"][^'"]+['"]\s*\)\s*;?/g, '');
    if (code.trim()) out.push(code.trim());
  }
  return out;
}

/** 提取并移除 <style> 内容 */
export function extractStyles(s: string): string[] {
  const out: string[] = [];
  const re = /<style\b[^>]*>([\s\S]*?)<\/style\s*>/gi;
  let m: RegExpExecArray | null;
  while ((m = re.exec(s)) !== null) {
    if (m[1].trim()) out.push(m[1].trim());
  }
  return out;
}

/**
 * 样式最小化:禁止外部资源、固定定位和可能覆盖应用的高危声明。
 * CSS 解析仍非完整沙箱，因此只保留无 URL/导入/表达式的局部展示规则。
 */
export function sanitizeScopedCss(css: string): string {
  const deniedProperties = new Set([
    'position', 'z-index', 'inset', 'inset-block', 'inset-inline', 'top', 'right', 'bottom', 'left',
    'pointer-events', 'behavior', 'binding', '-moz-binding', 'cursor', 'content',
  ]);
  const unsafeValue = /url\s*\(|expression\s*\(|@import|javascript:|(?:^|[^\w-])(?:-?\d*\.?\d+)(?:vw|vh|vmin|vmax)(?:[^\w-]|$)/i;
  const declarations = (input: string): string => splitCssDeclarations(input)
    .map((declaration) => {
      const colon = declaration.indexOf(':');
      if (colon <= 0) return '';
      const property = declaration.slice(0, colon).trim().toLowerCase();
      const value = declaration.slice(colon + 1).trim();
      if (!/^--[\w-]+$|^[a-z-]+$/.test(property)) return '';
      if (deniedProperties.has(property) || unsafeValue.test(value) || /<\/style/i.test(value)) return '';
      return `${property}: ${value}`;
    })
    .filter(Boolean)
    .join('; ');

  const cleanBlocks = (input: string, keyframe = false): string => splitCssBlocks(input)
    .map(({ head, inner, isAt }) => {
      const name = head.trim();
      if (isAt) {
        if (/^@(?:-webkit-)?keyframes\s+[\w-]+$/i.test(name)) {
          return `${name}{${cleanBlocks(inner, true)}}`;
        }
        return '';
      }
      const body = declarations(inner);
      if (!body) return '';
      if (keyframe && !/^(?:from|to|(?:\d{1,3}(?:\.\d+)?)%)(?:\s*,\s*(?:from|to|(?:\d{1,3}(?:\.\d+)?)%))*$/i.test(name)) return '';
      return `${name} { ${body} }`;
    })
    .filter(Boolean)
    .join('\n');

  return cleanBlocks(css.replace(/<\/style/gi, ''));
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
 * 可见 HTML 白名单:不允许 style 属性、事件处理器、外部媒体或 javascript URL。
 * 链接与图片仅放行 https(与资源界面一致):角色卡作者 HTML 中的外链图(如
 * WuWa 开场界面的 Logo)与站外链接需要保留;data: 仍允许(状态栏内联图)。
 * 表单控件(input/select/textarea/option)放行:角色卡开场界面(如 WuWa 开场
 * 身份/版本/区域选择)依赖表单交互;disabled/checked/selected/value 等状态
 * 属性保留,事件处理器一律剥离(交互经 jQuery 子集绑定)。
 */
export function sanitizeVisibleHtml(html: string): string {
  return sanitizeHtml(html, {
    allowedTags: [
      'div', 'span', 'p', 'br', 'hr', 'strong', 'em', 'b', 'i', 'u', 's', 'small',
      'ul', 'ol', 'li', 'dl', 'dt', 'dd', 'table', 'thead', 'tbody', 'tfoot', 'tr', 'th', 'td',
      'details', 'summary', 'blockquote', 'code', 'pre', 'a', 'img', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6',
      // button 保留:状态栏脚本的标签页切换依赖 <button>(事件经 jQuery 子集绑定,属性被清洗)
      'button',
      // 表单控件:开场界面(身份/版本/区域选择、协议勾选)依赖输入交互
      'form', 'label', 'input', 'select', 'option', 'optgroup', 'textarea', 'fieldset', 'legend',
    ],
    allowedAttributes: {
      // id 必须保留:状态栏脚本(jQuery 子集)用 #id 选择器定位元素,剥离后脚本静默空转
      '*': ['class', 'title', 'aria-label', 'role', 'data-*', 'id'],
      a: ['href', 'title', 'target', 'rel', 'class', 'aria-label'],
      img: ['src', 'alt', 'title', 'class', 'width', 'height'],
      th: ['colspan', 'rowspan', 'scope', 'class'],
      td: ['colspan', 'rowspan', 'class'],
      form: ['action', 'method', 'class', 'id'],
      label: ['for', 'class', 'id'],
      input: ['type', 'name', 'value', 'placeholder', 'checked', 'disabled', 'min', 'max', 'maxlength', 'size', 'class', 'id'],
      select: ['name', 'multiple', 'disabled', 'size', 'class', 'id'],
      option: ['value', 'selected', 'disabled', 'class', 'id'],
      optgroup: ['label', 'disabled', 'class', 'id'],
      textarea: ['name', 'rows', 'cols', 'placeholder', 'disabled', 'maxlength', 'class', 'id'],
      fieldset: ['disabled', 'class', 'id'],
    },
    allowedSchemes: ['data', 'https'],
    allowedSchemesByTag: { img: ['data', 'https'] },
    allowProtocolRelative: false,
    transformTags: {
      a: (_tagName, attribs) => ({
        tagName: 'a',
        attribs: { ...attribs, target: '_blank', rel: 'noopener noreferrer' },
      }),
    },
    disallowedTagsMode: 'discard',
  });
}

/** 移除 head/script/style 骨架,仅保留 body 内部可见内容 */
function extractBody(s: string): string {
  let t = s;
  const headMatch = /<head\b[\s\S]*?<\/head>/i.exec(t);
  if (headMatch) {
    const bodyMatch = /<body\b[^>]*>([\s\S]*?)<\/body>/i.exec(t);
    t = bodyMatch ? bodyMatch[1] : t.slice(headMatch[0].length);
  }
  // 兜底:无 head 时剥掉残余 script/style 骨架
  t = t.replace(/<script\b[\s\S]*?<\/script\s*>/gi, '');
  t = t.replace(/<style\b[\s\S]*?<\/style\s*>/gi, '');
  return t.trim();
}

/**
 * CSS 作用域化:每条规则加祖先前缀 `[data-kd-scope="scopeId"]`。
 * - 普通规则:选择器加前缀(已是前缀则跳过)
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
        .map((s) => (s.trim() ? (s.trim().startsWith(attr) ? s.trim() : `${attr} ${s.trim()}`) : s))
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

export interface ScopedScriptHtml {
  /** 可渲染 HTML(含作用域化 style 内联 + body 内容) */
  html: string;
  /** 需执行的脚本源码(已剥 import) */
  scripts: string[];
  /** 容器唯一 scopeId */
  scopeId: string;
}

let scopeSeq = 0;

/**
 * 把脚本的 replaceString 转成作用域化 HTML + 待执行脚本。
 * 生成结构:
 *   <div data-kd-scope="{scopeId}">
 *     <style>{作用域化样式}</style>
 *     {body 可见内容}
 *   </div>
 * 脚本调用方在容器挂载后执行(先 setMvuHostContext,再 flushJqReady)。
 * seed 可选:传入后 scopeId 稳定为 `k{seed}`(消息渲染复用同一容器,避免脚本重复执行)。
 */
export function buildScopedScriptHtml(replaceString: string, seed?: string): ScopedScriptHtml {
  let s = replaceString;
  // 1) 剥 markdown 代码围栏
  const fence = /^```(?:html|html\.handlebars)?\s*\r?\n?/i;
  const fenceEnd = /\r?\n?```\s*$/;
  if (fence.test(s)) s = s.replace(fence, '');
  if (fenceEnd.test(s)) s = s.replace(fenceEnd, '');

  const styles = extractStyles(s);
  const scripts = extractScripts(s);
  const body = sanitizeVisibleHtml(extractBody(s));
  // 替换值经清洗后无可见内容且无脚本(如「状态栏代码块」把占位符包成围栏,
  // 围栏内标签被白名单丢弃)时返回空,调用方跳过注入,避免消息尾部出现空容器
  if (!body.trim() && scripts.length === 0) {
    return { html: '', scripts, scopeId: '' };
  }
  const scopeId = seed ? `k${seed}` : `k${Date.now().toString(36)}${(scopeSeq++).toString(36)}`;

  let cssText = '';
  for (const css of styles) {
    const scoped = scopeCss(sanitizeScopedCss(css), scopeId);
    cssText += applyKeyframeRename(scoped.css, scoped.keyframes) + '\n';
  }

  const html = `<div data-kd-scope="${scopeId}">${cssText ? `<style>${cssText}</style>` : ''}${body}</div>`;
  return { html, scripts, scopeId };
}

/**
 * 对消息文本应用角色卡正则脚本,返回可渲染 HTML。
 * 兼容旧签名(纯 HTML,不含脚本执行)——ChatWindow 改用 buildScopedScriptHtml 以获得脚本。
 * 1) 原文上按脚本链顺序把命中片段替换为占位符(控制字符,不受转义影响)
 * 2) 整体 HTML 转义(LLM 输出不可信)
 * 3) 占位符还原为脚本 replaceString(作者可信,清洗后保留 HTML)
 * 无效正则 / 空替换串 / 禁用的脚本跳过。
 */
export function renderScriptedHtml(text: string, scripts: RegexScript[]): string {
  // 第 1 步:原文占位符化
  const placeholders: Array<{ ph: string; value: string }> = [];
  let source = text;
  for (const script of scripts) {
    if (!script.enabled) continue;
    if (!script.replace_string) continue;
    const parsed = splitJsRegex(script.find_regex);
    if (!parsed || !parsed.pattern) continue;
    let re: RegExp;
    try {
      re = new RegExp(parsed.pattern, parsed.flags.includes('g') ? parsed.flags : parsed.flags + 'g');
    } catch {
      continue; // 无效正则跳过
    }
    source = source.replace(re, (...args: unknown[]) => {
      const ph = `\u0000KDAI_PH_${placeholders.length}\u0000`;
      placeholders.push({ ph, value: cleanScriptHtml(expandReplaceRefs(script.replace_string, args)) });
      return ph;
    });
  }
  if (placeholders.length === 0) {
    // 无脚本命中:直接转义输出(纯文本)
    return escapeHtml(text);
  }
  // 第 2 步:整体转义(占位符为控制字符,不受影响)
  let html = escapeHtml(source);
  // 第 3 步:还原占位符(不二次转义)
  for (const { ph, value } of placeholders) {
    html = html.split(ph).join(value);
  }
  return html;
}

export interface ScopedRenderResult {
  /** 完整可 v-html 的 HTML(含全部作用域容器) */
  html: string;
  /** 待执行脚本块:scopeId 对应容器,scripts 为源码 */
  blocks: Array<{ scopeId: string; scripts: string[] }>;
}

/**
 * 状态栏「渲染脚本」判定:启用、有替换串、正则应 <StatusPlaceHolderImpl/>,
 * 且替换串里不含占位符本身(不含 = 直接把占位符渲染成界面;含 = 只是把占位符
 * 包回代码围栏的「包裹脚本」,如碧蓝卡「状态栏代码块」,不应抢在渲染脚本前)。
 * 渲染脚本必须排在包裹脚本之前执行,否则占位符先被包裹脚本吞掉,渲染脚本
 * 永远匹配不到(碧蓝卡「ui」手机界面空白)。
 */
function isStatusRenderScript(s: RegexScript): boolean {
  return (
    s.enabled &&
    !!s.replace_string &&
    /StatusPlaceHolderImpl/i.test(s.find_regex) &&
    !/StatusPlaceHolderImpl/i.test(s.replace_string)
  );
}

/**
 * 应用脚本后返回渲染结果:完整 html + 待执行脚本块。
 * 未命中任何脚本返回 null(由调用方走 markdown 渲染)。
 * seed 可选:稳定 scopeId(同消息重渲染复用同一容器,避免脚本重复执行)。
 * ChatWindow 一次性 v-html html;DOM 挂载后逐块 setMvuHostContext 并执行 scripts。
 */
export function renderScopedScripts(
  text: string,
  scripts: RegexScript[],
  seed?: string,
): ScopedRenderResult | null {
  // 先剥离 mvu <UpdateVariable> 块(仅用于变量更新,不进入任何渲染)
  const { cleaned } = parseUpdateVariable(text);
  let source = cleaned;
  // 1) 隐藏类脚本(空替换串 + markdown_only)先剥除命中片段:<thinking>/剧情/信息/摘要等
  //    块不显示给用户,只保留开场剧情。仅处理 markdown_only=true 的脚本——markdown_only=false
  //    的「对AI隐藏状态栏」等仅注入提示词,显示层剥掉会把渲染脚本要用的
  //    <StatusPlaceHolderImpl/> 占位符一并移除,导致界面不渲染。
  for (const script of scripts) {
    if (!script.enabled) continue;
    if (script.replace_string && script.replace_string.trim()) continue; // 只处理隐藏类
    if (!script.markdown_only) continue; // 显示层只应用 markdown_only 脚本
    const parsed = splitJsRegex(script.find_regex);
    if (!parsed || !parsed.pattern) continue;
    let re: RegExp;
    try {
      re = new RegExp(parsed.pattern, parsed.flags.includes('g') ? parsed.flags : parsed.flags + 'g');
    } catch {
      continue;
    }
    source = source.replace(re, '');
  }
  // 2) 渲染脚本(非空替换):渲染脚本优先,避免被包裹脚本抢占
  //   (渲染脚本把 <StatusPlaceHolderImpl/> 渲染成界面;包裹脚本只把它包回围栏)
  const ordered = [...scripts].sort(
    (a, b) => Number(isStatusRenderScript(b)) - Number(isStatusRenderScript(a)),
  );
  const placeholders: Array<{ ph: string; value: string }> = [];
  for (const script of ordered) {
    if (!script.enabled) continue;
    if (!script.replace_string) continue;
    const parsed = splitJsRegex(script.find_regex);
    if (!parsed || !parsed.pattern) continue;
    let re: RegExp;
    try {
      re = new RegExp(parsed.pattern, parsed.flags.includes('g') ? parsed.flags : parsed.flags + 'g');
    } catch {
      continue;
    }
    source = source.replace(re, (...args: unknown[]) => {
      const ph = `\u0000KDAI_PH_${placeholders.length}\u0000`;
      placeholders.push({ ph, value: expandReplaceRefs(script.replace_string, args) });
      return ph;
    });
  }
  if (placeholders.length === 0) return null;
  // 只构建一次,保证 html 容器 scopeId 与 blocks 一致
  const built = placeholders.map((p, i) => ({
    index: i,
    scoped: buildScopedScriptHtml(p.value, seed ? `${seed}-${i}` : undefined),
  }));
  // 过滤空容器:替换值经清洗后为空(如包裹脚本把占位符包成围栏后,围栏内标签被
  // 白名单丢弃)时不再注入,避免消息尾部出现空 div;也不产生脚本块
  const valid = built.filter((b) => b.scoped.html.trim().length > 0);
  const blocks: Array<{ scopeId: string; scripts: string[] }> = valid.map((b) => ({
    scopeId: b.scoped.scopeId,
    scripts: b.scoped.scripts,
  }));
  for (const b of valid) {
    source = source.replace(placeholders[b.index].ph, `\u0000KDAI_BLOCK_${b.index}\u0000`);
  }
  // 正文段(非脚本)走 markdown 渲染,保留换行;脚本占位符按段拆分后独立注入 scoped 容器
  const segments = source.split(/(\u0000KDAI_BLOCK_\d+\u0000)/g);
  let html = '';
  for (const seg of segments) {
    const m = /^\u0000KDAI_BLOCK_(\d+)\u0000$/.exec(seg);
    if (m) {
      const item = built[Number(m[1])];
      if (item && item.scoped.html.trim().length > 0) html += item.scoped.html;
    } else if (seg) {
      html += renderMdSegment(seg);
    }
  }
  return { html, blocks };
}

/** 正文段渲染:markdown 渲染,保留换行段落;剥离残留脚本占位符 */
function renderMdSegment(seg: string): string {
  let s = seg;
  // 占位符若残留(替换异常)则静默丢弃,避免控制字符进入页面
  s = s.replace(/\u0000/g, '');
  if (!s.trim()) return '';
  return renderMarkdown(s);
}

/** 兼容旧调用:清洗脚本 HTML(不含 script,样式作用域化由 buildScopedScriptHtml 负责) */
export function cleanScriptHtml(replaceString: string): string {
  return buildScopedScriptHtml(replaceString).html;
}

/**
 * 角色卡脚本中是否存在把 <StatusPlaceHolderImpl/> 渲染为 HTML 状态栏的脚本
 * (启用、有替换串、正则应占位符)。存在时状态栏文本可借此渲染为 HTML 卡片。
 */
export function hasStatusPlaceholderScript(scripts: RegexScript[]): boolean {
  return scripts.some((s) => s.enabled && !!s.replace_string && /StatusPlaceHolderImpl/i.test(s.find_regex));
}

/**
 * 组装消息渲染文本:带状态栏且角色卡存在 HTML 状态栏脚本时,在正文后追加占位符,
 * 使状态栏被「状态栏」正则脚本识别渲染为 HTML 卡片(脚本用变量树填充动态数据);
 * 否则原样返回正文(状态栏走纯文本气泡 extra.status_bar)。
 * 正文已含 <StatusPlaceHolderImpl/>(如备用开场自带界面占位符)时不再追加,避免双卡片重复渲染。
 */
export function buildMessageRenderText(
  text: string,
  statusBarText: string | null | undefined,
  hasStatusScript: boolean,
): string {
  if (statusBarText && hasStatusScript && !/<StatusPlaceHolderImpl/i.test(text)) {
    return `${text}\n<StatusPlaceHolderImpl/>`;
  }
  return text;
}

/**
 * 应用「隐藏类」脚本(replace_string 为空的脚本,如「对 AI 隐藏状态栏」):
 * 把命中片段替换为空,用于 HTML 渲染关闭时剥掉 `<StatusPlaceHolderImpl/>` 等占位符,
 * 避免其裸露在 markdown 正文中。
 */
export function stripHiddenPlaceholders(text: string, scripts: RegexScript[]): string {
  let out = text;
  for (const script of scripts) {
    if (!script.enabled) continue;
    if (script.replace_string && script.replace_string.trim()) continue; // 只处理隐藏类
    const parsed = splitJsRegex(script.find_regex);
    if (!parsed || !parsed.pattern) continue;
    let re: RegExp;
    try {
      re = new RegExp(parsed.pattern, parsed.flags.includes('g') ? parsed.flags : parsed.flags + 'g');
    } catch {
      continue;
    }
    out = out.replace(re, '');
  }
  return out;
}

/**
 * 从消息文本提取「远程资源界面」加载目标。
 * 兼容酒馆角色卡常见开场模式 —— `$('body').load('https://…')` /
 * `$("body").load("https://…")`(作者把资源下载界面挂到开场消息):
 * 原版酒馆助手直接注入 body 执行,kedai 改为沙箱 iframe 隔离加载。
 * 仅接受 https URL(拒绝 http/javascript:/data: 等,防本地读取与注入)。
 * 返回 URL;未命中返回 null。
 */
export function extractBodyLoadUrl(text: string): string | null {
  if (!text) return null;
  // $('body').load('URL') / $("body").load("URL") / body.load(...) / #id.load(...):
  // 兼容 jQuery 选择器写法与引号变体,URL 一律经 isSafeResourceUrl(https) 把关
  const re = /\.load\s*\(\s*['"]([^'"]+)['"]\s*\)/i;
  const m = re.exec(text);
  if (!m) return null;
  const url = m[1].trim();
  return isSafeResourceUrl(url) ? url : null;
}

/** 资源界面 URL 白名单:仅 https,拒绝本地/数据/脚本协议 */
export function isSafeResourceUrl(url: string): boolean {
  try {
    const u = new URL(url);
    return u.protocol === 'https:' && !u.username && !u.password;
  } catch {
    return false;
  }
}

/**
 * 构建「远程资源界面」卡片 HTML(下载资源界面)。
 *
 * 加载流程(绕开作者服务器的 X-Frame-Options / CSP frame-ancestors 与父页面
 * CSP 对内联脚本的拦截):
 *   1) 父页面(带 API token)经后端 /api/resource/proxy 取作者页面 HTML
 *      (后端 https + SSRF 防护 + 禁重定向;服务端 fetch 不受 XFO 限制);
 *   2) iframe 用 src 加载独立宿主文档 /resource-frame.html(不继承父 CSP,
 *      允许作者页面脚本执行,见后端 resource_frame_headers);父页面把抓取的
 *      HTML 通过 postMessage 投递,宿主文档用 DOM 重建(appendChild)触发脚本
 *      执行——srcdoc/blob 都会继承父 CSP、document.write 会丢 module script,
 *      独立文档 + appendChild 是已验证可行的组合;
 *   3) iframe sandbox="allow-scripts" 无 allow-same-origin → opaque origin,
 *      作者页面脚本可运行(界面功能所需),但拿不到父页面 DOM/localStorage/token。
 * 本函数只生成卡片壳(标题栏 + iframe + 说明),投递由调用方 hydrate 完成。
 * seed 用于生成稳定容器 id(同一消息重渲染复用,避免 iframe 重复创建)。
 */
export function buildRemoteResourceHtml(url: string, seed?: string): string {
  const id = seed ? `sv-res-${seed}` : `sv-res-${Date.now().toString(36)}${(scopeSeq++).toString(36)}`;
  const escUrl = escapeHtml(url);
  const urlAttr = encodeURIComponent(url);
  // 随机 nonce 放 URL fragment(不随 HTTP 请求发送),宿主文档 ready/boot 双向认证
  const nonce = `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`;
  return (
    `<div class="sv-resource-card" id="${escapeHtml(id)}" data-kd-resource="1" data-kd-resource-url="${escapeHtml(urlAttr)}" data-kd-resource-nonce="${escapeHtml(nonce)}">` +
    `<div class="sv-resource-card-head">` +
    `<span class="sv-resource-card-title">角色卡资源界面</span>` +
    `<a class="sv-resource-card-open" href="${escUrl}" target="_blank" rel="noopener noreferrer">在新窗口打开</a>` +
    `</div>` +
    `<iframe class="sv-resource-card-frame" data-kd-resource-frame="1" sandbox="allow-scripts allow-popups allow-forms" ` +
    `referrerpolicy="no-referrer" src="/resource-frame.html#${escapeHtml(nonce)}" title="角色卡资源界面"></iframe>` +
    `<div class="sv-resource-card-note">资源页面由角色卡作者提供,已隔离加载;若未显示,请使用上方链接在新窗口打开。</div>` +
    `</div>`
  );
}
