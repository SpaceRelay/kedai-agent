// Agent 执行流程步骤的工具模式三态工具函数
// 与后端 PlanStep.tools 语义对齐:null=不使用工具;[]=全部工具;非空数组=白名单。
// 从 SettingsModal.vue 抽出为纯函数,便于单测(含「白名单模式切换」回归测试)。

import type { AgentFlowStep } from '../api/types';

/** 工具模式三态 */
export type StepToolMode = 'none' | 'all' | 'list';

/**
 * 由 tools 字段读回当前模式:
 *   null/undefined → none;空数组 → all;非空数组(含仅占位空串的 ['']) → list
 */
export function stepToolMode(s: Pick<AgentFlowStep, 'tools'>): StepToolMode {
  if (s.tools === null || s.tools === undefined) return 'none';
  return s.tools.length === 0 ? 'all' : 'list';
}

/**
 * 切换工具模式(直接写回 s.tools):
 *   none → null;all → [];list → 非空数组(占位空串,由白名单输入框填充后保存时清洗)
 * 注意:list 必须产生非空数组,否则 stepToolMode 读回 all,下拉弹回、白名单输入框不显示。
 */
export function setStepToolMode(s: Pick<AgentFlowStep, 'tools'>, mode: StepToolMode): void {
  if (mode === 'none') {
    s.tools = null;
  } else if (mode === 'all') {
    s.tools = [];
  } else {
    // 白名单:已有非空列表保留(来回切换不清空);否则置占位空串等待用户输入
    s.tools = Array.isArray(s.tools) && s.tools.length > 0 ? s.tools : [''];
  }
}

/** 白名单文本 → tools 数组(逗号/中文逗号/空白分隔,去空白与空项) */
export function stepToolsText(s: Pick<AgentFlowStep, 'tools'>): string {
  return (s.tools ?? []).join(', ');
}

/** 白名单输入文本 → tools 数组 */
export function setStepToolsText(s: Pick<AgentFlowStep, 'tools'>, text: string): void {
  s.tools = text
    .split(/[,，\s]+/)
    .map((t) => t.trim())
    .filter(Boolean);
}

/**
 * 保存前清洗 tools:去空白项。语义区分:
 *   - 显式 [] (全部工具) 原样保留为 []——绝不能转成 null(否则"全部"变"不使用")
 *   - 仅占位空串 [''](选了白名单但没填工具名)→ null(不使用),避免提交空字符串
 *   - 真实非空列表去空白项后保留
 */
export function cleanStepTools(tools: AgentFlowStep['tools']): AgentFlowStep['tools'] {
  if (tools === null || tools === undefined) return null;
  const cleaned = tools
    .map((t) => (typeof t === 'string' ? t.trim() : String(t)))
    .filter((t) => t.length > 0);
  if (cleaned.length > 0) return cleaned;
  // 清洗后为空:显式 [] = 全部工具(保留);[''] 占位 = 白名单未填(不使用)
  return tools.length === 0 ? [] : null;
}

/** 全部步骤保存前统一清洗 tools(就地修改步骤数组元素) */
export function cleanStepsTools(steps: AgentFlowStep[]): void {
  for (const s of steps) {
    s.tools = cleanStepTools(s.tools);
  }
}

/**
 * 步骤工具配置的警示文案(F8,2026-09-10 六模式实跑修复)。
 * 背景:`tools: []` 语义是「全部工具」(非「不使用」),极易误配——实测「理解意图」
 * 这类 generates=false 的分析步骤被配成全部工具,实际下发 11 个工具(含编排/写类)。
 * 返回需展示的警示列表(空数组 = 无警示);纯函数便于单测。
 */
export function stepToolsWarnings(
  s: Pick<AgentFlowStep, 'tools' | 'generates'>,
): string[] {
  const mode = stepToolMode(s);
  const out: string[] = [];
  if (mode === 'all') {
    out.push(
      '「全部工具」会下发全部已注册工具(含 agentgo/agentend 等编排类与 write 等写类);仅需要自由选用工具的生成步骤才选它。',
    );
  }
  if (s.generates === false && mode === 'all') {
    out.push(
      '本步骤已设为「不生成正文」(如「理解意图」),通常只需只读工具或不用工具;选「全部工具」会把它当作执行步骤,建议改为「不使用工具」或「白名单」。',
    );
  }
  return out;
}
