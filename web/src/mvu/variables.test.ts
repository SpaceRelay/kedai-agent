import { describe, it, expect } from 'vitest';
import {
  pathGet,
  pathSet,
  pathDelete,
  statValue,
  applyCommands,
  emptyVariables,
  collectFromInitVarContent,
} from './variables';
import type { UpdateCommand } from './parser';

describe('pathGet / pathSet', () => {
  it('点路径取值', () => {
    const obj = { a: { b: { c: 1 } } };
    expect(pathGet(obj, 'a.b.c')).toBe(1);
    expect(pathGet(obj, 'a.x')).toBeUndefined();
    expect(pathGet(null, 'a')).toBeUndefined();
  });

  it('点路径设值并自动建对象', () => {
    const obj: Record<string, unknown> = {};
    pathSet(obj, 'a.b.c', 42);
    expect(obj).toEqual({ a: { b: { c: 42 } } });
  });

  it('数组索引支持', () => {
    const obj: Record<string, unknown> = {};
    pathSet(obj, 'list.0.name', 'x');
    expect((obj.list as unknown[])[0]).toEqual({ name: 'x' });
  });

  it('拒绝 __proto__/constructor/prototype 危险段(防原型污染)', () => {
    const obj: Record<string, unknown> = {};
    pathSet(obj, '__proto__.polluted', 'yes');
    expect(({} as Record<string, unknown>).polluted).toBeUndefined();
    expect(obj).toEqual({});
    pathSet(obj, 'constructor.prototype.hacked', 'yes');
    expect(({} as Record<string, unknown>).hacked).toBeUndefined();
    pathSet(obj, 'a.prototype.b', 1);
    expect(pathGet(obj, 'a.prototype.b')).toBeUndefined();
    expect(pathGet(obj, '__proto__.x')).toBeUndefined();
    // 普通路径不受影响
    pathSet(obj, 'a.b', 1);
    expect(pathGet(obj, 'a.b')).toBe(1);
  });
});

describe('pathDelete', () => {
  it('删除叶子键与中间路径', () => {
    const obj: Record<string, unknown> = { a: { b: { c: 1 } }, keep: 1 };
    pathDelete(obj, 'a.b.c');
    expect(obj).toEqual({ a: { b: {} }, keep: 1 });
    pathDelete(obj, 'a');
    expect(obj).toEqual({ keep: 1 });
  });

  it('路径不存在时静默(不抛错)', () => {
    const obj: Record<string, unknown> = {};
    pathDelete(obj, 'x.y.z');
    expect(obj).toEqual({});
  });
});

describe('statValue', () => {
  it('取成对数组的 [0]', () => {
    expect(statValue(['新', '原因'])).toBe('新');
    expect(statValue(5)).toBe(5);
  });
});

describe('applyCommands', () => {
  it('应用命令到变量树并生成 display', () => {
    const vars = emptyVariables();
    const cmds: UpdateCommand[] = [
      { path: '世界.年分', oldValue: '2026', newValue: '2027', reason: '时间流逝' },
      { path: '芽衣.好感度', oldValue: 0, newValue: 15, reason: '初识' },
    ];
    applyCommands(vars, cmds);
    expect(statValue(pathGet(vars.stat_data, '世界.年分'))).toBe('2027');
    expect(pathGet(vars.display_data, '世界.年分')).toBe('2026->2027(时间流逝)');
    expect(statValue(pathGet(vars.stat_data, '芽衣.好感度'))).toBe(15);
  });

  it('remove 语义:stat_data 删键,display_data 保留展示记录', () => {
    const vars = emptyVariables();
    applyCommands(vars, [{ op: 'remove', path: 'a', oldValue: 1, newValue: null, reason: '删除' }]);
    expect(pathGet(vars.stat_data, 'a')).toBeUndefined();
    expect(pathGet(vars.display_data, 'a')).toBe('->null(删除)');
  });

  it('move 语义:源存在时目标设值、源删除', () => {
    const vars = emptyVariables();
    applyCommands(vars, [{ op: 'set', path: 'src', oldValue: null, newValue: 9, reason: 'x' }]);
    applyCommands(vars, [{ op: 'move', path: 'dst', from: 'src', oldValue: null, newValue: null, reason: '移动' }]);
    expect(statValue(pathGet(vars.stat_data, 'dst'))).toBe(9);
    expect(pathGet(vars.stat_data, 'src')).toBeUndefined();
  });

  it('move 语义:源缺失时整条跳过(不置 null、不设目标)', () => {
    const vars = emptyVariables();
    applyCommands(vars, [{ op: 'move', path: 'dst', from: 'missing', oldValue: null, newValue: null, reason: '移动' }]);
    expect(pathGet(vars.stat_data, 'dst')).toBeUndefined();
    expect(pathGet(vars.stat_data, 'missing')).toBeUndefined();
  });

  it('危险路径命令被静默拒绝', () => {
    const vars = emptyVariables();
    applyCommands(vars, [{ op: 'set', path: '__proto__.polluted', oldValue: null, newValue: 1, reason: 'x' }]);
    expect(({} as Record<string, unknown>).polluted).toBeUndefined();
  });
});

describe('collectFromInitVarContent', () => {
  it('解析 _.set 语句', () => {
    const out = collectFromInitVarContent(
      '_.set("芽衣.当前打扮", "连帽外套", "初始");\n_.set("世界.年分", "旧年", 2026);',
    );
    expect(statValue(pathGet(out, '芽衣.当前打扮'))).toBe('初始');
    expect(statValue(pathGet(out, '世界.年分'))).toBe(2026);
  });

  it('JSON 对象包裹为成对数组', () => {
    const out = collectFromInitVarContent('{"a": 1, "b": {"c": "x"}}');
    expect(statValue(pathGet(out, 'a'))).toBe(1);
    expect(statValue(pathGet(out, 'b.c'))).toBe('x');
  });

  it('YAML 风格缩进键值对', () => {
    const out = collectFromInitVarContent(
      '世界:\n  年分: 2024\n  月份: 2\n芽衣:\n  当前打扮: "宽大白外套"\n  好感度: 15\n身体资讯:\n  情绪: "平静"\n',
    );
    expect(statValue(pathGet(out, '世界.年分'))).toBe(2024);
    expect(statValue(pathGet(out, '世界.月份'))).toBe(2);
    expect(statValue(pathGet(out, '芽衣.当前打扮'))).toBe('宽大白外套');
    expect(statValue(pathGet(out, '芽衣.好感度'))).toBe(15);
    expect(statValue(pathGet(out, '身体资讯.情绪'))).toBe('平静');
  });
});
