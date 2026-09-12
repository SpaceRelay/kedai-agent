// 任务模式执行上下文:一次模式执行的输入聚合(目标/有效设置快照/执行者/取消通道)。
// 设计依据 docs/任务引擎六模式.md 第四节;能力缝雏形(第六节),未来 webhook/后台 jobs
// 等触发源复用同一上下文模型。
use crate::services::settings_service::RuntimeSettings;
use tokio::sync::watch;

/// 一次任务模式执行的上下文。
/// settings 为构造时的 task_settings() 快照(for_mode(Task) 合并值):执行全程
/// 读快照不回头读全局设置,与 legacy 各阶段「执行期设置变更不影响本轮」语义一致。
pub(crate) struct TaskRunContext {
    /// 任务 id(tasks 表主键;虚拟 session_id 与事件/落库均以此为准)
    pub task_id: String,
    /// 用户消息正文:solo/plan 为任务目标;approve 续跑为「目标 + 已批准计划」组合文本
    pub goal: String,
    /// 任务模式有效设置快照(温度/top_p/max_tokens/max_tool_rounds/Agent 提示词等)
    pub settings: RuntimeSettings,
    /// 执行者人设角色 id(空 = 通用执行者)
    pub character_id: Option<String>,
    /// 任务取消通道(TaskService cancel token 机制的接收端;true = 已请求停止)。
    /// 直接作为 run_tool_loop 的 abort 传入,stop 语义与 legacy/聊天路径一致。
    pub cancel: watch::Receiver<bool>,
}
