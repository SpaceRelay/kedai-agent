// 引擎事件桥:任务模式没有聊天 SSE 客户端,引擎(execute_generation/run_tool_loop)
// 只认 mpsc::Sender<SseEvent>。此处自建通道 + drain 任务,把引擎事件翻译为任务事件
// (SseEvent::Task kind=agent_status)经 TaskService broadcast 转发到任务事件流,
// 供「调用情况」面板/事件监控观察主 agent 进度(docs/功能.md 第三节·5)。
// label 为事件文案的执行者称谓(「主 agent」/team 的「主 agent N」/「子 agent」)。
// 批次 R4:Token 不再只计字数丢弃,改经 DeltaBatcher 攒批(200ms/80 字先到先发)
// 转发为 kind=delta 暂态事件,前端任务工作台据此实时显示「正在生成」;
// 攒批器携带 phase/step_index 调用上下文(与 task_llm_calls 落库行同口径)。
use crate::models::types::{SseEvent, TaskEventKind};
use crate::services::task_core::{TaskBackend, DELTA_FLUSH_WINDOW};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// 事件桥通道容量:drain 即时消费(broadcast send 不阻塞),64 足以吸收流式 Token 突发。
const SINK_CAPACITY: usize = 64;

/// 事件 detail 截断上限(防异常长文本撑大 broadcast 帧)。
const DETAIL_MAX_CHARS: usize = 200;

/// 启动事件桥:返回 (引擎侧发送端, drain 任务句柄)。
/// 调用方在引擎循环结束后应先 drop 发送端再 await drain,保证事件全部转发完毕。
///
/// drain 必须持续消费到 tx 全部 drop(recv 返回 None)才退出:引擎侧 send_event
/// 在 rx 关闭时会置 abort 并中断整轮(对齐聊天路径「客户端断开即中断」语义)。
///
/// phase/step_index 为本桥对应调用的归属标识(run_agent_loop 恒 phase=agent,
/// team 主 agent 复用同桥、step_index 为子目标全局下标;custom 工具步骤 phase=step;
/// 子 agent phase=subagent),随 delta 事件透出供前端缓冲归键。
pub(crate) fn spawn(
    svc: Arc<dyn TaskBackend>,
    task_id: String,
    label: &str,
    phase: &str,
    step_index: Option<usize>,
) -> (mpsc::Sender<SseEvent>, JoinHandle<()>) {
    let label = label.to_string();
    let phase = phase.to_string(); // drain 任务要求 'static,引用先行 owned 化
    let (tx, mut rx) = mpsc::channel::<SseEvent>(SINK_CAPACITY);
    let drain = tokio::spawn(async move {
        // 流式正文攒批出口(批次 R4):Token 逐条到达,攒批(时间窗/字符阈值先到
        // 先发)后发射 kind=delta;不逐条直发是为保护 64 容量的任务事件 broadcast
        // (逐条会灌满通道,致 SSE 端 Lagged 连 status/llm_call 等关键事件一起丢)。
        let mut batcher = svc.delta_batcher(&task_id, &phase, step_index);
        // 时间窗闸门由定时 tick 驱动(字符阈值在 push 内即时判定)
        let mut tick = tokio::time::interval(DELTA_FLUSH_WINDOW);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // 流式正文字符聚合计数:Finish 收尾事件汇总携带(「流式输出 N 字」)
        let mut streamed_chars = 0usize;
        loop {
            tokio::select! {
                ev = rx.recv() => {
                    match ev {
                        Some(SseEvent::Token { text }) => {
                            streamed_chars += text.chars().count();
                            batcher.push(&text);
                        }
                        Some(ev) => {
                            // 事件边界先落批:保证 delta 先于后续状态/收尾事件到达前端
                            batcher.flush();
                            if let Some(detail) = map_event(&ev, streamed_chars, &label) {
                                svc.emit_event(TaskEventKind::AgentStatus, &task_id, None, None, Some(detail));
                            }
                        }
                        None => break,
                    }
                }
                _ = tick.tick() => batcher.flush_if_due(),
            }
        }
        // drain 收尾:不足一批的尾段也要发出(delta 暂态可丢,但不主动丢)
        batcher.flush();
    });
    (tx, drain)
}

/// 引擎事件 → 任务事件 detail 文本映射;返回 None 表示不转发。
/// 仅覆盖 run_tool_loop 实际会产出的事件(Token/ToolCall/ToolResult/Step/授权请求),
/// 其余(Finish/Error/Interrupted 由聊天主流程发,任务模式走不到)防御性映射。
fn map_event(ev: &SseEvent, streamed_chars: usize, label: &str) -> Option<String> {
    let detail = match ev {
        SseEvent::Token { .. } => return None,
        SseEvent::Step { step, detail, .. } => match detail {
            Some(d) if !d.is_empty() => format!("{step}:{d}"),
            _ => step.clone(),
        },
        SseEvent::ToolCall { name, .. } => format!("{label} 调用工具 {name}"),
        // 被任务策略拒绝的工具要与正常返回区分开:否则调用情况里只看到「已返回结果」,
        // 用户不知道是策略拦截还是工具执行失败(tool_policy_denied 由闸门回灌)。
        SseEvent::ToolResult { name, output, .. } => {
            let denied = output
                .as_object()
                .and_then(|o| o.get("code"))
                .and_then(|c| c.as_str())
                .is_some_and(|code| code == "tool_policy_denied");
            if denied {
                let reason = output
                    .as_object()
                    .and_then(|o| o.get("error"))
                    .and_then(|e| e.as_str())
                    .unwrap_or("");
                if reason.is_empty() {
                    format!("工具 {name} 被任务策略拒绝")
                } else {
                    format!("工具 {name} 被任务策略拒绝:{reason}")
                }
            } else {
                format!("工具 {name} 已返回结果")
            }
        }
        // 任务模式已改走策略闸门(未放行工具直接拒绝为 tool_policy_denied),
        // 正常不会产生授权请求;此映射仅作防御性兜底。
        SseEvent::ToolAuthorizationRequired { name, .. } => {
            format!("工具 {name} 需要授权(任务模式按策略处理)")
        }
        SseEvent::Finish { .. } => format!("{label} 生成完成(流式输出 {streamed_chars} 字)"),
        SseEvent::Error { message, .. } => format!("{label} 出错:{message}"),
        SseEvent::Interrupted => format!("{label} 已中断"),
        _ => return None,
    };
    Some(detail.chars().take(DETAIL_MAX_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 策略拒绝的工具要与正常返回区分:调用情况里不能只显示「已返回结果」。
    #[test]
    fn policy_denied_tool_result_is_reported_as_rejected() {
        let ev = SseEvent::ToolResult {
            name: "write".into(),
            output: json!({ "error": "写入角色文件 a.md(宽松模式需授权)", "code": "tool_policy_denied" }),
            call_id: Some("c1".into()),
            render_kind: None,
        };
        let detail = map_event(&ev, 0, "主 agent").unwrap();
        assert!(detail.contains("被任务策略拒绝"), "实际:{detail}");
        assert!(
            detail.contains("写入角色文件 a.md"),
            "应带上拒绝理由,实际:{detail}"
        );
    }

    #[test]
    fn normal_tool_result_keeps_original_copy() {
        let ev = SseEvent::ToolResult {
            name: "read".into(),
            output: json!({ "ok": true }),
            call_id: Some("c1".into()),
            render_kind: None,
        };
        let detail = map_event(&ev, 0, "主 agent").unwrap();
        assert_eq!(detail, "工具 read 已返回结果");
    }
}
