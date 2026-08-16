import { describe, expect, it } from 'vitest';
import { filterCommands, wordBeforeCursor } from './slashSuggest';
import type { SlashCommandMeta } from '../api';

const commands: SlashCommandMeta[] = [
  { name: 'echo', description: '原样回显参数', params: '<text>' },
  { name: 'var', description: '写入 global 作用域变量', params: '<key> :: <value>' },
  { name: 'setvar', description: '/var 的别名', params: '<key> :: <value>' },
  { name: 'getvar', description: '读取变量(合并视图)', params: '<key>' },
  { name: 'addvar', description: '对 global 数值变量累加', params: '<key> :: <数值>' },
  { name: 'help', description: '列出全部 Slash 命令与用法', params: '' },
];

describe('filterCommands', () => {
  it('前缀匹配命令名,保持清单顺序', () => {
    expect(filterCommands('e', commands).map((c) => c.name)).toEqual(['echo']);
    expect(filterCommands('var', commands).map((c) => c.name)).toEqual(['var']);
    expect(filterCommands('set', commands).map((c) => c.name)).toEqual(['setvar']);
    expect(filterCommands('get', commands).map((c) => c.name)).toEqual(['getvar']);
    expect(filterCommands('add', commands).map((c) => c.name)).toEqual(['addvar']);
  });

  it('精确前缀命中单个', () => {
    const got = filterCommands('echo', commands);
    expect(got.map((c) => c.name)).toEqual(['echo']);
  });

  it('大小写不敏感', () => {
    expect(filterCommands('VAR', commands).map((c) => c.name)).toEqual(['var']);
  });

  it('空输入返回空列表', () => {
    expect(filterCommands('', commands)).toEqual([]);
    expect(filterCommands('   ', commands)).toEqual([]);
  });

  it('无匹配返回空列表', () => {
    expect(filterCommands('zzz', commands)).toEqual([]);
  });
});

describe('wordBeforeCursor', () => {
  it('取光标前的最后一个单词', () => {
    expect(wordBeforeCursor('hello /var x', 10)).toBe('/var');
  });

  it('光标在词中间时取光标前的部分', () => {
    expect(wordBeforeCursor('hello /var x', 9)).toBe('/va');
  });

  it('无单词(空串或仅空白)返回空串', () => {
    expect(wordBeforeCursor('', 0)).toBe('');
    expect(wordBeforeCursor('   ', 3)).toBe('');
  });

  it('光标在词中间时取光标前的部分', () => {
    expect(wordBeforeCursor('/va', 2)).toBe('/v');
  });

  it('光标位置越界时按文本长度截断', () => {
    expect(wordBeforeCursor('/var', 999)).toBe('/var');
  });
});
