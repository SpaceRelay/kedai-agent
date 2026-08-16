// 消息渲染面板(阶段五 5b,TH-render 等价物)纯函数
// 约定式识别:消息内代码块文本含 `<html` 标记且含 `<head` 或 `<body` 时,
// 视为整页 HTML 面板,渲染为独立 iframe 宿主文档(见 server-rs render_frame_template.html)。

/** 代码块文本是否命中渲染面板判定(TH-render 约定:<html 必含,<head 与 <body 至少其一) */
export function isRenderCodeBlock(text: string): boolean {
  if (!text) return false;
  return text.includes('<html') && (text.includes('<head') || text.includes('<body'));
}

/** 从 `pre code` 容器提取未转义原文(面板 HTML 源码) */
export function extractCodeText(container: HTMLElement): string {
  return container.textContent ?? '';
}

/**
 * 拼装完整面板 HTML 文档。
 * - 代码块可能是半截文档(只有 <body> 片段),自动补齐 <!doctype>/<html>/<head>/<body>;
 * - 已含 <html> 时原样保留(头部标签缺失由浏览器容错);
 * - 所有父级通知消息统一带 channel/nonce(宿主按 nonce 认证,防伪造面板)。
 */
export function buildRenderDocument(code: string, channel: string, nonce: string): string {
  const inner = code.replace(/<!doctype[^>]*>/gi, '').trim();
  if (/<html[\s>]/i.test(inner)) {
    return inner;
  }
  return [
    '<!doctype html>',
    '<html><head><meta charset="utf-8">',
    `</head><body data-kd-render-panel="${channel}#${nonce}">`,
    inner,
    '</body></html>',
  ].join('\n');
}

/** 宿主文档 channel(与 server-rs render_frame_template.html 一致) */
export const RENDER_PANEL_CHANNEL = 'kedai-render-panel-v1';
