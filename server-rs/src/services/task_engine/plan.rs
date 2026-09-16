// plan 模式:只规划不执行(零副作用纪律)——产出计划落库后进 planned 待批准态,
// 放弃 = stop;批准(POST /api/tasks/{id}/approve,可携修改后计划)的续跑由本模块
// ApprovedPlanExecutor 承担(2026-08 实测修复:旧实现 approve 强制 solo 只吃 goal,
// 已批准计划仅落库存档,步骤永远 pending、step result 全空,任务却 done)。
// 规划调用复用 legacy 的 plan_task_retry(含解析/截断打捞/分级重试与 planner 追踪落库);
// 续跑执行段复用 solo 的 run_agent_loop 与 legacy 的 summarize_task_retry(语义对齐)。
// 结果契约(批次 R1):
// - planned 态:计划清单文本(「计划已产出,共 N 步:…」)落 tasks.result,语义 =
//   待批准的计划清单(批准前预览;经 TaskTerminal::AwaitApproval →
//   finalize_terminal → planned_mode_run 写入,只更新 result 列、不动状态);
// - 续跑完成:result = 汇总文本 + "\n\n## 最终计划\n" + 最终计划段(每步:名称、
//   状态 done/error、result 概要按字符截断 ≤200),前端按 `## ` 段拆成独立卡
//   (对齐 team.rs「## 审计结论」拆卡契约;两模式 result 段结构各自独立)。
use super::context::TaskRunContext;
use super::executor::{usage_as_output, ModeExecutor};
use super::solo::{run_agent_loop, AgentLoopCall};
use crate::agents::engine::AgentEngine;
use crate::models::types::{TaskEventKind, TaskStatus, TaskStep, TaskStepStatus, TokenUsage};
use crate::services::task_core::{TaskBackend, TaskTerminal};
use futures::future::BoxFuture;
use std::sync::Arc;

/// plan 批准续跑执行器:消费已批准计划(task.plan 非空时由 run_approved 派发)。
/// 逐步骤执行:每步一次 run_agent_loop(步骤 name+goal 为该步指令,整体目标与计划
/// 作上下文;虚拟 session 复用 task:{id}),完成即回写该步 status/result;单步失败
/// 记 error 继续后续步骤(对齐 legacy),取消即中断。全部步骤完成后
/// summarize_task_retry(SUMMARIZER_PROMPT + 空输出分级重试)汇总产出最终 result;
/// 含 error 步骤时终态 partial(对齐 legacy「有产出则 partial」语义)。
pub(crate) struct ApprovedPlanExecutor {
    svc: Arc<dyn TaskBackend>,
    engine: Arc<AgentEngine>,
    /// 已批准计划(approve 处已落库,可为用户修改版)
    plan: Vec<TaskStep>,
}

impl ApprovedPlanExecutor {
    pub(crate) fn new(
        svc: Arc<dyn TaskBackend>,
        engine: Arc<AgentEngine>,
        plan: Vec<TaskStep>,
    ) -> Self {
        ApprovedPlanExecutor { svc, engine, plan }
    }

    async fn run_inner(&self, ctx: TaskRunContext) -> Result<(TaskTerminal, TokenUsage), String> {
        let svc = &self.svc;
        let mut total = TokenUsage::default();
        let mut plan = self.plan.clone();
        let count = plan.len();
        let mut had_error = false;

        // approve 已置 planning,执行段归执行器所有:推进 running
        svc.set_status(&ctx.task_id, TaskStatus::Running);

        // ===== 逐步执行:每步独立 agent 调用,完成即回写该步 =====
        for i in 0..count {
            if *ctx.cancel.borrow() {
                // 取消:剩余 pending 步骤统一置 error「任务已停止」再收尾(与 team 口径对齐)
                fail_pending_steps_on_cancel(svc, &ctx.task_id, &mut plan);
                return Err("任务已停止".into());
            }
            plan[i].status = TaskStepStatus::Running;
            svc.set_plan(&ctx.task_id, &plan);

            // 当前步骤指令在前(该步 name+goal),整体目标与已批准计划作上下文在后。
            // 整体目标文本随每步重复下发是有意的:各步共用同一虚拟 session 但每步都是
            // 独立调用,重复携带上下文可保模型对全局意图的记忆,不轻易精简
            let goal = format!(
                "当前步骤(第 {}/{} 步):{}\n{}\n\n整体目标与已批准计划(执行上下文):\n{}\n\n请完成当前步骤并直接产出该步骤的结果。",
                i + 1,
                count,
                plan[i].name,
                plan[i].goal,
                ctx.goal
            );
            let call = AgentLoopCall {
                task_id: ctx.task_id.clone(),
                session_id: format!("task:{}", ctx.task_id),
                goal,
                settings: ctx.settings.clone(),
                character_id: ctx.character_id.clone(),
                phase: "agent",
                step_index: Some(i),
                label: format!("步骤 {}", i + 1),
            };
            match run_agent_loop(svc.clone(), self.engine.clone(), call, ctx.cancel.clone()).await {
                Ok((text, usage)) => {
                    total.prompt_tokens += usage.prompt_tokens;
                    total.completion_tokens += usage.completion_tokens;
                    total.total_tokens += usage.total_tokens;
                    svc.record_usage(&ctx.task_id, "agent", Some(i), &usage_as_output(&usage));
                    plan[i].status = TaskStepStatus::Done;
                    plan[i].result = text;
                }
                Err(e) => {
                    plan[i].status = TaskStepStatus::Error;
                    plan[i].result = e.clone();
                    svc.set_plan(&ctx.task_id, &plan);
                    if *ctx.cancel.borrow() {
                        // 取消:剩余 pending 步骤统一置 error「任务已停止」再收尾(与 team 口径对齐)
                        fail_pending_steps_on_cancel(svc, &ctx.task_id, &mut plan);
                        return Err("任务已停止".into());
                    }
                    // 单步失败记 error 继续后续步骤(对齐 legacy 执行段语义)
                    had_error = true;
                    continue;
                }
            }
            svc.set_plan(&ctx.task_id, &plan);
        }

        // ===== 汇总:SUMMARIZER_PROMPT + 空输出分级重试(对齐 legacy 执行段语义) =====
        if *ctx.cancel.borrow() {
            return Err("任务已停止".into());
        }
        let Some(task) = svc.get(&ctx.task_id) else {
            return Err("任务不存在".into());
        };
        let out =
            super::retry::summarize_task_retry(svc.as_ref(), &task, &plan, &ctx.cancel).await?;
        svc.record_usage(&ctx.task_id, "summary", None, &out);
        total.prompt_tokens += out.prompt_tokens;
        total.completion_tokens += out.completion_tokens;
        total.total_tokens += out.prompt_tokens + out.completion_tokens;
        // 结果契约(批次 R1,对齐 team.rs「## 审计结论」拆卡):result = 汇总文本 +
        // 「## 最终计划」段(各步名称/状态/result 概要);planned 态写入的计划清单
        // 文本由 TaskTerminal::Complete → complete_mode_run 以此整体覆盖
        let text = format!(
            "{}\n\n## 最终计划\n{}",
            out.text.trim(),
            format_final_plan_section(&plan)
        );
        // 含 error 步骤但成果已产出 → partial(对齐 legacy「有产出则 partial」语义)
        let status = if had_error {
            TaskStatus::Partial
        } else {
            TaskStatus::Done
        };
        Ok((
            TaskTerminal::Complete {
                result: text,
                status,
                error: None,
            },
            total,
        ))
    }
}

/// 「## 最终计划」段单步概要长度上限(按字符,中文安全):步骤 result 原文可能
/// 很长,仅概要进最终 result,防 result 膨胀(批次 R1)。
const FINAL_PLAN_STEP_SUMMARY_MAX: usize = 200;

/// 「## 最终计划」段内容(批次 R1;前端拆卡契约,对齐 team.rs「## 审计结论」:
/// 前端按 `## ` 段拆成独立卡)。每步一行:「序号. 名称(状态):result 概要」,
/// 状态取 TaskStepStatus::as_str()(done/error 等线格式)。
/// 概要两条防线:① 换行折叠为空格——步骤原文可能含 `## ` 标题行,不折叠会在
/// 段内制造假标题行、撑破前端按 `## ` 段拆卡的契约;② 按字符截断 ≤200。
/// 步骤 result 为空时行尾不带冒号与概要(保持简洁)。
fn format_final_plan_section(plan: &[TaskStep]) -> String {
    let mut out = String::new();
    for (i, s) in plan.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("{}. {}({})", i + 1, s.name, s.status.as_str()));
        let flat = s.result.replace(['\r', '\n'], " ");
        let flat = flat.trim();
        if !flat.is_empty() {
            let summary: String = flat.chars().take(FINAL_PLAN_STEP_SUMMARY_MAX).collect();
            out.push_str(&format!(":{summary}"));
        }
    }
    out
}

/// 取消兜底:剩余 pending 步骤统一置 error「任务已停止」并落库(与 team 口径对齐:
/// 收尾只写任务终态,步骤态归执行器负责,不留永远 pending 的步骤)。
/// 本执行器单线程逐步推进,取消检查点不存在 running 态步骤(当前步已在 Err 分支置
/// error),故只需扫 pending。
fn fail_pending_steps_on_cancel(svc: &Arc<dyn TaskBackend>, task_id: &str, plan: &mut [TaskStep]) {
    let mut dirty = false;
    for s in plan.iter_mut() {
        if s.status == TaskStepStatus::Pending {
            s.status = TaskStepStatus::Error;
            s.result = "任务已停止".into();
            dirty = true;
        }
    }
    if dirty {
        svc.set_plan(task_id, plan);
    }
}

impl ModeExecutor for ApprovedPlanExecutor {
    fn run<'a>(
        &'a self,
        ctx: TaskRunContext,
    ) -> BoxFuture<'a, Result<(TaskTerminal, TokenUsage), String>> {
        Box::pin(async move { self.run_inner(ctx).await })
    }
}

/// plan 执行器:只需任务后端(规划/落库/事件),不经聊天引擎。
pub(crate) struct PlanExecutor {
    svc: Arc<dyn TaskBackend>,
}

impl PlanExecutor {
    pub(crate) fn new(svc: Arc<dyn TaskBackend>) -> Self {
        PlanExecutor { svc }
    }
}

impl ModeExecutor for PlanExecutor {
    fn run<'a>(
        &'a self,
        ctx: TaskRunContext,
    ) -> BoxFuture<'a, Result<(TaskTerminal, TokenUsage), String>> {
        Box::pin(async move {
            // 显式置 planning(run 入口 reset_task 已是 planning,幂等;语义上规划阶段归执行器所有)
            self.svc.set_status(&ctx.task_id, TaskStatus::Planning);
            let (steps, out) = super::retry::plan_task_retry(
                self.svc.as_ref(),
                &ctx.task_id,
                &ctx.goal,
                ctx.character_id.as_deref(),
                &ctx.cancel,
            )
            .await?;
            // 规划产出后落库前再查一次取消:已停止则不进 planned(终态由收尾写 ended)
            if *ctx.cancel.borrow() {
                return Err("任务已停止".into());
            }
            // usage 落库(批次 4.3b 口径补齐:规划轮 phase=planner;批准后的执行段
            // usage 由 approve 续跑的 ApprovedPlanExecutor 逐步补记,phase=agent/summary)
            self.svc.record_usage(&ctx.task_id, "planner", None, &out);
            // 计划落库 → planned 待批准 → 广播批准请求;全程不执行任何步骤(零副作用)
            self.svc.set_plan(&ctx.task_id, &steps);
            self.svc.set_status(&ctx.task_id, TaskStatus::Planned);
            self.svc.emit_event(
                TaskEventKind::ApprovalRequired,
                &ctx.task_id,
                None,
                Some(TaskStatus::Planned),
                Some("计划已产出,待批准".into()),
            );
            // 批次 R1:本清单文本经 TaskTerminal::AwaitApproval 落 tasks.result
            //(planned 态 result 语义 = 待批准的计划清单,批准前预览用)
            let mut summary = format!("计划已产出,共 {} 步:", steps.len());
            for (i, s) in steps.iter().enumerate() {
                summary.push_str(&format!("\n{}. {}:{}", i + 1, s.name, s.goal));
            }
            let usage = TokenUsage {
                prompt_tokens: out.prompt_tokens,
                completion_tokens: out.completion_tokens,
                total_tokens: out.prompt_tokens + out.completion_tokens,
                ..Default::default()
            };
            Ok((TaskTerminal::AwaitApproval { plan_text: summary }, usage))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str, status: TaskStepStatus, result: &str) -> TaskStep {
        TaskStep {
            name: name.into(),
            goal: String::new(),
            status,
            result: result.into(),
        }
    }

    /// 每步一行:序号、名称、状态(as_str 线格式)、result 概要;空结果行尾不带冒号
    #[test]
    fn final_plan_section_lists_steps_with_status() {
        let plan = vec![
            step("搜集资料", TaskStepStatus::Done, "资料摘要"),
            step("撰写正文", TaskStepStatus::Error, "模型超时"),
            step("空步", TaskStepStatus::Done, ""),
        ];
        let section = format_final_plan_section(&plan);
        assert!(section.contains("1. 搜集资料(done):资料摘要"), "{section}");
        assert!(section.contains("2. 撰写正文(error):模型超时"), "{section}");
        assert!(section.contains("3. 空步(done)"), "{section}");
        assert!(
            !section.contains("空步(done):"),
            "空结果行尾不应带冒号: {section}"
        );
    }

    /// 概要按字符截断 ≤200(中文安全,不切 char 边界),防 result 膨胀
    #[test]
    fn final_plan_section_truncates_long_step_result() {
        let long = "好".repeat(250);
        let section = format_final_plan_section(&[step("长步", TaskStepStatus::Done, &long)]);
        assert!(section.contains(&"好".repeat(200)), "{section}");
        assert!(
            !section.contains(&"好".repeat(201)),
            "概要应截断到 200 字符: {} chars",
            section.chars().count()
        );
    }

    /// 概要折叠换行为空格:步骤原文含 `## ` 标题行时不得在段内制造假标题行,
    /// 防撑破前端按 `## ` 段拆卡的契约(每步恒一行)
    #[test]
    fn final_plan_section_collapses_newlines() {
        let plan = [step(
            "多行步",
            TaskStepStatus::Done,
            "第一行\n\n## 伪造标题\n第二行",
        )];
        let section = format_final_plan_section(&[plan[0].clone()]);
        let lines: Vec<&str> = section.lines().collect();
        assert_eq!(lines.len(), 1, "每步恒一行: {section}");
        assert!(section.contains("第一行"), "{section}");
        assert!(section.contains("第二行"), "{section}");
        assert!(!section.contains("\n## "), "换行应被折叠: {section}");
    }
}
