// 规划阶段:plan_phase(状态机切入 Planning、构建计划并落库 plan_goals、
// 推送「计划中…」SSE 事件、检查中断)。
// (自 engine/mod.rs 拆分迁入,纯代码移动,逻辑不变)
// 依赖经 `use super::*` 取自 engine/mod.rs(与 executor/mvu 等子模块同一约定)。
use super::*;

impl AgentEngine {
    /// 规划阶段:状态机切入 Planning,更新 agent 会话状态为 planning,构建计划
    /// (custom 按配置步骤序列,其余模式按输入与模式生成),把 plan_goals 落库,
    /// 推送「计划中…」SSE 事件并检查中断。
    /// 对应 run_body 内「1. 规划阶段」段;L2 中层定位:引擎单阶段的编排封装。
    #[allow(clippy::too_many_arguments)] // 编排函数参数即上下文,拆 struct 收益低
    pub(super) async fn plan_phase(
        &self,
        state_machine: &mut StateMachine,
        agent_session: &AgentSessionRecord,
        req: &AgentRunRequest,
        user_input: &str,
        tx: &mpsc::Sender<SseEvent>,
        abort: &watch::Receiver<bool>,
        flag: &AbortFlag,
        session_id: &str,
    ) -> Result<Plan, String> {
        state_machine.transition(AgentState::Planning, session_id)?;
        self.agent_sessions
            .update(&agent_session.id, Some("planning"), None, None, None)
            .map_err(|e| e.to_string())?;
        // custom 模式:按用户配置的步骤序列构建计划(路由层已校验;此处对配置被外部
        // 手改的情况兜底,失败直接中止本轮)
        let plan = if req.mode == "custom" {
            match make_custom_plan(req.flow.as_deref().unwrap_or(&[])) {
                Ok(p) => p,
                Err(e) => return Err(e),
            }
        } else {
            make_plan(user_input, &req.mode)
        };
        let plan_goals: Vec<String> = plan
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {}", i + 1, s.goal))
            .collect();
        self.agent_sessions
            .update(&agent_session.id, None, Some(&plan_goals), Some(0), None)
            .map_err(|e| e.to_string())?;
        send_event(
            step_evt("计划中…", Some(plan.summary.clone()), None, None),
            tx,
            abort,
            flag,
        )
        .await?;
        logging::agent_step(session_id, "plan", Some(&plan.summary));
        check_aborted(abort)?;
        Ok(plan)
    }
}
