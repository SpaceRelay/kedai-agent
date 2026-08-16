// [InitVar] 世界书条目初始化:mvu 约定 —— 名字含 [InitVar] 的条目(可禁用)提供变量初始值。
// 条目内容支持两种形式:逐条 `_.set(...)` 语句,或整体 JSON 对象。
// 约定源自 MagVarUpdate(原作者:MagicalAstrogy,github.com/MagicalAstrogy/MagVarUpdate,MIT)。
import type { MvuVariables } from './variables';
import { collectFromInitVarContent, emptyVariables, deepMerge, statPair } from './variables';
import type { JsonValue } from './parser';
import { parseSetStatement } from './parser';
import { pathGet, pathSet } from './variables';

export interface InitVarEntry {
  comment: string;
  content: string;
}

/** 是否应视为 InitVar 条目 */
export function isInitVarEntry(comment: string): boolean {
  return comment.includes('[InitVar]');
}

/** 从一组条目收集初始变量(条目顺序即应用顺序) */
export function collectInitVars(entries: InitVarEntry[]): MvuVariables {
  const vars = emptyVariables();
  for (const e of entries) {
    if (!isInitVarEntry(e.comment)) continue;
    if (!e.content) continue;
    const collected = collectFromInitVarContent(e.content);
    if (Object.keys(collected).length === 0) continue;
    deepMerge(vars.stat_data, collected);
    // display_data:镜像初始值("初始" 原因)
    mirrorDisplay(vars, collected);
  }
  return vars;
}

function mirrorDisplay(vars: MvuVariables, collected: Record<string, unknown>): void {
  const walk = (prefix: string, node: Record<string, unknown>): void => {
    for (const [k, v] of Object.entries(node)) {
      const path = prefix ? `${prefix}.${k}` : k;
      if (Array.isArray(v) && v.length >= 1) {
        const val = v[0] as JsonValue;
        pathSet(vars.display_data, path, `${String(val)}->${String(val)}(初始)`);
      } else if (typeof v === 'object' && v !== null) {
        walk(path, v as Record<string, unknown>);
      } else {
        pathSet(vars.display_data, path, `${String(v)}->${String(v)}(初始)`);
      }
    }
  };
  walk('', collected);
}

// 复导出常用符号,避免调用方多处 import
export { pathGet, pathSet, statPair, deepMerge };
