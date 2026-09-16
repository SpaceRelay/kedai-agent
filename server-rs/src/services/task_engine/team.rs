// team 模式(批次 4.3b,首版只做自动拓扑):规划器拆目标并归并 2~4 个主 agent
//(每主 1~4 子目标,全局 ≤15 封顶)→ 各主并行跑 solo 同款工具自循环(独立 run_id/
// task:{id}:main:{n} 虚拟 session、AbortFlag、cancel 同源;可经 agentgo 派子 agent,
// 子 agent 路径与 multi 相同);主 agent 内按子目标逐个独立执行(每子目标一次调用,
// 各写各自步骤的 result/status,同一主复用同一虚拟 session 保持人设一致)→
// 审计 agent 一致性/质量/覆盖度审查(缺漏打回指定主补做,最多 1 轮;补做完成后
// 追加一次终审——只产出结论文本,不再打回)→ 升华整合输出。
// 结果契约:result = 整合文本 + "\n\n## 审计结论\n" + 审计文本(前端按此拆卡);
// 无打回时审计文本 = 首次审计结论,有打回时 = 终审结论。
// plan 步骤名 = 「【主Agent-N】子目标名」(前端分工卡按此前缀分组)。
// 手动拓扑(用户指定主 agent 数量/人设/分工)预留,待前端入口批次(docs/功能.md)。
use super::context::TaskRunContext;
use super::executor::{usage_as_output, ModeExecutor};
use super::solo::{run_agent_loop, AgentLoopCall};
use crate::agents::engine::AgentEngine;
use crate::models::types::{
    LlmMessage, TaskEventKind, TaskStatus, TaskStep, TaskStepStatus, TokenUsage,
};
use crate::services::prompt_kit::untrusted_boundary;
use crate::services::task_core::prompt_consts::{
    SUMMARIZER_PROMPT, TEAM_AUDIT_PROMPT, TEAM_FINAL_AUDIT_PROMPT, TEAM_PLANNER_PROMPT,
};
use crate::services::task_core::{TaskBackend, TaskGenOutput, TaskTerminal};
use futures::future::BoxFuture;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::task::JoinSet;

/// run_mains_parallel 并行 JoinSet 的单主产出:(主序号, [(子目标序号, 子目标名, 生成结果)])
type MainJoinOutput = (
    usize,
    Vec<(usize, String, Result<(String, TokenUsage), String>)>,
);

/// 主 agent 分工数上限(规划器约定 2~4;超出部分归并进末位主 agent)
const TEAM_MAX_MAINS: usize = 4;
/// 每主 agent 子目标数上限(规划器约定 1~4;超出部分截断并记日志)
const TEAM_MAX_GOALS_PER_MAIN: usize = 4;
/// 全局子目标数上限(4 主 × 4 子 = 16 可超,超出部分从末位主倒序截断,每主至少留 1)
const TEAM_MAX_GOALS_GLOBAL: usize = 15;
/// 规划/审计调用的温度(结构化 JSON 输出,与 legacy planner 同口径)
const TEAM_JSON_TEMPERATURE: f64 = 0.3;
/// 规划调用初始 max_tokens(推理模型 reasoning 与正文共用预算,同 PLAN_INITIAL_MAX_TOKENS)
const TEAM_PLAN_INITIAL_MAX_TOKENS: u32 = 2048;
/// 规划解析最大尝试次数(截断/空输出翻倍预算,同 PLAN_MAX_ATTEMPTS)
const TEAM_PLAN_MAX_ATTEMPTS: u32 = 3;
/// 重试预算翻倍上限(与设置页 max_tokens 上限一致)
const TEAM_RETRY_MAX_TOKENS_CAP: u32 = 131_072;
/// 结构化/长文本阶段(审计、终审、汇总)的初始输出预算下限。
/// 2026-09-15 实录:审计 `max_tokens=10000` 被 reasoning 吃掉 9894、正文仅 178 字符
/// 即被截断,自愈翻倍到 20000 重发才成功——首发即给足可省掉这一整个 50 秒左右
/// 的空烧往返。默认 `default_max_tokens=1024` 时更小,结构化 JSON 必然腰斩。
const TEAM_STRUCTURED_MIN_TOKENS: u32 = 16384;

/// 一个主 agent 的分工:分工名 + 子目标(1~4 个)
#[derive(Debug, Clone)]
struct TeamMain {
    name: String,
    goals: Vec<TaskStep>,
}

/// 审计打回条目:定位粒度到「主 agent 的某个子目标」。
/// `step` 为 1-based 的主内子目标序号(审计输出新增可选字段);缺省(None)= 审计未
/// 指明子目标,退回整主补做,保持对旧审计输出的兼容。定位到具体子目标可避免
/// 「打回一个子目标却重跑该主全部子目标、各再造一版」的版本爆炸(实跑问题 2)。
#[derive(Debug, Clone, PartialEq)]
struct Kickback {
    /// 主 agent 序号(0-based)
    main: usize,
    /// 主内子目标序号(0-based;None = 整主)
    step: Option<usize>,
    /// 补做指令
    instruction: String,
}

/// 审计结论:通过/打回列表/结论文本。
/// 解析失败或输出截断时**按未通过兜底**(实跑问题 2:旧实现默认 pass=true,审计可
/// 静默失效;审计是质量闸门,读不出明确通过就不应判定通过)——结论段给干净说明,
/// 不把半截 JSON 原样灌进 result;打回为空时由调用方映射为 partial 终态。
#[derive(Debug, Clone)]
struct AuditVerdict {
    pass: bool,
    kickbacks: Vec<Kickback>,
    conclusion: String,
}

/// 规划器输出拓扑的原始 JSON 形态
#[derive(Deserialize)]
struct TeamTopologyRaw {
    mains: Vec<TeamMainRaw>,
}

#[derive(Deserialize)]
struct TeamMainRaw {
    name: String,
    #[serde(default)]
    goals: Vec<TaskStep>,
}

/// 解析 team 规划输出:剥 markdown 代码块 → 取首个 '{' 到末个 '}' 子串 → serde 解析;
/// 过滤空分工/空子目标,归并超编主 agent(>4 并入第 4 位)、截断超编子目标(每主 >4
/// 截前 4),再做全局 ≤15 封顶(4×4=16 可超,从末位主倒序截断,每主至少留 1)。
/// mains 为空报错(交给重试);仅 1 个主 agent 时接受(降级拓扑,记日志),不强行拆分。
fn parse_team_topology(text: &str) -> Result<Vec<TeamMain>, String> {
    let t = text.trim();
    let stripped = t
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let candidate = match (stripped.find('{'), stripped.rfind('}')) {
        (Some(lo), Some(hi)) if lo < hi => &stripped[lo..=hi],
        _ => stripped,
    };
    let raw: TeamTopologyRaw =
        serde_json::from_str(candidate).map_err(|e| format!("解析团队拓扑失败: {e}"))?;
    let mut mains: Vec<TeamMain> = raw
        .mains
        .into_iter()
        .filter(|m| !m.name.trim().is_empty())
        .map(|m| TeamMain {
            name: m.name.trim().to_string(),
            goals: m
                .goals
                .into_iter()
                .filter(|g| !g.name.trim().is_empty() || !g.goal.trim().is_empty())
                .collect(),
        })
        .filter(|m| !m.goals.is_empty())
        .collect();
    if mains.is_empty() {
        return Err("解析团队拓扑失败: 规划输出不含任何有效主 agent 分工".into());
    }
    // 超编归并:第 4 位之后的主 agent 子目标并入第 4 位
    if mains.len() > TEAM_MAX_MAINS {
        let tail = mains.split_off(TEAM_MAX_MAINS);
        let merged: usize = tail.iter().map(|m| m.goals.len()).sum();
        for m in tail {
            mains[TEAM_MAX_MAINS - 1].goals.extend(m.goals);
        }
        tracing::warn!(
            merged_goals = merged,
            "team 规划主 agent 超编,已归并进末位主 agent"
        );
    }
    // 每主子目标超编截断(保留前 TEAM_MAX_GOALS_PER_MAIN)
    for (i, m) in mains.iter_mut().enumerate() {
        if m.goals.len() > TEAM_MAX_GOALS_PER_MAIN {
            tracing::warn!(
                main = i + 1,
                dropped = m.goals.len() - TEAM_MAX_GOALS_PER_MAIN,
                "team 规划子目标超编,已截断"
            );
            m.goals.truncate(TEAM_MAX_GOALS_PER_MAIN);
        }
    }
    // 全局子目标 ≤15 封顶(4 主 × 4 子 = 16 可超):从末位主倒序截断,每主至少留 1
    let mut total: usize = mains.iter().map(|m| m.goals.len()).sum();
    if total > TEAM_MAX_GOALS_GLOBAL {
        let mut dropped = 0usize;
        for m in mains.iter_mut().rev() {
            if total <= TEAM_MAX_GOALS_GLOBAL {
                break;
            }
            let overflow = total - TEAM_MAX_GOALS_GLOBAL;
            // 每主至少保留 1 个子目标(空分工已在前面过滤,此处守住不再制造空主)
            let drop = overflow.min(m.goals.len().saturating_sub(1));
            m.goals.truncate(m.goals.len() - drop);
            total -= drop;
            dropped += drop;
        }
        tracing::warn!(
            dropped = dropped,
            "team 规划子目标全局超编,已按 ≤15 封顶截断"
        );
    }
    if mains.len() == 1 {
        tracing::warn!("team 规划仅产出 1 个主 agent,按降级拓扑执行");
    }
    Ok(mains)
}

/// 解析审计输出:{"通过":bool,"打回":[{"main":1-based 序号,"step":主内 1-based 子目标
/// 序号(可选),"instruction":"..."}],"结论":"..."}。
/// 解析失败/截断一律按**未通过**兜底(见 AuditVerdict 文档;实跑问题 2 修复);
/// 打回序号越界的条目丢弃(防 LLM 幻觉引用不存在的主 agent);同一定位(主 + 子目标)
/// 重复打回只保留首条(防同一虚拟 session 在补做轮被并发 spawn 两次)。
fn parse_audit(text: &str, mains_count: usize) -> AuditVerdict {
    let t = text.trim();
    let stripped = t
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let candidate = match (stripped.find('{'), stripped.rfind('}')) {
        (Some(lo), Some(hi)) if lo < hi => &stripped[lo..=hi],
        _ => stripped,
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) else {
        // 解析失败(含推理模型烧光预算的腰斩 JSON):审计质量闸门读不出明确通过,
        // 按未通过处理;结论段给干净说明——半截 JSON 原样进 result「## 审计结论」
        // 卡既难看又误导;原文记 warn 字段备查
        tracing::warn!(
            head = stripped.chars().take(120).collect::<String>(),
            "team 审计输出无法解析,按未通过兜底"
        );
        let conclusion = if stripped.starts_with('{') {
            "(审计输出不完整,无法确认通过)".to_string()
        } else {
            t.to_string()
        };
        return AuditVerdict {
            pass: false,
            kickbacks: Vec::new(),
            conclusion,
        };
    };
    let pass = v.get("通过").and_then(|b| b.as_bool()).unwrap_or(false);
    let conclusion = v
        .get("结论")
        .and_then(|s| s.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        // JSON 合法但缺「结论」:回填固定文案,不把裸 JSON(整段原文是 `{"通过":…}`)
        // 灌进 result 的「## 审计结论」卡——那既难看又误导(实跑问题 2 连带给修复)
        .unwrap_or_else(|| "(审计未给出结论)".to_string());
    let mut kickbacks: Vec<Kickback> = Vec::new();
    if let Some(arr) = v.get("打回").and_then(|a| a.as_array()) {
        for item in arr {
            let main = item.get("main").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
            // step 为可选的主内 1-based 子目标序号;缺省 = 整主补做(兼容旧输出)
            let step = item
                .get("step")
                .and_then(|n| n.as_u64())
                .map(|n| n as usize);
            let instruction = item
                .get("instruction")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if main >= 1 && main <= mains_count && !instruction.is_empty() {
                let idx = main - 1;
                // step 越界(0)视为未指明 → 整主补做,不错失问题上报;先归一到 0-based
                // 再做去重比较(否则 1-based 与已存 0-based 不一致,去重失效)
                let step0 = step.filter(|s| *s >= 1).map(|s| s - 1);
                // 同一定位(主 + 子目标)重复打回只保留首条(补做轮每定位一个 spawn)
                if kickbacks.iter().any(|k| k.main == idx && k.step == step0) {
                    tracing::warn!(main = main, "team 审计打回条目与既有条目同定位,已保留首条");
                    continue;
                }
                kickbacks.push(Kickback {
                    main: idx,
                    step: step0,
                    instruction,
                });
            } else {
                tracing::warn!(
                    main = main,
                    "team 审计打回条目无效(序号越界或指令为空),已丢弃"
                );
            }
        }
    }
    AuditVerdict {
        pass,
        kickbacks,
        conclusion,
    }
}

/// 截断自愈预算决策(纯函数):仅 finish_reason=length 触发,预算翻倍、不低于
/// `HEAL_BUDGET_FLOOR` 并封顶。对齐 solo/custom 逐步执行的自愈语义
///(2026-09-03 实测:team 审计/终审/汇总直调 generate_text 无自愈,推理模型
/// reasoning 烧光 1024 预算产出腰斩 JSON;2026-09-15 实录审计 10000 预算被
/// reasoning 吃掉 9894、正文仅 178 字符)。
/// 算法与触发条件收敛在 `utils::retry`(2026-09-13 批次 4.1 四路合一;
/// 2026-09-15 引入下限);本路径单次重发。
fn trunc_heal_budget(finish_reason: Option<&str>, max_tokens: u32) -> Option<u32> {
    if finish_reason != Some("length") {
        return None;
    }
    crate::utils::retry::heal_budget_with_floor(
        max_tokens,
        TEAM_RETRY_MAX_TOKENS_CAP,
        crate::utils::retry::HEAL_BUDGET_FLOOR,
    )
}

/// 审计/终审/汇总的实际输出预算:在用户设置之上保证 `TEAM_STRUCTURED_MIN_TOKENS`
/// 下限(结构化 JSON 与长文本汇总在小预算下必被 reasoning 挤断),并受封顶约束。
fn structured_budget(setting: u32) -> u32 {
    // 下限与封顶都是常量,且 MIN <= CAP,故 clamp 不会 panic(见两常量定义处)
    setting.clamp(TEAM_STRUCTURED_MIN_TOKENS, TEAM_RETRY_MAX_TOKENS_CAP)
}

/// 推理感知的截断自愈预算(纯函数,2026-09-15):在 `trunc_heal_budget` 之上,
/// 若该轮推理已挤占预算(有 reasoning 观测),按「已消耗 + 原预算」给足,让正文有
/// 与原预算等宽的空间,避免翻倍后仍被推理吃掉、再烧一个完整往返。
///
/// 只在这一形态加强:无 reasoning 观测(非思考模型)或推理未挤占时,维持既有翻倍语义。
fn trunc_heal_budget_reasoning_aware(
    finish_reason: Option<&str>,
    max_tokens: u32,
    completion_tokens: i64,
    reasoning_tokens: i64,
) -> Option<u32> {
    let base = trunc_heal_budget(finish_reason, max_tokens)?;
    // 无推理观测(0 = 非思考模型或上游未下发)时不加强,保持既有语义
    if reasoning_tokens <= 0 || completion_tokens <= 0 {
        return Some(base);
    }
    // reasoning 与正文共用 completion 预算,推理占满时正文只能腰斩;
    // 目标 = 已消耗 + 原预算(正文至少还有与原预算等宽的空间)
    let target = (completion_tokens.max(0) as u32)
        .saturating_add(max_tokens)
        .min(TEAM_RETRY_MAX_TOKENS_CAP);
    Some(target.max(base))
}

/// 审计/终审/汇总共用的纯生成出口:先按原预算调 generate_text,finish=length
/// 时按「翻倍(不低于下限)+ 推理感知」重发一次(截断自愈)。两次调用均经
/// generate_text 落 task_llm_calls,调用情况面板可见「截断 → 提高预算重发」
/// 完整链路;token 用量两次都入 total,record_usage 两次都落(与 solo 自愈留痕同口径)。
#[allow(clippy::too_many_arguments)]
async fn generate_text_healed(
    svc: &Arc<dyn TaskBackend>,
    task_id: &str,
    phase: &str,
    messages: Vec<LlmMessage>,
    max_tokens: u32,
    temperature: f64,
    top_p: f64,
    cancel: &watch::Receiver<bool>,
    total: &mut TokenUsage,
) -> Result<TaskGenOutput, String> {
    let mut out = svc
        .generate_text(
            task_id,
            phase,
            None,
            messages.clone(),
            Vec::new(),
            max_tokens,
            temperature,
            top_p,
            cancel.clone(),
        )
        .await?;
    svc.record_usage(task_id, phase, None, &out);
    total.prompt_tokens += out.prompt_tokens;
    total.completion_tokens += out.completion_tokens;
    total.total_tokens += out.prompt_tokens + out.completion_tokens;
    let Some(retry_budget) = trunc_heal_budget_reasoning_aware(
        out.finish_reason.as_deref(),
        max_tokens,
        out.completion_tokens,
        out.reasoning_tokens,
    ) else {
        return Ok(out);
    };
    tracing::warn!(
        phase = phase,
        max_tokens = max_tokens,
        reasoning_tokens = out.reasoning_tokens,
        completion_tokens = out.completion_tokens,
        retry_max_tokens = retry_budget,
        "team 纯生成调用截断,提高输出上限重发"
    );
    svc.emit_event(
        TaskEventKind::AgentStatus,
        task_id,
        None,
        None,
        Some(format!(
            "(截断自愈){phase} 输出截断,输出上限提高至 {retry_budget} 重发"
        )),
    );
    out = svc
        .generate_text(
            task_id,
            phase,
            None,
            messages,
            Vec::new(),
            retry_budget,
            temperature,
            top_p,
            cancel.clone(),
        )
        .await?;
    svc.record_usage(task_id, phase, None, &out);
    total.prompt_tokens += out.prompt_tokens;
    total.completion_tokens += out.completion_tokens;
    total.total_tokens += out.prompt_tokens + out.completion_tokens;
    Ok(out)
}

/// team 执行器:任务后端(规划/审计/汇总纯生成出口 + 落库)+ 聊天引擎(主 agent 工具循环)。
pub(crate) struct TeamExecutor {
    svc: Arc<dyn TaskBackend>,
    engine: Arc<AgentEngine>,
}

impl TeamExecutor {
    pub(crate) fn new(svc: Arc<dyn TaskBackend>, engine: Arc<AgentEngine>) -> Self {
        TeamExecutor { svc, engine }
    }

    /// team 规划:TEAM_PLANNER_PROMPT + 世界书背景(untrusted 包裹),解析失败按
    /// finish_reason 分级重试(截断/空输出翻倍预算,最多 TEAM_PLAN_MAX_ATTEMPTS 次)。
    /// 调用追踪经 generate_text 统一出口落 task_llm_calls(phase=planner)。
    async fn plan_team(
        &self,
        ctx: &TaskRunContext,
    ) -> Result<(Vec<TeamMain>, TaskGenOutput), String> {
        let mut sys = String::from(TEAM_PLANNER_PROMPT);
        let world = self.svc.world_context(ctx.character_id.as_deref());
        if !world.is_empty() {
            sys.push_str(&format!("\n\n{}", untrusted_boundary("world_book", &world)));
        }
        let messages = vec![
            LlmMessage::plain("system", &sys),
            LlmMessage::plain("user", &ctx.goal),
        ];
        let mut max_tokens = TEAM_PLAN_INITIAL_MAX_TOKENS;
        let mut last_err = String::from("规划器未产出有效拓扑");
        for attempt in 1..=TEAM_PLAN_MAX_ATTEMPTS {
            if attempt > 1 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if *ctx.cancel.borrow() {
                    return Err("任务已停止".into());
                }
            }
            let out = self
                .svc
                .generate_text(
                    &ctx.task_id,
                    "planner",
                    None,
                    messages.clone(),
                    Vec::new(),
                    max_tokens,
                    TEAM_JSON_TEMPERATURE,
                    ctx.settings.default_top_p,
                    ctx.cancel.clone(),
                )
                .await?;
            match parse_team_topology(&out.text) {
                Ok(mains) => return Ok((mains, out)),
                Err(e) => last_err = e,
            }
            // 失败 attempt 同步入账 usage(口径同 plan_task_retry;2026-09-10 实测修复)
            self.svc.record_usage(&ctx.task_id, "planner", None, &out);
            let reason = out.finish_reason.as_deref().unwrap_or("");
            tracing::warn!(
                attempt = attempt,
                finish_reason = reason,
                error = last_err.clone(),
                "team 规划输出解析失败,准备重试"
            );
            if reason == "length" || out.text.trim().is_empty() {
                // 与 trunc_heal_budget 同源(批次 4.1 收敛后的补充):翻倍+封顶,达上限保持原值
                max_tokens =
                    crate::utils::retry::doubled_heal_budget(max_tokens, TEAM_RETRY_MAX_TOKENS_CAP)
                        .unwrap_or(max_tokens);
            }
        }
        Err(format!(
            "{last_err}(已重试 {} 次)",
            TEAM_PLAN_MAX_ATTEMPTS - 1
        ))
    }

    /// 一个主 agent 的完整执行:按子目标逐个独立调用 run_agent_loop(每子目标一次
    /// agent 调用,同一主的多个子目标复用同一虚拟 session task:{id}:main:{n} 保持
    /// 人设一致)。返回 (主序号 0-based, 各子目标结果[全局步骤下标, 子目标名, 结果]),
    /// 由调用方逐个回写 plan 步骤并聚合进审计/汇总输入。
    #[allow(clippy::too_many_arguments)]
    async fn run_main(
        &self,
        ctx: &TaskRunContext,
        main_index: usize,
        main: &TeamMain,
        step_idxs: &[usize],
        extra_instruction: Option<&str>,
        goal_filter: Option<&[usize]>,
        prior_seed: Vec<(String, String)>,
    ) -> (
        usize,
        Vec<(usize, String, Result<(String, TokenUsage), String>)>,
    ) {
        let n = main_index + 1;
        let session_id = format!("task:{}:main:{}", ctx.task_id, n);
        let mut results = Vec::with_capacity(main.goals.len());
        // 本主已完成子目标的产出:(子目标名, 产出) —— 注入后续子目标,让同一主内各
        // 子目标在彼此成果上衔接,而非各自重造一整份交付物(实跑问题 2 结构性根因)。
        // 首轮为空、循环内自然累积;补做轮因跳过未点名子目标(见下方 continue),必须由
        // 调用方经 prior_seed 显式种入该主既有产出,否则补做子目标既看不到同主其他成果,
        // 也看不到自己被替换的旧版本,版本冲突会在打回场景原样复现。
        let mut prior: Vec<(String, String)> = prior_seed;
        for (k, g) in main.goals.iter().enumerate() {
            // 子目标粒度补做:仅重跑被点名的子目标(其余保留首轮产出);
            // 被跳过的子目标仍要入 results(带既有产出)以便调用方统一回写/重建
            if let Some(only) = goal_filter {
                if !only.contains(&k) {
                    continue;
                }
            }
            // 取消(含上一子目标被中断后):剩余子目标统一标记中断,保证每步都有终态
            if *ctx.cancel.borrow() {
                for (k2, g2) in main.goals.iter().enumerate().skip(k) {
                    if goal_filter.is_some_and(|only| !only.contains(&k2)) {
                        continue;
                    }
                    results.push((step_idxs[k2], g2.name.clone(), Err("任务已停止".into())));
                }
                break;
            }
            let mut goal = format!(
                "总体目标:\n{}\n\n你是主 Agent-{n},负责分工「{}」。当前子目标(第 {}/{} 个):{}\n{}\n",
                ctx.goal,
                main.name,
                k + 1,
                main.goals.len(),
                g.name,
                g.goal
            );
            // 同主前序产出:仅供衔接与保持一致,明确禁止整篇复述或另造版本
            if !prior.is_empty() {
                goal.push_str(
                    "\n本主此前已完成子目标的产出(仅供衔接与保持一致;不要在本次产出中整篇复述或另起一版):\n",
                );
                for (name, text) in &prior {
                    let brief: String = text.trim().chars().take(1200).collect();
                    goal.push_str(&format!("- 「{name}」:{brief}\n"));
                }
            }
            if let Some(extra) = extra_instruction {
                goal.push_str(&format!("\n审计补做指令:\n{extra}\n"));
            }
            goal.push_str("请完成该子目标并直接产出最终结果。");
            let call = AgentLoopCall {
                task_id: ctx.task_id.clone(),
                session_id: session_id.clone(),
                goal,
                settings: ctx.settings.clone(),
                character_id: ctx.character_id.clone(),
                phase: "agent",
                step_index: Some(step_idxs[k]),
                label: format!("主 agent {n} 子目标 {}", k + 1),
            };
            let result = run_agent_loop(
                self.svc.clone(),
                self.engine.clone(),
                call,
                ctx.cancel.clone(),
            )
            .await;
            // 成功产出进 prior 供后续子目标衔接(失败不注入,避免把错误文本当参考)
            if let Ok((text, _)) = &result {
                prior.push((g.name.clone(), text.clone()));
            }
            results.push((step_idxs[k], g.name.clone(), result));
        }
        (main_index, results)
    }

    async fn run_inner(&self, ctx: TaskRunContext) -> Result<(TaskTerminal, TokenUsage), String> {
        let svc = &self.svc;
        let mut total = TokenUsage::default();

        // ===== a. 规划器:自动拓扑(reset_task 已置 planning;规划阶段归执行器所有) =====
        svc.set_status(&ctx.task_id, TaskStatus::Planning);
        let (mains, plan_out) = self.plan_team(&ctx).await?;
        svc.record_usage(&ctx.task_id, "planner", None, &plan_out);
        total.prompt_tokens += plan_out.prompt_tokens;
        total.completion_tokens += plan_out.completion_tokens;
        total.total_tokens += plan_out.prompt_tokens + plan_out.completion_tokens;
        if *ctx.cancel.borrow() {
            return Err("任务已停止".into());
        }

        // plan 步骤:「【主Agent-N】子目标名」(前端分工卡分组契约);
        // step_ranges[n] = 主 agent n 在 plan 中的步骤下标区间
        let mut plan: Vec<TaskStep> = Vec::new();
        let mut step_ranges: Vec<Vec<usize>> = Vec::new();
        for (i, m) in mains.iter().enumerate() {
            let mut idxs = Vec::new();
            for g in &m.goals {
                idxs.push(plan.len());
                plan.push(TaskStep {
                    name: format!("【主Agent-{}】{}", i + 1, g.name),
                    goal: g.goal.clone(),
                    status: TaskStepStatus::Pending,
                    result: String::new(),
                });
            }
            step_ranges.push(idxs);
        }
        svc.set_plan(&ctx.task_id, &plan);
        svc.set_status(&ctx.task_id, TaskStatus::Running);

        // ===== b. 各主 agent 并行(JoinSet;独立 run_id/虚拟 session,cancel 同源) =====
        let (mut outputs, cancelled) = self
            .run_mains_parallel(&ctx, &mains, &step_ranges, &mut plan, &mut total, None)
            .await;
        if cancelled {
            return Err("任务已停止".into());
        }
        if outputs.iter().all(|o| o.is_none()) {
            return Err("全部主 agent 执行失败,团队无产出".into());
        }

        // ===== c. 审计 agent:一致性/质量/覆盖度审查;缺漏打回指定主补做(最多 1 轮) =====
        let audit_input = build_audit_input(&ctx.goal, &mains, &outputs);
        let audit_messages = vec![
            LlmMessage::plain("system", TEAM_AUDIT_PROMPT),
            LlmMessage::plain("user", &audit_input),
        ];
        // 截断自愈:审计是结构化 JSON 输出,推理模型烧光预算会腰斩 JSON(实测)
        let audit_out = generate_text_healed(
            svc,
            &ctx.task_id,
            "audit",
            audit_messages,
            structured_budget(ctx.settings.default_max_tokens),
            TEAM_JSON_TEMPERATURE,
            ctx.settings.default_top_p,
            &ctx.cancel,
            &mut total,
        )
        .await?;
        if *ctx.cancel.borrow() {
            return Err("任务已停止".into());
        }
        let verdict = parse_audit(&audit_out.text, mains.len());
        // 进入最终 result「## 审计结论」段的文本:无打回 = 首次审计结论;
        // 有打回 = 补做后的终审结论(修复:旧实现永远挂首次打回原文,任务 done
        // 而 result 结尾仍显示「需要补全」)
        let mut final_conclusion = verdict.conclusion.clone();
        // 终态闸门(实跑问题 2):审计/终审未通过则任务不得判 done。
        // 无打回时即首次审计结论;有打回时以补做后的终审裁定覆盖。
        let mut final_pass = verdict.pass;

        // 打回补做:仅一轮,补做完成后追加终审(只产出结论文本,不再打回)
        if !verdict.pass && !verdict.kickbacks.is_empty() {
            svc.emit_event(
                TaskEventKind::AgentStatus,
                &ctx.task_id,
                None,
                None,
                Some(format!("审计打回 {} 处补做", verdict.kickbacks.len())),
            );
            let (new_outputs, cancelled) = self
                .run_mains_parallel(
                    &ctx,
                    &mains,
                    &step_ranges,
                    &mut plan,
                    &mut total,
                    Some(&verdict.kickbacks),
                )
                .await;
            outputs = new_outputs;
            if cancelled {
                return Err("任务已停止".into());
            }
            if outputs.iter().all(|o| o.is_none()) {
                return Err("补做后全部主 agent 无产出".into());
            }

            // 终审:基于补做后的最终产出给出结论文本(空输出兜底回退首次审计结论,
            // 与审计「增强环节异常不沉任务」同口径)
            let review_input = build_final_review_input(&ctx.goal, &mains, &outputs, &verdict);
            let review_messages = vec![
                LlmMessage::plain("system", TEAM_FINAL_AUDIT_PROMPT),
                LlmMessage::plain("user", &review_input),
            ];
            let review_out = generate_text_healed(
                svc,
                &ctx.task_id,
                "final_audit",
                review_messages,
                structured_budget(ctx.settings.default_max_tokens),
                TEAM_JSON_TEMPERATURE,
                ctx.settings.default_top_p,
                &ctx.cancel,
                &mut total,
            )
            .await?;
            if *ctx.cancel.borrow() {
                return Err("任务已停止".into());
            }
            // 终审:基于补做后的最终产出做裁定(结构化 JSON);解析失败/空输出按未通过
            // 兜底(终审是终态闸门,读不出明确通过就不判 done);结论段空输出时回退首次
            // 审计结论,避免「## 审计结论」段空白
            let review_verdict = parse_audit(&review_out.text, mains.len());
            final_pass = review_verdict.pass;
            if review_out.text.trim().is_empty() {
                tracing::warn!("team 终审返回空内容,审计结论段回退为首次审计结论");
            } else {
                final_conclusion = review_verdict.conclusion;
            }
        }

        // ===== d. 升华整合:整合文本 + 审计结论段(前端拆卡契约) =====
        // 汇总输入携带审计结论与已选定版本,并要求「只整合、不另起一版」——旧实现
        // 汇总与审计互不可见,汇总自由再生产一版,导致正文与审计结论不同源
        //(实跑问题 2)。
        let summary_input = build_summary_input(&ctx.goal, &mains, &outputs, &final_conclusion);
        let mut sum_sys = String::from(SUMMARIZER_PROMPT);
        let world = svc.world_context(ctx.character_id.as_deref());
        if !world.is_empty() {
            sum_sys.push_str(&format!("\n\n{}", untrusted_boundary("world_book", &world)));
        }
        let summary_messages = vec![
            LlmMessage::plain("system", &sum_sys),
            LlmMessage::plain("user", &summary_input),
        ];
        // 空输出重试一次(对齐 legacy 汇总空输出语义;此处简化为同参数单重重试)
        // 截断自愈与 record_usage/total 入账由 generate_text_healed 承担
        let mut summary_out = generate_text_healed(
            svc,
            &ctx.task_id,
            "summary",
            summary_messages.clone(),
            structured_budget(ctx.settings.default_max_tokens),
            ctx.settings.default_temperature,
            ctx.settings.default_top_p,
            &ctx.cancel,
            &mut total,
        )
        .await?;
        if summary_out.text.trim().is_empty() && !*ctx.cancel.borrow() {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            summary_out = svc
                .generate_text(
                    &ctx.task_id,
                    "summary",
                    None,
                    summary_messages,
                    Vec::new(),
                    structured_budget(ctx.settings.default_max_tokens),
                    ctx.settings.default_temperature,
                    ctx.settings.default_top_p,
                    ctx.cancel.clone(),
                )
                .await?;
            svc.record_usage(&ctx.task_id, "summary", None, &summary_out);
            total.prompt_tokens += summary_out.prompt_tokens;
            total.completion_tokens += summary_out.completion_tokens;
            total.total_tokens += summary_out.prompt_tokens + summary_out.completion_tokens;
        }
        if summary_out.text.trim().is_empty() {
            return Err("团队汇总返回空内容".into());
        }

        // ===== e. 收尾:终态失败步骤或审计/终审未通过 → partial;全部通过 → done =====
        // 实跑问题 2:审计不通过(含解析失败兜底未通过、补做后终审仍不过)不得判定
        // done——审计是质量闸门,其结论必须反映到终态;未通过时任务以 partial 交付,
        // 由用户决定是否追加指令继续修。
        // 失败步骤判定以 plan 终态为准(而非累加式 had_error):补做成功的步骤已被覆盖为
        // Done,不应再拖累终态——旧实现 had_error 一旦置位永不复位,导致「失败 → 打回补做
        // 成功 → 终审通过」仍判 partial 且 error 为空、用户无从解释(实跑问题 2)。
        let failed_steps = error_step_labels(&plan);
        let failed = has_error_steps(&plan);
        let text = format!(
            "{}\n\n## 审计结论\n{}",
            summary_out.text.trim(),
            final_conclusion
        );
        // partial 原因:优先说明失败子目标(具体可定位),否则归因于审计/终审未通过;
        // Done 时为 None(complete_mode_run 写空串清掉上一轮残留 error)
        let error = if failed {
            Some(format!(
                "以下子目标执行失败,成果不完整:{}",
                failed_steps.join("、")
            ))
        } else if !final_pass {
            Some("审计/终审未通过,未达交付标准(详见「审计结论」)".to_string())
        } else {
            None
        };
        // 有可解释 error(失败子目标或审计/终审未通过)即 partial,否则 done
        let status = if error.is_some() {
            TaskStatus::Partial
        } else {
            TaskStatus::Done
        };
        Ok((
            TaskTerminal::Complete {
                result: text,
                status,
                error,
            },
            total,
        ))
    }

    /// 并行跑一批主 agent(首轮 = 全部;补做轮 = 打回指定子集/子目标,目标文本附补做指令)。
    /// 主 agent 内按子目标逐个独立执行,每子目标完成即推进其 plan 步骤
    ///(running→done/error,写库成功才发事件由 set_plan 保证)。
    /// 各主产出**在收尾时从 plan 统一重建**(而非就地拼接本轮 sections):plan 是子目标
    /// 结果的唯一真相源,重建保证补做替换旧版本、未打回主保留原产出、不出现新旧并存
    ///(实跑问题 2)。
    /// 返回 (各主产出, 是否被任务取消中断);某主部分子目标失败时产出保留成功部分
    ///(附失败说明供审计覆盖度判定),全部子目标失败才记 None。
    #[allow(clippy::too_many_arguments)]
    async fn run_mains_parallel(
        &self,
        ctx: &TaskRunContext,
        mains: &[TeamMain],
        step_ranges: &[Vec<usize>],
        plan: &mut [TaskStep],
        total: &mut TokenUsage,
        kickbacks: Option<&[Kickback]>,
    ) -> (Vec<Option<String>>, bool) {
        let svc = &self.svc;
        // 确定本轮要跑的主 agent 子集:(主序号, 补做指令, 限定的子目标下标集)。
        // 子目标下标先按该主实际子目标数过滤:越界/为空 → 退回整主(审计 step 越界
        // 或该主全部被点名时都不至于漏跑或空跑)。
        let batch: Vec<(usize, Option<String>, Option<Vec<usize>>)> = match kickbacks {
            None => (0..mains.len()).map(|i| (i, None, None)).collect(),
            Some(list) => {
                // 同一主的多个打回条目合并为一轮 spawn:step=None 视为整主(不设过滤),
                // 有具体 step 的取并集(指令合并);避免同一虚拟 session 被并发 spawn。
                let mut merged: Vec<(usize, Option<Vec<usize>>, Vec<String>)> = Vec::new();
                for k in list {
                    match merged.iter_mut().find(|(m, _, _)| *m == k.main) {
                        Some((_, steps, ins)) => {
                            match (&mut *steps, k.step) {
                                // 任一条目未指明子目标 = 整主补做(放宽为不过滤)
                                (s @ Some(_), None) => *s = None,
                                (Some(s), Some(step)) => {
                                    if !s.contains(&step) {
                                        s.push(step);
                                    }
                                }
                                (None, _) => {}
                            }
                            ins.push(k.instruction.clone());
                        }
                        None => merged.push((
                            k.main,
                            k.step.map(|s| vec![s]),
                            vec![k.instruction.clone()],
                        )),
                    }
                }
                merged
                    .into_iter()
                    .map(|(i, steps, ins)| {
                        // 过滤越界子目标:全部越界 → 整主(不设过滤),否则只跑命中项
                        let filtered = steps.and_then(|s| {
                            let max = mains[i].goals.len();
                            let kept: Vec<usize> = s.into_iter().filter(|x| *x < max).collect();
                            if kept.is_empty() {
                                None
                            } else {
                                Some(kept)
                            }
                        });
                        (i, Some(ins.join("\n")), filtered)
                    })
                    .collect()
            }
        };
        // 补做轮的 prior 种子在此快照:必须早于下方「置 running」——否则被打回子目标
        // 自身已被置为 Running,会被 before_kickback_prior 的 Done 过滤跳过,补做者
        // 看不到自己的旧版本(实测踩坑:只剩同门未打回子目标的产出)。
        // 按 batch 顺序与 spawn 一一对应;整主/首轮为空 vec(首轮走循环内自然累积)。
        let prior_seeds: Vec<Vec<(String, String)>> = batch
            .iter()
            .map(|(i, _, filter)| before_kickback_prior(step_ranges, plan, *i, filter.as_deref()))
            .collect();
        // 步骤置 running 并落库(事件随 set_plan 发射):只置本轮真正会跑的步骤——
        // 子目标粒度补做时被跳过的步骤保持原终态,不得留 running(实跑问题 2)。
        for (i, _, filter) in &batch {
            for &si in &step_ranges[*i] {
                let will_run = match filter {
                    None => true,
                    Some(steps) => {
                        // filter 存的是主内子目标下标;映射回全局步骤下标比对
                        main_goal_index(step_ranges, *i, si).is_some_and(|k| steps.contains(&k))
                    }
                };
                if will_run {
                    plan[si].status = TaskStepStatus::Running;
                }
            }
        }
        svc.set_plan(&ctx.task_id, plan);
        // 本轮 batch 的主序号留底:JoinError 兜底置 error 时用(spawn 循环会消耗 batch)
        let batch_idx: Vec<usize> = batch.iter().map(|(i, _, _)| *i).collect();

        let mut join: JoinSet<MainJoinOutput> = JoinSet::new();
        for ((i, extra, goal_filter), prior_seed) in batch.into_iter().zip(prior_seeds) {
            let this = Self {
                svc: self.svc.clone(),
                engine: self.engine.clone(),
            };
            let ctx2 = TaskRunContext {
                task_id: ctx.task_id.clone(),
                token: ctx.token,
                goal: ctx.goal.clone(),
                settings: ctx.settings.clone(),
                character_id: ctx.character_id.clone(),
                cancel: ctx.cancel.clone(),
            };
            let main = mains[i].clone();
            let step_idxs = step_ranges[i].clone();
            join.spawn(async move {
                this.run_main(
                    &ctx2,
                    i,
                    &main,
                    &step_idxs,
                    extra.as_deref(),
                    goal_filter.as_deref(),
                    prior_seed,
                )
                .await
            });
        }

        let mut cancelled = false;
        while let Some(res) = join.join_next().await {
            // 主序号不再参与聚合(产出统一从 plan 重建),仅供 JoinError 分支追溯留底
            let (_i, sub_results) = match res {
                Ok(v) => v,
                Err(e) => {
                    // JoinError(panic 等):按该主失败处理,不中断其余主;JoinError 不携带
                    // 业务下标,无法反查是哪一主 panic——保守地把本轮 batch 中仍 running 的
                    // 步骤统一置 error(终态至少 partial,不留永远 running)
                    tracing::warn!(error = e.to_string(), "team 主 agent 任务异常终止");
                    fail_batch_running_steps(plan, &batch_idx, step_ranges);
                    svc.set_plan(&ctx.task_id, plan);
                    continue;
                }
            };
            // 逐子目标回写各自步骤(未在本轮 results 中的子目标保持原状态/产出)
            for (si, _goal_name, result) in sub_results {
                match result {
                    Ok((text, usage)) => {
                        total.prompt_tokens += usage.prompt_tokens;
                        total.completion_tokens += usage.completion_tokens;
                        total.total_tokens += usage.total_tokens;
                        // usage 落库(phase=agent,step_index=该子目标的全局步骤下标;
                        // 子 agent 行在 run_subtask 内落)
                        svc.record_usage(&ctx.task_id, "agent", Some(si), &usage_as_output(&usage));
                        plan[si].status = TaskStepStatus::Done;
                        // 替换式回写:补做产出覆盖旧版本,杜绝新旧并存(实跑问题 2)
                        plan[si].result = text;
                    }
                    Err(e) => {
                        if *ctx.cancel.borrow() {
                            cancelled = true;
                        }
                        plan[si].status = TaskStepStatus::Error;
                        plan[si].result = e.clone();
                    }
                }
            }
            svc.set_plan(&ctx.task_id, plan);
        }
        // 统一从 plan 重建各主产出(唯一真相源;补做已替换旧版本,未打回主保留原产出)
        (rebuild_outputs(mains, step_ranges, plan), cancelled)
    }
}

/// 补做轮的 prior 种子:从 plan 取该主既有子目标产出,供补做子目标衔接并保持版本一致。
/// 首轮 prior 为空、由 run_main 循环内自然累积;补做轮会跳过未点名子目标(goal_filter),
/// 循环内累积不到任何东西 —— 必须在此显式种入,否则补做子目标既看不到同主其他成果,
/// 也看不到自己被替换的旧版本,「多版本并存」会在打回场景原样复现(实跑问题 2)。
/// 被打回子目标的旧版本特别标注「旧版本(本次产出将替换它)」,让模型明确是替换而非新增。
/// 仅当本轮确为子目标粒度补做(goal_filter 有值)时种入;整主补做走首轮同构路径。
fn before_kickback_prior(
    step_ranges: &[Vec<usize>],
    plan: &[TaskStep],
    main_index: usize,
    goal_filter: Option<&[usize]>,
) -> Vec<(String, String)> {
    let Some(only) = goal_filter else {
        return Vec::new();
    };
    let Some(range) = step_ranges.get(main_index) else {
        return Vec::new();
    };
    let mut seed = Vec::new();
    for (k, &si) in range.iter().enumerate() {
        let step = &plan[si];
        // 只种入有产出的终态子目标(失败/未跑的没有可衔接内容)
        if step.status != TaskStepStatus::Done {
            continue;
        }
        let name = strip_main_prefix(&step.name).to_string();
        if only.contains(&k) {
            seed.push((
                format!("{name}(旧版本,本次产出将替换它)"),
                step.result.clone(),
            ));
        } else {
            seed.push((name, step.result.clone()));
        }
    }
    seed
}

/// 从 plan 重建各主 agent 产出:plan 是子目标结果唯一真相源。每主按子目标顺序拼接
/// 「子目标 N「名」:产出」(N 为主内 1-based 序号)——编号是审计/终审输出 `step`
/// 字段的定位契约:审计提示词要求回填「该主第几个子目标」,输入里显式编号模型才能
/// 数对,避免靠猜序号导致打回重跑错子目标或误退整主(实跑问题 2)。
/// 失败子目标附错误说明供审计判定覆盖度。**该主无任何 Done 子目标时返回 None**
///(全败主不产生产出,维持「全部主失败 → 团队无产出」语义)。
fn rebuild_outputs(
    mains: &[TeamMain],
    step_ranges: &[Vec<usize>],
    plan: &[TaskStep],
) -> Vec<Option<String>> {
    mains
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let mut sections: Vec<String> = Vec::new();
            let mut any_done = false;
            for (k, &si) in step_ranges[i].iter().enumerate() {
                let step = &plan[si];
                let name = strip_main_prefix(&step.name);
                match step.status {
                    TaskStepStatus::Done => {
                        any_done = true;
                        sections.push(format!("子目标 {}「{name}」:\n{}", k + 1, step.result));
                    }
                    TaskStepStatus::Error => {
                        sections.push(format!(
                            "子目标 {}「{name}」:(执行失败:{})",
                            k + 1,
                            step.result
                        ));
                    }
                    _ => {}
                }
            }
            if any_done {
                Some(sections.join("\n\n"))
            } else {
                None
            }
        })
        .collect()
}

/// 去掉 plan 步骤名的「【主Agent-N】」前缀(前端分工卡分组契约的前缀由执行器添加)
fn strip_main_prefix(name: &str) -> &str {
    match name.find('】') {
        Some(i) => name[i + '】'.len_utf8()..].trim_start(),
        None => name,
    }
}

/// 全局步骤下标 → 该主内子目标下标(反查;不在该主范围内返回 None)
fn main_goal_index(step_ranges: &[Vec<usize>], main: usize, step: usize) -> Option<usize> {
    step_ranges.get(main)?.iter().position(|s| *s == step)
}

/// JoinError(panic 等)兜底:无法从 JoinError 反查是哪一主 panic(不携带业务下标),
/// 保守地把本轮 batch 中仍 running 的步骤统一置 error(终态至少 partial,不留永远
/// running 的步骤)。其余主若仍在跑,其步骤会在完成回写时覆盖为最终态,此处写的只是
/// 瞬时兜底;已终态步骤与 batch 外步骤不动。返回置 error 的步骤数。
/// 终态 partial 判定由收尾处扫描 plan 中残留的 Error 步骤承担(不再单独记 had_error,
/// 避免「首轮失败 → 打回补做成功 → had_error 未复位 → 仍判 partial」的假阴性)。
fn fail_batch_running_steps(
    plan: &mut [TaskStep],
    batch_idx: &[usize],
    step_ranges: &[Vec<usize>],
) -> usize {
    let mut failed = 0usize;
    for &i in batch_idx {
        for &si in &step_ranges[i] {
            if plan[si].status == TaskStepStatus::Running {
                plan[si].status = TaskStepStatus::Error;
                plan[si].result = "主 agent 任务异常终止".into();
                failed += 1;
            }
        }
    }
    failed
}

/// 收尾判定:plan 中是否残留终态 Error 的子目标步骤(补做成功的步骤已被覆盖为 Done,
/// 不再拖累终态)→ 决定任务落 done 还是 partial。
fn has_error_steps(plan: &[TaskStep]) -> bool {
    plan.iter().any(|s| s.status == TaskStepStatus::Error)
}

/// 收集 plan 中终态 Error 的子目标名(带主 agent 序号,便于用户定位是哪个分工),
/// 用于 partial 终态的原因文本。空 = 无失败步骤。
fn error_step_labels(plan: &[TaskStep]) -> Vec<String> {
    plan.iter()
        .filter(|s| s.status == TaskStepStatus::Error)
        .map(|s| {
            // plan 步骤名前缀「【主Agent-N】」由执行器构造(前端分工卡分组契约),可靠
            match s
                .name
                .strip_prefix("【主Agent-")
                .and_then(|r| r.split('】').next())
                .and_then(|n| n.parse::<usize>().ok())
            {
                Some(n) => format!("主 Agent-{n}「{}」", strip_main_prefix(&s.name)),
                None => strip_main_prefix(&s.name).to_string(),
            }
        })
        .collect()
}

/// 审计/汇总的输入文本:总体目标 + 各主产出(失败主保留错误文本,供覆盖度判定)
fn build_audit_input(goal: &str, mains: &[TeamMain], outputs: &[Option<String>]) -> String {
    let mut user = format!("用户目标:\n{goal}\n\n各主 agent 产出:\n");
    for (i, m) in mains.iter().enumerate() {
        let body = match &outputs[i] {
            Some(text) => text.clone(),
            None => "(该主 agent 执行失败,无产出)".to_string(),
        };
        user.push_str(&format!(
            "### 主 Agent-{}「{}」:\n{}\n\n",
            i + 1,
            m.name,
            body
        ));
    }
    user
}

/// 汇总的输入文本:总体目标 + 各主产出 + 审计结论,并要求「只整合已选定版本、
/// 不得另起一版」。旧实现汇总输入不含审计结论,汇总者对审计认定的不一致毫不知情,
/// 自由再生产一版,最终 result 正文与「## 审计结论」段不同源(实跑问题 2)。
fn build_summary_input(
    goal: &str,
    mains: &[TeamMain],
    outputs: &[Option<String>],
    audit_conclusion: &str,
) -> String {
    let mut user = build_audit_input(goal, mains, outputs);
    user.push_str("审计结论(必须遵守;若指出某主产出为旧版/有缺漏,以补做后的最终版本为准):\n");
    user.push_str(audit_conclusion);
    user.push_str(
        "\n\n请只整合上述各主 agent 的最终产出,输出一份统一的最终成果:\
         不要另起一版、不要改写或替换已通过审计的产出内容、不要新增未被要求的内容。\
         若各主产出存在重复或前后不一致,以审计结论所指的最终版本为准。",
    );
    user
}

/// 终审的输入文本:补做后的各主产出 + 首轮审计意见(结论与打回清单),
/// 供终审员判断首轮指出的问题是否已解决(终审只产出结论文本,不再打回)。
fn build_final_review_input(
    goal: &str,
    mains: &[TeamMain],
    outputs: &[Option<String>],
    verdict: &AuditVerdict,
) -> String {
    let mut user = build_audit_input(goal, mains, outputs);
    user.push_str("首轮审计结论:\n");
    user.push_str(&verdict.conclusion);
    user.push_str("\n\n首轮打回(均已完成补做,补做产出已替换旧版本):\n");
    for k in &verdict.kickbacks {
        let target = match k.step {
            Some(s) => format!("主 Agent-{} 第 {} 个子目标", k.main + 1, s + 1),
            None => format!("主 Agent-{}", k.main + 1),
        };
        user.push_str(&format!("- {target}:{}\n", k.instruction));
    }
    user
}

impl ModeExecutor for TeamExecutor {
    fn run<'a>(
        &'a self,
        ctx: TaskRunContext,
    ) -> BoxFuture<'a, Result<(TaskTerminal, TokenUsage), String>> {
        Box::pin(async move { self.run_inner(ctx).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合法拓扑直接解析;过滤空分工/空子目标
    #[test]
    fn parse_team_topology_plain() {
        let text = r#"{"mains":[{"name":"调研","goals":[{"name":"子一","goal":"做一"},{"name":"子二","goal":"做二"}]},{"name":"写作","goals":[{"name":"子三","goal":"做三"}]}]}"#;
        let mains = parse_team_topology(text).expect("合法拓扑应解析成功");
        assert_eq!(mains.len(), 2);
        assert_eq!(mains[0].goals.len(), 2);
        assert_eq!(mains[1].name, "写作");
    }

    /// 剥 markdown 代码块 + 前言后记容忍
    #[test]
    fn parse_team_topology_fenced_and_prose() {
        let text = "好的,拓扑如下:\n```json\n{\"mains\":[{\"name\":\"甲\",\"goals\":[{\"name\":\"g\",\"goal\":\"x\"}]}]}\n```\n以上。";
        let mains = parse_team_topology(text).unwrap();
        assert_eq!(mains.len(), 1, "单主降级拓扑应被接受");
    }

    /// 超编归并与截断:6 主 → 4 主(尾部并入第 4 位),每主上限 4(归并后第 4 位
    /// 9 个子目标 → 截前 4)
    #[test]
    fn parse_team_topology_clamps_overflow() {
        let mut text = String::from(r#"{"mains":["#);
        for i in 1..=6 {
            if i > 1 {
                text.push(',');
            }
            text.push_str(&format!(
                r#"{{"name":"主{i}","goals":[{{"name":"a","goal":"x"}},{{"name":"b","goal":"y"}},{{"name":"c","goal":"z"}}]}}"#
            ));
        }
        text.push_str("]}");
        let mains = parse_team_topology(&text).unwrap();
        assert_eq!(mains.len(), 4, "超编主 agent 应归并为 4");
        for m in &mains {
            assert!(m.goals.len() <= 4, "每主子目标应截断到 4");
        }
        assert_eq!(mains[3].goals.len(), 4, "归并后第 4 位应截断到 4");
        let total: usize = mains.iter().map(|m| m.goals.len()).sum();
        assert!(total <= 15, "全局子目标应 ≤15");
    }

    /// 全局 ≤15 封顶真实生效:每主上限调至 4 后 4×4=16 可超,从末位主倒序截断,
    /// 每主至少保留 1 个子目标(4×2=8<15 时旧上限永远触不到,属逻辑矛盾修复)
    #[test]
    fn parse_team_topology_global_cap_fifteen() {
        let mut text = String::from(r#"{"mains":["#);
        for i in 1..=4 {
            if i > 1 {
                text.push(',');
            }
            text.push_str(&format!(
                r#"{{"name":"主{i}","goals":[{{"name":"a","goal":"x"}},{{"name":"b","goal":"y"}},{{"name":"c","goal":"z"}},{{"name":"d","goal":"w"}}]}}"#
            ));
        }
        text.push_str("]}");
        let mains = parse_team_topology(&text).unwrap();
        let total: usize = mains.iter().map(|m| m.goals.len()).sum();
        assert_eq!(total, 15, "全局子目标应封顶 15: {mains:?}");
        for m in &mains {
            assert!(!m.goals.is_empty(), "每主至少保留 1 个子目标: {mains:?}");
        }
        assert_eq!(mains[3].goals.len(), 3, "应从末位主截断: {mains:?}");
    }

    /// 空拓扑/垃圾文本报错(交给重试)
    #[test]
    fn parse_team_topology_rejects_empty() {
        assert!(parse_team_topology("{\"mains\":[]}").is_err());
        assert!(parse_team_topology("这不是 JSON").is_err());
        assert!(parse_team_topology("").is_err());
        // 空分工/全空子目标被过滤后为空 → 报错
        assert!(parse_team_topology(
            r#"{"mains":[{"name":"","goals":[]},{"name":"甲","goals":[]}]}"#
        )
        .is_err());
    }

    /// 审计输出:通过/打回/结论;序号 1-based 转 0-based;越界条目丢弃;
    /// step 可选(指明则打回粒度到子目标,缺省则整主)
    #[test]
    fn parse_audit_kickbacks() {
        let v = parse_audit(
            r#"{"通过":false,"打回":[{"main":2,"instruction":"补数据"},{"main":9,"instruction":"越界"},{"main":1,"instruction":""}],"结论":"主二缺数据"}"#,
            3,
        );
        assert!(!v.pass);
        assert_eq!(
            v.kickbacks,
            vec![Kickback {
                main: 1,
                step: None,
                instruction: "补数据".to_string()
            }],
            "越界与空指令条目应丢弃"
        );
        assert_eq!(v.conclusion, "主二缺数据");

        let ok = parse_audit(r#"{"通过":true,"打回":[],"结论":"覆盖完整"}"#, 2);
        assert!(ok.pass && ok.kickbacks.is_empty());

        // step 指明时打回粒度到子目标(1-based → 0-based);step 越界(0)按整主
        let scoped = parse_audit(
            r#"{"通过":false,"打回":[{"main":1,"step":2,"instruction":"只补第二个子目标"},{"main":2,"step":0,"instruction":"step=0 无效按整主"}],"结论":"x"}"#,
            2,
        );
        assert_eq!(
            scoped.kickbacks,
            vec![
                Kickback {
                    main: 0,
                    step: Some(1),
                    instruction: "只补第二个子目标".to_string()
                },
                Kickback {
                    main: 1,
                    step: None,
                    instruction: "step=0 无效按整主".to_string()
                },
            ]
        );
    }

    /// 审计输出无法解析:按未通过兜底(实跑问题 2);结论不落半截 JSON
    #[test]
    fn parse_audit_fallback_not_passed() {
        let v = parse_audit("看起来都不错", 2);
        assert!(!v.pass, "非 JSON 输出应兜底未通过(审计闸门不得静默失效)");
        assert!(v.kickbacks.is_empty());
        assert_eq!(v.conclusion, "看起来都不错");
    }

    /// 截断自愈预算决策(2026-09-03 实测:推理模型 reasoning 烧光
    /// default_max_tokens=1024,审计 finish=length 产出腰斩 JSON):
    /// 仅 finish=length 触发;翻倍且不低于 HEAL_BUDGET_FLOOR,封顶 TEAM_RETRY_MAX_TOKENS_CAP。
    /// (2026-09-15:小预算不再「翻倍到 2048 这种仍被推理烧光」的档位,直接抬到下限。)
    #[test]
    fn trunc_heal_budget_only_on_length() {
        assert_eq!(
            trunc_heal_budget(Some("length"), 1024),
            Some(16_384),
            "小预算应抬到下限(而非仅翻倍到 2048)"
        );
        assert_eq!(
            trunc_heal_budget(Some("length"), 16_384),
            Some(32_768),
            "下限之上仍按翻倍"
        );
        assert_eq!(trunc_heal_budget(Some("length"), 40000), Some(80_000));
        assert_eq!(trunc_heal_budget(Some("length"), 131_072), None);
        assert_eq!(trunc_heal_budget(Some("stop"), 1024), None);
        assert_eq!(trunc_heal_budget(None, 1024), None);
    }

    /// 推理感知加强(2026-09-15 实录:审计 10000 预算被 reasoning 吃掉 9894、
    /// 正文仅 178 字符即截断,翻倍到 20000 重发才成功):
    /// 有 reasoning 观测时按「已消耗 + 原预算」一次给足;无观测时维持既有翻倍语义。
    #[test]
    fn trunc_heal_budget_reasoning_aware_gives_room_for_body() {
        // 实录形态:10000 预算,completion 10005(其中 reasoning 9894)
        // 目标 = 10005 + 10000 = 20005,比单纯翻倍(20000)略高,足以容下正文
        assert_eq!(
            trunc_heal_budget_reasoning_aware(Some("length"), 10_000, 10_005, 9_894),
            Some(20_005)
        );
        // 无 reasoning 观测(非思考模型):维持既有翻倍语义
        assert_eq!(
            trunc_heal_budget_reasoning_aware(Some("length"), 10_000, 10_000, 0),
            Some(20_000)
        );
        // 非 length 不触发(加强不得绕过触发条件)
        assert_eq!(
            trunc_heal_budget_reasoning_aware(Some("stop"), 10_000, 10_000, 9_000),
            None
        );
        // 推理感知结果同样受封顶约束
        assert_eq!(
            trunc_heal_budget_reasoning_aware(Some("length"), 131_072, 131_072, 130_000),
            None,
            "已达封顶:无增长即不重发"
        );
    }

    /// 腰斩 JSON 兜底(自愈重发仍截断的末层防线):按未通过兜底,结论段
    /// 不落半截 JSON(原样进 result「## 审计结论」卡既难看又误导),给干净说明
    #[test]
    fn audit_truncated_json_fallback_clean_conclusion() {
        let v = parse_audit(r#"{"通过":false,"打回":[{"main":2"#, 2);
        assert!(!v.pass, "截断审计输出应兜底未通过");
        assert!(v.kickbacks.is_empty());
        assert!(
            !v.conclusion.contains('{'),
            "截断 JSON 不应原样进结论段,实际:{}",
            v.conclusion
        );
    }

    /// 审计打回按定位去重:同一主同一子目标重复打回只保留首条;
    /// 「同主不同子目标」与「同主整主 + 具体子目标」均保留(粒度不同)
    #[test]
    fn parse_audit_dedups_same_target() {
        let v = parse_audit(
            r#"{"通过":false,"打回":[{"main":1,"step":1,"instruction":"首条指令"},{"main":2,"instruction":"主二补做"},{"main":1,"step":1,"instruction":"重复条目应丢弃"},{"main":1,"step":2,"instruction":"同主不同子目标保留"}],"结论":"x"}"#,
            3,
        );
        assert!(!v.pass);
        assert_eq!(
            v.kickbacks,
            vec![
                Kickback {
                    main: 0,
                    step: Some(0),
                    instruction: "首条指令".to_string()
                },
                Kickback {
                    main: 1,
                    step: None,
                    instruction: "主二补做".to_string()
                },
                Kickback {
                    main: 0,
                    step: Some(1),
                    instruction: "同主不同子目标保留".to_string()
                },
            ],
            "同一主同一子目标重复打回应只保留首条"
        );
    }

    /// 从 plan 重建各主产出:失败子目标附错误说明;全败主为 None;
    /// 【主Agent-N】前缀被剥离
    #[test]
    fn rebuild_outputs_from_plan() {
        let mains = vec![
            TeamMain {
                name: "甲".into(),
                goals: vec![],
            },
            TeamMain {
                name: "乙".into(),
                goals: vec![],
            },
        ];
        let step_ranges = vec![vec![0, 1], vec![2]];
        let plan = vec![
            TaskStep {
                name: "【主Agent-1】子一".into(),
                goal: String::new(),
                status: TaskStepStatus::Done,
                result: "产出甲一".into(),
            },
            TaskStep {
                name: "【主Agent-1】子二".into(),
                goal: String::new(),
                status: TaskStepStatus::Error,
                result: "上游抖动".into(),
            },
            TaskStep {
                name: "【主Agent-2】子三".into(),
                goal: String::new(),
                status: TaskStepStatus::Error,
                result: "也失败了".into(),
            },
        ];
        let outputs = rebuild_outputs(&mains, &step_ranges, &plan);
        let o0 = outputs[0].as_deref().unwrap_or("");
        assert!(
            o0.contains("子目标 1「子一」"),
            "应带主内 1-based 编号且剥离前缀: {o0}"
        );
        assert!(o0.contains("产出甲一"), "应含成功产出: {o0}");
        assert!(o0.contains("执行失败"), "失败子目标应附说明: {o0}");
        assert!(outputs[1].is_none(), "全败主产出应为 None");
        assert_eq!(strip_main_prefix("【主Agent-3】写作"), "写作");
        assert_eq!(strip_main_prefix("无前缀"), "无前缀");
    }

    /// JoinError(panic)兜底:无法从 JoinError 反查是哪一主 panic,保守地把本轮 batch 中
    /// 仍 running 的步骤统一置 error(终态至少 partial,不留永远 running 的步骤);
    /// 已终态步骤与 batch 外步骤不动。
    ///(集成侧不可注入:mock 链路无法让某一主 panic,故抽取 fail_batch_running_steps 单测覆盖。)
    #[test]
    fn fail_batch_running_steps_marks_running_error() {
        fn mk(status: TaskStepStatus) -> TaskStep {
            TaskStep {
                name: "子目标".into(),
                goal: String::new(),
                status,
                result: String::new(),
            }
        }
        let mut plan = vec![
            mk(TaskStepStatus::Done),
            mk(TaskStepStatus::Running),
            mk(TaskStepStatus::Running),
            mk(TaskStepStatus::Pending),
        ];
        // 4 主各领 1 步;本轮 batch = 主 0/1/3(主 2 不在本轮)
        let step_ranges = vec![vec![0], vec![1], vec![2], vec![3]];
        let failed = fail_batch_running_steps(&mut plan, &[0, 1, 3], &step_ranges);
        assert_eq!(failed, 1, "仅 batch 内仍 running 的步骤应置 error");
        assert_eq!(plan[0].status, TaskStepStatus::Done, "已终态步骤不动");
        assert_eq!(plan[1].status, TaskStepStatus::Error);
        assert!(!plan[1].result.is_empty(), "置 error 应附原因文本");
        assert_eq!(plan[2].status, TaskStepStatus::Running, "batch 外步骤不动");
        assert_eq!(
            plan[3].status,
            TaskStepStatus::Pending,
            "非 running 步骤不动"
        );

        // 幂等:无 running 步骤时不重复置位、不污染状态
        let failed2 = fail_batch_running_steps(&mut plan, &[0], &step_ranges);
        assert_eq!(failed2, 0, "无 running 步骤应返回 0");
        assert_eq!(plan[0].status, TaskStepStatus::Done);
    }

    /// 终态判定以 plan 终态为准:补做把步骤覆盖为 Done 后,不得再判 partial
    ///(旧 had_error 累加标志永不复位 → 假 partial,实跑问题 2)
    #[test]
    fn terminal_error_steps_drive_partial() {
        let mk = |name: &str, status: TaskStepStatus| TaskStep {
            name: name.into(),
            goal: String::new(),
            status,
            result: "r".into(),
        };
        let plan = vec![
            mk("【主Agent-1】子一", TaskStepStatus::Done),
            mk("【主Agent-2】子二", TaskStepStatus::Error),
        ];
        assert!(has_error_steps(&plan), "存在 Error 步骤应判失败");
        assert_eq!(
            error_step_labels(&plan),
            vec!["主 Agent-2「子二」".to_string()],
            "原因文本应带主序号与子目标名"
        );
        // 补做成功:同一位置覆盖为 Done → 不再有失败步骤(不应判 partial)
        let mut healed = plan.clone();
        healed[1].status = TaskStepStatus::Done;
        assert!(!has_error_steps(&healed), "补做成功后不得再判 partial");
        assert!(error_step_labels(&healed).is_empty());
    }

    /// 补做轮 prior 种子:子目标粒度补做时,种入该主既有 Done 产出;被打回子目标
    /// 的旧版本特别标注「旧版本」;未打回的同门产出原样注入;整主补做/首轮为空
    /// (实跑问题 2:补做轮跳过未点名子目标导致 prior 为空、版本冲突原样复现)
    #[test]
    fn kickback_prior_seeds_sibling_outputs() {
        let plan = vec![
            TaskStep {
                name: "【主Agent-1】子一".into(),
                goal: String::new(),
                status: TaskStepStatus::Done,
                result: "甲产出".into(),
            },
            TaskStep {
                name: "【主Agent-1】子二".into(),
                goal: String::new(),
                status: TaskStepStatus::Done,
                result: "旧乙产出".into(),
            },
            TaskStep {
                name: "【主Agent-1】子三".into(),
                goal: String::new(),
                status: TaskStepStatus::Error,
                result: "失败".into(),
            },
        ];
        let step_ranges = vec![vec![0, 1, 2]];
        // 只补做子目标下标 1(第 2 个):应种入子一(同门)+ 子二的旧版本;失败子三不种
        let seed = before_kickback_prior(&step_ranges, &plan, 0, Some(&[1]));
        assert_eq!(seed.len(), 2, "只种入 Done 产出: {seed:?}");
        assert_eq!(seed[0].0, "子一", "未打回同门产出原样种入");
        assert_eq!(seed[0].1, "甲产出");
        assert_eq!(seed[1].0, "子二(旧版本,本次产出将替换它)");
        assert_eq!(seed[1].1, "旧乙产出");
        // 整主补做(goal_filter=None):不种,走首轮同构路径(循环内自然累积)
        assert!(before_kickback_prior(&step_ranges, &plan, 0, None).is_empty());
    }

    /// 审计 JSON 合法但缺「结论」字段:不把裸 JSON 灌进「## 审计结论」卡
    #[test]
    fn parse_audit_missing_conclusion_uses_placeholder() {
        let v = parse_audit(r#"{"通过":true,"打回":[]}"#, 2);
        assert!(v.pass);
        assert_eq!(v.conclusion, "(审计未给出结论)");
        assert!(!v.conclusion.contains('{'), "不得回填裸 JSON");
    }
}
