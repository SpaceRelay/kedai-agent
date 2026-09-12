// 引擎共享小类型:AbortFlag(中止标志)、AgentRunRequest(运行请求)、
// RunHandle(runs map 值)、RunContext(run_body 内共享可变状态聚合)。
// (自 engine/mod.rs 拆分迁入,纯代码移动,逻辑不变)
// 可见性说明:RunHandle/RunContext 原为 engine/mod.rs 私有条目,此处改为 pub(super)
// (= 对 engine 可见,字段同);AbortFlag 的 tx 字段仍仅本文件内访问,保持私有。
// 可见范围与拆分前完全一致,未放宽。
use super::*;

/// 中止标志(watch channel;true = 已请求中断)
pub struct AbortFlag {
    tx: watch::Sender<bool>,
}

impl AbortFlag {
    // pub(crate):任务引擎(task_engine)需自建中止标志(send_event 的断开置位用;
    // 中断信号本体走任务取消通道,不用本 flag 的接收端)
    pub(crate) fn new() -> (Arc<AbortFlag>, watch::Receiver<bool>) {
        let (tx, rx) = watch::channel(false);
        (Arc::new(AbortFlag { tx }), rx)
    }

    pub fn abort(&self) {
        let _ = self.tx.send(true);
    }
}

#[derive(Debug, Clone)]
pub struct AgentRunRequest {
    pub session_id: String,
    pub character_id: String,
    pub user_input: String,
    pub mode: String, // fast | deep | agent | custom
    pub params: GenerationParams,
    /// 上下文窗口上限(token):历史超出后按时间裁剪(最旧优先丢弃)
    pub max_context_tokens: Option<u32>,
    /// 自定义流程步骤快照(custom 模式;路由层已校验并过滤启用步骤)
    pub flow: Option<Vec<PlanStep>>,
    /// 重生成锚点(阶段六 6f):非 None 时收尾原地更新该 assistant 消息行
    /// (旧内容并入 extra.swipes 版本数组),而不是新增一条消息。
    pub regenerate_assistant_id: Option<i64>,
}

/// 运行中的句柄(runs map 的值)
pub(super) struct RunHandle {
    pub(super) run_id: uuid::Uuid,
    pub(super) flag: Arc<AbortFlag>,
    pub(super) active: bool,
}

/// run_body 内共享可变状态的聚合(供阶段拆分函数按字段借用,避免长参数列表)。
/// 各字段均为 &mut,调用方在同一函数内可对结构体的不同字段做 disjoint borrow
/// (如构建消息时同时可变借用 session_vars 与 assistant_vars)。
pub(super) struct RunContext<'a> {
    pub(super) assistant_vars: &'a mut AssistantVars,
    pub(super) session_vars: &'a mut HashMap<String, String>,
    /// 7 作用域变量(计划二):渲染/宏展开的作用域读写目标;收尾统一落库。
    /// Arc<Mutex> 共享:阶段三 3b 脚本执行(EvalBridge)经 clone 共享同一容器,
    /// 脚本对 global/character/script 等作用域的写回随收尾 take_others 一并落库。
    pub(super) scopes: Arc<Mutex<crate::parsing::scopes::ScopeVars>>,
    pub(super) llm_messages: &'a mut Vec<LlmMessage>,
    pub(super) total_usage: &'a mut TokenUsage,
}
