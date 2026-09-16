// 任务终态值(批次 B 依赖倒置):执行器「返回值即终态」,引擎统一按本枚举分派落库。
//
// 设计要点:
// - 收敛原先分散在 legacy/followup 执行器内的 TaskService 写库调用(OnSuccess::SelfFinalized
//   旁路),执行器只描述「到达了什么终态」,不再关心具体写入形态;
// - 五种变体一一对应 TaskService 现有写入形态(complete/planned/followup/finalize/仅清理),
//   语义与原路径逐字节等价(followup 的「写 result 文本 ≠ 落消息文本」由变体字段承载);
// - Failed 不携带「是否取消」:该判定由引擎在收尾时统一读取消通道
//   (finalize_terminal 的 ended_by_cancel 入参),执行器只报告失败原因文本。
use crate::models::types::TaskStatus;

/// 任务执行终态(执行器唯一产出).
pub(crate) enum TaskTerminal {
    /// 成功产出:result 落库 + 终态 status(可 partial)+ 可选可解释 error。
    Complete {
        result: String,
        status: TaskStatus,
        error: Option<String>,
    },
    /// plan 计划产出轮:计划清单文本落 result,任务保持 planned 待批准。
    AwaitApproval { plan_text: String },
    /// followup 追加指令完成:new_result 落 result,msg_text 落 kind=followup 消息。
    Followup {
        msg_text: String,
        new_result: String,
        status: TaskStatus,
    },
    /// 失败收尾(含取消):error 为可解释文本;是否取消由引擎读取消通道判定。
    Failed { error: Option<String> },
    /// 任务已不存在等场景:不改终态,仅清理本轮取消条目(finalize_run 的
    /// error=None 且非取消分支等价语义)。
    Noop,
}
