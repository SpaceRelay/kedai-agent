//! 验证 token 计数缓存在「摘要循环」场景下的实际提速。
//! 模拟 trim_tool_history 的预算循环:每轮摘要后重算全部消息。
//! 运行: cargo run --release --example perf_probe3
use std::time::Instant;

fn main() {
    let mut ts = kedai_server::services::token_service::TokenService::new();
    let model = "gpt-4o";

    // 模拟 12 轮工具循环历史,每轮 assistant(arguments 大) + tool(输出大)
    let mut msgs: Vec<kedai_server::models::types::LlmMessage> = Vec::new();
    msgs.push(kedai_server::models::types::LlmMessage::plain(
        "system",
        "系统提示",
    ));
    msgs.push(kedai_server::models::types::LlmMessage::plain(
        "user",
        "任务目标",
    ));
    for i in 0..12 {
        msgs.push(kedai_server::models::types::LlmMessage {
            role: "assistant".into(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(vec![kedai_server::models::types::ToolCallArgs {
                id: format!("call-{i}"),
                name: "write".into(),
                arguments: format!("{{\"content\":\"{}\"}}", "参".repeat(2000)),
            }]),
            tool_call_id: None,
        });
        msgs.push(kedai_server::models::types::LlmMessage {
            role: "tool".into(),
            content: "结".repeat(2000),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(format!("call-{i}")),
        });
    }

    // ① 冷缓存:首次全量计数
    let t = Instant::now();
    let n = ts.count_message_tokens(&msgs, model);
    let cold = t.elapsed();
    println!(
        "① 冷缓存首次全量计数      : {:>8.3} ms  ({} tokens)",
        cold.as_secs_f64() * 1000.0,
        n
    );

    // ② 热缓存:同数组再次计数(裁剪循环里的重复调用)
    let t = Instant::now();
    for _ in 0..20 {
        ts.count_message_tokens(&msgs, model);
    }
    let hot = t.elapsed();
    println!(
        "② 热缓存 20 次重复计数    : {:>8.3} ms  ({:.3} ms/次)",
        hot.as_secs_f64() * 1000.0,
        hot.as_secs_f64() * 1000.0 / 20.0
    );

    // ③ 模拟摘要循环:每轮改动 2 条消息后重算(其余命中缓存)
    let t = Instant::now();
    for i in 0..12 {
        // 改动第 i 轮的 assistant 参数与 tool 内容(模拟 summarize_round)
        let a = 2 + i * 2;
        if let Some(calls) = msgs[a].tool_calls.as_mut() {
            calls[0].arguments = "{\"_trimmed\":true,\"chars\":2000}".into();
        }
        msgs[a + 1].content = "(较早工具结果已省略:工具 \"write\" 原输出约 2000 字符)".into();
        ts.count_message_tokens(&msgs, model);
    }
    let loop_time = t.elapsed();
    println!(
        "③ 摘要循环 12 轮(增量式)  : {:>8.3} ms",
        loop_time.as_secs_f64() * 1000.0
    );

    // 对照:清空缓存后同样的循环(等价改造前的行为)
    let mut ts2 = kedai_server::services::token_service::TokenService::new();
    let mut msgs2 = msgs.clone();
    // 还原成未摘要状态
    for i in 0..12 {
        let a = 2 + i * 2;
        if let Some(calls) = msgs2[a].tool_calls.as_mut() {
            calls[0].arguments = format!("{{\"content\":\"{}\"}}", "参".repeat(2000));
        }
        msgs2[a + 1].content = "结".repeat(2000);
    }
    let t = Instant::now();
    for i in 0..12 {
        let a = 2 + i * 2;
        if let Some(calls) = msgs2[a].tool_calls.as_mut() {
            calls[0].arguments = "{\"_trimmed\":true,\"chars\":2000}".into();
        }
        msgs2[a + 1].content = "(较早工具结果已省略)".into();
        // 只清计数缓存(保留已加载 BPE 词表),才与改造前「每轮全量重编码」等价
        ts2.clear_count_cache();
        ts2.count_message_tokens(&msgs2, model);
    }
    let baseline = t.elapsed();
    println!(
        "④ 对照(每轮全量重编码)    : {:>8.3} ms",
        baseline.as_secs_f64() * 1000.0
    );
    println!(
        "\n循环场景提速: {:.1}×  ({} ms → {} ms)",
        baseline.as_secs_f64() / loop_time.as_secs_f64(),
        (baseline.as_secs_f64() * 1000.0) as i64,
        (loop_time.as_secs_f64() * 1000.0) as i64
    );
}
