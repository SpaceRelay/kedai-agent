import { describe, it, expect } from 'vitest';
import { createLodashShim } from './host';

describe('createLodashShim', () => {
  const _ = createLodashShim();

  it('_.get 支持路径与默认值,stat_data 叶子自动解包取 [0]', () => {
    const data = { user: { name: '指挥官' }, pair: ['勇气', '条件'] };
    expect(_.get(data, 'user.name')).toBe('指挥官');
    expect(_.get(data, 'user.missing', '缺省')).toBe('缺省');
    // MagVarUpdate 生态约定:叶子为 [新值, 条件] 成对数组,取 [0]
    expect(_.get(data, 'pair')).toBe('勇气');
  });

  it('_.set 写入路径', () => {
    const obj: Record<string, unknown> = {};
    _.set(obj, 'a.b.c', 1);
    expect((obj.a as Record<string, unknown>).b).toEqual({ c: 1 });
  });

  it('_.isEmpty 覆盖 null/undefined/空串/空数组/空对象', () => {
    expect(_.isEmpty(null)).toBe(true);
    expect(_.isEmpty(undefined)).toBe(true);
    expect(_.isEmpty('')).toBe(true);
    expect(_.isEmpty([])).toBe(true);
    expect(_.isEmpty({})).toBe(true);
    expect(_.isEmpty('abc')).toBe(false);
    expect(_.isEmpty([1])).toBe(false);
    expect(_.isEmpty({ a: 1 })).toBe(false);
    expect(_.isEmpty(0)).toBe(false);
  });

  it('_.forEach 遍历对象与数组,回调 (value, key)', () => {
    const seen: string[] = [];
    _.forEach({ a: 1, b: 2 }, (v: unknown, k: string | number) => seen.push(`${k}=${v}`));
    expect(seen.sort()).toEqual(['a=1', 'b=2']);
    seen.length = 0;
    _.forEach(['x', 'y'], (v: unknown, k: string | number) => seen.push(`${k}=${v}`));
    expect(seen).toEqual(['0=x', '1=y']);
  });

  it('_.has / _.keys / _.values / isArray / isObject / isNil', () => {
    expect(_.has({ a: { b: 1 } }, 'a.b')).toBe(true);
    expect(_.has({ a: 1 }, 'a.x')).toBe(false);
    expect(_.keys({ a: 1, b: 2 })).toEqual(['a', 'b']);
    expect(_.values({ a: 1, b: 2 })).toEqual([1, 2]);
    expect(_.isArray([1])).toBe(true);
    expect(_.isObject({})).toBe(true);
    expect(_.isObject(null)).toBe(false);
    expect(_.isNil(null)).toBe(true);
    expect(_.isNil(0)).toBe(false);
  });
});
