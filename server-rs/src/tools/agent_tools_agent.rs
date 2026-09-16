// Agent 强化工具集 · agent 域(中层 L3 青层工具域):
//   agentgo   排出子智能体(后台异步生成,read/todo 轮询结果)
//   sleep     等待
//   agentend  结束子智能体任务
//   todo      列出计划表(计划/子任务/工具调用历史)
// 子智能体后台执行(run_subtask)以角色设定 + 常驻世界书(constant 条目)构建子上下文。
// 批次 4.3b 子 agent 工具化:引擎就绪时 run_subtask 改走 run_tool_loop + 工具白名单
//(读/搜索类安全工具,写类剔除;agent_depth+1 深度守卫,子 agent 不得再派子 agent);
// 任务模式(task: 前缀虚拟 session)落库走内存覆盖层,进度经任务事件桥发 agent_status。
use crate::models::types::{GenerationParams, LlmMessage, SseEvent, ToolContext, ToolDefinition};
use crate::tools::registry::ToolRegistry;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;

use super::agent_tools::ToolDeps;
use super::agent_tools_shared::subtask_candidates;

// 子 agent 工具白名单(批次 4.3b):读/搜索类安全工具;写类(write/replace/create/
// memory_write/update_variables)与编排类(agentgo/agentend——嵌套派发由深度守卫
// 与白名单双重排除;子 agent 不得再派子 agent)一律剔除。
// 常量本体在 `tools::tool_sets::SUBAGENT`(单一出处);语义:显式列举,
// 同时作为 run_tool_loop 的闸门名单自动放行。

/// 子任务输出上限的下限(2026-09-15 实测):旧实现 `clamp(64, 4096)` 把调用方传的
/// 小预算静默抬到 64 就执行,而推理模型单是 reasoning 就能烧掉数千 token——
/// 64 token 的预算必然「推理耗尽、正文为空」,再自愈到 128 依然烧在推理上。
/// 实测日志:子任务 max_tokens=64 → self_heal 到 128 → 正文仍为空。
/// 故下限抬到 16384:推理与正文都能落地,不再出现「必然截断」的档位。
const SUBAGENT_MIN_MAX_TOKENS: u32 = 16384;
/// 子任务输出上限的上限(与 settings 侧 `1..=131072` 校验区间一致)。
const SUBAGENT_MAX_MAX_TOKENS: u32 = 131_072;

// ==================== agentgo:排出子智能体(后台异步,read/todo 轮询) ====================
pub(super) fn register_agentgo(registry: &ToolRegistry, deps: Arc<ToolDeps>) {
    registry.register(
        ToolDefinition {
            name: "agentgo".into(),
            description: "排出子智能体:为每个任务在后台用当前模型独立生成一段内容(带角色设定与常驻世界书上下文)。返回 task_id,后续用 read(type=subtask) 或 todo 轮询结果;用 agentend 结束任务。写指令时按此模板逐条给全:\n契约: <引用来源与版本>\n角色: <谁>\n单一交付物: <一个名词短语>\n判据: <能以是/否回答的验收句>\n输出: <形态与边界(如仅 JSON,首字符 { 末字符 },不得含围栏;或散文不限格式)>\n长度: 目标 ≤ <N> 字;若被迫截断,须以单独一行 END 收尾\n引用: 只允许引用既有字段名,禁止自拟字段\n结果末尾缺 END 即视为截断,应重发或明确标注 gap,不得当作完成。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string", "description": "任务名" },
                                "instruction": { "type": "string", "description": "子任务指令" },
                                "max_tokens": { "type": "integer", "description": "输出上限(默认 512;低于 16384 会被抬到 16384,上限 131072)" }
                            },
                            "required": ["name", "instruction"]
                        }
                    }
                },
                "required": ["tasks"]
            }),
        },
        Arc::new(move |args: Value, ctx: ToolContext| {
            let deps = deps.clone();
            Box::pin(async move {
                // 读取调度限制(深度/并发):settings 已在 load 时钳制到合法区间,此处直接使用
                // (设置快照:不留锁跨 await)
                let (max_depth, max_concurrency) = {
                    let s = deps.settings_snapshot();
                    (s.subagent_max_depth, s.subagent_max_concurrency)
                };
                // 深度守卫:主 Agent 为 depth 0,子 agent 内以 agent_depth+1 运行
                //(批次 4.3b 子 agent 工具化后带白名单工具,agentgo 不在白名单内,
                // 本守卫作为嵌套派发的第二道防线兜底)
                if ctx.agent_depth >= max_depth {
                    return Err(format!(
                        "子智能体嵌套超过 {max_depth} 层,请让主智能体直接处理"
                    ));
                }
                let tasks = args.get("tasks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                if tasks.is_empty() {
                    return Err("缺少 tasks 参数".into());
                }
                if tasks.len() > 5 {
                    return Err("一次最多排出 5 个子智能体".into());
                }
                // 并发守卫:统计本会话仍在 running 状态的子任务数;
                // 顺序执行(tokio::spawn 后立即返回)下通常不超限,守卫防
                // 多轮循环派发/异常重试把在跑任务堆满,拒绝时不创建新任务
                let running = deps
                    .subtasks
                    .list_by_session(&ctx.session_id)
                    .iter()
                    .filter(|t| t.status == "running")
                    .count();
                if running + tasks.len() > max_concurrency as usize {
                    return Err(format!(
                        "子智能体并发已满 {max_concurrency}(在跑 {running} 个,本次再排 {} 个),请稍后重试或先用 todo 轮询已有任务",
                        tasks.len()
                    ));
                }
                // 逐项校验 + 部分派发(审计项 A):必须先全量校验再创建/派发,
                // 顺序不可颠倒——旧实现边遍历边 create+spawn,任一空项整批 return Err
                // 会让前面已 spawn 的合法任务变成孤儿(调用方只见错误、不知已派出)。
                // 现在:非法项进 rejected,合法项照常 create+spawn,互不牵连。
                let mut launched: Vec<Value> = Vec::new();
                let mut rejected: Vec<Value> = Vec::new();
                // 预算被抬高的项(2026-09-15):入参低于下限时不拒绝,但必须显式告知
                // 实际生效值——旧实现静默抬升,调用方以为在压 token、实际按另一个数执行。
                let mut low_budget: Vec<Value> = Vec::new();
                for (index, t) in tasks.iter().enumerate() {
                    // 畸形输入按「拒绝该项」处理,不 panic:非对象元素没有可用 name/instruction
                    let Some(obj) = t.as_object() else {
                        rejected.push(json!({
                            "index": index,
                            "name": "",
                            "reject_reason": "任务项不是对象(需要 {name, instruction})",
                        }));
                        continue;
                    };
                    // trim 后判定:纯空白 " " 不再被当作合法任务(旧实现 is_empty 判空漏过)
                    let name = obj
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let instruction = obj
                        .get("instruction")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let mut reasons: Vec<&str> = Vec::new();
                    if name.is_empty() {
                        reasons.push("name 为空");
                    }
                    if instruction.is_empty() {
                        reasons.push("instruction 为空");
                    }
                    if !reasons.is_empty() {
                        // 闭包内不得因单项非法 return Err:否则整批失败且已派发的成孤儿
                        rejected.push(json!({
                            "index": index,
                            "name": name,
                            "reject_reason": reasons.join("; "),
                        }));
                        continue;
                    }
                    // 原始入参(未钳制):用于判断是否被抬升,并如实回显
                    let requested = obj.get("max_tokens").and_then(|v| v.as_u64());
                    let max_tokens = requested
                        .unwrap_or(512)
                        .clamp(SUBAGENT_MIN_MAX_TOKENS as u64, SUBAGENT_MAX_MAX_TOKENS as u64)
                        as u32;
                    // 低于下限:不拒绝(避免主 agent 写得保守就整批失败),但显式记账
                    if requested.is_some_and(|v| v < SUBAGENT_MIN_MAX_TOKENS as u64) {
                        low_budget.push(json!({
                            "index": index,
                            "name": name,
                            "requested_max_tokens": requested,
                            "resolved_max_tokens": max_tokens,
                            "note": "入参低于下限,已按下限执行;推理模型在小预算下会耗尽推理、正文为空",
                        }));
                    }
                    // 合法项:用 trim 后的值创建与派发(维持原校验/派发顺序)
                    let record = deps.subtasks.create(
                        &ctx.session_id,
                        &ctx.character_id,
                        &name,
                        &instruction,
                    )?;
                    let deps2 = deps.clone();
                    let task_id = record.id.clone();
                    let session_id = ctx.session_id.clone();
                    let character_id = ctx.character_id.clone();
                    let agent_depth = ctx.agent_depth;
                    tokio::spawn(async move {
                        run_subtask(
                            deps2,
                            task_id,
                            session_id,
                            character_id,
                            instruction,
                            max_tokens,
                            agent_depth,
                        )
                        .await;
                    });
                    // 回显实际生效预算:调用方据此确认平台真按自己给的值执行
                    launched.push(json!({
                        "task_id": record.id,
                        "name": name,
                        "status": "pending",
                        "resolved_max_tokens": max_tokens,
                    }));
                }
                // 合法项为 0 且存在 rejected:无任何派发,整批错误(文案带逐项原因供模型自纠);
                // 原「tasks 为空」全局守卫已在前面返回,此处覆盖全废场景。
                if launched.is_empty() {
                    let detail = rejected
                        .iter()
                        .map(|r| {
                            format!(
                                "#{} {}: {}",
                                r["index"],
                                r["name"].as_str().unwrap_or(""),
                                r["reject_reason"].as_str().unwrap_or("")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    return Err(format!("agentgo 未派出任何子任务,逐项原因: {detail}"));
                }
                // 兼容既有调用/测试:保留 tasks 键;rejected/low_budget 为新增键(无时为空数组)
                Ok(json!({
                    "ok": true,
                    "tasks": launched,
                    "rejected": rejected,
                    "low_budget": low_budget,
                })
                .to_string())
            })
        }),
    );
}

/// 后台子任务执行:构建上下文 → 生成 → 写回结果。
/// 引擎就绪(ToolDeps.engine 已注入)时走工具化路径:run_tool_loop + 白名单工具,
/// agent_depth+1 深度守卫(批次 4.3b);否则回退纯生成(单测/早期构造)。
/// 任务模式(task: 前缀虚拟 session)额外落 task_llm_calls/task_usage(phase=subagent)
/// 并经任务事件桥发 agent_status;agent_subtasks 表由内存覆盖层接管(FK 守卫)。
#[allow(clippy::too_many_arguments)]
async fn run_subtask(
    deps: Arc<ToolDeps>,
    task_id: String,
    session_id: String,
    character_id: String,
    instruction: String,
    max_tokens: u32,
    agent_depth: u32,
) {
    // 已被 agentend 提前结束 → 不再启动
    if deps.subtasks.is_ended(&task_id) {
        return;
    }
    let _ = deps.subtasks.set_running(&task_id);
    let cancel = deps.subtasks.register_cancel(&task_id);

    // 子上下文:角色设定 + 常驻世界书条目(外部文本经 untrusted 边界包裹,WP7)
    let mut sys = String::from("你是主 Agent 排出的子智能体,负责独立完成交给你的子任务。");
    if let Some(c) = deps.characters.get(&character_id) {
        sys.push_str(&format!(
            "\n\n角色「{}」设定:\n{}",
            c.chara_name,
            crate::services::prompt_kit::untrusted_boundary("character", c.description.trim())
        ));
    }
    let world = collect_constant_world_text(&deps, &character_id);
    if !world.is_empty() {
        sys.push_str(&format!(
            "\n\n常驻世界书设定:\n{}",
            crate::services::prompt_kit::untrusted_boundary("world_book", &world)
        ));
    }
    sys.push_str("\n\n请直接输出子任务的最终结果(不要模拟对话、不要重复指令)。");
    let mut messages = vec![
        LlmMessage::plain("system", &sys),
        LlmMessage::plain("user", &instruction),
    ];

    // 任务模式识别:task: 前缀虚拟 session(multi/team),第二段为任务 id
    //(team 主 agent 为 task:{id}:main:{n},其子的 session 再带 :sub: 后缀,均同前缀)
    let task_ref = session_id
        .strip_prefix("task:")
        .map(|rest| rest.split(':').next().unwrap_or(rest).to_string());
    let engine = deps.engine.get().and_then(|w| w.upgrade());
    let svc = match &task_ref {
        Some(_) => deps.tasks.get().and_then(|w| w.upgrade()),
        None => None,
    };

    match engine {
        Some(engine) => {
            run_subtask_with_tools(
                &deps,
                &engine,
                svc,
                task_ref,
                &task_id,
                &session_id,
                &character_id,
                &mut messages,
                max_tokens,
                agent_depth,
                cancel,
            )
            .await;
        }
        None => {
            // 回退路径:引擎未就绪(单测/早期构造)——保持原纯生成行为
            run_subtask_plain(&deps, &task_id, &messages, max_tokens, cancel).await;
        }
    }
    deps.subtasks.unregister_cancel(&task_id);
}

/// 子 agent 工具化路径:run_tool_loop + tool_sets::SUBAGENT 白名单(经 ToolGate::listed)。
/// 事件出口:任务模式经任务事件桥(sink)发 agent_status;聊天路径丢弃
///(子任务原本不产生用户可见事件;drain 必须持续消费,rx 关闭会让引擎置 abort)。
#[allow(clippy::too_many_arguments)]
async fn run_subtask_with_tools(
    deps: &Arc<ToolDeps>,
    engine: &Arc<crate::agents::engine::AgentEngine>,
    svc: Option<Arc<crate::services::task_service::TaskService>>,
    task_ref: Option<String>,
    task_id: &str,
    session_id: &str,
    character_id: &str,
    messages: &mut Vec<LlmMessage>,
    max_tokens: u32,
    agent_depth: u32,
    cancel: tokio::sync::watch::Receiver<bool>,
) {
    use crate::agents::engine::executor::run_tool_loop;
    use crate::agents::engine::AbortFlag;
    use crate::agents::state_machine::StateMachine;

    // 白名单 ∩ 已注册工具(防御插件卸载等运行期变化);
    // 白名单常量单一出处见 tools::tool_sets::SUBAGENT
    let whitelist: Vec<String> = crate::tools::tool_sets::SUBAGENT
        .iter()
        .map(|s| s.to_string())
        .collect();
    let tools: Vec<ToolDefinition> = crate::tools::tool_sets::filter_by_const(
        engine.tool_definitions(),
        crate::tools::tool_sets::SUBAGENT,
    );
    // 子 agent 恒自动放行白名单工具(名单内视为已授权,与改造前一致);
    // 名单外(模型臆造调用)立即拒绝——子 agent 无 UI 授权上下文。
    let gate = crate::agents::engine::executor::ToolGate::listed(&whitelist);
    // 设置快照:不留锁跨 await
    let max_rounds = deps.settings_snapshot().max_tool_rounds;
    let params = GenerationParams {
        temperature: 0.7,
        top_p: 0.9,
        max_tokens,
        stop: None,
        tools,
        max_tool_rounds: Some(max_rounds),
        tool_choice: crate::models::types::ToolChoice::Auto,
        parallel_tool_calls: None,
    };

    // 运行身份:任务模式用派生虚拟 id(task: 前缀,llm_requests 跳过守卫同源);
    // 聊天路径沿用真实 session_id(llm_requests 可正常落库,FK 指向真实 sessions 行)
    let sub_session = match &task_ref {
        Some(_) => format!("{session_id}:sub:{task_id}"),
        None => session_id.to_string(),
    };
    let mut state_machine = StateMachine::new(&sub_session);
    let run_id = uuid::Uuid::new_v4().to_string();
    let (flag, _flag_rx) = AbortFlag::new();
    let tool_ctx = ToolContext {
        session_id: sub_session.clone(),
        character_id: character_id.to_string(),
        // 深度 +1:子 agent 内再触 agentgo 时守卫按嵌套层判定(白名单已剔除,双保险)
        agent_depth: agent_depth + 1,
    };
    let (tx, drain) = match (&svc, &task_ref) {
        // 批次 R4:事件桥携 phase=subagent(与下方 record_llm_call 落库口径一致,
        // 子 agent 不挂步骤下标),Token 经桥攒批为 delta 事件透出
        (Some(svc), Some(tid)) => crate::services::task_engine::sink::spawn(
            svc.clone(),
            tid.clone(),
            "子 agent",
            "subagent",
            None,
        ),
        _ => spawn_discard_sink(),
    };
    let mut total_usage = crate::models::types::TokenUsage::default();
    let started = std::time::Instant::now();
    let result = run_tool_loop(
        engine,
        &mut state_machine,
        None, // 子 agent 不建 agent_sessions 行(聊天/任务路径一致)
        &sub_session,
        messages,
        &params,
        &tool_ctx,
        &tx,
        &cancel,
        &flag,
        &mut total_usage,
        &run_id,
        gate,
    )
    .await;
    drop(tx);
    let _ = drain.await;

    // 任务模式:调用追踪 + usage 落库统一出口(phase=subagent,与 legacy 按调用计口径一致)
    if let (Some(svc), Some(tid)) = (&svc, &task_ref) {
        let model = engine.model();
        // 截断自愈留痕落库(问题①,与 solo.rs run_agent_loop 同口径):
        // 被截断的那次调用补落一行 status=error,再落最终行
        if let Ok(res) = &result {
            for heal in &res.self_heals {
                let heal_out = crate::services::task_service::TaskGenOutput {
                    text: String::new(),
                    finish_reason: heal.finish_reason.clone(),
                    prompt_tokens: heal.prompt_tokens,
                    completion_tokens: heal.completion_tokens,
                    reasoning_tokens: 0,
                    reasoning_chars: 0,
                    tool_calls: Vec::new(),
                };
                svc.record_llm_call(
                    tid,
                    "subagent",
                    None,
                    &model,
                    messages,
                    &format!(
                        "(截断自愈){},输出上限翻倍至 {} 重发",
                        heal.note, heal.retried_max_tokens
                    ),
                    Some(&heal_out),
                    std::time::Duration::ZERO,
                    "error",
                );
                // 补落被截断那次调用的 usage(2026-09-10 实测修复,口径同 solo.rs)
                svc.record_usage(tid, "subagent", None, &heal_out);
            }
        }
        let (response, out, status) = match &result {
            Ok(res) if !res.interrupted => {
                let text = res.content.trim().to_string();
                let status = if text.is_empty() { "empty" } else { "ok" };
                let out = crate::services::task_service::TaskGenOutput {
                    text: text.clone(),
                    // 上游 finish_reason 经 run_tool_loop 末轮透出(可观测性问题①)
                    finish_reason: res.finish_reason.clone(),
                    prompt_tokens: total_usage.prompt_tokens,
                    completion_tokens: total_usage.completion_tokens,
                    reasoning_tokens: 0,
                    reasoning_chars: 0,
                    tool_calls: Vec::new(),
                };
                (text, Some(out), status)
            }
            Ok(_) => ("(已中断)".to_string(), None, "error"),
            // 子任务追踪是字符串契约,分类在此落回文案
            Err(e) => (e.message().to_string(), None, "error"),
        };
        svc.record_llm_call(
            tid,
            "subagent",
            None,
            &model,
            messages,
            &response,
            out.as_ref(),
            started.elapsed(),
            status,
        );
        // usage 仅成功调用落库(对齐 legacy:失败/中断不记 task_usage 行)
        if let Some(out) = &out {
            svc.record_usage(tid, "subagent", None, out);
        }
    }

    // 结果写回(语义同原实现:ended 不覆盖、空内容记 error、成功截断写回)
    match result {
        Ok(res) if !res.interrupted => {
            let content = res.content.trim().to_string();
            if deps.subtasks.is_ended(task_id) {
                // 生成期间被 agentend/任务 stop 结束:保持 ended,不写结果
            } else {
                // 判定抽为纯函数(classify_subtask_result),截断即失败的语义可单测
                match classify_subtask_result(&content, res.finish_reason.as_deref(), max_tokens) {
                    SubtaskVerdict::Empty { error } => {
                        let _ = deps.subtasks.set_error(task_id, &error);
                    }
                    SubtaskVerdict::Truncated { error } => {
                        // 截断算失败而非 done(审计项 B):半截正文的交付不可用,原先只要
                        // 正文非空就静默 set_done,导致「自然完成 / max_tokens 截断 /
                        // 拒绝执行」三态都 status=done 且 error 为空,是假成功的主通道。
                        // 这里保留被截断的正文到 result 列(仍有参考价值,不能丢内容),
                        // 同时把定性原因写入 error。
                        let _ = deps.subtasks.set_failed(
                            task_id,
                            &truncate_subtask_result(deps, &content),
                            &error,
                        );
                    }
                    SubtaskVerdict::Done => {
                        let _ = deps
                            .subtasks
                            .set_done(task_id, &truncate_subtask_result(deps, &content));
                    }
                }
            }
        }
        Ok(_) => {
            // 中断(agentend / 任务 stop 联动结束):保持 ended;非常态中断记 error
            if !deps.subtasks.is_ended(task_id) {
                let _ = deps.subtasks.set_error(task_id, "子任务已中断");
            }
        }
        Err(e) => {
            if !deps.subtasks.is_ended(task_id) {
                let _ = deps.subtasks.set_error(task_id, e.message());
            }
        }
    }
}

/// 子任务写回判定(审计项 B,纯函数便于单测):
/// 正文为空归 Empty(error 带 finish_reason,区分「真空响应」与「截断」);
/// 正文非空但 finish_reason=length 归 Truncated(截断即交付不可用,算失败,不写 done);
/// 其余归 Done。
/// 注意:finish_reason 为 None(纯生成回退路径)时维持原「非空即成功」行为,不误伤。
enum SubtaskVerdict {
    Empty { error: String },
    Truncated { error: String },
    Done,
}

fn classify_subtask_result(
    content: &str,
    finish_reason: Option<&str>,
    max_tokens: u32,
) -> SubtaskVerdict {
    if content.is_empty() {
        let error = match finish_reason.filter(|r| !r.is_empty()) {
            Some(r) => format!("子任务返回空内容(finish_reason={r})"),
            None => "子任务返回空内容".to_string(),
        };
        return SubtaskVerdict::Empty { error };
    }
    if finish_reason == Some("length") {
        return SubtaskVerdict::Truncated {
            error: format!("子任务输出被截断(finish_reason=length,输出上限 {max_tokens} token)"),
        };
    }
    SubtaskVerdict::Done
}

/// 子 agent 纯生成回退路径(引擎未注入时;行为与批次 4.3b 之前一致)
async fn run_subtask_plain(
    deps: &Arc<ToolDeps>,
    task_id: &str,
    messages: &[LlmMessage],
    max_tokens: u32,
    cancel: tokio::sync::watch::Receiver<bool>,
) {
    let params = GenerationParams {
        temperature: 0.7,
        top_p: 0.9,
        max_tokens,
        stop: None,
        tools: Vec::new(),
        max_tool_rounds: None,
        tool_choice: crate::models::types::ToolChoice::Auto,
        parallel_tool_calls: None,
    };

    let connector = deps.connector.read().await.clone();
    let res = connector.generate(messages, params, cancel).await;
    drop(connector);

    match res {
        Ok(chunks) => {
            let mut content = String::new();
            for c in chunks {
                if let crate::models::types::LlmStreamChunk::Token(t) = c {
                    content.push_str(&t);
                }
            }
            if deps.subtasks.is_ended(task_id) {
                // 生成期间被 agentend 结束:保持 ended,不写结果
            } else if content.trim().is_empty() {
                let _ = deps.subtasks.set_error(task_id, "子任务返回空内容");
            } else {
                let _ = deps
                    .subtasks
                    .set_done(task_id, &truncate_subtask_result(deps, content.trim()));
            }
        }
        Err(e) => {
            if !deps.subtasks.is_ended(task_id) {
                let _ = deps.subtasks.set_error(task_id, e.message());
            }
        }
    }
}

/// 聊天路径子 agent 的事件出口:run_tool_loop 需要事件通道,但子任务原本不产生
/// 用户可见事件——drain 全部丢弃(必须持续消费到 tx drop:rx 提前关闭会让引擎
/// send_event 置 abort 中断整轮,与 sink.rs 的 drain 语义一致)。
fn spawn_discard_sink() -> (mpsc::Sender<SseEvent>, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<SseEvent>(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    (tx, drain)
}

/// 子任务结果超长截断:超 subagent_result_max_chars 时保留前 N 字符并附尾注,
/// 不静默丢内容(原长写入尾注,调用方可知全貌)。按字符截断,避开 UTF-8 边界问题。
fn truncate_subtask_result(deps: &ToolDeps, content: &str) -> String {
    // 设置快照:不留锁跨 await
    let max_chars = deps.settings_snapshot().subagent_result_max_chars as usize;
    truncate_subtask_result_with_limit(content, max_chars)
}

/// 截断纯函数(限长可注入,测试用)
fn truncate_subtask_result_with_limit(content: &str, max_chars: usize) -> String {
    let total = content.chars().count();
    if total <= max_chars {
        return content.to_string();
    }
    let clipped: String = content.chars().take(max_chars).collect();
    format!("{clipped}\n[子智能体结果已截断,原长 {total} 字符]")
}

/// 测试入口:跨模块(agent_tools tests)验证截断行为
#[cfg(test)]
pub(super) fn truncate_subtask_result_for_test(deps: &ToolDeps, content: &str) -> String {
    truncate_subtask_result(deps, content)
}

/// 测试入口:跨模块验证写回判定(截断即失败)
#[cfg(test)]
pub(super) fn classify_subtask_result_for_test(
    content: &str,
    finish_reason: Option<&str>,
    max_tokens: u32,
) -> (&'static str, String) {
    match classify_subtask_result(content, finish_reason, max_tokens) {
        SubtaskVerdict::Empty { error } => ("empty", error),
        SubtaskVerdict::Truncated { error } => ("truncated", error),
        SubtaskVerdict::Done => ("done", String::new()),
    }
}

/// 常驻世界书文本(子任务上下文用:constant 且启用)。
/// 过滤/排序/格式化统一走 prompt_kit 共享原语,勿在此复制实现(AGENTS.md 明文纪律)。
fn collect_constant_world_text(deps: &ToolDeps, character_id: &str) -> String {
    let mut entries: Vec<crate::parsing::world_book::WorldEntry> = Vec::new();
    if let Some(raw) = deps.characters.get(character_id).and_then(|c| c.data_raw) {
        entries.extend(crate::parsing::world_book::character_book_entries(&raw));
    }
    entries.extend(deps.world_books.collect_entries_for_character(character_id));
    crate::services::prompt_kit::constant_world_body(&entries)
}

// ==================== sleep:等待 ====================
pub(super) fn register_sleep(registry: &ToolRegistry, _deps: Arc<ToolDeps>) {
    registry.register(
        ToolDefinition {
            name: "sleep".into(),
            description: "等待指定毫秒数(上限 60000ms)。需要等待子任务/外部过程完成时使用。".into(),
            parameters: json!({
                "type": "object",
                "properties": { "ms": { "type": "integer", "description": "等待毫秒数(1~60000)" } },
                "required": ["ms"]
            }),
        },
        Arc::new(|args: Value, _ctx: ToolContext| {
            Box::pin(async move {
                let ms = args
                    .get("ms")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    .clamp(1, 60_000) as u64;
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                Ok(json!({ "ok": true, "slept_ms": ms }).to_string())
            })
        }),
    );
}

// ==================== agentend:结束子智能体任务 ====================
pub(super) fn register_agentend(registry: &ToolRegistry, deps: Arc<ToolDeps>) {
    registry.register(
        ToolDefinition {
            name: "agentend".into(),
            description: "结束(取消)已排出的子智能体任务,传入 task_ids 数组。正在生成的任务会被中断,未开始的不会启动。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "agentgo 返回的 task_id 列表"
                    }
                },
                "required": ["task_ids"]
            }),
        },
        Arc::new(move |args: Value, ctx: ToolContext| {
            let deps = deps.clone();
            Box::pin(async move {
                let ids = args.get("task_ids").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                if ids.is_empty() {
                    return Err("缺少 task_ids 参数".into());
                }
                let mut results: Vec<Value> = Vec::new();
                // 未命中的 id 单独收口(2026-09-15):旧实现恒返回 ok:true,拼错 id 时
                // 调用方无法从返回体区分「已清理」与「根本没这个任务」,只能细读
                // 每条结果的 existed 字段。现在 ok 只在「全部命中」时为真,
                // missing 明确列出未命中的 id,让拼写错误立刻可见。
                let mut missing: Vec<String> = Vec::new();
                for id in ids {
                    let id = id.as_str().unwrap_or("").to_string();
                    if id.is_empty() {
                        continue;
                    }
                    // 审计项 F:end() 之前先取记录,拿到本次调用前的真实状态。
                    // 旧实现只回 ended:true——它对「本次真的中断了活跃任务」与
                    // 「任务早已 ended,本次是空操作」同样成立,无法证明中断成功。
                    let rec = deps.subtasks.get(&id);
                    let existed = rec.is_some();
                    if !existed {
                        missing.push(id.clone());
                    }
                    let prior_status = rec
                        .as_ref()
                        .map(|r| r.status.clone())
                        .unwrap_or_default();
                    // 批次 4:end 返回显式结果(Interrupted / AlreadyFinished / Missing),
                    // 不再靠 bool + prior_status 反推;终态记录保持原 status 与 result。
                    let outcome = deps.subtasks.end(&id);
                    let (ended, interrupted, outcome_label) = match &outcome {
                        crate::services::agent_subtask_service::SubtaskEndOutcome::Missing => {
                            (false, false, "missing")
                        }
                        crate::services::agent_subtask_service::SubtaskEndOutcome::Interrupted {
                            ..
                        } => (true, true, "interrupted"),
                        crate::services::agent_subtask_service::SubtaskEndOutcome::AlreadyFinished {
                            ..
                        } => (true, false, "already_finished"),
                    };
                    results.push(json!({
                        "task_id": id,
                        "existed": existed,
                        "prior_status": prior_status,
                        "ended": ended,
                        "interrupted": interrupted,
                        // 显式三态:调用方无需自行组合 existed/prior_status 才能判语义
                        "outcome": outcome_label,
                    }));
                }
                Ok(json!({
                    "ok": missing.is_empty(),
                    "results": results,
                    "missing": missing,
                    "session_id": ctx.session_id,
                })
                .to_string())
            })
        }),
    );
}

// ==================== todo:列出计划表 ====================
pub(super) fn register_todo(registry: &ToolRegistry, deps: Arc<ToolDeps>) {
    registry.register(
        ToolDefinition {
            name: "todo".into(),
            description: "列出当前计划表:Agent 执行计划步骤、全部子智能体任务及其状态(pending/running/done/error/ended)与结果、最近的工具调用历史。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "include_subtasks": { "type": "boolean", "description": "是否包含子任务详情(默认 true)" }
                }
            }),
        },
        Arc::new(move |_args: Value, ctx: ToolContext| {
            let deps = deps.clone();
            Box::pin(async move {
                let mut out = json!({ "ok": true });
                if let Some(agent_session) = deps.agent_sessions.find_by_session(&ctx.session_id) {
                    out["plan"] = json!(agent_session.plan);
                    out["agent_state"] = json!(agent_session.state);
                    out["step_index"] = json!(agent_session.step_index);
                    let calls: Vec<Value> = deps
                        .agent_sessions
                        .list_tool_calls(&agent_session.id)
                        .iter()
                        .rev()
                        .take(20)
                        .map(|c| json!({ "name": c.name, "input": c.input, "output": c.output, "duration_ms": c.duration_ms }))
                        .collect();
                    out["tool_calls"] = json!(calls);
                    out["tool_calls_available"] = json!(true);
                } else {
                    // 任务模式虚拟 session(如 task:{id} / task:{id}:main:{n} /
                    // task:{id}:sub:{tid})没有 agent_sessions 行,tool_calls 恒为空数组。
                    // 显式标注不可用,避免「恒空数组」被审计误判为「无调用发生」(审计项 E);
                    // 真实调用证据请走 read(type=subtask)/任务详情。
                    out["plan"] = json!([]);
                    out["tool_calls"] = json!([]);
                    out["tool_calls_available"] = json!(false);
                    out["tool_calls_note"] = json!(
                        "任务模式虚拟会话无 agent_sessions 行,本字段不可用;请改用 read(type=subtask)/任务详情核对调用"
                    );
                }
                // 子任务清单走 shared helper(审计项 E):能取到任务前缀就用前缀列举,
                // 使同任务的 :main:/:sub: 派生 session 也能看到全部兄弟子任务(跨 agent
                // 证据可见,构成「交接登记表」);聊天路径退回精确 session。复用既有内存
                // 覆盖层(list_by_session_prefix),不新增存储。
                let tasks: Vec<Value> = subtask_candidates(&deps, &ctx.session_id)
                    .iter()
                    .map(|t| {
                        json!({
                            "task_id": t.id,
                            "name": t.name,
                            "status": t.status,
                            "instruction": t.instruction,
                            "result": t.result,
                            "error": t.error,
                            // 批次 4:终态时刻一并回显,调用方据 finished_at 判「何时完成」,
                            // 不必再用 ended 兼表「完成后召回」与「中途中断」
                            "finished_at": t.finished_at,
                        })
                    })
                    .collect();
                out["subtasks"] = json!(tasks);
                Ok(out.to_string())
            })
        }),
    );
}
