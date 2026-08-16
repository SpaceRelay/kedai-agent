// markdown.ts — 消息 Markdown 渲染(默认开启)
// 安全策略:html:false(LLM 输出不可信,原始 HTML 一律转义),链接新窗口。
// 渲染前剥离 mvu 的 <UpdateVariable> 块(其内容只用于变量更新,不展示)
// 与 <status_current_variable> 段标签(原版 MagVarUpdate 的变量状态段标记)。
import MarkdownIt from 'markdown-it';
import { parseUpdateVariable } from './mvu/parser';

/** <status_current_variable> 起止标签剥离(大小写不敏感;内容保留) */
const STATUS_VAR_RE = /<\/?\s*status_current_variable\s*>/gi;

/** 剥离协议块/段标签:UpdateVariable 块 + status_current_variable 标签 */
export function stripProtocolBlocks(text: string): string {
  let t = parseUpdateVariable(text).cleaned;
  if (STATUS_VAR_RE.test(t)) {
    STATUS_VAR_RE.lastIndex = 0;
    t = t.replace(STATUS_VAR_RE, '');
  }
  return t;
}

let md: MarkdownIt | null = null;

/** markdown 扩展插件(由插件框架注册) */
const mdExtensions: Array<(md: MarkdownIt) => void> = [];

/** 注册 markdown 扩展(插件框架调用) */
export function registerMarkdownExtension(ext: (md: MarkdownIt) => void): void {
  mdExtensions.push(ext);
  md = null; // 触发惰性重建
}

function buildMd(): MarkdownIt {
  const instance = new MarkdownIt({
    html: false,
    linkify: true,
    breaks: true,
    typographer: false,
  });

  // 链接:新窗口 + rel 安全属性
  const defaultLink = instance.renderer.rules.link_open ?? ((tokens, idx, options, _env, self) => self.renderToken(tokens, idx, options));
  instance.renderer.rules.link_open = (tokens, idx, options, env, self) => {
    tokens[idx].attrSet('target', '_blank');
    tokens[idx].attrSet('rel', 'noopener noreferrer');
    return defaultLink(tokens, idx, options, env, self);
  };

  for (const ext of mdExtensions) ext(instance);
  return instance;
}

function ensureMd(): MarkdownIt {
  if (!md) md = buildMd();
  return md;
}

/** 渲染 markdown 到安全 HTML(默认供 v-html 使用) */
export function renderMarkdown(text: string): string {
  if (!text) return '';
  // 剥离 mvu UpdateVariable 块与 status_current_variable 段标签
  const cleaned = stripProtocolBlocks(text);
  if (!cleaned.trim()) return '';
  return ensureMd().render(cleaned);
}

/** 仅剥离协议块/段标签(供纯文本场景复用) */
export function stripMvuBlocks(text: string): string {
  return stripProtocolBlocks(text);
}

/** 导出 md 实例(供测试/高级插件) */
export function getMarkdownIt(): MarkdownIt {
  return ensureMd();
}
