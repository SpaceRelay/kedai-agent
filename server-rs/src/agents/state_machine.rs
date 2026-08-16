// Agent 状态机(与 Node 版 state-machine.ts 对齐)

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
        Idle => &[Planning, Finished],
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
        // idle → tool_call 非法
        assert!(sm.transition(AgentState::ToolCall, "s1").is_err());
        assert_eq!(sm.current(), AgentState::Idle);
    }

    #[test]
    fn test_finished_to_executing() {
        let mut sm = StateMachine::new("s1");
        sm.restore(AgentState::Finished);
        sm.transition(AgentState::Executing, "s1").unwrap();
    }
}
