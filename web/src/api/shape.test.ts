import { describe, expect, it } from 'vitest';
import {
  requireArrayField,
  requireNumberField,
  requireObject,
  requireObjectField,
  requireNullableObjectField,
  requireStringField,
  requireStringMapField,
} from './shape';

// 形状闸门单测(批次 1):每个 helper 的「合法值通过 / 非法值抛错」两态都覆盖。
// 关键不变量:抛错时**优先透出服务端 error 原文**(旧版 200 错误体的兼容读取),
// 无原文时给出「XX 响应格式异常」,绝不让 undefined 静默流向下游。

describe('requireObject', () => {
  it('对象原样返回', () => {
    const v = { connector: 'mock' };
    expect(requireObject(v, '连接器信息')).toBe(v);
  });

  it('数组 / null / 标量一律抛错', () => {
    for (const bad of [[], null, undefined, 42, 'x']) {
      expect(() => requireObject(bad, '连接器信息')).toThrow('连接器信息响应格式异常');
    }
  });

  it('空对象是合法对象(不误伤「无字段」的正常响应)', () => {
    expect(requireObject({}, '连接器信息')).toEqual({});
  });

  it('旧版 200 错误体:字段缺失时透出服务端 error 原文', () => {
    expect(() => requireObjectField({ error: '后端未就绪' }, 'config', '注入配置')).toThrow(
      '后端未就绪',
    );
    expect(() => requireArrayField({ error: '列表拉取失败' }, 'items', '列表')).toThrow(
      '列表拉取失败',
    );
  });
});

describe('requireObjectField', () => {
  it('取出对象字段', () => {
    const cfg = { mode: 'simple', floors: [] };
    expect(requireObjectField({ ok: true, config: cfg }, 'config', '注入配置')).toBe(cfg);
  });

  it('字段缺失 / null / 数组均抛错(数组不是对象)', () => {
    expect(() => requireObjectField({ ok: true }, 'config', '注入配置')).toThrow(
      '注入配置响应格式异常',
    );
    expect(() => requireObjectField({ config: null }, 'config', '注入配置')).toThrow();
    expect(() => requireObjectField({ config: [] }, 'config', '注入配置')).toThrow();
  });
});

describe('requireArrayField', () => {
  it('数组(含空数组)通过', () => {
    expect(requireArrayField({ memories: [] }, 'memories', '记忆列表')).toEqual([]);
    expect(requireArrayField({ memories: [1, 2] }, 'memories', '记忆列表')).toEqual([1, 2]);
  });

  it('字段缺失 / 非数组抛错', () => {
    expect(() => requireArrayField({}, 'memories', '记忆列表')).toThrow('记忆列表响应格式异常');
    expect(() => requireArrayField({ memories: {} }, 'memories', '记忆列表')).toThrow();
    expect(() => requireArrayField({ memories: null }, 'memories', '记忆列表')).toThrow();
  });
});

describe('requireStringField', () => {
  it('字符串通过;数字 / 缺失抛错', () => {
    expect(requireStringField({ model: 'gpt-4o' }, 'model', '模型信息')).toBe('gpt-4o');
    expect(() => requireStringField({ model: 4 }, 'model', '模型信息')).toThrow();
    expect(() => requireStringField({}, 'model', '模型信息')).toThrow();
  });
});

describe('requireNumberField', () => {
  it('有限数字通过(0 合法)', () => {
    expect(requireNumberField({ total: 0 }, 'total', 'Token 计数')).toBe(0);
    expect(requireNumberField({ total: 12.5 }, 'total', 'Token 计数')).toBe(12.5);
  });

  it('数字字符串不通过(避免隐式算术错误);NaN/Infinity 视为非法', () => {
    expect(() => requireNumberField({ total: '12' }, 'total', 'Token 计数')).toThrow();
    expect(() => requireNumberField({ total: NaN }, 'total', 'Token 计数')).toThrow();
    expect(() => requireNumberField({ total: Infinity }, 'total', 'Token 计数')).toThrow();
    expect(() => requireNumberField({}, 'total', 'Token 计数')).toThrow();
  });
});

describe('requireNullableObjectField', () => {
  it('显式 null 合法(无记录态),对象合法', () => {
    expect(requireNullableObjectField({ trace: null }, 'trace', 'Agent 记录')).toBeNull();
    const t = { state: 'idle' };
    expect(requireNullableObjectField({ trace: t }, 'trace', 'Agent 记录')).toBe(t);
  });

  it('字段缺失抛错(缺字段 ≠ 无记录)', () => {
    expect(() => requireNullableObjectField({}, 'trace', 'Agent 记录')).toThrow(
      'Agent 记录响应格式异常',
    );
    expect(() => requireNullableObjectField({ trace: 3 }, 'trace', 'Agent 记录')).toThrow();
  });
});

describe('requireStringMapField', () => {
  it('字符串字典通过', () => {
    expect(requireStringMapField({ entries: { a: '1' } }, 'entries', '初始变量')).toEqual({
      a: '1',
    });
  });

  it('非对象抛错', () => {
    expect(() => requireStringMapField({ entries: [] }, 'entries', '初始变量')).toThrow();
    expect(() => requireStringMapField({}, 'entries', '初始变量')).toThrow();
  });
});
