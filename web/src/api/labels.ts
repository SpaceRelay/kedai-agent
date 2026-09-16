// 任务模式 / 任务消息种类的**唯一中文文案源**(批次 G.2)。
//
// 背景:这些文案此前分别手写在 TaskBoard.vue(MODE_LABELS / MESSAGE_KIND_LABELS)与
// TaskModeSelect.vue(<option> 内联)两处,新增一个模式要改「后端枚举 + 发射点 + types.ts
// + task store switch + 两处组件文案」——漏改不会报错,只会在 UI 上暴露为空白/错文案。
//
// 收敛方式:
//   - 文案集中本文件,组件一律引用;
//   - 用 `satisfies Record<TaskRunMode, string>` 做**穷尽校验**:后端新增模式而此处漏配,
//     `vue-tsc` 直接报错(把「漏改」从运行期静默变成编译期失败)。
//   - MESSAGE_KIND_LABELS 的键来自后端 task_messages.kind,非联合类型,故用
//     `Record<string, string>` 并保留「未登记返回空」的既有语义。
import type { TaskRunMode } from './types';

/** 模式 → 短标签(任务卡头部展示)。穷尽校验:漏配模式编译期报错。 */
export const MODE_LABELS = {
  legacy: '三段式',
  solo: '单 Agent',
  multi: '多 Agent',
  plan: '先规划后批准',
  team: '团队协作',
  custom: '自定义流程',
} satisfies Record<TaskRunMode, string>;

/** 模式 → 下拉选项文案(选择器)。在短标签基础上标注默认项。 */
export const MODE_OPTION_LABELS = {
  legacy: '模式:三段式(默认)',
  solo: '模式:单 Agent',
  multi: '模式:多 Agent',
  plan: '模式:先规划后批准',
  team: '模式:团队协作',
  custom: '模式:自定义流程',
} satisfies Record<TaskRunMode, string>;

/** 下拉展示顺序(与后端 TaskRunMode 定义顺序一致,便于对照) */
export const MODE_ORDER: TaskRunMode[] = ['legacy', 'solo', 'multi', 'plan', 'team', 'custom'];

/**
 * 任务消息种类 → 小标签。键为后端 `task_messages.kind`(字符串,非联合类型),
 * 故不设穷尽校验;未登记种类返回空串(不显示标签,与既有行为一致)。
 * `normal` 为旧行遗留,不显示标签。
 */
export const MESSAGE_KIND_LABELS: Record<string, string> = {
  goal: '目标',
  result: '成果',
  followup: '追加',
  plan_chat: '规划对话',
};

/** 消息种类标签(未登记返回空串) */
export function messageKindLabel(kind: string): string {
  return MESSAGE_KIND_LABELS[kind] ?? '';
}
