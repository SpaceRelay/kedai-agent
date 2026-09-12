// mvu 状态纯函数:消息级快照回放、深度合并与 UpdateVariable 命令应用(store 层调用)
import { applyCommands } from './variables';
import { emptyVariables } from './variables';
import type { MvuVariables } from './variables';
import { parseUpdateVariable } from './parser';
import type { UpdateCommand } from './parser';
import { applyInitVarContent } from './initvar';
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
  msgs: Array<{ role: string; content?: string; extra?: Record<string, unknown> }>,
): MvuVariables {
  const vars = emptyVariables();
  let snapshotSeen = false;
  for (const m of msgs) {
    if (m.role !== 'assistant') continue;
    const snap = (m.extra as { mvu?: MvuSnapshot } | undefined)?.mvu;
    if (!snap?.stat_data) {
      // 无快照的 assistant 消息(开场白):携带 <initvar> 且尚未见任何快照时,
      // 作为初始变量播种(开场白是种子消息,不经 applyMvuUpdate,快照里不会有它)。
      // 一旦快照开始出现则以快照为准(快照生成时已含播种结果),避免复活已删除的键。
      if (!snapshotSeen && Object.keys(vars.stat_data).length === 0 && m.content?.includes('<initvar>')) {
        const parsed = parseUpdateVariable(m.content);
        if (parsed.initvar) applyInitVarContent(vars, parsed.initvar);
      }
      continue;
    }
    snapshotSeen = true;
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
 * initvarRaw:消息携带的 <initvar> 段,仅在变量树为空时先播种再应用命令
 * (开场初始化语义;已有变量时不覆盖进行中的进度)。
 */
export function applyMvuCommands(
  current: MvuVariables,
  commands: UpdateCommand[],
  initvarRaw?: string,
): MvuVariables {
  const vars: MvuVariables = {
    stat_data: JSON.parse(JSON.stringify(current.stat_data)) as Record<string, unknown>,
    display_data: JSON.parse(JSON.stringify(current.display_data)) as Record<string, unknown>,
  };
  if (initvarRaw && Object.keys(vars.stat_data).length === 0) {
    applyInitVarContent(vars, initvarRaw);
  }
  applyCommands(vars, commands);
  return vars;
}
