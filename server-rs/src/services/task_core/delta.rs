// delta 流式增量攒批(批次 R4;批次 B.3 依赖倒置搬迁至 task_core):
// LLM 流式 Token 按「时间窗 200ms 或字符数 ≥80,先到先发」攒批后发射 kind=delta 事件。
// 两条闸门都是为保护 capacity 有限的任务事件 broadcast(逐 token 直发会在毫秒级灌满通道,
// 消费端 Lagged 丢的将不只是 delta)。仅依赖 SseEvent 广播发送端,属纯共享实现;
// 唯一发射点是 DeltaBatcher::flush。
use crate::models::types::{SseEvent, TaskEventKind};
use std::time::Duration;
// 用 tokio Instant 而非 std:暂停时钟(start_paused)单测可推进虚拟时间,
// 且与引擎事件桥 drain 的 tokio interval 计时同源
use tokio::time::Instant;

/// delta 攒批时间窗(批次 R4):单批首字符入批超过该时长即发射。
/// 与 DELTA_FLUSH_CHARS 先到先发,共同保护任务事件广播——
/// 逐 token 转发(mock 逐字、真实上游每 chunk 数 token)会在毫秒级灌满通道,
/// Lagged 丢的将不只是 delta,还有 status/plan/llm_call 等关键事件。
pub(crate) const DELTA_FLUSH_WINDOW: Duration = Duration::from_millis(200);

/// delta 攒批字符阈值(批次 R4):单批攒满该字符数立即发射,不等时间窗。
/// 80 字 ≈ 一次快速突发的 1~2 个 SSE 帧,攒批后单次生成的 delta 数量级为
/// 「字符数/80」或「时长/200ms」,远低于通道容量。
pub(crate) const DELTA_FLUSH_CHARS: usize = 80;

/// delta 攒批器(批次 R4):LLM 流式 Token 按「时间窗 200ms 或字符数 ≥80,
/// 先到先发」攒批后发射 kind=delta 事件。两条闸门都是为保护 capacity 64 的
/// 任务事件 broadcast(EVENTS_CAPACITY):逐 token 直发会在毫秒级灌满通道,
/// 消费端 Lagged 丢的将不只是 delta。
/// **纪律例外(注释说明)**:「仅 DB 写成功后发射」不适用于 delta——流式正文
/// 增量是暂态事件,无对应 DB 行可落;权威数据以调用结束后的 task_llm_calls
/// 落库行(kind=llm_call 事件引导前端重拉 calls 端点)为准。delta 允许丢弃:
/// broadcast Lagged 跳过不影响正确性,前端按 llm_call 事件清缓冲对齐。
/// 使用方:
/// - 引擎事件桥 sink drain 循环(run_tool_loop 的 Token 事件);
/// - 任务侧 generate_text 旁路(planner/audit/summary 等纯生成步的 chunk)。
///
/// 与截断自愈的交互口径:自愈重发的新一轮 Token 与旧轮同 key(phase:step_index)
/// 的 delta 接续发出,前端缓冲继续追加;整轮结束后 record_llm_call 落库触发的
/// llm_call 事件引导前端清缓冲、以落库行对齐权威数据(delta 只是暂态展示)。
pub(crate) struct DeltaBatcher {
    tx: tokio::sync::broadcast::Sender<SseEvent>,
    task_id: String,
    phase: String,
    step_index: Option<usize>,
    /// 本批已攒文本
    buf: String,
    /// 本批字符数(buf.chars().count() 的增量缓存,避免每次 O(n) 重数)
    chars: usize,
    /// 本批首字符入批时间(时间窗计时起点;None = 当前无在攒批)
    since: Option<Instant>,
}

impl DeltaBatcher {
    pub(crate) fn new(
        tx: tokio::sync::broadcast::Sender<SseEvent>,
        task_id: &str,
        phase: &str,
        step_index: Option<usize>,
    ) -> Self {
        DeltaBatcher {
            tx,
            task_id: task_id.to_string(),
            phase: phase.to_string(),
            step_index,
            buf: String::new(),
            chars: 0,
            since: None,
        }
    }

    /// 推入一段流式增量;达到字符阈值立即发射,否则攒入本批。
    pub(crate) fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.buf.is_empty() {
            self.since = Some(Instant::now());
        }
        self.chars += text.chars().count();
        self.buf.push_str(text);
        if self.chars >= DELTA_FLUSH_CHARS {
            self.flush();
        }
    }

    /// 时间窗到点检查(由调用方的定时 tick 驱动);窗口未到或批空不发射。
    pub(crate) fn flush_if_due(&mut self) {
        if !self.buf.is_empty()
            && self
                .since
                .is_some_and(|t| t.elapsed() >= DELTA_FLUSH_WINDOW)
        {
            self.flush();
        }
    }

    /// 立即发出本批(事件边界/流收尾调用:保证 delta 先于后续状态事件到达,
    /// 且不足一批的尾段不丢失)。无订阅者时 send 返回 Err,属正常,忽略。
    pub(crate) fn flush(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.buf);
        self.chars = 0;
        self.since = None;
        let _ = self.tx.send(SseEvent::Task {
            task_id: self.task_id.clone(),
            kind: Some(TaskEventKind::Delta),
            title: None,
            status: None,
            detail: Some(text),
            finish_reason: None,
            phase: Some(self.phase.clone()),
            step_index: self.step_index,
        });
    }
}
