// 渲染面板纯函数测试(阶段五 5b)
import { describe, expect, it } from 'vitest';
import { buildRenderDocument, isRenderCodeBlock, RENDER_PANEL_CHANNEL } from './renderPanel';

describe('isRenderCodeBlock', () => {
  it('含 <html 与 <head/<body 的整页代码块命中', () => {
    expect(isRenderCodeBlock('<!doctype html><html><head>…</head><body>x</body></html>')).toBe(true);
    expect(isRenderCodeBlock('<html>\n<head><title>t</title></head>\n<body>你好</body>\n</html>')).toBe(true);
    expect(isRenderCodeBlock('<html lang="zh"><body>仅 body</body></html>')).toBe(true);
    expect(isRenderCodeBlock('<html><head></head></html>')).toBe(true);
  });

  it('只有 <html 无 <head/<body 不命中', () => {
    expect(isRenderCodeBlock('<html></html>')).toBe(false);
  });

  it('只有 <head 或 <body 无 <html 不命中(普通片段,走 scoped 注入)', () => {
    expect(isRenderCodeBlock('<body>状态栏</body>')).toBe(false);
    expect(isRenderCodeBlock('<head><style>a{}</style></head>')).toBe(false);
  });

  it('非 HTML 代码块与空文本不命中', () => {
    expect(isRenderCodeBlock('')).toBe(false);
    expect(isRenderCodeBlock('const x = 1;')).toBe(false);
    expect(isRenderCodeBlock('markdown 普通段落')).toBe(false);
  });
});

describe('buildRenderDocument', () => {
  it('代码块是半截文档时补齐完整 HTML 骨架', () => {
    const doc = buildRenderDocument('<div>面板内容</div>', RENDER_PANEL_CHANNEL, 'n1');
    expect(doc).toContain('<!doctype html>');
    expect(doc).toContain('<html>');
    expect(doc).toContain('<head>');
    expect(doc).toContain('<body');
    expect(doc).toContain('面板内容');
    expect(doc).toContain(`data-kd-render-panel="${RENDER_PANEL_CHANNEL}#n1"`);
  });

  it('已含 <html 的代码块原样保留(含 doctype 时剥离避免重复)', () => {
    const src = '<!doctype html><html><head><title>t</title></head><body>x</body></html>';
    expect(buildRenderDocument(src, RENDER_PANEL_CHANNEL, 'n2')).not.toContain('<!doctype');
    expect(buildRenderDocument('<html><body>x</body></html>', RENDER_PANEL_CHANNEL, 'n2')).toBe('<html><body>x</body></html>');
  });
});
