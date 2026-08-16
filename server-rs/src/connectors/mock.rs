// Mock 连接器(与 Node 版 connectors/mock.ts 对齐):逐字流式 + 固定回复
// 测试钩子:最后一条 user 消息含 [[tool:name {...}]] 时,首轮返回 ToolCall 分块;
// 后续轮次(消息数组含 role="tool" 结果)返回固定回复 —— 供集成测试验证工具循环。
use crate::models::types::{GenerationParams, LlmMessage, LlmStreamChunk, ToolCallArgs};
use std::time::Duration;
use tokio::sync::watch;

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
        _params: GenerationParams,
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
                });
                return Ok(chunks);
            }
            let reply = "（模拟回复）工具循环已完成,最终回复。";
            chunks.push(LlmStreamChunk::Token(reply.to_string()));
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: 5,
                completion_tokens: reply.chars().count() as i64,
                total_tokens: 5 + reply.chars().count() as i64,
                prompt_cache_hit_tokens: 0,
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
            });
            return Ok(chunks);
        }

        if has_tool_result {
            let reply = "（模拟回复）工具调用已完成,结果已回填上下文,这是最终的回复内容。";
            chunks.push(LlmStreamChunk::Token(reply.to_string()));
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: 10,
                completion_tokens: reply.chars().count() as i64,
                total_tokens: 10 + reply.chars().count() as i64,
                prompt_cache_hit_tokens: 0,
            });
            return Ok(chunks);
        }

        // 测试钩子:[[tool:name {"json"}]] → 返回 ToolCall
        if let Some((name, args)) = extract_tool_marker(&last_user) {
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
            });
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
            });
            return Ok(chunks);
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
            });
            return Ok(chunks);
        }

        // 测试钩子:[[reply:内容...]] → 直接回复指定内容(整条回复 = 标记后的文本)。
        // 供集成测试验证酒馆助手输出协议(<UpdateVariable> 解析/剥离/变量持久化)。
        if let Some(marker_end) = last_user.find("[[reply:") {
            let rest = &last_user[marker_end + 8..];
            let reply = rest
                .find("]]")
                .map(|end| &rest[..end])
                .unwrap_or(rest)
                .to_string();
            let prompt_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
            chunks.push(LlmStreamChunk::Token(reply.clone()));
            chunks.push(LlmStreamChunk::Usage {
                prompt_tokens: (prompt_chars as f64 / 4.0).ceil() as i64,
                completion_tokens: (reply.chars().count() as f64 / 4.0).ceil() as i64,
                total_tokens: (prompt_chars as f64 / 4.0).ceil() as i64
                    + (reply.chars().count() as f64 / 4.0).ceil() as i64,
                prompt_cache_hit_tokens: 0,
            });
            return Ok(chunks);
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
            });
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
            });
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
        });
        Ok(chunks)
    }
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
