// sanitize.ts — 脚本注入 HTML 的白名单清洗配置(setter/append/游离元素落 DOM 共用)
import sanitizeHtml from 'sanitize-html';
import { applyKeyframeRename, sanitizeScopedCss, scopeCss } from '../cssSanitize';

/** 脚本注入 HTML 的白名单(setter/append 共用):含表格/图片/表单控件(角色卡界面结构所需);
 *  img/a 仅 https 与 data 协议,事件处理器仍剥离(脚本交互走 on() 绑定) */
const SCRIPT_HTML_WHITELIST: sanitizeHtml.IOptions = {
  allowedTags: [
    'span', 'b', 'strong', 'i', 'em', 'small', 'br', 'div', 'p',
    'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'ul', 'ol', 'li', 'dl', 'dt', 'dd',
    'table', 'thead', 'tbody', 'tfoot', 'tr', 'th', 'td',
    'img', 'a', 'button', 'label', 'select', 'option', 'optgroup', 'input', 'textarea',
    'details', 'summary', 'blockquote', 'code', 'pre', 'hr',
  ],
  allowedAttributes: {
    '*': ['class', 'aria-label', 'style', 'id', 'data-*', 'title', 'role'],
    th: ['colspan', 'rowspan', 'scope'],
    td: ['colspan', 'rowspan'],
    img: ['src', 'alt', 'width', 'height'],
    a: ['href', 'target', 'rel'],
    input: ['type', 'name', 'value', 'placeholder', 'checked', 'disabled', 'min', 'max', 'maxlength', 'size'],
    select: ['name', 'multiple', 'disabled', 'size'],
    option: ['value', 'selected', 'disabled'],
    optgroup: ['label', 'disabled'],
    textarea: ['name', 'rows', 'cols', 'placeholder', 'disabled', 'maxlength'],
    label: ['for'],
  },
  allowedSchemes: ['https', 'data'],
  allowedSchemesByTag: { img: ['https', 'data'] },
  allowProtocolRelative: false,
};

/**
 * 运行期脚本注入 HTML 的清洗($().html()/append()/appendCreated 落 DOM 通道):
 * SCRIPT_HTML_WHITELIST 不含 <style>(43 标签白名单),作者脚本运行期注入的样式段
 * (如飞讯终端 $().append('<style>…</style>'))会被整段剥掉 → UI 裸渲染。
 * 此处把 <style> 段切出单走样式管线(与 render.ts 静态管道同一实现:
 * sanitizeScopedCss 声明级清洗 → scopeCss 容器作用域化 → keyframes 重命名回写 CSS 引用),
 * 非样式段仍走白名单,按原顺序重组。
 * scopeId 即容器 data-kd-scope 值:覆层 DOM 经 $('body') → 容器映射落在容器内,
 * 作用域前缀 [data-kd-scope="..."] 的后代选择器能命中。
 */
export function sanitizeScriptHtmlWithStyles(html: string, scopeId: string): string {
  const re = /<style\b[^>]*>([\s\S]*?)<\/style\s*>/gi;
  let out = '';
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(html)) !== null) {
    if (m.index > last) out += sanitizeHtml(html.slice(last, m.index), SCRIPT_HTML_WHITELIST);
    const scoped = scopeCss(sanitizeScopedCss(m[1]), scopeId);
    const css = applyKeyframeRename(scoped.css, scoped.keyframes);
    // sanitizeScopedCss 已剥 `</style`,重组不会提前闭合;空样式段不产出空标签
    if (css.trim()) out += `<style>${css}</style>`;
    last = m.index + m[0].length;
  }
  if (last < html.length) out += sanitizeHtml(html.slice(last), SCRIPT_HTML_WHITELIST);
  return out;
}

export { SCRIPT_HTML_WHITELIST };
