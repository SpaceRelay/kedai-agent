import { describe, it, expect } from 'vitest';
import {
  parseUpdateVariable,
  parseSetStatement,
  parseJsValue,
  hasUpdateVariable,
} from './parser';

describe('parseUpdateVariable', () => {
  it('提取并剥离 UpdateVariable 块', () => {
    const text = '正文开头\n<UpdateVariable>\n  <Analysis>判断</Analysis>\n  _.set("user.身份", "旧", "新");//原因\n</UpdateVariable>\n正文结尾';
    const { cleaned, commands } = parseUpdateVariable(text);
    expect(cleaned).not.toContain('UpdateVariable');
    expect(cleaned).toContain('正文开头');
    expect(cleaned).toContain('正文结尾');
    expect(cleaned.trim()).toBe('正文开头\n\n正文结尾'.replace(/\n\n/, '\n\n'));
    expect(commands).toHaveLength(1);
    expect(commands[0].path).toBe('user.身份');
    expect(commands[0].oldValue).toBe('旧');
    expect(commands[0].newValue).toBe('新');
    expect(commands[0].reason).toBe('原因');
  });

  it('解析数字与嵌套路径', () => {
    const text = '<UpdateVariable>_.set("心之所向.好感度", 0, 15);</UpdateVariable>';
    const { commands } = parseUpdateVariable(text);
    expect(commands).toHaveLength(1);
    expect(commands[0].newValue).toBe(15);
    expect(commands[0].oldValue).toBe(0);
  });

  it('多语句依次解析', () => {
    const text =
      '<UpdateVariable>\n_.set("a.b", 1, 2);\n_.set("c", "x", "y");\n</UpdateVariable>';
    const { commands } = parseUpdateVariable(text);
    expect(commands).toHaveLength(2);
    expect(commands[0].path).toBe('a.b');
    expect(commands[1].path).toBe('c');
  });

  it('容忍大小写与属性', () => {
    const text = '<updatevariable lang="zh">_.set("a",1,2)</updatevariable>';
    const { commands } = parseUpdateVariable(text);
    expect(commands).toHaveLength(1);
  });

  it('无块时原样返回', () => {
    const { cleaned, commands } = parseUpdateVariable('普通消息');
    expect(cleaned).toBe('普通消息');
    expect(commands).toHaveLength(0);
  });

  it('块内注释不干扰解析', () => {
    const text =
      '<UpdateVariable>\n// 这是注释\n_.set("a", 1, 2);// 行注释\n/* 块注释 */ _.set("b", 3, 4);\n</UpdateVariable>';
    const { commands } = parseUpdateVariable(text);
    expect(commands).toHaveLength(2);
    expect(commands[0].reason).toContain('行注释');
  });

  it('字符串内逗号与括号不切分', () => {
    const text = '<UpdateVariable>_.set("名字", "a,b(1)", "c");</UpdateVariable>';
    const { commands } = parseUpdateVariable(text);
    expect(commands).toHaveLength(1);
    expect(commands[0].newValue).toBe('c');
  });

  it('hasUpdateVariable 快速判断', () => {
    expect(hasUpdateVariable('<UpdateVariable>x</UpdateVariable>')).toBe(true);
    expect(hasUpdateVariable('普通')).toBe(false);
  });
});

describe('parseJsValue', () => {
  it('字符串字面量(单双引号)', () => {
    expect(parseJsValue("'hi'")).toBe('hi');
    expect(parseJsValue('"hi"')).toBe('hi');
  });
  it('数字/布尔/null', () => {
    expect(parseJsValue('15')).toBe(15);
    expect(parseJsValue('true')).toBe(true);
    expect(parseJsValue('null')).toBe(null);
  });
  it('数组/对象', () => {
    expect(parseJsValue('[1,2,3]')).toEqual([1, 2, 3]);
    expect(parseJsValue('{"a":1}')).toEqual({ a: 1 });
  });
});

describe('parseJsonPatch(酒馆助手格式)', () => {
  it('JSONPatch 块:剥离 + 解析 replace/delta/remove/move', () => {
    const text =
      '正文开头\n<UpdateVariable>\n<Analysis>好感度上升</Analysis>\n<JSONPatch>\n[\n  { "op": "replace", "path": "/心之所向/好感度", "value": 150 },\n  { "op": "delta", "path": "/世界/年分", "value": 1 },\n  { "op": "remove", "path": "/芽衣/旧字段" },\n  { "op": "move", "from": "/a/b", "to": "/a/c" }\n]\n</JSONPatch>\n</UpdateVariable>\n正文结尾';
    const { cleaned, commands } = parseUpdateVariable(text);
    expect(cleaned).not.toContain('UpdateVariable');
    expect(cleaned).toContain('正文开头');
    expect(cleaned).toContain('正文结尾');
    expect(commands).toHaveLength(4);
    expect(commands[0]).toMatchObject({ op: 'set', path: '心之所向.好感度', newValue: 150 });
    expect(commands[1]).toMatchObject({ op: 'delta', path: '世界.年分', newValue: 1 });
    expect(commands[2]).toMatchObject({ op: 'remove', path: '芽衣.旧字段' });
    expect(commands[3]).toMatchObject({ op: 'move', path: 'a.c', from: 'a.b' });
  });

  it('JSONPatch 优先于同块内 _.set(不混用)', () => {
    const text =
      '<UpdateVariable><JSONPatch>[{"op":"replace","path":"/x","value":1}]</JSONPatch>_.set("y", 0, 2);</UpdateVariable>';
    const { commands } = parseUpdateVariable(text);
    expect(commands).toHaveLength(1);
    expect(commands[0].path).toBe('x');
  });

  it('无 JSONPatch 回落 _.set 解析', () => {
    const text = '<UpdateVariable>_.set("a.b", 1, 2);//原因</UpdateVariable>';
    const { commands } = parseUpdateVariable(text);
    expect(commands).toHaveLength(1);
    expect(commands[0].path).toBe('a.b');
  });
});
