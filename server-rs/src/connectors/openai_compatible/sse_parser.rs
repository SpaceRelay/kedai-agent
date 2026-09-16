// SSE 增量解析:事件切分 / delta 聚合 / usage 解析(自 openai_compatible.rs 迁入)
use crate::models::llm_error::{LlmError, LlmErrorKind};
use crate::models::types::{LlmStreamChunk, ToolCallArgs};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct SseParser {
    buffer: Vec<u8>,
    pending_calls: HashMap<u64, ToolCallArgs>,
    done: bool,
    /// 连续非 JSON data 行计数(兼容 keepalive 等;超阈值视为协议损坏)
    bad_json_count: usize,
}

/// 非 JSON data 行容忍阈值:低于此值静默忽略(兼容 keepalive/注释行),
/// 达到后报错,避免上游错误被无限吞掉导致「空成功响应」
pub(super) const MAX_BAD_JSON_EVENTS: usize = 20;

impl SseParser {
    pub(super) fn push(
        &mut self,
        bytes: &[u8],
        out: &mut Vec<LlmStreamChunk>,
    ) -> Result<(), LlmError> {
        self.buffer.extend_from_slice(bytes);
        while let Some((end, separator_len)) = find_event_end(&self.buffer) {
            let event = self.buffer.drain(..end).collect::<Vec<_>>();
            self.buffer.drain(..separator_len);
            self.parse_event(&event, out)?;
        }
        Ok(())
    }

    pub(super) fn finish(&mut self, out: &mut Vec<LlmStreamChunk>) -> Result<(), LlmError> {
        if !self.buffer.is_empty() {
            let event = std::mem::take(&mut self.buffer);
            self.parse_event(&event, out)?;
        }
        self.flush_tool_calls(out)
    }

    pub(super) fn is_done(&self) -> bool {
        self.done
    }

    fn parse_event(&mut self, event: &[u8], out: &mut Vec<LlmStreamChunk>) -> Result<(), LlmError> {
        let text = std::str::from_utf8(event)
            .map_err(|e| LlmError::generation(format!("SSE 响应不是有效 UTF-8: {e}")))?;
        let payload = text
            .lines()
            .filter_map(|line| {
                line.strip_suffix('\r')
                    .unwrap_or(line)
                    .strip_prefix("data:")
            })
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if payload.is_empty() {
            return Ok(());
        }
        if payload.trim() == "[DONE]" {
            self.done = true;
            self.flush_tool_calls(out)?;
            return Ok(());
        }
        // 非 JSON data 行:兼容 keepalive/注释(计数容忍),超阈值视为协议损坏。
        // 标准 OpenAI error 对象({"error":{...}})直接暴露为错误,不再静默吞掉。
        let v: Value = match serde_json::from_str(&payload) {
            Ok(value) => value,
            Err(_) => {
                self.bad_json_count += 1;
                if self.bad_json_count >= MAX_BAD_JSON_EVENTS {
                    let sample: String = payload.chars().take(200).collect();
                    return Err(LlmError::generation(format!(
                        "上游返回过多非 JSON data 事件({} 个),疑似协议损坏: {sample}",
                        self.bad_json_count
                    )));
                }
                return Ok(());
            }
        };
        // 标准错误对象:暴露给调用方,避免前端收到「空成功 finish」
        if let Some(err) = v.get("error") {
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("未知上游错误");
            let err_type = err.get("type").and_then(Value::as_str).unwrap_or("");
            let text = if err_type.is_empty() {
                format!("上游返回错误: {message}")
            } else {
                format!("上游返回错误 [{err_type}]: {message}")
            };
            // 流内错误对象的分类取自上游**结构化字段** type/code(协议词汇),
            // 不解析面向人的 message——HTTP 200 的流内报错此前靠文案子串才被判为
            // 限流/鉴权,一律归 generation_failed 会让可重试的瞬时限流变成不可重试。
            return Err(LlmError::new(classify_upstream_error_object(err), text));
        }
        // 合法 JSON 事件到来即重置连续计数
        self.bad_json_count = 0;
        if let Some(content) = v
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
        {
            if !content.is_empty() {
                out.push(LlmStreamChunk::Token(content.to_string()));
            }
        }
        if let Some(reasoning) = v
            .pointer("/choices/0/delta/reasoning_content")
            .and_then(Value::as_str)
        {
            if !reasoning.is_empty() {
                out.push(LlmStreamChunk::Reasoning(reasoning.to_string()));
            }
        }
        if let Some(calls) = v
            .pointer("/choices/0/delta/tool_calls")
            .and_then(Value::as_array)
        {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let entry = self
                    .pending_calls
                    .entry(index)
                    .or_insert_with(|| ToolCallArgs {
                        id: format!("call_{index}"),
                        name: String::new(),
                        arguments: String::new(),
                    });
                if let Some(id) = call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    entry.id = id.to_string();
                }
                if let Some(name) = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                {
                    entry.name = name.to_string();
                }
                if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                {
                    entry.arguments.push_str(arguments);
                }
            }
        }
        if let Some(usage) = v.get("usage") {
            let (
                prompt_tokens,
                completion_tokens,
                total_tokens,
                prompt_cache_hit_tokens,
                prompt_cache_miss_tokens,
                reasoning_tokens,
            ) = parse_usage(usage);
            out.push(LlmStreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                prompt_cache_hit_tokens,
                prompt_cache_miss_tokens,
                reasoning_tokens,
            });
        }
        // finish_reason 处理:
        // - tool_calls:flush 工具调用(确保参数聚合完整),并同样产出 Finish 块——
        //   带工具轮此前 finish 无值,任务模式调用面板的 finish 列空白(2026-09-10
        //   六模式实测修复 F6);Finish 块不进事件流,仅上层 process_chunk 消费,
        //   且截断自愈只认 length,故不影响既有行为。
        // - 其他非空值(stop/length/content_filter 等)产出 Finish 块供上层诊断
        //   (任务模式据此区分「真空响应」与「max_tokens 截断」)。
        // 不提前 flush 半截工具调用,残留的 pending 由 finish() 在流结束时统一兜底。
        if let Some(reason) = v
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            if reason == "tool_calls" {
                self.flush_tool_calls(out)?;
            }
            if !reason.is_empty() {
                out.push(LlmStreamChunk::Finish {
                    reason: reason.to_string(),
                });
            }
        }
        Ok(())
    }

    fn flush_tool_calls(&mut self, out: &mut Vec<LlmStreamChunk>) -> Result<(), LlmError> {
        let mut calls: Vec<_> = self.pending_calls.drain().collect();
        calls.sort_by_key(|(index, _)| *index);
        // 完整性校验:index 重复时保留后写入的 id(OpenAI 规范同一 index 只应出现一次);
        // 缺 name 的调用过滤(无法执行);arguments 非空但非法 JSON 视为协议损坏,报错暴露。
        let mut seen_ids: HashMap<String, ()> = HashMap::new();
        for (_, call) in &calls {
            if !call.name.is_empty() {
                if !call.id.is_empty() && seen_ids.contains_key(&call.id) {
                    return Err(LlmError::generation(format!(
                        "上游返回重复工具调用 id:{} (名称 {})",
                        call.id, call.name
                    )));
                }
                if !call.id.is_empty() {
                    seen_ids.insert(call.id.clone(), ());
                }
                if !call.arguments.trim().is_empty() {
                    // 保留 serde 原错(含行列位置),否则「参数不是合法 JSON」只剩半截
                    // 原文而无法判断是截断、转义还是编码问题
                    serde_json::from_str::<Value>(&call.arguments).map_err(|e| {
                        LlmError::generation(format!(
                            "工具 \"{}\" 的 arguments 不是合法 JSON({e}): {}",
                            call.name,
                            call.arguments.chars().take(200).collect::<String>()
                        ))
                    })?;
                }
            }
        }
        out.extend(calls.into_iter().filter_map(|(_, call)| {
            (!call.name.is_empty()).then_some(LlmStreamChunk::ToolCall(call))
        }));
        Ok(())
    }
}

/// 流内错误对象 → 分类:读上游**枚举化的** `error.type`/`error.code` 字段。
///
/// 这是协议字段翻译(OpenAI 兼容错误对象的既定词汇),不是对自由文案的子串猜测——
/// 判定只发生在连接器边界,分类结果随错误向上层传递,上层不再解析文案。
/// 无匹配时按可重试的上游故障处理(与 HTTP 5xx/连接中断同语义)。
fn classify_upstream_error_object(err: &Value) -> LlmErrorKind {
    let hint = ["type", "code"]
        .iter()
        .filter_map(|k| err.get(*k).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    // 判定顺序与批次 4.3 前的文案匹配保持一致:超时 > 限流 > 鉴权 > 其他
    if hint.contains("timeout") {
        LlmErrorKind::Timeout
    } else if hint.contains("rate_limit") || hint.contains("too_many_requests") {
        LlmErrorKind::RateLimited
    } else if hint.contains("authentication")
        || hint.contains("permission")
        || hint.contains("unauthorized")
        || hint.contains("invalid_api_key")
    {
        LlmErrorKind::AuthFailed
    } else {
        LlmErrorKind::Upstream
    }
}

pub(super) fn find_event_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

/// 从 OpenAI 兼容 usage 对象解析计数,返回 (prompt, completion, total, cache_hit, cache_miss, reasoning)。
/// 缓存字段两种风格取其一非空即可:
///   - DeepSeek 系:prompt_cache_hit_tokens / prompt_cache_miss_tokens 原样透传(优先);
///   - OpenAI 风格:prompt_tokens_details.cached_tokens 作为 hit,miss = prompt - cached 推导。
///
/// 两者都缺失(无缓存观测的提供商)时 hit/miss 为 0。
/// reasoning 取自 completion_tokens_details.reasoning_tokens(推理模型;无此字段为 0)。
pub(super) fn parse_usage(u: &Value) -> (i64, i64, i64, i64, i64, i64) {
    let prompt_tokens = u.get("prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
    let completion_tokens = u
        .get("completion_tokens")
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let total_tokens = u.get("total_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
    let reasoning_tokens = u
        .pointer("/completion_tokens_details/reasoning_tokens")
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let deepseek_hit = u
        .get("prompt_cache_hit_tokens")
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let deepseek_miss = u
        .get("prompt_cache_miss_tokens")
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let openai_cached = u
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    // DeepSeek 字段优先;缺失时回退 OpenAI 风格(cached_tokens 为 hit,miss 推导)
    let (hit, miss) = if deepseek_hit > 0 || deepseek_miss > 0 {
        (deepseek_hit, deepseek_miss)
    } else if openai_cached > 0 {
        (openai_cached, (prompt_tokens - openai_cached).max(0))
    } else {
        (0, 0)
    };
    (
        prompt_tokens,
        completion_tokens,
        total_tokens,
        hit,
        miss,
        reasoning_tokens,
    )
}
