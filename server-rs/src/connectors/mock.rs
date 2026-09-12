// Mock 连接器(与 Node 版 connectors/mock.ts 对齐):逐字流式 + 固定回复
// 测试钩子:最后一条 user 消息含 [[tool:name {...}]] 时,首轮返回 ToolCall 分块;
// 后续轮次(消息数组含 role="tool" 结果)返回固定回复 —— 供集成测试验证工具循环。
// [[tool_raw:name {...}]] 模拟「max_tokens 把 tool_call 参数 JSON 切成半截」
// (任务引擎截断自愈,问题①);has_tool_result 固定回复分支内含回复钩子守卫
// ([[reply_if:]]/[[reply:]] 优先,规划器侦察轮后的计划 JSON 产出用,问题②)。
use crate::models::types::{GenerationParams, LlmMessage, LlmStreamChunk, ToolCallArgs};
use std::time::Duration;
use tokio::sync::watch;

/// [[tool_raw:]] 钩子的截断预算门限:max_tokens 低于该值视为「输出预算不足」,
/// 返回左半 arguments(非法 JSON)+ Finish{length};达到即返回完整 tool_call。
/// 默认输出上限 1024 → 截断;自愈翻倍 2048 → 完整(与 RETRY 翻倍语义对齐)。
const TOOL_RAW_MIN_BUDGET: u32 = 2048;

pub struct MockConnector;

impl MockConnector {
    pub fn new() -> Self {
        MockConnector
    }

    pub fn list_models(&self) -> Vec<String> {
        vec!["mock-demo".to_string()]
    }

    pub fn test(&self) -> (bool, String) {
        (true, "Mock 连接器就绪(演示模式)".to_string())
    }

    /// 逐字流式输出固定回复;每字符 sleep 8ms;abort 时返回 Err("生成已中断")
    pub async fn generate(
        &self,
        messages: &[LlmMessage],
        params: GenerationParams,
        abort: watch::Receiver<bool>,
    ) -> Result<Vec<LlmStreamChunk>, String> {
        let mut chunks = Vec::new();
        // 工具循环第二轮:已有 tool 结果 → 返回固定完成回复
        // (先于通用分支检查 tool_loop 多轮钩子,保证显式多轮测试钩子不被通用完成回复短路)
        let has_tool_result = messages.iter().any(|m| m.role == "tool");

        // 取最后一个 user 消息,截 60 字符
        let last_user = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        // 测试钩子:[[fail:消息]] → 模拟模型请求失败(顶层错误终态测试用;
        // 上游错误应产生 Error 事件而非「空内容 finish 伪装成功」)
        if let Some(msg) = extract_fail_marker(&last_user) {
            return Err(msg);
        }

        // 测试钩子:[[tool_loop:name|N args...]] → 前 N 轮持续返回 ToolCall(工具循环轮次
        // 上限边界测试)。mock 无状态:已执行轮数 = 消息数组中 role="tool" 的消息条数
        // (每轮工具执行后回填一条 tool 结果);第 k 轮返回 id=mock-call-k;
        // tool 消息数 >= N 后返回正文,模拟模型完成。
        if let Some((name, n, args)) = extract_tool_loop_marker(&last_user) {
            let executed = messages.iter().filter(|m| m.role == "tool").count();
            if executed < n {
                chunks.push(LlmStreamChunk::ToolCall(ToolCallArgs {
                    id: format!("mock-call-{}", executed + 1),
                    name,
                    arguments: args,
                }));
                chunks.push(LlmStreamChunk::Usage {
                    prompt_tokens: 5,
                    completion_tokens: 3,
                    total_tokens: 8,
                    prompt_cache_hit_tokens: 0,
                    prompt_cache_miss_tokens: 0,
                    reasoning_tokens: 0,
                });
                chunks.push(tool_calls_finish());
                return Ok(chunks);
            }
            let reply = "（模拟回复）工具循环已完成,最终回复。";
            chunks.push(LlmStreamChunk::Token(reply.to_string()));
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: 5,
                completion_tokens: reply.chars().count() as i64,
                total_tokens: 5 + reply.chars().count() as i64,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
                reasoning_tokens: 0,
            });
            chunks.push(LlmStreamChunk::Finish {
                reason: "stop".into(),
            });
            return Ok(chunks);
        }

        // 测试钩子:[[tool_echo:name args]] → 首轮返回 2 个 ToolCall(id 不同,模拟并行工具调用);
        // 后续轮(已有 tool 结果)回显完整 LLM 消息结构:assistant 消息带 `[calls:N]` 标记
        // (N = 该 assistant 消息携带的 tool_calls 数),tool 消息为 `[tool] 内容`。
        // 供集成测试断言「一轮并行调用回填为 1 条 assistant(tool_calls=[2]) + 2 条 tool」。
        if let Some((name, args)) = extract_tool_echo_marker(&last_user) {
            if messages.iter().any(|m| m.role == "tool") {
                let lines: Vec<String> = messages
                    .iter()
                    .map(|m| {
                        let calls = m
                            .tool_calls
                            .as_ref()
                            .map(|cs| format!(" [calls:{}]", cs.len()))
                            .unwrap_or_default();
                        format!("[{}{}] {}", m.role, calls, m.content)
                    })
                    .collect();
                let reply = lines.join("\n---SEP---\n");
                let prompt_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
                chunks.push(LlmStreamChunk::Token(reply.clone()));
                chunks.push(LlmStreamChunk::Usage {
                    prompt_tokens: (prompt_chars as f64 / 4.0).ceil() as i64,
                    completion_tokens: (reply.chars().count() as f64 / 4.0).ceil() as i64,
                    total_tokens: (prompt_chars as f64 / 4.0).ceil() as i64
                        + (reply.chars().count() as f64 / 4.0).ceil() as i64,
                    prompt_cache_hit_tokens: 0,
                    prompt_cache_miss_tokens: 0,
                    reasoning_tokens: 0,
                });
                chunks.push(LlmStreamChunk::Finish {
                    reason: "stop".into(),
                });
                return Ok(chunks);
            }
            chunks.push(LlmStreamChunk::ToolCall(ToolCallArgs {
                id: "mock-echo-1".into(),
                name: name.clone(),
                arguments: args.clone(),
            }));
            chunks.push(LlmStreamChunk::ToolCall(ToolCallArgs {
                id: "mock-echo-2".into(),
                name,
                arguments: args,
            }));
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: 5,
                completion_tokens: 3,
                total_tokens: 8,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
                reasoning_tokens: 0,
            });
            chunks.push(tool_calls_finish());
            return Ok(chunks);
        }

        if has_tool_result {
            // 回复钩子守卫(问题②规划器侦察):工具结果回填后的再生成,若消息仍携带
            // [[reply_if:]]/[[reply:]] 钩子,应产出钩子指定内容(如规划器侦察轮后的
            // 计划 JSON),而非通用完成回复。既有工具循环测试(user 消息只含
            // [[tool:...]]、不含回复钩子)不受影响;无 tool 结果时回复钩子仍在
            // 下方原位置判定(与 empty_if 等钩子的相对优先级不变)。
            if let Some(reply) = extract_reply_if_marker(&last_user, messages) {
                return Ok(text_reply_chunks(reply, messages));
            }
            if let Some(reply) = extract_reply_marker(&last_user) {
                return Ok(text_reply_chunks(reply, messages));
            }
            let reply = "（模拟回复）工具调用已完成,结果已回填上下文,这是最终的回复内容。";
            chunks.push(LlmStreamChunk::Token(reply.to_string()));
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: 10,
                completion_tokens: reply.chars().count() as i64,
                total_tokens: 10 + reply.chars().count() as i64,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
                reasoning_tokens: 0,
            });
            chunks.push(LlmStreamChunk::Finish {
                reason: "stop".into(),
            });
            return Ok(chunks);
        }

        // 测试钩子:[[tool:name {"json"}]] → 返回 ToolCall
        // 仅当该工具在本轮 tools 白名单内才产出(见 tool_offered):反射轮只下发
        // censor_text/revise_passage/read,若无视白名单会凭空再执行一次 write,
        // 产生重复副作用与多余快照——真实模型受 tools 参数约束,不可能这样调用。
        if let Some((name, args)) =
            extract_tool_marker(&last_user).filter(|(n, _)| tool_offered(&params, n))
        {
            chunks.push(LlmStreamChunk::ToolCall(ToolCallArgs {
                id: "mock-call-1".into(),
                name,
                arguments: args,
            }));
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: 5,
                completion_tokens: 3,
                total_tokens: 8,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
                reasoning_tokens: 0,
            });
            chunks.push(tool_calls_finish());
            return Ok(chunks);
        }

        // 测试钩子:[[tool_raw:name {"json"}]] → 模拟「max_tokens 把 tool_call 参数
        // JSON 切成半截」(问题①截断自愈,2026-08-31 deepseek 实测形态):输出预算不足
        // (max_tokens < TOOL_RAW_MIN_BUDGET)时返回 ToolCall(arguments = args 左半,
        // 非法 JSON)+ Finish{reason:"length"};预算充足(自愈翻倍重发后)返回完整
        // ToolCall(arguments = args 原文)。重发为同消息数组原样重发,故按预算区分两轮。
        if let Some((name, args)) = extract_tool_raw_marker(&last_user) {
            if params.max_tokens < TOOL_RAW_MIN_BUDGET {
                let half: String = args.chars().take(args.chars().count() / 2).collect();
                chunks.push(LlmStreamChunk::ToolCall(ToolCallArgs {
                    id: "mock-raw-1".into(),
                    name,
                    arguments: half,
                }));
                chunks.push(LlmStreamChunk::Usage {
                    prompt_tokens: 5,
                    completion_tokens: 3,
                    total_tokens: 8,
                    prompt_cache_hit_tokens: 0,
                    prompt_cache_miss_tokens: 0,
                    reasoning_tokens: 0,
                });
                chunks.push(LlmStreamChunk::Finish {
                    reason: "length".into(),
                });
                return Ok(chunks);
            }
            chunks.push(LlmStreamChunk::ToolCall(ToolCallArgs {
                id: "mock-raw-1".into(),
                name,
                arguments: args,
            }));
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: 5,
                completion_tokens: 3,
                total_tokens: 8,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
                reasoning_tokens: 0,
            });
            chunks.push(tool_calls_finish());
            return Ok(chunks);
        }

        // 测试钩子:[[empty]] → 返回空内容(仅 Usage,无 Token)。
        // 用于端到端验证「反思失败有界」:模型输出为空 → 反思必失败 → 引擎应重试有限次后正常结束,
        // 绝不进入无限循环(回归:2026-08-06 日志中 1ms 间隔的 reflect 洪流)。
        if last_user.contains("[[empty]]") {
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: 3,
                completion_tokens: 0,
                total_tokens: 3,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
                reasoning_tokens: 0,
            });
            return Ok(chunks);
        }

        // 测试钩子:[[empty_if:子串]] → 仅当任一 system 消息含该子串时返回空内容(仅 Usage);
        // 否则忽略本钩子、继续后续匹配。用于按调用方区分空输出注入点(任务模式 WP6:
        // 规划/步骤正常返回而「任务汇总者」调用返回空,测汇总路径的空输出分级重试)。
        // 与 [[reply:]] 共存时置于其后不影响 reply 在首个 "]]" 的截断语义。
        if let Some(needle) = extract_empty_if_marker(&last_user) {
            if messages
                .iter()
                .any(|m| m.role == "system" && m.content.contains(&needle))
            {
                chunks.push(LlmStreamChunk::Usage {
                    prompt_tokens: 3,
                    completion_tokens: 0,
                    total_tokens: 3,
                    prompt_cache_hit_tokens: 0,
                    prompt_cache_miss_tokens: 0,
                    reasoning_tokens: 0,
                });
                return Ok(chunks);
            }
        }

        // 测试钩子:[[floors]] → 回显完整 LLM 消息序列(每行 `[角色] 内容`),
        // 供集成测试断言提示词注入结果:简单模式注入文本、楼层(含宏展开)与位置。
        if messages
            .iter()
            .any(|m| m.role == "user" && m.content.contains("[[floors]]"))
        {
            let lines: Vec<String> = messages
                .iter()
                .map(|m| format!("[{}] {}", m.role, m.content))
                .collect();
            let reply = lines.join("\n---FLOOR-SEP---\n");
            let prompt_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
            let completion_tokens = reply.chars().count() as i64;
            chunks.push(LlmStreamChunk::Token(reply));
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: (prompt_chars as f64 / 4.0).ceil() as i64,
                completion_tokens,
                total_tokens: (prompt_chars as f64 / 4.0).ceil() as i64 + completion_tokens,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
                reasoning_tokens: 0,
            });
            chunks.push(LlmStreamChunk::Finish {
                reason: "stop".into(),
            });
            return Ok(chunks);
        }

        // 测试钩子:[[reply_if:子串|内容]] → 仅当任一 system 消息含「子串」时回复「内容」;
        // 同一 user 消息可携带多条本钩子,按出现顺序取首个命中的(全部未命中则继续后续匹配)。
        // 仅匹配 system 是有意设计:钩子文本本身常经 user 消息贯穿多阶段(任务引擎
        // planner/主 agent/审计/汇总共用同一目标文本),若匹配 user 会逐阶段自命中;
        // system 由各调用方提示词常量区分(team 规划器/审计员/汇总者,custom 步骤指令等)。
        // 供集成测试按「调用方身份」区分回复(批次 4.3b 六模式 team/custom 用)。
        if let Some(reply) = extract_reply_if_marker(&last_user, messages) {
            return Ok(text_reply_chunks(reply, messages));
        }

        // 测试钩子:[[reply_stream:内容...]] → 与 [[reply:]] 同截断语义(取到首个 "]]",
        // 无 "]]" 取到行尾),但逐字切分为多 Token chunk 且无 sleep(批次 R4 任务模式
        // 流式输出测试用:[[reply:]] 系单 chunk 直出,无法验证攒批「事件数远小于
        // token 数」;默认分支虽逐字但每字 8ms,长文本测试过慢)。
        if let Some(reply) = extract_reply_stream_marker(&last_user) {
            let prompt_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
            let prompt_tokens = (prompt_chars as f64 / 4.0).ceil() as i64;
            let completion_tokens = reply.chars().count() as i64;
            for ch in reply.chars() {
                if *abort.borrow() {
                    return Err("生成已中断".into());
                }
                chunks.push(LlmStreamChunk::Token(ch.to_string()));
            }
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
                reasoning_tokens: 0,
            });
            chunks.push(LlmStreamChunk::Finish {
                reason: "stop".into(),
            });
            return Ok(chunks);
        }

        // 测试钩子:[[reply:内容...]] → 直接回复指定内容(整条回复 = 标记后的文本)。
        // 供集成测试验证酒馆助手输出协议(<UpdateVariable> 解析/剥离/变量持久化)。
        if let Some(reply) = extract_reply_marker(&last_user) {
            return Ok(text_reply_chunks(reply, messages));
        }

        // 测试钩子:[[mvu_tool:name|json]] → 返回 ToolCall(两步生成第二轮专用)。
        // 与 [[reply:...]] 配合:reply 解析在首个 "]]" 截断,标记残余随角色回复保留到
        // 第二轮 user 消息;约定 args 为单行 JSON,此处取到行尾/首个 "]]" 即可还原。
        if let Some((name, args)) = extract_mvu_tool_marker(&last_user) {
            chunks.push(LlmStreamChunk::ToolCall(ToolCallArgs {
                id: "mock-mvu-call-1".into(),
                name,
                arguments: args,
            }));
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: 5,
                completion_tokens: 3,
                total_tokens: 8,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
                reasoning_tokens: 0,
            });
            chunks.push(tool_calls_finish());
            return Ok(chunks);
        }

        // 测试钩子:[[mvu_text:内容]] → 返回 Token(两步生成第二轮文本协议回退专用,
        // 模拟模型直接输出 <StatusBar> 等协议文本;与 [[reply:]] 配合方式同 mvu_tool)。
        if let Some(text) = extract_mvu_text_marker(&last_user) {
            let prompt_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
            chunks.push(LlmStreamChunk::Token(text.clone()));
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: (prompt_chars as f64 / 4.0).ceil() as i64,
                completion_tokens: (text.chars().count() as f64 / 4.0).ceil() as i64,
                total_tokens: (prompt_chars as f64 / 4.0).ceil() as i64
                    + (text.chars().count() as f64 / 4.0).ceil() as i64,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
                reasoning_tokens: 0,
            });
            chunks.push(LlmStreamChunk::Finish {
                reason: "stop".into(),
            });
            return Ok(chunks);
        }

        // 测试钩子:[[finish:原因|内容]] → 返回 Token(内容)+ Finish{reason:原因}
        //(可观测性问题①:模拟上游 max_tokens 截断,任务模式 finish_reason 透出测试用)。
        // 「原因」如 length/stop/content_filter;「内容」可省(缺省给固定半截文本)。
        // 内容与 [[reply:]] 同截断语义:取到首个 "]]" 为止,内容内含 "]]" 须用
        // \u005d 转义。置于各内容钩子之后、默认回复之前,不与 reply/reply_if 组合使用
        //(同时出现时 reply 系钩子优先,本钩子不生效)。
        if let Some((reason, text)) = extract_finish_marker(&last_user) {
            let prompt_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
            chunks.push(LlmStreamChunk::Token(text.clone()));
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: (prompt_chars as f64 / 4.0).ceil() as i64,
                completion_tokens: (text.chars().count() as f64 / 4.0).ceil() as i64,
                total_tokens: (prompt_chars as f64 / 4.0).ceil() as i64
                    + (text.chars().count() as f64 / 4.0).ceil() as i64,
                prompt_cache_hit_tokens: 0,
                prompt_cache_miss_tokens: 0,
                reasoning_tokens: 0,
            });
            chunks.push(LlmStreamChunk::Finish { reason });
            return Ok(chunks);
        }

        let excerpt: String = last_user.chars().take(60).collect();
        let reply = format!(
            "（模拟回复）我已收到你的消息:「{excerpt}」\n\n当前为演示模式,未连接真实模型。\n可在左下角「连接状态」处配置 OpenAI 兼容后端,或保持 Mock 体验完整 Agent 流程。\n\n*Agent 引擎已先后完成规划、执行、反思,输出质量检查通过。*"
        );
        let prompt_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
        let prompt_tokens = (prompt_chars as f64 / 4.0).ceil() as i64;
        let completion_tokens = (reply.chars().count() as f64 / 4.0).ceil() as i64;

        for ch in reply.chars() {
            if *abort.borrow() {
                return Err("生成已中断".into());
            }
            chunks.push(LlmStreamChunk::Token(ch.to_string()));
            tokio::time::sleep(Duration::from_millis(8)).await;
        }
        chunks.push(LlmStreamChunk::Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
            reasoning_tokens: 0,
        });
        chunks.push(LlmStreamChunk::Finish {
            reason: "stop".into(),
        });
        Ok(chunks)
    }
}

/// 带工具调用轮次的 Finish 块(F6,2026-09-10):与真实连接器行为对齐——
/// sse_parser 在 finish_reason=tool_calls 时也产出 Finish{tool_calls},
/// 使任务模式调用面板的 finish 列不再为空。
fn tool_calls_finish() -> LlmStreamChunk {
    LlmStreamChunk::Finish {
        reason: "tool_calls".into(),
    }
}

/// 文本回复 chunk 序列(Token + 按字符估算的 Usage + Finish{stop});
/// [[reply:]]/[[reply_if:]] 原位置与 has_tool_result 分支内的回复钩子守卫共用。
fn text_reply_chunks(reply: String, messages: &[LlmMessage]) -> Vec<LlmStreamChunk> {
    let prompt_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
    let prompt_tokens = (prompt_chars as f64 / 4.0).ceil() as i64;
    let completion_tokens = (reply.chars().count() as f64 / 4.0).ceil() as i64;
    vec![
        LlmStreamChunk::Token(reply),
        LlmStreamChunk::Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            prompt_cache_hit_tokens: 0,
            prompt_cache_miss_tokens: 0,
            reasoning_tokens: 0,
        },
        LlmStreamChunk::Finish {
            reason: "stop".into(),
        },
    ]
}

/// 提取 [[reply:内容...]] 标记;内容取到首个 "]]"(无 "]]" 则取到行尾)。
fn extract_reply_marker(input: &str) -> Option<String> {
    let marker_end = input.find("[[reply:")?;
    let rest = &input[marker_end + 8..];
    Some(
        rest.find("]]")
            .map(|end| &rest[..end])
            .unwrap_or(rest)
            .to_string(),
    )
}

/// 提取 [[reply_stream:内容...]] 标记(批次 R4 逐字流式钩子);截断语义同 [[reply:]]。
fn extract_reply_stream_marker(input: &str) -> Option<String> {
    let marker_end = input.find("[[reply_stream:")?;
    let rest = &input[marker_end + "[[reply_stream:".len()..];
    Some(
        rest.find("]]")
            .map(|end| &rest[..end])
            .unwrap_or(rest)
            .to_string(),
    )
}

/// 提取 [[tool_raw:name {"json"}]] 标记(问题①截断 tool_call 模拟);返回 (name, 完整 args)。
/// args 取到首个 "]]"(与 [[tool:]] 同截断语义,args 内不得含 "]]")。
fn extract_tool_raw_marker(input: &str) -> Option<(String, String)> {
    let start = input.find("[[tool_raw:")?;
    let rest = &input[start + "[[tool_raw:".len()..];
    let end = rest.find("]]")?;
    let inner = &rest[..end];
    let (name, args) = inner.split_once(' ')?;
    Some((name.trim().to_string(), args.trim().to_string()))
}

/// 提取 [[finish:原因|内容]] 标记(可观测性问题①截断模拟);返回 (reason, 内容)。
/// 原因取到 '|' 或首个 "]]"(空白/空串视为未命中);内容取到首个 "]]"
///(与 [[reply:]] 同截断语义:内容内含 "]]" 须用 \u005d 转义),省略时给固定半截文本。
fn extract_finish_marker(input: &str) -> Option<(String, String)> {
    let start = input.find("[[finish:")?;
    let rest = &input[start + "[[finish:".len()..];
    let end = rest.find("]]").unwrap_or(rest.len());
    let inner = &rest[..end];
    let (reason, content) = match inner.split_once('|') {
        Some((r, c)) => (r.trim(), c.to_string()),
        None => (
            inner.trim(),
            "（模拟回复）输出在 max_tokens 处被截断,后半句".to_string(),
        ),
    };
    if reason.is_empty() {
        return None;
    }
    Some((reason.to_string(), content))
}

/// 提取 [[mvu_tool:name|args]] 标记;返回 (name, arguments_json)。
/// args 取到首个换行或 "]]"(测试约定 args 为单行 JSON;与 [[reply:]] 截断配合时无 "]]")。
fn extract_mvu_tool_marker(input: &str) -> Option<(String, String)> {
    let start = input.find("[[mvu_tool:")?;
    let rest = &input[start + "[[mvu_tool:".len()..];
    let end = rest.find('\n').unwrap_or(rest.len());
    let end = rest[..end].find("]]").unwrap_or(end);
    let inner = &rest[..end];
    let (name, args) = inner.split_once('|')?;
    Some((name.trim().to_string(), args.trim().to_string()))
}

/// 提取 [[mvu_text:内容]] 标记;内容取到首个换行或 "]]"(约定单行)。
/// 测试约定:SB{内容} 展开为 <StatusBar>内容</StatusBar> —— 避免第一轮正文携带
/// 完整协议块被收尾剥离,第二轮才还原为真实标签(两步生成文本协议回退测试用)。
fn extract_mvu_text_marker(input: &str) -> Option<String> {
    let start = input.find("[[mvu_text:")?;
    let rest = &input[start + "[[mvu_text:".len()..];
    let end = rest.find('\n').unwrap_or(rest.len());
    let end = rest[..end].find("]]").unwrap_or(end);
    let text = rest[..end].trim().to_string();
    if text.is_empty() {
        return None;
    }
    if let Some(inner) = text.strip_prefix("SB{").and_then(|t| t.strip_suffix('}')) {
        return Some(format!("<StatusBar>{inner}</StatusBar>"));
    }
    Some(text)
}

/// 本轮是否下发了该工具(tools 白名单语义)。
///
/// 真实模型只能调用调用方提供的工具;mock 的 `[[tool:...]]` 钩子此前无视该约束,
/// 导致「工具白名单被收窄」的轮次(如反射轮仅 censor_text/revise_passage/read)
/// 仍会重复产出同一写工具调用,凭空多出副作用与快照——那是 mock 不忠实,而非真实行为。
/// tools 为空表示本轮未启用工具(不靠白名单限制,保留旧行为,兼容既有用例)。
fn tool_offered(params: &GenerationParams, name: &str) -> bool {
    params.tools.is_empty() || params.tools.iter().any(|t| t.name == name)
}

/// 提取 [[tool:name {"json"}]] 标记;返回 (name, arguments_json)
fn extract_tool_marker(input: &str) -> Option<(String, String)> {
    let start = input.find("[[tool:")?;
    let rest = &input[start + 7..];
    let end = rest.find("]]")?;
    let inner = &rest[..end];
    let (name, args) = inner.split_once(' ')?;
    Some((name.trim().to_string(), args.trim().to_string()))
}

/// 提取 [[tool_echo:name {"json"}]] 标记(并行工具调用回显钩子);返回 (name, arguments_json)
fn extract_tool_echo_marker(input: &str) -> Option<(String, String)> {
    let start = input.find("[[tool_echo:")?;
    let rest = &input[start + "[[tool_echo:".len()..];
    let end = rest.find("]]")?;
    let inner = &rest[..end];
    let (name, args) = inner.split_once(' ')?;
    Some((name.trim().to_string(), args.trim().to_string()))
}

/// 提取 [[tool_loop:name|N args...]] 标记;返回 (name, 轮数, arguments_json)。
/// N 为工具循环持续轮数(含首轮);args 为每次调用回传的 arguments(单行 JSON)。
fn extract_tool_loop_marker(input: &str) -> Option<(String, usize, String)> {
    let start = input.find("[[tool_loop:")?;
    let rest = &input[start + "[[tool_loop:".len()..];
    let end = rest.find("]]")?;
    let inner = &rest[..end];
    let (name, tail) = inner.split_once('|')?;
    let (n_str, args) = tail.split_once(' ')?;
    let n: usize = n_str.trim().parse().ok()?;
    Some((name.trim().to_string(), n, args.trim().to_string()))
}

/// 提取 [[empty_if:子串]] 标记;子串取到首个 "]]"(空白视为未命中)。
/// 命中后由调用方判定 system 消息是否含该子串,决定是否返回空内容。
fn extract_empty_if_marker(input: &str) -> Option<String> {
    let start = input.find("[[empty_if:")?;
    let rest = &input[start + "[[empty_if:".len()..];
    let end = rest.find("]]")?;
    let needle = rest[..end].trim().to_string();
    if needle.is_empty() {
        None
    } else {
        Some(needle)
    }
}

/// 提取 [[reply_if:子串|内容]] 标记(可多条):按出现顺序返回首个「子串命中任一
/// system 消息」的钩子内容;子串为空或全部未命中返回 None。
/// 内容取到首个 "]]"(与 [[reply:]] 同截断语义;内容内含 "]]" 须用 \u005d 转义)。
/// 命中判定排除「钩子语法自身」的出现:任务目标文本常携带本钩子并被各阶段包进
/// system(如 custom 的任务目标上下文),其中的子串是数据不是指令,不视为命中。
fn extract_reply_if_marker(input: &str, messages: &[LlmMessage]) -> Option<String> {
    const MARK: &str = "[[reply_if:";
    let mut search_from = 0usize;
    while let Some(rel) = input[search_from..].find(MARK) {
        let start = search_from + rel;
        let rest = &input[start + MARK.len()..];
        let end = rest.find("]]")?;
        let inner = &rest[..end];
        if let Some((needle, content)) = inner.split_once('|') {
            let needle = needle.trim();
            if !needle.is_empty() && system_contains_needle(messages, needle) {
                return Some(content.to_string());
            }
        }
        search_from = start + MARK.len();
    }
    None
}

/// 子串命中判定:任一 system 消息含该子串,且出现位置不是紧跟在 "[[reply_if:"
/// 之后(钩子语法自身的出现是数据,不算命中——否则携带钩子的目标文本被包进
/// system 后会在每个阶段自命中)。
fn system_contains_needle(messages: &[LlmMessage], needle: &str) -> bool {
    messages.iter().filter(|m| m.role == "system").any(|m| {
        m.content
            .match_indices(needle)
            .any(|(idx, _)| !m.content[..idx].ends_with("[[reply_if:"))
    })
}

/// 提取 [[fail:消息]] 标记;返回错误消息(模拟模型请求失败)
fn extract_fail_marker(input: &str) -> Option<String> {
    let start = input.find("[[fail:")?;
    let rest = &input[start + "[[fail:".len()..];
    let end = rest.find("]]")?;
    let msg = rest[..end].trim().to_string();
    if msg.is_empty() {
        None
    } else {
        Some(msg)
    }
}

impl Default for MockConnector {
    fn default() -> Self {
        Self::new()
    }
}
