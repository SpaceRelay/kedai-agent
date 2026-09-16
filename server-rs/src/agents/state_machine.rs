// Agent 状态机(与 Node 版 state-machine.ts 对齐)。
// 状态迁移校验:非法迁移记 warn 日志(transition 返回 Err;调用方应记录,不得静默丢弃)。
// 状态机本身不做落库——持久化终态以 agent_sessions/tasks 行为准。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Planning,
    Executing,
    ToolCall,
    Reflecting,
    Finished,
    Interrupted,
    Error,
}

impl AgentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Planning => "planning",
            AgentState::Executing => "executing",
            AgentState::ToolCall => "tool_call",
            AgentState::Reflecting => "reflecting",
            AgentState::Finished => "finished",
            AgentState::Interrupted => "interrupted",
            AgentState::Error => "error",
        }
    }
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct TransitionCtx {
    pub session_id: String,
    pub current: AgentState,
}

pub struct StateMachine {
    state: AgentState,
}

/// 允许迁移表
fn transitions(from: AgentState) -> &'static [AgentState] {
    use AgentState::*;
    match from {
        // Idle 可直接进 Executing:任务/工具路径无独立规划阶段(solo/multi 进入即执行,
        // 见 task_engine/solo.rs),补入本迁移使该真实路径合法。
        // Idle 可直接进 ToolCall(2026-09-15):工具循环首轮就是「第一步调工具」,
        // 引擎在 idle 上直接迁 ToolCall 是正常路径;此前不在表内,每轮都记一条
        // 「非法状态迁移:idle → tool_call」warn,把真实日志淹没成噪声。
        Idle => &[Planning, Executing, ToolCall, Finished],
        Planning => &[Executing, Finished, Interrupted, Error],
        Executing => &[ToolCall, Reflecting, Finished, Interrupted, Error],
        ToolCall => &[Executing, Interrupted, Error],
        Reflecting => &[Executing, Finished, Interrupted, Error],
        Finished => &[Idle, Executing],
        Interrupted => &[Executing, Finished],
        Error => &[Finished],
    }
}

impl StateMachine {
    pub fn new(_session_id: &str) -> Self {
        StateMachine {
            state: AgentState::Idle,
        }
    }

    pub fn current(&self) -> AgentState {
        self.state
    }

    /// 迁移;幂等(from == to 直接返回);非法迁移返回错误
    pub fn transition(&mut self, to: AgentState, _session_id: &str) -> Result<(), String> {
        let from = self.state;
        if from == to {
            return Ok(());
        }
        if !transitions(from).contains(&to) {
            return Err(format!("非法状态迁移:{from} → {to}"));
        }
        self.state = to;
        Ok(())
    }

    /// 尽力而为迁移:合法迁移返回 true;非法迁移记 warn 日志后返回 false,状态不变。
    /// 用于「中断/收尾竞态」等失败可接受的路径,替代 `let _ = transition(...)` 的静默吞错。
    pub fn transition_best_effort(&mut self, to: AgentState, session_id: &str) -> bool {
        // from 必须在迁移前取:transition 失败时 state 不变,但成功时已改写,
        // 提前取值可让日志同时表达「从哪来、到哪去」。
        let from = self.state;
        match self.transition(to, session_id) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    session_id,
                    from = %from,
                    to = %to,
                    error = %e,
                    "状态迁移被拒绝(尽力而为路径继续)"
                );
                false
            }
        }
    }

    /// 无校验强制设状态(用于从持久化恢复)
    pub fn restore(&mut self, state: AgentState) {
        self.state = state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        let mut sm = StateMachine::new("s1");
        sm.transition(AgentState::Planning, "s1").unwrap();
        assert_eq!(sm.current(), AgentState::Planning);
        sm.transition(AgentState::Executing, "s1").unwrap();
        assert_eq!(sm.current(), AgentState::Executing);
        sm.transition(AgentState::Reflecting, "s1").unwrap();
        assert_eq!(sm.current(), AgentState::Reflecting);
        sm.transition(AgentState::Finished, "s1").unwrap();
        assert_eq!(sm.current(), AgentState::Finished);
    }

    #[test]
    fn test_idempotent() {
        let mut sm = StateMachine::new("s1");
        sm.transition(AgentState::Planning, "s1").unwrap();
        sm.transition(AgentState::Planning, "s1").unwrap(); // 幂等
        assert_eq!(sm.current(), AgentState::Planning);
    }

    #[test]
    fn test_invalid_transition() {
        let mut sm = StateMachine::new("s1");
        // idle → reflecting 非法(反思必须先进入执行态)
        assert!(sm.transition(AgentState::Reflecting, "s1").is_err());
        assert_eq!(sm.current(), AgentState::Idle);
    }

    /// 2026-09-15:工具循环首轮「idle → tool_call」是真实合法路径(引擎在 idle 上
    /// 直接迁 ToolCall),此前被记为非法,每轮刷一条 warn 噪声。
    #[test]
    fn test_idle_directly_to_tool_call_is_valid() {
        let mut sm = StateMachine::new("s1");
        sm.transition(AgentState::ToolCall, "s1").unwrap();
        assert_eq!(sm.current(), AgentState::ToolCall);
        // 工具调用后回到执行态(工具循环的正常收束)
        sm.transition(AgentState::Executing, "s1").unwrap();
        assert_eq!(sm.current(), AgentState::Executing);
    }

    #[test]
    fn test_finished_to_executing() {
        let mut sm = StateMachine::new("s1");
        sm.restore(AgentState::Finished);
        sm.transition(AgentState::Executing, "s1").unwrap();
    }

    #[test]
    fn test_best_effort_valid_transition_returns_true_and_changes_state() {
        let mut sm = StateMachine::new("s1");
        // idle → planning 合法:返回 true 且状态已变
        assert!(sm.transition_best_effort(AgentState::Planning, "s1"));
        assert_eq!(sm.current(), AgentState::Planning);
    }

    #[test]
    fn test_best_effort_invalid_transition_returns_false_and_keeps_state() {
        let mut sm = StateMachine::new("s1");
        // idle → reflecting 非法:返回 false 且状态不变
        assert!(!sm.transition_best_effort(AgentState::Reflecting, "s1"));
        assert_eq!(sm.current(), AgentState::Idle);
    }

    #[test]
    fn test_best_effort_invalid_transition_emits_warn_log() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

        impl Write for CaptureWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for CaptureWriter {
            type Writer = CaptureWriter;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let writer = CaptureWriter::default();
        let buf = writer.0.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer)
            .with_max_level(tracing::Level::WARN)
            .finish();
        let mut sm = StateMachine::new("s1");
        tracing::subscriber::with_default(subscriber, || {
            // 非法迁移:应发出 warn 日志(idle → reflecting 不在允许表内)
            assert!(!sm.transition_best_effort(AgentState::Reflecting, "s1"));
        });
        let text = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            text.contains("WARN") && text.contains("状态迁移被拒绝"),
            "应捕获到警告日志,实际输出:{text}"
        );
    }
}
