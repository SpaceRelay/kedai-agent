import { describe, expect, it } from 'vitest';
import { parseScriptTreeImport, stripByExportWith } from './scriptTreeIO';

describe('parseScriptTreeImport(导入解析 + id 重分配,6e)', () => {
  it('合法 script/folder 数组解析并递归重分配 id', () => {
    const raw = [
      {
        type: 'script',
        enabled: true,
        name: '自动回复',
        id: 'old-1',
        content: "console.log('hi')",
        info: '说明',
        data: { count: 1 },
        export_with: { data: true, button: false },
      },
      {
        type: 'folder',
        enabled: true,
        name: '工具集',
        id: 'old-2',
        icon: 'fa-solid fa-folder',
        scripts: [
          { type: 'script', enabled: true, name: '子脚本', id: 'old-3', content: 'x()' },
        ],
      },
    ];
    const tree = parseScriptTreeImport(raw);
    expect(tree).toHaveLength(2);
    const script = tree[0];
    expect(script.type).toBe('script');
    if (script.type !== 'script') throw new Error('unreachable');
    expect(script.name).toBe('自动回复');
    expect(script.content).toBe("console.log('hi')");
    expect(script.info).toBe('说明');
    expect(script.id).not.toBe('old-1');
    expect(script.id).toMatch(/^[0-9a-f-]{8,}$|^s-/);

    const folder = tree[1];
    expect(folder.type).toBe('folder');
    if (folder.type !== 'folder') throw new Error('unreachable');
    expect(folder.id).not.toBe('old-2');
    expect(folder.scripts).toHaveLength(1);
    expect(folder.scripts[0].id).not.toBe('old-3');
    // 两次解析 id 均不同(恒分配新 id,避免与既有脚本撞车)
    const again = parseScriptTreeImport(raw);
    expect(again[0].id).not.toBe(script.id);
  });

  it('enabled 缺省视为 true,button 结构兜底', () => {
    const tree = parseScriptTreeImport([
      { type: 'script', name: '缺省启用', content: 'x' },
    ]);
    const s = tree[0];
    if (s.type !== 'script') throw new Error('unreachable');
    expect(s.enabled).toBe(true);
    expect(s.button).toBeUndefined();
  });

  it('非法输入抛错(非数组 / 未知 type / 缺字段)', () => {
    expect(() => parseScriptTreeImport({})).toThrow(/顶层应为数组/);
    expect(() => parseScriptTreeImport(null)).toThrow(/顶层应为数组/);
    expect(() =>
      parseScriptTreeImport([{ type: 'widget', name: 'x' }]),
    ).toThrow(/未知类型/);
    expect(() =>
      parseScriptTreeImport([{ type: 'script', name: '缺内容' }]),
    ).toThrow(/缺少名称或内容/);
    expect(() =>
      parseScriptTreeImport([{ type: 'folder', scripts: '不是数组' }]),
    ).toThrow(/缺少名称/);
  });
});

describe('stripByExportWith(export_with 剥离,6e)', () => {
  function node(over: Record<string, unknown> = {}) {
    return {
      type: 'script',
      enabled: true,
      name: 's',
      id: 'a',
      content: 'body()',
      info: '说明',
      data: { k: 1 },
      button: { enabled: true, buttons: [] },
      ...over,
    };
  }

  it('缺省(无 export_with)视为全导出,原样保留', () => {
    const out = stripByExportWith([node()]);
    const s = out[0];
    if (s.type !== 'script') throw new Error('unreachable');
    expect(s.content).toBe('body()');
    expect(s.info).toBe('说明');
    expect(s.data).toEqual({ k: 1 });
  });

  it('仅 data/button 时剥离 content/info,保留结构', () => {
    const out = stripByExportWith([node({ export_with: { data: true, button: true } })]);
    const s = out[0];
    if (s.type !== 'script') throw new Error('unreachable');
    expect('content' in s).toBe(false);
    expect('info' in s).toBe(false);
    expect(s.data).toEqual({ k: 1 });
    expect(s.button).toBeDefined();
    expect(s.name).toBe('s');
  });

  it('data=false 时清空 data;button=false 时清空 button', () => {
    const out = stripByExportWith([node({ export_with: { data: false, button: false } })]);
    const s = out[0];
    if (s.type !== 'script') throw new Error('unreachable');
    expect('data' in s).toBe(false);
    expect('button' in s).toBe(false);
    // data/button 均不导出时,视为全导出(保留 content/info)
    expect(s.content).toBe('body()');
  });

  it('文件夹内子脚本同样剥离', () => {
    const tree = [
      {
        type: 'folder',
        enabled: true,
        name: 'f',
        id: 'f1',
        scripts: [node({ export_with: { data: true, button: false } })],
      },
    ];
    const out = stripByExportWith(tree as never);
    const f = out[0];
    if (f.type !== 'folder') throw new Error('unreachable');
    const s = f.scripts[0];
    if (s.type !== 'script') throw new Error('unreachable');
    expect('content' in s).toBe(false);
    expect(s.data).toEqual({ k: 1 });
    expect('button' in s).toBe(false);
  });
});
