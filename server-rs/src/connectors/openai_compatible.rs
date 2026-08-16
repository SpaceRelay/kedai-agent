// OpenAI 兼容连接器(与 Node 版 connectors/openai-compatible.ts 对齐)
use crate::models::types::{GenerationParams, LlmMessage, LlmStreamChunk, ToolCallArgs};
use futures::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// 生成请求自动重试次数上限(总尝试次数 = 1 + 重试次数)
const MAX_ATTEMPTS: usize = 3;
/// 连接/响应头阶段超时(SSE 流开始前;流开始后读取不受此限制)
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 建连超时
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 构造带超时的 HTTP 客户端(连接阶段超时;整体请求超时由调用方按流式语义控制)
fn make_client() -> Client {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .unwrap_or_else(|_| Client::new())
}

/// 重试退避:base 500ms 指数增长(2^(attempt-1)),上限 5s,加 ≤250ms jitter;
/// 提供 Retry-After(秒)时以其为准(上限 30s,防止误配过大)。
fn provider_host(base_url: &str) -> String {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| "invalid-host".into())
}

fn retry_delay(attempt: usize, retry_after: Option<u64>) -> Duration {
    if let Some(secs) = retry_after {
        return Duration::from_secs(secs.min(30));
    }
    let base_ms = 500u64.saturating_mul(1u64 << (attempt.saturating_sub(1) as u32));
    let base_ms = base_ms.min(5000);
    // 无 rand 依赖:以系统时钟纳秒做轻量 jitter,避免多请求同时重试打满上游
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 % 250)
        .unwrap_or(0);
    Duration::from_millis(base_ms + jitter)
}

/// 重试等待:可响应中断(abort 时立即返回 Err,不空等退避)
async fn wait_retry(delay: Duration, abort: &mut watch::Receiver<bool>) -> Result<(), String> {
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(()),
        changed = abort.changed() => {
            if changed.is_err() || *abort.borrow() {
                Err("生成已中断".into())
            } else {
                Ok(())
            }
        }
    }
}

/// 记录重试日志(不向 SSE 流注入事件——LlmStreamChunk 无提示通道,避免协议侵入)
fn log_retry(msg: &str, attempt: usize) {
    crate::utils::logger::info(
        "模型请求重试",
        &[
            ("message", Value::String(msg.to_string())),
            ("attempt", Value::from(attempt)),
        ],
    );
}

pub struct OpenAiCompatibleConnector {
    base_url: String,
    api_key: String,
    model: String,
    client: Client,
}

impl OpenAiCompatibleConnector {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        OpenAiCompatibleConnector {
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            client: make_client(),
        }
    }

    /// 当前模型名
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 切换模型(保留 base_url / api_key)
    pub fn with_model(&self, model: &str) -> Self {
        OpenAiCompatibleConnector {
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            model: model.to_string(),
            client: make_client(),
        }
    }

    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        if !self.api_key.is_empty() {
            if let Ok(v) =
                reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            {
                h.insert(reqwest::header::AUTHORIZATION, v);
            }
        }
        h
    }

    /// 拉取模型列表;失败或格式不符时回退当前模型(不致命)。
    /// 对缺 /v1 的旧配置做一次补 /v1 的兜底请求,避免「{base}/models → 404」。
    pub async fn list_models_async(&self) -> Vec<String> {
        let mut attempts = vec![format!("{}/models", self.base_url)];
        if self.base_url.ends_with("/v1") {
            attempts.push(format!("{}/models", self.base_url.trim_end_matches("/v1")));
        } else {
            attempts.push(format!("{}/v1/models", self.base_url));
        }
        for url in attempts {
            let resp = match self
                .client
                .get(&url)
                .headers(self.auth_headers())
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r,
                _ => continue,
            };
            match resp.json::<Value>().await {
                Ok(v) => {
                    let models: Vec<String> = v
                        .get("data")
                        .and_then(|d| d.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|m| {
                                    m.get("id")
                                        .and_then(|id| id.as_str())
                                        .map(|s| s.to_string())
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if !models.is_empty() {
                        return models;
                    }
                }
                Err(_) => continue,
            }
        }
        vec![self.model.clone()]
    }

    /// 真实探测 `/models`,区分端点不可达、认证失败与模型列表回退。
    pub async fn test_connection(&self) -> Value {
        let url = format!("{}/models", self.base_url);
        let response = self
            .client
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await;
        let Ok(response) = response else {
            return json!({
                "ok": false, "message": "API 端点不可达", "endpoint_reachable": false,
                "authenticated": false, "fallback_used": false, "http_status": null, "models": []
            });
        };
        let status = response.status();
        if matches!(status.as_u16(), 401 | 403) {
            return json!({
                "ok": false, "message": "API 端点可达,但认证失败", "endpoint_reachable": true,
                "authenticated": false, "fallback_used": false, "http_status": status.as_u16(), "models": []
            });
        }
        if !status.is_success() {
            return json!({
                "ok": false, "message": format!("API 端点可达,返回 HTTP {status}"), "endpoint_reachable": true,
                "authenticated": true, "fallback_used": false, "http_status": status.as_u16(), "models": []
            });
        }
        let body = response.json::<Value>().await.ok();
        let models: Vec<String> = body
            .as_ref()
            .and_then(|value| value.get("data"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let fallback_used = models.is_empty();
        let models = if fallback_used {
            vec![self.model.clone()]
        } else {
            models
        };
        json!({
            "ok": true,
            "message": if fallback_used { "连接成功,但模型列表无效,已回退当前模型".to_string() } else { format!("连接成功,可用模型 {} 个", models.len()) },
            "endpoint_reachable": true, "authenticated": true, "fallback_used": fallback_used,
            "http_status": status.as_u16(), "models": models
        })
    }

    /// 流式生成:POST {baseUrl}/chat/completions,SSE 逐行解析
    /// 支持 function calling:params.tools 非空时下发 tools;delta.tool_calls 按 index 聚合
    pub async fn generate(
        &self,
        messages: &[LlmMessage],
        params: GenerationParams,
        abort: watch::Receiver<bool>,
    ) -> Result<Vec<LlmStreamChunk>, String> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.generate_stream(messages, params, abort, tx).await?;
        let mut chunks = Vec::new();
        while let Some(chunk) = rx.recv().await {
            chunks.push(chunk);
        }
        Ok(chunks)
    }

    /// 流式生成:网络块到达后立即增量解析并发送,等待读取期间也监听中断。
    /// 自动重试:对 408/429/5xx 与连接错误做指数退避(尊重 Retry-After),仅在
    /// 尚未收到任何可见 token/工具调用前重试(避免重复输出);最大 MAX_ATTEMPTS 次。
    pub async fn generate_stream(
        &self,
        messages: &[LlmMessage],
        params: GenerationParams,
        mut abort: watch::Receiver<bool>,
        tx: mpsc::UnboundedSender<LlmStreamChunk>,
    ) -> Result<(), String> {
        let url = format!("{}/chat/completions", self.base_url);
        let tool_names: Vec<Value> = params
            .tools
            .iter()
            .map(|tool| Value::String(tool.name.clone()))
            .collect();
        crate::utils::logger::info(
            "provider_round_start",
            &[
                ("mode", Value::String("stream".into())),
                ("model", Value::String(self.model.clone())),
                (
                    "provider_host",
                    Value::String(provider_host(&self.base_url)),
                ),
                ("step", Value::String("generate".into())),
                ("tool_names", Value::Array(tool_names)),
                ("tool_count", Value::from(params.tools.len())),
                (
                    "tool_choice",
                    Value::String(format!("{:?}", params.tool_choice)),
                ),
            ],
        );
        let mut body = json!({
            "model": self.model,
            "messages": to_openai_messages(messages),
            "stream": true,
            "temperature": params.temperature,
            "top_p": params.top_p,
            "max_tokens": params.max_tokens,
        });
        if let Some(stop) = &params.stop {
            if !stop.is_empty() {
                body["stop"] = json!(stop);
            }
        }
        if !params.tools.is_empty() {
            // OpenAI function calling 标准格式:{"type":"function","function":{name,description,parameters}}
            // 部分兼容后端严格要求 type 字段,缺省会 400
            body["tools"] = Value::Array(
                params
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect(),
            );
            // tool_choice:按 GenerationParams 策略序列化(默认 auto,保持原行为);
            // 支持 none(禁止调用)/ required(强制调用至少一个)/ function(name)(指定工具)
            body["tool_choice"] = match &params.tool_choice {
                crate::models::types::ToolChoice::Auto => Value::from("auto"),
                crate::models::types::ToolChoice::None => Value::from("none"),
                crate::models::types::ToolChoice::Required => Value::from("required"),
                crate::models::types::ToolChoice::Function(name) => json!({
                    "type": "function",
                    "function": { "name": name }
                }),
            };
            // 并行工具调用开关:仅在显式指定时下发(缺省让后端自行决定)
            if let Some(parallel) = params.parallel_tool_calls {
                body["parallel_tool_calls"] = Value::from(parallel);
            }
        }

        // 请求 + 读响应头阶段:可重试。一旦进入 SSE 读取阶段(收到首个字节)即不再重试。
        let mut attempt = 0usize;
        let resp = loop {
            attempt += 1;
            if *abort.borrow() {
                return Err("生成已中断".into());
            }
            let req = self
                .client
                .post(&url)
                .headers(self.auth_headers())
                .json(&body);
            // 发送 + 等待响应头;连接/整体超时由 timeout 兜底(流开始后不再受此限制)
            let sent = tokio::time::timeout(REQUEST_TIMEOUT, req.send()).await;
            let resp = match sent {
                Err(_) => {
                    let e = format!("请求 OpenAI 兼容接口超时({}s)", REQUEST_TIMEOUT.as_secs());
                    if attempt < MAX_ATTEMPTS {
                        log_retry(&e, attempt);
                        wait_retry(retry_delay(attempt, None), &mut abort).await?;
                        continue;
                    }
                    return Err(e);
                }
                Ok(Err(e)) => {
                    let e = format!("请求 OpenAI 兼容接口失败: {e}");
                    if attempt < MAX_ATTEMPTS {
                        log_retry(&e, attempt);
                        wait_retry(retry_delay(attempt, None), &mut abort).await?;
                        continue;
                    }
                    return Err(e);
                }
                Ok(Ok(r)) => r,
            };
            let status = resp.status();
            if status.is_success() {
                break resp;
            }
            // 可重试状态:408/429/5xx(尊重 Retry-After);其余直接失败
            let retryable = matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504);
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            let text = resp.text().await.unwrap_or_default();
            let truncated: String = text.chars().take(300).collect();
            if retryable && attempt < MAX_ATTEMPTS {
                log_retry(&format!("上游返回 {status}"), attempt);
                wait_retry(retry_delay(attempt, retry_after), &mut abort).await?;
                continue;
            }
            return Err(format!("OpenAI 兼容接口返回 {status}: {truncated}"));
        };

        let mut parser = SseParser::default();
        let mut stream = resp.bytes_stream();
        loop {
            tokio::select! {
                changed = abort.changed() => {
                    if changed.is_err() || *abort.borrow() {
                        return Err("生成已中断".into());
                    }
                }
                next = stream.next() => match next {
                    Some(Ok(bytes)) => {
                        let mut chunks = Vec::new();
                        parser.push(&bytes, &mut chunks)?;
                        for chunk in chunks {
                            tx.send(chunk).map_err(|_| "生成接收端已关闭".to_string())?;
                        }
                        if parser.is_done() {
                            break;
                        }
                    }
                    Some(Err(e)) => return Err(format!("读取响应失败: {e}")),
                    None => break,
                }
            }
        }
        let mut chunks = Vec::new();
        parser.finish(&mut chunks)?;
        for chunk in chunks {
            tx.send(chunk).map_err(|_| "生成接收端已关闭".to_string())?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
    pending_calls: HashMap<u64, ToolCallArgs>,
    done: bool,
    /// 连续非 JSON data 行计数(兼容 keepalive 等;超阈值视为协议损坏)
    bad_json_count: usize,
}

/// 非 JSON data 行容忍阈值:低于此值静默忽略(兼容 keepalive/注释行),
/// 达到后报错,避免上游错误被无限吞掉导致「空成功响应」
const MAX_BAD_JSON_EVENTS: usize = 20;

impl SseParser {
    fn push(&mut self, bytes: &[u8], out: &mut Vec<LlmStreamChunk>) -> Result<(), String> {
        self.buffer.extend_from_slice(bytes);
        while let Some((end, separator_len)) = find_event_end(&self.buffer) {
            let event = self.buffer.drain(..end).collect::<Vec<_>>();
            self.buffer.drain(..separator_len);
            self.parse_event(&event, out)?;
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<LlmStreamChunk>) -> Result<(), String> {
        if !self.buffer.is_empty() {
            let event = std::mem::take(&mut self.buffer);
            self.parse_event(&event, out)?;
        }
        self.flush_tool_calls(out)
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn parse_event(&mut self, event: &[u8], out: &mut Vec<LlmStreamChunk>) -> Result<(), String> {
        let text =
            std::str::from_utf8(event).map_err(|e| format!("SSE 响应不是有效 UTF-8: {e}"))?;
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
                    return Err(format!(
                        "上游返回过多非 JSON data 事件({} 个),疑似协议损坏: {sample}",
                        self.bad_json_count
                    ));
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
            return Err(if err_type.is_empty() {
                format!("上游返回错误: {message}")
            } else {
                format!("上游返回错误 [{err_type}]: {message}")
            });
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
            let (prompt_tokens, completion_tokens, total_tokens, prompt_cache_hit_tokens) =
                parse_usage(usage);
            out.push(LlmStreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                prompt_cache_hit_tokens,
            });
        }
        // 仅当 finish_reason == "tool_calls" 时 flush 工具调用:确保工具参数聚合完整。
        // 其他 finish_reason(stop/length 等)不提前 flush,避免半截工具调用被当作完整调用发出;
        // 残留的 pending 由 finish() 在流结束时统一兜底。
        if v.pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
            .map(|r| r == "tool_calls")
            .unwrap_or(false)
        {
            self.flush_tool_calls(out)?;
        }
        Ok(())
    }

    fn flush_tool_calls(&mut self, out: &mut Vec<LlmStreamChunk>) -> Result<(), String> {
        let mut calls: Vec<_> = self.pending_calls.drain().collect();
        calls.sort_by_key(|(index, _)| *index);
        // 完整性校验:index 重复时保留后写入的 id(OpenAI 规范同一 index 只应出现一次);
        // 缺 name 的调用过滤(无法执行);arguments 非空但非法 JSON 视为协议损坏,报错暴露。
        let mut seen_ids: HashMap<String, ()> = HashMap::new();
        for (_, call) in &calls {
            if !call.name.is_empty() {
                if !call.id.is_empty() && seen_ids.contains_key(&call.id) {
                    return Err(format!(
                        "上游返回重复工具调用 id:{} (名称 {})",
                        call.id, call.name
                    ));
                }
                if !call.id.is_empty() {
                    seen_ids.insert(call.id.clone(), ());
                }
                if !call.arguments.trim().is_empty() {
                    serde_json::from_str::<Value>(&call.arguments).map_err(|_| {
                        format!(
                            "工具 \"{}\" 的 arguments 不是合法 JSON: {}",
                            call.name,
                            call.arguments.chars().take(200).collect::<String>()
                        )
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

fn find_event_end(buffer: &[u8]) -> Option<(usize, usize)> {
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

/// LlmMessage → OpenAI 兼容 messages 数组(处理 assistant.tool_calls 与 role=tool)
fn to_openai_messages(messages: &[LlmMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), json!(m.role));
            if m.role == "tool" {
                obj.insert(
                    "tool_call_id".into(),
                    json!(m.tool_call_id.clone().unwrap_or_default()),
                );
                obj.insert("content".into(), json!(m.content));
            } else if let Some(calls) = &m.tool_calls {
                obj.insert("content".into(), Value::Null);
                // DeepSeek 系后端要求思考模式多轮调用时回传 reasoning_content,否则 400
                if let Some(rc) = &m.reasoning_content {
                    if !rc.is_empty() {
                        obj.insert("reasoning_content".into(), json!(rc));
                    }
                }
                obj.insert(
                    "tool_calls".into(),
                    Value::Array(
                        calls
                            .iter()
                            .map(|tc| {
                                json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": { "name": tc.name, "arguments": tc.arguments }
                                })
                            })
                            .collect(),
                    ),
                );
            } else {
                obj.insert("content".into(), json!(m.content));
            }
            Value::Object(obj)
        })
        .collect()
}

/// 从 OpenAI 兼容 usage 对象解析计数,返回 (prompt, completion, total, cache_hit)。
/// DeepSeek 系提供 prompt_cache_hit_tokens;缺失(其他提供商)时为 0。
fn parse_usage(u: &Value) -> (i64, i64, i64, i64) {
    let prompt_tokens = u.get("prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
    let completion_tokens = u
        .get("completion_tokens")
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let total_tokens = u.get("total_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
    let prompt_cache_hit_tokens = u
        .get("prompt_cache_hit_tokens")
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    (
        prompt_tokens,
        completion_tokens,
        total_tokens,
        prompt_cache_hit_tokens,
    )
}

/// 连接器统一枚举(与 Node 版 connectors/index.ts 对齐)
pub enum Connector {
    Mock(super::mock::MockConnector),
    OpenAi(super::openai_compatible::OpenAiCompatibleConnector),
}

pub fn connector_type(config_connector: &str) -> String {
    if config_connector == "openai-compatible" {
        "openai-compatible".into()
    } else {
        "mock".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::types::ToolDefinition;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::{timeout, Duration};

    fn test_params() -> GenerationParams {
        GenerationParams {
            temperature: 0.8,
            top_p: 0.9,
            max_tokens: 128,
            stop: None,
            tools: Vec::new(),
            max_tool_rounds: None,
            tool_choice: crate::models::types::ToolChoice::Auto,
            parallel_tool_calls: None,
        }
    }

    async fn slow_sse_server(delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let _ = socket.read(&mut request).await;
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
            let first = b"data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n";
            socket
                .write_all(format!("{:X}\r\n", first.len()).as_bytes())
                .await
                .unwrap();
            socket.write_all(first).await.unwrap();
            socket.write_all(b"\r\n").await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(delay).await;
            let done = b"data: [DONE]\n\n";
            socket
                .write_all(format!("{:X}\r\n", done.len()).as_bytes())
                .await
                .unwrap();
            socket.write_all(done).await.unwrap();
            socket.write_all(b"\r\n0\r\n\r\n").await.unwrap();
        });
        format!("http://{addr}")
    }

    /// 多轮工具调用回传:assistant 消息必须带 reasoning_content(DeepSeek 思考模式要求)
    #[test]
    fn tool_messages_echo_reasoning() {
        let msgs = vec![
            LlmMessage {
                role: "assistant".into(),
                content: String::new(),
                reasoning_content: Some("先掷骰再回答".into()),
                tool_calls: Some(vec![ToolCallArgs {
                    id: "call_1".into(),
                    name: "role".into(),
                    arguments: r#"{"sides":6}"#.into(),
                }]),
                tool_call_id: None,
            },
            LlmMessage {
                role: "tool".into(),
                content: r#"{"rolls":[4]}"#.into(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".into()),
            },
        ];
        let out = to_openai_messages(&msgs);
        // assistant:tool_calls 标准结构 + reasoning_content 回传
        assert_eq!(out[0]["role"], "assistant");
        assert_eq!(out[0]["tool_calls"][0]["type"], "function");
        assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "role");
        assert_eq!(out[0]["reasoning_content"], "先掷骰再回答");
        // tool:tool_call_id + content
        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[1]["tool_call_id"], "call_1");
        assert_eq!(out[1]["content"], r#"{"rolls":[4]}"#);
    }

    /// SSE 数据可能在任意字节位置分块,只有完整事件才应产出 token。
    #[test]
    fn sse_parser_handles_arbitrary_chunk_boundaries() {
        let mut parser = SseParser::default();
        let mut out = Vec::new();
        for part in [
            b"data: {\"choices\":[{\"delta\":{\"content\":\"\xe4".as_slice(),
            b"\xbd\xa0\"}}]}\r".as_slice(),
            b"\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"hao\"}}]}\n\n".as_slice(),
            b"data: [DONE]\n\n".as_slice(),
        ] {
            parser.push(part, &mut out).unwrap();
        }
        assert!(parser.is_done());
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[0], LlmStreamChunk::Token(text) if text == "你"));
        assert!(matches!(&out[1], LlmStreamChunk::Token(text) if text == "hao"));
    }

    /// 慢速上游的首块必须在流结束前交给调用方。
    #[tokio::test]
    async fn generate_stream_emits_before_response_finishes() {
        let base_url = slow_sse_server(Duration::from_secs(2)).await;
        let connector = OpenAiCompatibleConnector::new(&base_url, "key", "model");
        let (_abort_tx, abort_rx) = watch::channel(false);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            connector
                .generate_stream(
                    &[LlmMessage::plain("user", "hi")],
                    test_params(),
                    abort_rx,
                    tx,
                )
                .await
        });
        let chunk = timeout(Duration::from_millis(500), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(chunk, LlmStreamChunk::Token(text) if text == "first"));
        task.abort();
    }

    /// 等待慢速上游下一块时,abort 应立即结束读取而不是等网络返回。
    #[tokio::test]
    async fn generate_stream_aborts_while_waiting_for_next_chunk() {
        let base_url = slow_sse_server(Duration::from_secs(5)).await;
        let connector = OpenAiCompatibleConnector::new(&base_url, "key", "model");
        let (abort_tx, abort_rx) = watch::channel(false);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            connector
                .generate_stream(
                    &[LlmMessage::plain("user", "hi")],
                    test_params(),
                    abort_rx,
                    tx,
                )
                .await
        });
        let _ = timeout(Duration::from_millis(500), rx.recv())
            .await
            .unwrap();
        abort_tx.send(true).unwrap();
        let result = timeout(Duration::from_millis(500), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err(), "生成已中断");
    }

    /// usage 解析:DeepSeek 格式带 prompt_cache_hit_tokens(命中率数据源)
    #[test]
    fn parse_usage_reads_cache_hit_tokens() {
        // DeepSeek 风格:prompt_tokens = hit + miss
        let u = serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 200,
            "total_tokens": 1200,
            "prompt_cache_hit_tokens": 700,
            "prompt_cache_miss_tokens": 300,
        });
        assert_eq!(parse_usage(&u), (1000, 200, 1200, 700));

        // 无缓存字段的提供商(OpenAI 等)→ hit 为 0,不 panic
        let plain = serde_json::json!({
            "prompt_tokens": 500,
            "completion_tokens": 50,
            "total_tokens": 550,
        });
        assert_eq!(parse_usage(&plain), (500, 50, 550, 0));

        // 字段缺失/类型异常 → 全部回退 0
        assert_eq!(parse_usage(&serde_json::json!({})), (0, 0, 0, 0));
    }

    /// 启动一个模拟上游:捕获请求体后返回固定 SSE 流;返回 (base_url, body 持有者)
    async fn capture_request_server() -> (String, Arc<std::sync::Mutex<Option<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let holder = Arc::new(std::sync::Mutex::new(None::<String>));
        let holder2 = holder.clone();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let n = socket.read(&mut request).await.unwrap();
            let raw = String::from_utf8_lossy(&request[..n]).to_string();
            if let Some(idx) = raw.find("\r\n\r\n") {
                *holder2.lock().unwrap_or_else(|e| e.into_inner()) = Some(raw[idx + 4..].to_string());
            }
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .unwrap();
            let done = b"data: [DONE]\n\n";
            socket
                .write_all(format!("{:X}\r\n", done.len()).as_bytes())
                .await
                .unwrap();
            socket.write_all(done).await.unwrap();
            socket.write_all(b"\r\n0\r\n\r\n").await.unwrap();
        });
        (format!("http://{addr}"), holder)
    }

    /// M2:tool_choice 与 parallel_tool_calls 按 GenerationParams 序列化进请求体;
    /// 默认 Auto 时保持 "auto"(与旧行为一致)。
    #[tokio::test]
    async fn tool_choice_and_parallel_flag_serialized_in_request_body() {
        for (choice, expect) in [
            (crate::models::types::ToolChoice::Auto, json!("auto")),
            (
                crate::models::types::ToolChoice::Function("read".into()),
                json!({ "type": "function", "function": { "name": "read" } }),
            ),
        ] {
            let (base_url, body) = capture_request_server().await;
            let connector = OpenAiCompatibleConnector::new(&base_url, "key", "model");
            let (_abort_tx, abort_rx) = watch::channel(false);
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut params = test_params();
            params.tools = vec![ToolDefinition {
                name: "read".into(),
                description: "read".into(),
                parameters: serde_json::json!({"type": "object"}),
            }];
            params.tool_choice = choice;
            params.parallel_tool_calls = Some(false);
            connector
                .generate_stream(&[LlmMessage::plain("user", "hi")], params, abort_rx, tx)
                .await
                .unwrap();
            let raw = body.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default();
            let v: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(v["tool_choice"], expect, "tool_choice 序列化错误: {raw}");
            assert_eq!(v["parallel_tool_calls"], json!(false));
            assert!(v["tools"].is_array(), "tools 应下发: {raw}");
        }
    }

    /// M2:标准 OpenAI error 对象应暴露为错误,不再被静默吞掉(否则前端收到空成功 finish)
    #[test]
    fn sse_parser_surfaces_upstream_error_object() {
        let mut parser = SseParser::default();
        let mut out = Vec::new();
        let err = parser
            .push(
                b"data: {\"error\":{\"message\":\"insufficient_quota\",\"type\":\"insufficient_quota\"}}\n\n",
                &mut out,
            )
            .unwrap_err();
        assert!(
            err.contains("insufficient_quota"),
            "应暴露上游错误对象: {err}"
        );
    }

    /// M2:非 JSON data 行(keepalive/注释等)少量容忍,超过阈值视为协议损坏报错
    #[test]
    fn sse_parser_tolerates_bad_json_then_fails_at_threshold() {
        let mut parser = SseParser::default();
        let mut out = Vec::new();
        for _ in 0..5 {
            parser.push(b"data: ping\n\n", &mut out).unwrap();
        }
        assert!(out.is_empty(), "少量非 JSON 应静默忽略");

        let mut parser2 = SseParser::default();
        let mut out2 = Vec::new();
        let mut failed_at: Option<usize> = None;
        for i in 0..(MAX_BAD_JSON_EVENTS + 2) {
            let r = parser2.push(b"data: garbage\n\n", &mut out2);
            if r.is_err() {
                failed_at = Some(i);
                break;
            }
        }
        assert!(
            failed_at.is_some(),
            "连续非 JSON 达到阈值应报错(当前 {MAX_BAD_JSON_EVENTS})"
        );
        // bad_json_count 从 1 计数,达到阈值(20)即报错 → 第 20 个事件(0-indexed 19)失败
        assert_eq!(
            failed_at.unwrap(),
            MAX_BAD_JSON_EVENTS - 1,
            "应在恰好第 {MAX_BAD_JSON_EVENTS} 个坏事件时报错"
        );
    }

    /// M2:工具调用只在 finish_reason == "tool_calls" 时 flush;
    /// stop/length 等其他 finish 不提前 flush,避免半截调用被当作完整调用发出
    #[test]
    fn sse_parser_flushes_tool_calls_only_on_tool_calls_finish() {
        let mut parser = SseParser::default();
        let mut out = Vec::new();
        // 先聚合一个工具调用,但 finish_reason=stop → 不应 flush
        parser
            .push(
                b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"q\\\":1}\"}}]},\"finish_reason\":\"stop\"}]}\n\n",
                &mut out,
            )
            .unwrap();
        assert!(out.is_empty(), "非 tool_calls finish 不应 flush 工具调用");
        // finish_reason=tool_calls → flush
        parser
            .push(
                b"data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
                &mut out,
            )
            .unwrap();
        assert!(
            matches!(&out[0], LlmStreamChunk::ToolCall(c) if c.name == "read"),
            "tool_calls finish 应 flush 已聚合调用: {out:?}"
        );
    }

    /// M2:工具 arguments 非法 JSON 时 flush 报错(完整性校验),不再把坏参数交给执行器
    #[test]
    fn sse_parser_rejects_invalid_tool_arguments() {
        let mut parser = SseParser::default();
        let mut out = Vec::new();
        let err = parser
            .push(
                b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read\",\"arguments\":\"not-json\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                &mut out,
            )
            .unwrap_err();
        assert!(
            err.contains("不是合法 JSON"),
            "非法 arguments 应在 flush 时暴露: {err}"
        );
    }

    /// M2:重复工具调用 id → flush 报错(完整性校验)
    #[test]
    fn sse_parser_rejects_duplicate_tool_call_ids() {
        let mut parser2 = SseParser::default();
        let mut out2 = Vec::new();
        let err2 = parser2
            .push(
                b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"dup\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}},{\"index\":1,\"id\":\"dup\",\"function\":{\"name\":\"role\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                &mut out2,
            )
            .unwrap_err();
        assert!(err2.contains("重复工具调用 id"), "重复 id 应报错: {err2}");
    }
}
