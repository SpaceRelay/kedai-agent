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

/// 引擎顶层错误:**分类由生产者显式携带**,不再对错误文案做子串猜测
/// (旧 `classify_engine_error` 的字符串匹配路径已于批次 4.3 删除)。
///
/// 生产者两类:
/// - 连接器边界返回的 [`LlmError`](带超时/限流/鉴权/上游/生成分类);
/// - 引擎内部错误(状态机校验、参数非法、工具循环等),无上游分类可用。
///
/// `From<String>` 保留既有内部 `Result<_, String>` 辅助函数的 `?` 用法,
/// 使类型化改动集中在顶层而不重写整条内部链路。
#[derive(Debug, Clone)]
pub enum EngineError {
    /// 中断(用户停止/客户端断开):不是失败,调用方按 abort 标志判定,不发错误终态
    Interrupted,
    /// 连接器返回的已分类上游失败
    Llm(crate::models::llm_error::LlmError),
    /// 引擎内部错误,无上游分类
    Internal(String),
}

impl EngineError {
    /// 面向用户的文案
    pub fn message(&self) -> &str {
        match self {
            EngineError::Interrupted => "生成已中断",
            EngineError::Llm(e) => e.message(),
            EngineError::Internal(m) => m,
        }
    }

    /// SSE `Error.code` 线格式错误码;无上游分类的内部/中断错误归为 `generation_failed`。
    pub fn error_code(&self) -> &'static str {
        use crate::models::llm_error::LlmErrorKind;
        match self {
            EngineError::Llm(e) => e.code(),
            EngineError::Interrupted | EngineError::Internal(_) => LlmErrorKind::Generation.code(),
        }
    }

    /// 是否可重试:仅已分类的上游失败按分类判定(内部错误/中断不可重试)。
    pub fn retryable(&self) -> bool {
        match self {
            EngineError::Llm(e) => e.retryable(),
            EngineError::Interrupted | EngineError::Internal(_) => false,
        }
    }

    /// 内部/中断错误 → 无分类;连接器失败 → Some(分类)
    pub fn error_kind(&self) -> Option<crate::models::llm_error::LlmErrorKind> {
        match self {
            EngineError::Llm(e) => Some(e.kind()),
            EngineError::Interrupted | EngineError::Internal(_) => None,
        }
    }
}

impl From<crate::models::llm_error::LlmError> for EngineError {
    fn from(e: crate::models::llm_error::LlmError) -> Self {
        EngineError::Llm(e)
    }
}

impl From<String> for EngineError {
    fn from(e: String) -> Self {
        EngineError::Internal(e)
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
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
    /// 最后一步生成的 finish_reason(可观测性问题①,2026-09-15):步骤循环每轮
    /// 用 `result.finish_reason` 覆盖(与 content 的覆盖语义一致),收尾据此判定
    /// 聊天回复是否被 max_tokens 截断,并透出到 SseEvent::Finish / 落库 extra。
    pub(super) last_finish_reason: Option<String>,
}
