import { describe, expect, it } from 'vitest';
import { diffContracts, formatDiffItem, hasDiff } from './contractDiff';

const base = {
  version: 1,
  id: 'card',
  schema: { properties: {} },
  updateRules: {
    好感度: { path: '好感度', type: 'number', updateMode: 'every_turn', display: true },
    位置: { path: '位置', type: 'string', updateMode: 'every_turn', display: true },
  },
  guardrails: { maxOpsPerTurn: 16 },
};

describe('diffContracts', () => {
  it('相同契约无差异', () => {
    expect(diffContracts(base, { ...base })).toEqual([]);
    expect(hasDiff(base, { ...base })).toBe(false);
  });

  it('null → 契约视为全部新增', () => {
    const items = diffContracts(null, base);
    // 顶层键按 changed 报(updateRules 各字段 added)
    expect(items.some((i) => i.kind === 'added' && i.path === 'updateRules.好感度')).toBe(true);
    expect(hasDiff(null, base)).toBe(true);
  });

  it('契约 → null 视为全部移除', () => {
    const items = diffContracts(base, null);
    expect(items.some((i) => i.kind === 'removed' && i.path === 'updateRules.好感度')).toBe(true);
  });

  it('字段定义内部变化定位到子键', () => {
    const next = {
      ...base,
      updateRules: {
        ...base.updateRules,
        好感度: { ...base.updateRules.好感度, updateMode: 'every_n_turns', everyN: 3 },
      },
    };
    const items = diffContracts(base, next);
    const paths = items.map((i) => i.path);
    expect(paths).toContain('updateRules.好感度.updateMode');
    expect(paths).toContain('updateRules.好感度.everyN');
    // 未变字段不报
    expect(paths).not.toContain('updateRules.好感度.path');
    expect(paths).not.toContain('updateRules.位置.type');
  });

  it('新增/移除字段', () => {
    const next = {
      ...base,
      updateRules: {
        好感度: base.updateRules.好感度,
        衣服: { path: '衣服', type: 'string', updateMode: 'every_turn', display: true },
      },
    };
    let items = diffContracts(base, next);
    expect(items.filter((i) => i.path === 'updateRules.衣服')).toHaveLength(1);
    expect(items.find((i) => i.path === 'updateRules.衣服')?.kind).toBe('added');

    items = diffContracts(next, base);
    expect(items.find((i) => i.path === 'updateRules.衣服')?.kind).toBe('removed');
  });

  it('guardrails 变化按顶层粒度报', () => {
    const next = { ...base, guardrails: { maxOpsPerTurn: 8 } };
    const items = diffContracts(base, next);
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: 'changed', path: 'guardrails' });
  });
});

describe('formatDiffItem', () => {
  it('added/removed/changed 三种前缀', () => {
    expect(
      formatDiffItem({ kind: 'added', path: 'updateRules.衣服', after: { type: 'string' } }),
    ).toMatch(/^\+ updateRules\.衣服: /);
    expect(
      formatDiffItem({ kind: 'removed', path: 'updateRules.位置', before: { type: 'string' } }),
    ).toMatch(/^- updateRules\.位置: /);
    expect(
      formatDiffItem({ kind: 'changed', path: 'guardrails', before: 16, after: 8 }),
    ).toBe('~ guardrails: 16 → 8');
  });

  it('超长值截断到 60 字符', () => {
    const long = 'x'.repeat(100);
    const text = formatDiffItem({ kind: 'added', path: 'p', after: long });
    expect(text.length).toBeLessThanOrEqual(60 + '+ p: '.length + 1);
    expect(text).toContain('…');
  });
});
