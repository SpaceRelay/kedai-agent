// cardScripts 收集器测试(舰娘卡真实嵌套形态)
import { describe, expect, it } from 'vitest';
import { collectCardScripts, cardScriptCanonical, enabledCardScriptsOf } from './cardScripts';

describe('collectCardScripts', () => {
  it('舰娘卡真实形态:嵌套对象 {"0":{"1":{"0":script,"1":script}}} 递归拉平', () => {
    const dataRaw = {
      extensions: {
        tavern_helper: {
          '0': {
            '1': {
              '0': { type: 'script', enabled: true, name: 'swipe 监听', id: 'a1', content: 'eventOn()' },
              '1': { type: 'script', enabled: true, name: 'MVU Zod 脚本', id: 'a2', content: 'zod()' },
              '2': { type: 'script', enabled: true, name: '變量腳本', id: 'a3', content: 'vars()' },
            },
          },
          '1': { variables: {} },
        },
      },
    };
    const scripts = collectCardScripts(dataRaw);
    expect(scripts.map((s) => s.name)).toEqual(['swipe 监听', 'MVU Zod 脚本', '變量腳本']);
    expect(scripts.every((s) => s.enabled)).toBe(true);
  });

  it('数组形态与 V3 data.extensions 布局兼容', () => {
    const v3 = {
      data: {
        extensions: {
          tavern_helper: [
            { type: 'script', name: 'a', content: 'x()', enabled: true },
            { type: 'script', name: 'b', content: 'y()', enabled: false },
          ],
        },
      },
    };
    const scripts = collectCardScripts(v3);
    expect(scripts).toHaveLength(2);
    expect(scripts[1].enabled).toBe(false);
  });

  it('disabled:true 与 enabled:false 都视为停用;非脚本节点与 variables 树不误判', () => {
    const dataRaw = {
      extensions: {
        tavern_helper: {
          scripts: [
            { type: 'script', name: 'on', content: 'a()', enabled: true },
            { name: 'weak', content: 'b()', id: 'w1' }, // 弱匹配:无 type 但有 id
            { name: 'not-script', content: 'not code' }, // 无特征键 → 跳过
          ],
          variables: { foo: { name: 'x', content: 'y' } }, // 变量树同名对 → 跳过
        },
      },
    };
    const scripts = collectCardScripts(dataRaw);
    expect(scripts.map((s) => s.name)).toEqual(['on', 'weak']);
  });

  it('空/缺失 tavern_helper 返回空数组', () => {
    expect(collectCardScripts(undefined)).toEqual([]);
    expect(collectCardScripts({})).toEqual([]);
    expect(collectCardScripts({ extensions: {} })).toEqual([]);
  });
});

describe('cardScriptCanonical / enabledCardScriptsOf', () => {
  it('canonical 带 kind 标记与 content(内容进哈希)', () => {
    const c = cardScriptCanonical([{ id: 'a', name: 'n', content: 'code', enabled: true }]);
    expect(c[0]).toEqual({ kind: 'th-script', id: 'a', name: 'n', content: 'code', enabled: true });
  });

  it('enabledCardScriptsOf 只留启用且非空的脚本', () => {
    const detail = {
      data_raw: {
        extensions: {
          tavern_helper: [
            { type: 'script', name: 'a', content: 'x()', enabled: true },
            { type: 'script', name: 'b', content: 'y()', enabled: false },
            { type: 'script', name: 'c', content: '   ', enabled: true },
          ],
        },
      },
    };
    expect(enabledCardScriptsOf(detail).map((s) => s.name)).toEqual(['a']);
  });
});
