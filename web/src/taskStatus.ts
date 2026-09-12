// 任务状态映射:TaskBoard / Sidebar / AgentPanel 共用
// 覆盖三类状态:任务主状态 TaskStatus、计划步骤状态 TaskStepStatus、子任务状态 TaskSubtaskStatus
// (色块走结构主义三原色语义:绿=完成、黄=进行、红=失败、灰=待执行)

import type { TaskStatus, TaskStepStatus, TaskSubtaskStatus } from './api/types';

/** 三类任务相关状态的并集:任务主状态 / 计划步骤状态 / 子任务状态 */
export type AnyTaskStatus = TaskStatus | TaskStepStatus | TaskSubtaskStatus;

/**
 * 软断言 exhaustiveness:编译期若 switch 未覆盖全部字面量,`value` 推断不出 never 即报错;
 * 运行时遇到未来新增的状态值则返回 fallback 兜底,不崩 UI。
 */
function softNever(value: never, fallback: string): string {
  void value;
  return fallback;
}

/** 状态 → 色块/标签 class(done / active / partial / error / pending) */
export function taskStatusClass(status: AnyTaskStatus): string {
  switch (status) {
    case 'done': return 'done';
    case 'planning':
    case 'running': return 'active';
    // partial(部分完成)是**终态**,与 running/planning 的「还在跑」语义不同:
    // 旧实现与进行中同色(active 黄),用户看不出「已结束但未达标」,只当成仍在执行
    //(实跑问题 2 观感残留)。独立配色(橙)区分终态。
    case 'partial': return 'partial';
    case 'error':
    case 'ended': return 'error';
    case 'pending':
    // planned(plan 模式待批准):等待用户动作的暂停态,走 pending 灰
    case 'planned': return 'pending';
    default: return softNever(status, 'pending');
  }
}

/** 状态 → 中文文案 */
export function taskStatusLabel(status: AnyTaskStatus): string {
  switch (status) {
    case 'pending': return '待执行';
    case 'planning': return '规划中';
    case 'running': return '执行中';
    case 'planned': return '待批准';
    case 'done': return '已完成';
    case 'partial': return '部分完成';
    case 'error': return '出错';
    case 'ended': return '已停止';
    default: return softNever(status, status);
  }
}
