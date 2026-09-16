// 任务事件广播(WP4 任务模式实时化):TaskService 内嵌 broadcast 通道,
// 各 DB 写入方法落库成功后发射 SseEvent::Task(kind = created/status/plan/
// subtask/usage/deleted),GET /api/tasks/events 订阅本通道并转发为 SSE,
// 取代前端 1s REST 轮询。无订阅者时 send 返回 Err,属正常,一律忽略。
// 批次 R4:新增 kind=delta 流式增量(emit_delta + DeltaBatcher 攒批),
// 与 llm_call 事件同步携带 phase/step_index 供前端对齐流式缓冲。
use super::*;
// 批次 B.3 依赖倒置:DeltaBatcher 本体已机械搬迁至 task_core::delta
// (使 task_engine 侧可直接引用而不经 task_service),此处仅引入返回类型。
use crate::services::task_core::DeltaBatcher;

/// broadcast 通道容量:事件为瞬时通知,消费端(SSE 转发)实时读取;
/// 一次完整执行约产生十余条事件,64 足以吸收短时突发。
/// 溢出由接收端按 Lagged 跳过(任务事件允许丢,前端兜底仍可 REST 拉详情)。
pub(super) const EVENTS_CAPACITY: usize = 64;

impl TaskService {
    /// 订阅任务事件流(broadcast;晚加入的订阅者只能收到订阅之后的事件)。
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SseEvent> {
        self.events.subscribe()
    }

    /// 发射任务事件(仅在对应 DB 写入成功后调用)。
    /// send 仅在无任何订阅者时返回 Err,属正常,忽略。
    pub(crate) fn emit_event(
        &self,
        kind: TaskEventKind,
        task_id: &str,
        title: Option<String>,
        status: Option<TaskStatus>,
        detail: Option<String>,
    ) {
        let _ = self.events.send(SseEvent::Task {
            task_id: task_id.to_string(),
            kind: Some(kind),
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
            kind: Some(TaskEventKind::LlmCall),
            title: None,
            status: None,
            detail: Some(detail),
            finish_reason,
            phase: Some(phase.to_string()),
            step_index,
        });
    }
}

// delta 攒批器(批次 R4;批次 B.3 本体已搬迁至 task_core::delta,
// 语义与使用方说明见 task_core::delta)。
#[cfg(test)]
mod tests {
    use super::*;
    // 常量随攒批器本体迁至 task_core::delta,单测直接自该处引用
    use crate::services::task_core::delta::{DELTA_FLUSH_CHARS, DELTA_FLUSH_WINDOW};

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
        assert_eq!(kind, Some(TaskEventKind::Delta));
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
        assert_eq!(kind, Some(TaskEventKind::Delta));
        assert_eq!(detail.as_deref(), Some("少量"));
        assert_eq!(phase.as_deref(), Some("planner"));
        assert_eq!(step_index, None, "非步骤类阶段不携带 step_index");
    }
}
