// 消息构建模块(目录化拆分,纯代码移动,逻辑不变):
//   build.rs  6 层位置拼装 LLM 消息构建(build_llm_messages_with_position)与
//             测试兼容入口 build_llm_messages(仅 #[cfg(test)])
//   steps.rs  步骤参数派生、步骤提示词与反思回退定位(自 build.rs 细拆)
//   build_tests.rs build 的位置拼装/前缀稳定化/槽位布局回归测试(#[cfg(test)] 挂载)
//   inject.rs @INJECT 精确消息插入、反思建议注入、摘要槽/记忆槽插入
//   trim.rs   上下文裁剪(trim_to_context)与受保护头部长度
//   context.rs 上下文收集(collect_context,自 engine/mod.rs 迁入)与消息构建收尾
//              (finalize_messages),及收集产物 CollectedCtx
// 本文件只做再导出,保证 engine/mod.rs 的 `use self::messages::{...}` 无需改动。
// 可见性说明:子文件中原 pub(super)(= 对 engine 可见)的条目改为
// pub(in crate::agents::engine),可见范围与拆分前完全一致,未放宽。
mod build;
#[cfg(test)]
mod build_tests;
mod context;
mod inject;
mod steps;
mod trim;

pub(super) use build::build_llm_messages_with_position;
pub(super) use context::CollectedCtx;
pub(super) use inject::{
    append_memory_notice, apply_inject_insertions, inject_reflect_advice, insert_memory_slot,
    insert_recall_slot, insert_summary_slot, parse_inject_insertion, InjectAt, InjectInsertion,
};
pub(super) use steps::{retreat_to_generating_step, step_params_for, with_step_prompt};
pub(super) use trim::{trim_to_context, trim_tool_history, TOOL_HISTORY_SUMMARY_PREFIX};
