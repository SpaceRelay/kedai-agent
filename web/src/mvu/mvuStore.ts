// mvu 状态纯函数:消息级快照回放、深度合并与 UpdateVariable 命令应用(store 层调用)
import { applyCommands } from './variables';
import type { MvuVariables } from './variables';
import type { UpdateCommand } from './parser';
import type { MvuSnapshot } from '../api';

export type { MvuVariables };
export type MvuVars = MvuVariables;

/** 深度合并(对象递归,数组/标量覆盖) */
export function deepMergeVars(target: Record<string, unknown>, patch: Record<string, unknown>): void {
  for (const [k, v] of Object.entries(patch)) {
    const tv = target[k];
    if (
      typeof v === 'object' &&
      v !== null &&
      !Array.isArray(v) &&
      typeof tv === 'object' &&
      tv !== null &&
      !Array.isArray(tv)
    ) {
      deepMergeVars(tv as Record<string, unknown>, v as Record<string, unknown>);
    } else {
      target[k] = v;
    }
  }
}

/** 按消息顺序回放 mvu 变量快照(extra.mvu),得到当前会话变量状态 */
export function replayMvuVariables(
  msgs: Array<{ role: string; extra?: Record<string, unknown> }>,
): MvuVariables {
  const vars = { stat_data: {} as Record<string, unknown>, display_data: {} as Record<string, unknown> };
  for (const m of msgs) {
    if (m.role !== 'assistant') continue;
    const snap = (m.extra as { mvu?: MvuSnapshot } | undefined)?.mvu;
    if (!snap?.stat_data) continue;
    // 深度合并(对象递归;叶子覆盖),变量树按消息推进;
    // display_data 缺失时用 stat_data 镜像(后端快照只含 stat_data)
    deepMergeVars(vars.stat_data, snap.stat_data);
    deepMergeVars(vars.display_data, snap.display_data ?? snap.stat_data);
  }
  return vars;
}

/**
 * 深拷贝并应用 UpdateVariable 命令,返回新变量树。
 * 深拷贝:pathSet 会修改嵌套子树,浅拷贝会直接污染 reactive 原树节点,
 * 且快照与显示树会共享嵌套引用(后续命令异常时原树已被改坏)。
 */
export function applyMvuCommands(
  current: MvuVariables,
  commands: UpdateCommand[],
): MvuVariables {
  const vars: MvuVariables = {
    stat_data: JSON.parse(JSON.stringify(current.stat_data)) as Record<string, unknown>,
    display_data: JSON.parse(JSON.stringify(current.display_data)) as Record<string, unknown>,
  };
  applyCommands(vars, commands);
  return vars;
}
