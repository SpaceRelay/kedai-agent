import { describe, expect, it } from 'vitest';
import { renderToolInput, renderToolOutput } from './toolRender';

describe('renderToolOutput(工具结果 render intent,借鉴 harness)', () => {
  it('output 为 undefined 返回空串', () => {
    expect(renderToolOutput({ name: 'x', output: undefined })).toBe('');
  });

  it('缺失 renderKind 回退 JSON 直出', () => {
    expect(renderToolOutput({ name: 'x', output: { a: 1 } })).toBe(JSON.stringify({ a: 1 }, null, 2));
  });

  it('read 意图字符串原样输出', () => {
    expect(renderToolOutput({ name: 'read', output: '文件内容', renderKind: 'read' })).toBe('文件内容');
  });

  it('search 意图提取 results 列表', () => {
    const output = {
      query: 'x',
      results: [{ title: '结果一', url: 'https://a' }, { title: '结果二' }],
    };
    const text = renderToolOutput({ name: 'search', output, renderKind: 'search' });
    expect(text).toContain('1. 结果一');
    expect(text).toContain('https://a');
    expect(text).toContain('2. 结果二');
  });

  it('search 意图无 results 时回退 JSON', () => {
    const output = { foo: 'bar' };
    expect(renderToolOutput({ name: 'search', output, renderKind: 'search' })).toBe(JSON.stringify(output, null, 2));
  });

  it('generic 意图提取 result/value 标量', () => {
    expect(renderToolOutput({ name: 'calculator', output: { result: 408 }, renderKind: 'generic' })).toBe('408');
    expect(renderToolOutput({ name: 'memory_read', output: { value: 'abc' }, renderKind: 'generic' })).toBe('abc');
  });
});

describe('renderToolInput(工具输入展示)', () => {
  it('字符串输入原样输出', () => {
    expect(renderToolInput({ name: 'x', input: 'hello' })).toBe('hello');
  });

  it('对象输入格式化 JSON', () => {
    expect(renderToolInput({ name: 'x', input: { a: 1 } })).toBe(JSON.stringify({ a: 1 }, null, 2));
  });
});
