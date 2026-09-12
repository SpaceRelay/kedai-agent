// 任务事件广播(WP4 任务模式实时化):TaskService 内嵌 broadcast 通道,
// 各 DB 写入方法落库成功后发射 SseEvent::Task(kind = created/status/plan/
// subtask/usage/deleted),GET /api/tasks/events 订阅本通道并转发为 SSE,
// 取代前端 1s REST 轮询。无订阅者时 send 返回 Err,属正常,一律忽略。
// 批次 R4:新增 kind=delta 流式增量(emit_delta + DeltaBatcher 攒批),
// 与 llm_call 事件同步携带 phase/step_index 供前端对齐流式缓冲。
use super::*;
use std::time::Duration;
// 用 tokio Instant 而非 std:暂停时钟(start_paused)单测可推进虚拟时间,
// 且与 sink drain 的 tokio interval 计时同源
use tokio::time::Instant;

/// broadcast 通道容量:事件为瞬时通知,消费端(SSE 转发)实时读取;
/// 一次完整执行约产生十余条事件,64 足以吸收短时突发。
/// 溢出由接收端按 Lagged 跳过(任务事件允许丢,前端兜底仍可 REST 拉详情)。
pub(super) const EVENTS_CAPACITY: usize = 64;

/// delta 攒批时间窗(批次 R4):单批首字符入批超过该时长即发射。
/// 与 DELTA_FLUSH_CHARS 先到先发,共同保护 capacity 64 的任务事件广播——
/// 逐 token 转发(mock 逐字、真实上游每 chunk 数 token)会在毫秒级灌满通道,
/// Lagged 丢的将不只是 delta,还有 status/plan/llm_call 等关键事件。
pub(crate) const DELTA_FLUSH_WINDOW: Duration = Duration::from_millis(200);

/// delta 攒批字符阈值(批次 R4):单批攒满该字符数立即发射,不等时间窗。
/// 80 字 ≈ 一次快速突发的 1~2 个 SSE 帧,攒批后单次生成的 delta 数量级为
/// 「字符数/80」或「时长/200ms」,远低于通道容量。
pub(crate) const DELTA_FLUSH_CHARS: usize = 80;

impl TaskService {
    /// 订阅任务事件流(broadcast;晚加入的订阅者只能收到订阅之后的事件)。
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SseEvent> {
        self.events.subscribe()
    }

    /// 发射任务事件(仅在对应 DB 写入成功后调用)。
    /// send 仅在无任何订阅者时返回 Err,属正常,忽略。
    pub(crate) fn emit_event(
        &self,
        kind: &str,
        task_id: &str,
        title: Option<String>,
        status: Option<TaskStatus>,
        detail: Option<String>,
    ) {
        let _ = self.events.send(SseEvent::Task {
            task_id: task_id.to_string(),
            kind: Some(kind.to_string()),
            title,
            status,
            detail,
            finish_reason: None,
            phase: None,
            step_index: None,
        });
    }

    /// 构造一个挂在本服务广播通道上的 delta 攒批器(引擎桥 sink 与
    /// generate_text 旁路共用同一攒批出口,保证全任务模式口径一致)。
    /// kind=delta 事件的唯一发射点是 DeltaBatcher::flush。
    pub(crate) fn delta_batcher(
        &self,
        task_id: &str,
        phase: &str,
        step_index: Option<usize>,
    ) -> DeltaBatcher {
        DeltaBatcher::new(self.events.clone(), task_id, phase, step_index)
    }

    /// 发射 kind=llm_call 事件(可观测性问题①):在 emit_event 基础上携带
    /// 上游 finish_reason(stop/length 等;空串/None 即未知,序列化省略该字段)。
    /// 纪律同 emit_event:仅 task_llm_calls 落库成功后调用。
    /// phase/step_index(批次 R4)随事件透出:前端据此清理对应流式缓冲
    /// (delta 暂态数据由本落库行取代,对齐权威数据)。
    pub(crate) fn emit_llm_call(
        &self,
        task_id: &str,
        phase: &str,
        step_index: Option<usize>,
        detail: String,
        finish_reason: Option<String>,
    ) {
        let finish_reason = finish_reason.filter(|r| !r.is_empty());
        let _ = self.events.send(SseEvent::Task {
            task_id: task_id.to_string(),
            kind: Some("llm_call".to_string()),
            title: None,
            status: None,
            detail: Some(detail),
            finish_reason,
            phase: Some(phase.to_string()),
            step_index,
        });
    }
}

/// delta 攒批器(批次 R4):LLM 流式 Token 按「时间窗 200ms 或字符数 ≥80,
/// 先到先发」攒批后发射 kind=delta 事件。两条闸门都是为保护 capacity 64 的
/// 任务事件 broadcast(EVENTS_CAPACITY):逐 token 直发会在毫秒级灌满通道,
/// 消费端 Lagged 丢的将不只是 delta。
/// **纪律例外(注释说明)**:「仅 DB 写成功后发射」不适用于 delta——流式正文
/// 增量是暂态事件,无对应 DB 行可落;权威数据以调用结束后的 task_llm_calls
/// 落库行(kind=llm_call 事件引导前端重拉 calls 端点)为准。delta 允许丢弃:
/// broadcast Lagged 跳过不影响正确性,前端按 llm_call 事件清缓冲对齐。
/// 使用方:
/// - task_engine/sink.rs drain 循环(引擎桥:run_tool_loop 的 Token 事件);
/// - task_service/executor.rs generate_text 旁路(planner/audit/summary 等
///   纯生成步的 chunk)。
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
            kind: Some("delta".to_string()),
            title: None,
            status: None,
            detail: Some(text),
            finish_reason: None,
            phase: Some(self.phase.clone()),
            step_index: self.step_index,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个挂在裸 broadcast 通道上的攒批器(单测无需 TaskService 实例)
    fn batcher(
        phase: &str,
        step_index: Option<usize>,
    ) -> (DeltaBatcher, tokio::sync::broadcast::Receiver<SseEvent>) {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        (DeltaBatcher::new(tx, "t1", phase, step_index), rx)
    }

    /// 字符阈值:单批攒满 80 字立即发射;不足部分由 flush 收尾;两批拼接 == 原文。
    /// 事件须为 kind=delta 且携带 phase/step_index(前端缓冲 key 与对齐依据)。
    #[test]
    fn delta_batcher_flushes_on_char_threshold() {
        let (mut b, mut rx) = batcher("step", Some(0));
        let long = "字".repeat(DELTA_FLUSH_CHARS);
        b.push(&long);
        let Ok(SseEvent::Task {
            kind,
            detail,
            phase,
            step_index,
            ..
        }) = rx.try_recv()
        else {
            panic!("达到字符阈值应立即发射一条 delta");
        };
        assert_eq!(kind.as_deref(), Some("delta"));
        assert_eq!(detail.as_deref(), Some(long.as_str()));
        assert_eq!(phase.as_deref(), Some("step"));
        assert_eq!(step_index, Some(0));

        b.push("尾巴");
        assert!(rx.try_recv().is_err(), "未到阈值不应发射");
        b.flush();
        let Ok(SseEvent::Task { detail, .. }) = rx.try_recv() else {
            panic!("flush 应发出残留尾段");
        };
        assert_eq!(detail.as_deref(), Some("尾巴"));
        assert!(rx.try_recv().is_err(), "flush 后通道应空");
    }

    /// 时间窗:不足字符阈值时按 200ms 窗发射(保护 broadcast 容量的另一闸门);
    /// 窗口未到不发射。start_paused 下 Instant::now 受控,advance 推进虚拟时钟。
    #[tokio::test(start_paused = true)]
    async fn delta_batcher_flushes_on_time_window() {
        let (mut b, mut rx) = batcher("planner", None);
        b.push("少量");
        b.flush_if_due();
        assert!(rx.try_recv().is_err(), "时间窗未到不应发射");
        tokio::time::advance(DELTA_FLUSH_WINDOW + std::time::Duration::from_millis(1)).await;
        b.flush_if_due();
        let Ok(SseEvent::Task {
            kind,
            detail,
            phase,
            step_index,
            ..
        }) = rx.try_recv()
        else {
            panic!("时间窗到应发射");
        };
        assert_eq!(kind.as_deref(), Some("delta"));
        assert_eq!(detail.as_deref(), Some("少量"));
        assert_eq!(phase.as_deref(), Some("planner"));
        assert_eq!(step_index, None, "非步骤类阶段不携带 step_index");
    }
}
