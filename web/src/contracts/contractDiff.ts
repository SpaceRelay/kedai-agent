// 契约 diff 纯函数(P6 面板):编辑器保存前对比新旧契约,产出字段级变更摘要。
// 与后端 contracts DSL 的 camelCase 字段对齐(updateRules/guardrails/invariants)。

export type DiffKind = 'added' | 'removed' | 'changed';

export interface ContractDiffItem {
  kind: DiffKind;
  /** 字段路径,如 "updateRules.好感度.updateMode" */
  path: string;
  /** 旧值(changed/removed 时有) */
  before?: unknown;
  /** 新值(changed/added 时有) */
  after?: unknown;
}

/** 契约顶层参与 diff 的键(updateRules 按字段粒度,其余按值粒度) */
const TOP_KEYS = ['version', 'id', 'guardrails', 'invariants', 'displayRules', 'schema'] as const;

function diffUpdateRules(
  before: Record<string, unknown> | undefined,
  after: Record<string, unknown> | undefined,
  out: ContractDiffItem[],
): void {
  const b = before ?? {};
  const a = after ?? {};
  for (const key of new Set([...Object.keys(b), ...Object.keys(a)])) {
    const inB = key in b;
    const inA = key in a;
    const prefix = `updateRules.${key}`;
    if (!inB) {
      out.push({ kind: 'added', path: prefix, after: a[key] });
    } else if (!inA) {
      out.push({ kind: 'removed', path: prefix, before: b[key] });
    } else if (JSON.stringify(b[key]) !== JSON.stringify(a[key])) {
      // 字段定义内部再降一层:指出具体改动的子键(path/type/updateMode…)
      const bf = (b[key] ?? {}) as Record<string, unknown>;
      const af = (a[key] ?? {}) as Record<string, unknown>;
      let nested = false;
      for (const sub of new Set([...Object.keys(bf), ...Object.keys(af)])) {
        if (JSON.stringify(bf[sub]) !== JSON.stringify(af[sub])) {
          out.push({
            kind: sub in bf ? 'changed' : 'added',
            path: `${prefix}.${sub}`,
            before: bf[sub],
            after: af[sub],
          });
          nested = true;
        }
      }
      if (!nested) {
        out.push({ kind: 'changed', path: prefix, before: b[key], after: a[key] });
      }
    }
  }
}

function diffLeaves(
  path: string,
  before: unknown,
  after: unknown,
  out: ContractDiffItem[],
): void {
  if (JSON.stringify(before) !== JSON.stringify(after)) {
    out.push({ kind: 'changed', path, before, after });
  }
}

/** 对比新旧契约,返回变更列表(空数组=无差异)。 */
export function diffContracts(
  before: Record<string, unknown> | null,
  after: Record<string, unknown> | null,
): ContractDiffItem[] {
  const out: ContractDiffItem[] = [];
  const b = before ?? {};
  const a = after ?? {};
  diffUpdateRules(
    b.updateRules as Record<string, unknown> | undefined,
    a.updateRules as Record<string, unknown> | undefined,
    out,
  );
  for (const key of TOP_KEYS) {
    diffLeaves(key, b[key], a[key], out);
  }
  return out;
}

/** 变更项的人类可读单行摘要(面板列表展示用) */
export function formatDiffItem(item: ContractDiffItem): string {
  const label = item.kind === 'added' ? '+' : item.kind === 'removed' ? '-' : '~';
  const brief = (v: unknown): string => {
    const s = JSON.stringify(v);
    if (s === undefined) return 'undefined';
    return s.length > 60 ? `${s.slice(0, 57)}…` : s;
  };
  const value =
    item.kind === 'added'
      ? brief(item.after)
      : item.kind === 'removed'
        ? brief(item.before)
        : `${brief(item.before)} → ${brief(item.after)}`;
  return `${label} ${item.path}: ${value}`;
}

/** 是否存在任一差异(保存按钮脏检查用) */
export function hasDiff(
  before: Record<string, unknown> | null,
  after: Record<string, unknown> | null,
): boolean {
  return diffContracts(before, after).length > 0;
}
