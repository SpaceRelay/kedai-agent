// OpenAI 兼容连接器单测(自 openai_compatible.rs 尾部 #[cfg(test)] 迁入)
use super::sse_parser::{parse_usage, MAX_BAD_JSON_EVENTS};
use super::*;
use crate::models::types::{ToolCallArgs, ToolDefinition};
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

/// usage 解析:DeepSeek 格式带 prompt_cache_hit_tokens/prompt_cache_miss_tokens(命中率数据源)
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
    assert_eq!(parse_usage(&u), (1000, 200, 1200, 700, 300, 0));

    // 无缓存字段的提供商(OpenAI 等)→ hit/miss 为 0,不 panic
    let plain = serde_json::json!({
        "prompt_tokens": 500,
        "completion_tokens": 50,
        "total_tokens": 550,
    });
    assert_eq!(parse_usage(&plain), (500, 50, 550, 0, 0, 0));

    // 字段缺失/类型异常 → 全部回退 0
    assert_eq!(parse_usage(&serde_json::json!({})), (0, 0, 0, 0, 0, 0));
}

/// usage 解析:OpenAI 风格 prompt_tokens_details.cached_tokens 兼容——
/// hit 取 cached_tokens,miss 由 prompt_tokens - cached 推导
#[test]
fn parse_usage_reads_openai_cached_tokens() {
    let u = serde_json::json!({
        "prompt_tokens": 1000,
        "completion_tokens": 50,
        "total_tokens": 1050,
        "prompt_tokens_details": { "cached_tokens": 600 },
    });
    assert_eq!(parse_usage(&u), (1000, 50, 1050, 600, 400, 0));

    // 两种风格并存时 DeepSeek 字段优先
    let both = serde_json::json!({
        "prompt_tokens": 1000,
        "completion_tokens": 50,
        "total_tokens": 1050,
        "prompt_cache_hit_tokens": 800,
        "prompt_cache_miss_tokens": 200,
        "prompt_tokens_details": { "cached_tokens": 600 },
    });
    assert_eq!(parse_usage(&both), (1000, 50, 1050, 800, 200, 0));
}

/// usage 解析:推理模型 completion_tokens_details.reasoning_tokens(诊断空输出的关键证据)
#[test]
fn parse_usage_reads_reasoning_tokens() {
    let u = serde_json::json!({
        "prompt_tokens": 800,
        "completion_tokens": 10000,
        "total_tokens": 10800,
        "completion_tokens_details": { "reasoning_tokens": 10000 },
    });
    assert_eq!(parse_usage(&u), (800, 10000, 10800, 0, 0, 10000));
}

/// finish_reason:stop/length 产出 Finish 块;tool_calls 同样产出
/// Finish{tool_calls}(F6,2026-09-10:带工具轮 finish 列此前落空串)。
#[test]
fn sse_parser_emits_finish_chunk() {
    let mut parser = SseParser::default();
    let mut out = Vec::new();
    parser
        .push(
            b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
            &mut out,
        )
        .unwrap();
    assert!(
        matches!(&out[..], [crate::models::types::LlmStreamChunk::Finish { reason }] if reason == "length"),
        "length 应产出 Finish 块: {out:?}"
    );

    let mut parser2 = SseParser::default();
    let mut out2 = Vec::new();
    parser2
        .push(
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"tool_calls\"}]}\n\n",
            &mut out2,
        )
        .unwrap();
    assert!(
        out2
            .iter()
            .any(|c| matches!(c, crate::models::types::LlmStreamChunk::Finish { reason } if reason == "tool_calls")),
        "tool_calls 应产出 Finish{{tool_calls}} 块(F6): {out2:?}"
    );
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
        let raw = body
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["tool_choice"], expect, "tool_choice 序列化错误: {raw}");
        assert_eq!(v["parallel_tool_calls"], json!(false));
        assert!(v["tools"].is_array(), "tools 应下发: {raw}");
        assert_eq!(
            v["stream_options"],
            json!({ "include_usage": true }),
            "流式请求应声明 include_usage 以拿到 usage: {raw}"
        );
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
/// stop/length 等其他 finish 不提前 flush,避免半截调用被当作完整调用发出。
/// F6(2026-09-10):tool_calls 完成时同时产出 Finish{tool_calls}——
/// 带工具轮此前 finish 无值,任务模式调用面板 finish 列空白。
#[test]
fn sse_parser_flushes_tool_calls_only_on_tool_calls_finish() {
    let mut parser = SseParser::default();
    let mut out = Vec::new();
    // 先聚合一个工具调用,但 finish_reason=stop → 不应 flush(stop 会产出 Finish 诊断块,但不产 ToolCall)
    parser
        .push(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"q\\\":1}\"}}]},\"finish_reason\":\"stop\"}]}\n\n",
            &mut out,
        )
        .unwrap();
    assert!(
        !out.iter().any(|c| matches!(c, LlmStreamChunk::ToolCall(_))),
        "非 tool_calls finish 不应 flush 工具调用: {out:?}"
    );
    // finish_reason=tool_calls → flush,且产出 Finish{tool_calls}(F6)
    parser
        .push(
            b"data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
            &mut out,
        )
        .unwrap();
    assert!(
        out.iter()
            .any(|c| matches!(c, LlmStreamChunk::ToolCall(c) if c.name == "read")),
        "tool_calls finish 应 flush 已聚合调用: {out:?}"
    );
    assert!(
        out.iter()
            .any(|c| matches!(c, LlmStreamChunk::Finish { reason } if reason == "tool_calls")),
        "tool_calls finish 应产出 Finish{{tool_calls}} 供上层落库(F6): {out:?}"
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
