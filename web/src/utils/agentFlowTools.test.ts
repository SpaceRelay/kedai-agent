import { describe, expect, it } from 'vitest';
import type { AgentFlowStep } from '../api/types';
import {
  cleanStepsTools,
  cleanStepTools,
  setStepToolMode,
  setStepToolsText,
  stepToolMode,
  stepToolsText,
} from './agentFlowTools';

function step(tools: AgentFlowStep['tools'] = null): AgentFlowStep {
  return {
    id: 's1',
    name: '步骤',
    enabled: true,
    goal: 'g',
    action: 'direct',
    generates: true,
    tools,
    tool_choice: 'auto',
    tool_choice_function: null,
    parallel_tool_calls: null,
  };
}

describe('stepToolMode(读回)', () => {
  it('null/undefined → none,空数组 → all,非空数组 → list', () => {
    expect(stepToolMode(step(null))).toBe('none');
    expect(stepToolMode(step(undefined))).toBe('none');
    expect(stepToolMode(step([]))).toBe('all');
    expect(stepToolMode(step(['write']))).toBe('list');
    expect(stepToolMode(step(['']))).toBe('list', '占位空串应视为 list(白名单输入中)');
  });
});

describe('setStepToolMode(切换)', () => {
  it('none → null;all → []', () => {
    const s = step(['write']);
    setStepToolMode(s, 'none');
    expect(s.tools).toBeNull();
    setStepToolMode(s, 'all');
    expect(s.tools).toEqual([]);
  });

  it('list → 占位空串,读回仍为 list(回归:修复前 list 分支置 [] 导致弹回 all)', () => {
    const s = step(null);
    setStepToolMode(s, 'list');
    expect(s.tools).toEqual(['']);
    expect(stepToolMode(s)).toBe('list');
  });

  it('list 分支保留已有非空列表(幂等,不重置为占位)', () => {
    const s = step(['write', 'read']);
    setStepToolMode(s, 'list');
    expect(s.tools).toEqual(['write', 'read']);
  });
});

describe('stepToolsText / setStepToolsText', () => {
  it('白名单文本 → 数组(逗号/中文逗号/空白分隔,去空项)', () => {
    const s = step([]);
    setStepToolsText(s, 'write, read,  计算器');
    expect(s.tools).toEqual(['write', 'read', '计算器']);
    expect(stepToolsText(s)).toBe('write, read, 计算器');
  });

  it('空输入 → 空数组(读回 all 而非 list)', () => {
    const s = step([]);
    setStepToolsText(s, '  ');
    expect(s.tools).toEqual([]);
    expect(stepToolMode(s)).toBe('all');
  });
});

describe('cleanStepTools(保存前清洗)', () => {
  it('null/undefined → null;[] (全部) 原样保留', () => {
    expect(cleanStepTools(null)).toBeNull();
    expect(cleanStepTools(undefined)).toBeNull();
    expect(cleanStepTools([])).toEqual([]);
  });

  it('去空白项;仅占位空串 → null(不使用)', () => {
    expect(cleanStepTools([''])).toBeNull();
    expect(cleanStepTools(['', ' '])).toBeNull();
    expect(cleanStepTools(['write', ' read ', ''])).toEqual(['write', 'read']);
    expect(cleanStepTools(['a', ' b '])).toEqual(['a', 'b']);
  });

  it('cleanStepsTools 就地清洗全部步骤', () => {
    const steps = [step(['write']), step(['']), step([])];
    cleanStepsTools(steps);
    expect(steps[0].tools).toEqual(['write']);
    expect(steps[1].tools).toBeNull();
    expect(steps[2].tools).toEqual([]);
  });
});
