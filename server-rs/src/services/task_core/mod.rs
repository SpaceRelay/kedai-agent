// 任务核心契约(L1.5 依赖倒置底座,批次 B):任务引擎与任务服务两侧共用的
// 纯类型/常量与宿主能力缝的单一事实源。
// 任务引擎执行器只产出 TaskTerminal 值并只经 TaskBackend 调用宿主能力,
// 由 TaskService 一侧统一消费落库;执行器不再反向依赖 TaskService 的写入方法
//(规则 C 断环的落点)。
pub(crate) mod backend;
pub(crate) mod delta;
pub(crate) mod prompt_consts;
pub(crate) mod terminal;
pub(crate) mod types;

// 根级再导出:引擎/宿主两侧按 `task_core::{...}` 取用,隐藏模块内部布局。
// DELTA_FLUSH_CHARS 仅 delta.rs 内部与单测使用,不在此再导出。
pub(crate) use backend::{
    TaskBackend, TaskEvents, TaskFlowAccess, TaskGenerator, TaskPromptKit, TaskSettings, TaskStore,
    TaskTerminalSink, TaskTrace,
};
pub(crate) use delta::{DeltaBatcher, DELTA_FLUSH_WINDOW};
pub(crate) use terminal::TaskTerminal;
pub(crate) use types::{TaskGenOutput, TruncationHeal};
