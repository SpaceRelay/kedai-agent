// [InitVar] 世界书条目初始化:mvu 约定 —— 名字含 [InitVar] 的条目(可禁用)提供变量初始值。
// 条目内容支持两种形式:逐条 `_.set(...)` 语句,或整体 JSON 对象。
// 约定源自 MagVarUpdate(原作者:MagicalAstrogy,github.com/MagicalAstrogy/MagVarUpdate,MIT)。
import type { MvuVariables } from './variables';
import { collectFromInitVarContent, emptyVariables, deepMerge, statPair } from './variables';
import type { JsonValue } from './parser';
import { parseSetStatement } from './parser';
import { displayChange, isStatLeafArray, unwrapStatLeaf } from './unwrap';
import { pathGet, pathSet } from './variables';

export interface InitVarEntry {
  comment: string;
  content: string;
}

/** 是否应视为 InitVar 条目(大小写不敏感,兼容 [InitialVariables] 别名;
 *  社区卡标签大小写随意,如碧蓝卡 `[initvar]变量初始化` 全小写——
 *  与后端 is_init_var_comment 保持同一谓词,避免双过滤分叉) */
export function isInitVarEntry(comment: string): boolean {
  const upper = comment.toUpperCase();
  return upper.includes('[INITVAR]') || upper.includes('[INITIALVARIABLES]');
}

/** 从一组条目收集初始变量(条目顺序即应用顺序) */
export function collectInitVars(entries: InitVarEntry[]): MvuVariables {
  const vars = emptyVariables();
  for (const e of entries) {
    if (!isInitVarEntry(e.comment)) continue;
    applyInitVarContent(vars, e.content);
  }
  return vars;
}

/**
 * 把一段 initvar 内容(_.set 语句 / JSON / YAML 缩进键值)并入变量树:
 * stat_data 深度合并(叶子为 [值,"初始"]),display_data 镜像 "v->v(初始)"。
 * 返回是否实际并入了解析到的变量。供 [InitVar] 世界书条目与开场白 <initvar> 块共用。
 */
export function applyInitVarContent(vars: MvuVariables, content: string): boolean {
  if (!content) return false;
  const collected = collectFromInitVarContent(content);
  if (Object.keys(collected).length === 0) return false;
  deepMerge(vars.stat_data, collected);
  // display_data:镜像初始值("初始" 原因)
  mirrorDisplay(vars, collected);
  return true;
}

function mirrorDisplay(vars: MvuVariables, collected: Record<string, unknown>): void {
  const walk = (prefix: string, node: Record<string, unknown>): void => {
    for (const [k, v] of Object.entries(node)) {
      const path = prefix ? `${prefix}.${k}` : k;
      if (isStatLeafArray(v)) {
        // [值, "初始"] 叶子对 → 展示串镜像 "v->v(初始)"(判定与解包均见 unwrap.ts)
        const val = unwrapStatLeaf(v) as JsonValue;
        pathSet(vars.display_data, path, displayChange(val, val, '初始'));
      } else if (typeof v === 'object' && v !== null) {
        walk(path, v as Record<string, unknown>);
      } else {
        pathSet(vars.display_data, path, displayChange(v, v, '初始'));
      }
    }
  };
  walk('', collected);
}

// 复导出常用符号,避免调用方多处 import
export { pathGet, pathSet, statPair, deepMerge };
