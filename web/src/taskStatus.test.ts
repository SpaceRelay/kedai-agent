import { describe, expect, it } from 'vitest';
import { taskStatusClass, taskStatusLabel } from './taskStatus';
import type { AnyTaskStatus } from './taskStatus';
import type { TaskStatus, TaskStepStatus, TaskSubtaskStatus } from './api/types';

// 任务状态映射(纯函数):TaskBoard / Sidebar / AgentPanel 共用同一口径。
// 三类状态(任务主状态 TaskStatus / 计划步骤 TaskStepStatus / 子任务 TaskSubtaskStatus)
// 共用同一映射;switch 走软断言 exhaustiveness——编译期漏状态报错,运行时遇未来新值不崩 UI。
// partial(WP3 新增「部分完成」终态)走独立橙系配色(partial):成果已产出但含失败步骤,
// 既不是全绿(完成)也不是全红(失败),也不同于黄系「进行中」——它是终态(实跑问题 2 观感修复)。

describe('taskStatusLabel', () => {
  it('任务主状态(TaskStatus)映射为中文文案', () => {
    const cases: Array<[TaskStatus, string]> = [
      ['pending', '待执行'],
      ['planning', '规划中'],
      ['running', '执行中'],
      ['done', '已完成'],
      ['partial', '部分完成'],
      ['error', '出错'],
      ['ended', '已停止'],
    ];
    for (const [status, label] of cases) {
      expect(taskStatusLabel(status)).toBe(label);
    }
  });

  it('计划步骤状态(TaskStepStatus)共用同一映射', () => {
    const cases: Array<[TaskStepStatus, string]> = [
      ['pending', '待执行'],
      ['running', '执行中'],
      ['done', '已完成'],
      ['error', '出错'],
    ];
    for (const [status, label] of cases) {
      expect(taskStatusLabel(status)).toBe(label);
    }
  });

  it('子任务状态(TaskSubtaskStatus)共用同一映射', () => {
    const cases: Array<[TaskSubtaskStatus, string]> = [
      ['pending', '待执行'],
      ['running', '执行中'],
      ['done', '已完成'],
      ['error', '出错'],
      ['ended', '已停止'],
    ];
    for (const [status, label] of cases) {
      expect(taskStatusLabel(status)).toBe(label);
    }
  });

  it('未知状态(未来新增值)原样透传', () => {
    // 模拟后端未来新增的状态字面量:绕过编译期类型,验证运行时兜底不崩
    expect(taskStatusLabel('weird' as AnyTaskStatus)).toBe('weird');
  });
});

describe('taskStatusClass', () => {
  it('完成/进行中/部分完成/失败/待执行五色语义', () => {
    expect(taskStatusClass('done')).toBe('done');
    expect(taskStatusClass('planning')).toBe('active');
    expect(taskStatusClass('running')).toBe('active');
    expect(taskStatusClass('partial')).toBe('partial');
    expect(taskStatusClass('error')).toBe('error');
    expect(taskStatusClass('ended')).toBe('error');
    expect(taskStatusClass('pending')).toBe('pending');
  });

  it('三类状态共用同一套语义', () => {
    const step: TaskStepStatus = 'running';
    const subtask: TaskSubtaskStatus = 'ended';
    const task: TaskStatus = 'partial';
    expect(taskStatusClass(step)).toBe('active');
    expect(taskStatusClass(subtask)).toBe('error');
    expect(taskStatusClass(task)).toBe('partial');
  });

  it('未知状态(未来新增值)回退 pending 灰块', () => {
    expect(taskStatusClass('weird' as AnyTaskStatus)).toBe('pending');
  });
});
