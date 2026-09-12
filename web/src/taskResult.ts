// 任务 result 拆段(批次 R1):后端结果契约——
// team 模式 result = 整合文本 + "\n\n## 审计结论\n" + 审计文本(server-rs task_engine/team.rs);
// plan 模式 result = 汇总文本 + "\n\n## 最终计划\n" + 最终计划段(task_engine/plan.rs)。
// 前端把尾部标记段拆为独立卡;两模式段结构各自独立,互不拆对方标记。
// 纯函数抽出自 TaskBoard.vue(原 team 审计结论拆卡逻辑原样迁入),供单测锁定契约。

import type { TaskRunMode } from './api/types';

/** team 模式:审计结论段标记(契约见 server-rs task_engine/team.rs) */
export const AUDIT_MARK = '## 审计结论';

/** plan 模式:最终计划段标记(契约见 server-rs task_engine/plan.rs) */
export const FINAL_PLAN_MARK = '## 最终计划';

/** result 拆段结果:主卡文本 + 各模式独立卡文本(空串 = 无该段) */
export interface TaskResultSplit {
  /** 主成果文本(已去掉尾部拆出段;无拆段时为 result 原文) */
  main: string;
  /** team 模式:审计结论段(含标题行;无则空串) */
  audit: string;
  /** plan 模式:最终计划段(含标题行;无则空串) */
  finalPlan: string;
}

/** 按模式拆 result 尾段;标记缺失或其他模式时主卡显示原文 */
export function splitTaskResult(mode: TaskRunMode, result: string): TaskResultSplit {
  const out: TaskResultSplit = { main: result, audit: '', finalPlan: '' };
  const mark = mode === 'team' ? AUDIT_MARK : mode === 'plan' ? FINAL_PLAN_MARK : '';
  if (!mark) return out;
  // 取**最后**一处标记:后端把拆出段拼在 result 最末尾,正文里复述同名标题
  // (审计结论原文会作为汇总输入下发,模型有概率在正文中复述该标题)不应截错位置。
  // 用 indexOf 取首个会把正文中的复述标记当成段起点,导致主成果卡被截半、审计卡
  // 混入正文——观感上就是「多版本/内容混乱仍在」(实跑问题 2 连带给修复)。
  const idx = result.lastIndexOf(mark);
  if (idx < 0) return out;
  out.main = result.slice(0, idx).trimEnd();
  const section = result.slice(idx);
  if (mode === 'team') {
    out.audit = section;
  } else {
    out.finalPlan = section;
  }
  return out;
}
