import { describe, it, expect } from 'vitest';
import { renderMarkdown, stripMvuBlocks } from './markdown';

describe('renderMarkdown', () => {
  it('基础 markdown 渲染', () => {
    const html = renderMarkdown('**粗体** 与 `代码`');
    expect(html).toContain('<strong>粗体</strong>');
    expect(html).toContain('<code>代码</code>');
  });

  it('代码块渲染', () => {
    const html = renderMarkdown('```js\nconst a = 1;\n```');
    expect(html).toContain('<pre><code class="language-js">');
    expect(html).toContain('const a = 1;');
  });

  it('原始 HTML 被净化(html:false)', () => {
    const html = renderMarkdown('<script>alert(1)</script>**x**');
    expect(html).not.toContain('<script>');
    expect(html).toContain('<strong>x</strong>');
  });

  it('链接新窗口', () => {
    const html = renderMarkdown('[链接](https://example.com)');
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer"');
  });

  it('剥离 mvu UpdateVariable 块', () => {
    const html = renderMarkdown(
      '正文\n<UpdateVariable>\n_.set("a", 1, 2);\n</UpdateVariable>\n继续',
    );
    expect(html).not.toContain('UpdateVariable');
    expect(html).not.toContain('_.set');
    expect(html).toContain('正文');
    expect(html).toContain('继续');
  });

  it('空串/纯块返回空', () => {
    expect(renderMarkdown('')).toBe('');
    expect(renderMarkdown('<UpdateVariable>_.set("a",1,2)</UpdateVariable>')).toBe('');
  });

  it('剥离 <status_current_variable> 段标签(内容保留)', () => {
    const html = renderMarkdown('<status_current_variable>好感度: 88</status_current_variable>正文');
    expect(html).not.toContain('status_current_variable');
    expect(html).toContain('好感度: 88');
    expect(html).toContain('正文');
  });

  it('status_current_variable 大小写不敏感', () => {
    const html = renderMarkdown('<STATUS_CURRENT_VARIABLE>内容</STATUS_CURRENT_VARIABLE>');
    expect(html).not.toContain('STATUS_CURRENT_VARIABLE');
    expect(html).toContain('内容');
  });
});

describe('stripMvuBlocks', () => {
  it('仅剥离块', () => {
    expect(stripMvuBlocks('a<UpdateVariable>x</UpdateVariable>b')).toBe('ab');
  });

  it('同时剥离 UpdateVariable 块与 status_current_variable 标签', () => {
    expect(stripMvuBlocks('a<UpdateVariable>x</UpdateVariable><status_current_variable>y</status_current_variable>b')).toBe('ayb');
  });
});
