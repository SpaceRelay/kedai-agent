import { describe, expect, it } from 'vitest';
import { splitTaskResult, AUDIT_MARK, FINAL_PLAN_MARK } from './taskResult';

// 批次 R1:任务 result 拆段纯函数单测。
// 契约对齐 server-rs:team 模式 result = 整合文本 + "\n\n## 审计结论\n" + 审计文本
// (task_engine/team.rs);plan 模式 result = 汇总文本 + "\n\n## 最终计划\n" + 最终计划段
// (task_engine/plan.rs)。两模式段结构各自独立,互不拆对方标记。
describe('splitTaskResult 任务结果拆段', () => {
  it('plan 模式:尾部「## 最终计划」段拆出,主卡不含该段', () => {
    const r = splitTaskResult('plan', '汇总成果\n\n## 最终计划\n1. 步骤一(done):成果');
    expect(r.main).toBe('汇总成果');
    expect(r.finalPlan).toContain(FINAL_PLAN_MARK);
    expect(r.finalPlan).toContain('步骤一(done)');
    expect(r.audit).toBe('');
  });

  it('team 模式:尾部「## 审计结论」段拆出(批次 4 契约回归锁定)', () => {
    const r = splitTaskResult('team', '整合文本\n\n## 审计结论\n审计通过');
    expect(r.main).toBe('整合文本');
    expect(r.audit).toContain(AUDIT_MARK);
    expect(r.audit).toContain('审计通过');
    expect(r.finalPlan).toBe('');
  });

  it('plan 模式无标记:主卡为原文,finalPlan 空', () => {
    const r = splitTaskResult('plan', '只有成果文本');
    expect(r.main).toBe('只有成果文本');
    expect(r.finalPlan).toBe('');
  });

  it('legacy/solo 模式:不拆任何段(即使文本含标记)', () => {
    const r = splitTaskResult('legacy', '含 ## 最终计划 文本也不拆');
    expect(r.main).toBe('含 ## 最终计划 文本也不拆');
    expect(r.finalPlan).toBe('');
    expect(r.audit).toBe('');
  });

  it('两模式段结构独立:team 不拆最终计划、plan 不拆审计结论', () => {
    const t = splitTaskResult('team', '正文\n\n## 最终计划\nx');
    expect(t.main).toContain('## 最终计划');
    expect(t.finalPlan).toBe('');
    const p = splitTaskResult('plan', '正文\n\n## 审计结论\ny');
    expect(p.main).toContain('## 审计结论');
    expect(p.audit).toBe('');
  });

  it('team 正文复述「## 审计结论」标题:按最后一处拆段,主卡不被截半', () => {
    // 实跑问题 2:审计结论原文作为汇总输入下发,模型可能在正文中复述该标题。
    // 后端把真正的审计段拼在最末尾,故必须取 lastIndexOf 而非首个。
    const r = splitTaskResult(
      'team',
      '正文开头\n\n## 审计结论\n(正文里复述的标题,后面还有正文)\n\n正文结尾\n\n## 审计结论\n审计通过',
    );
    expect(r.main).toBe('正文开头\n\n## 审计结论\n(正文里复述的标题,后面还有正文)\n\n正文结尾');
    expect(r.audit).toBe('## 审计结论\n审计通过');
  });

  it('plan 正文复述「## 最终计划」标题:按最后一处拆段', () => {
    const r = splitTaskResult('plan', '正文提到 ## 最终计划 这个词\n\n## 最终计划\n1. 步骤一');
    expect(r.main).toBe('正文提到 ## 最终计划 这个词');
    expect(r.finalPlan).toBe('## 最终计划\n1. 步骤一');
  });
});
