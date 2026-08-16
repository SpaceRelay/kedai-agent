// slash 命令系统(阶段四 4a):注册表 + 内置命令 + 参数解析。
// 服务两条链路:
//   - 脚本侧:TavernHelper.triggerSlash → EvalBridge.trigger_slash → registry.execute;
//   - API 侧:GET /api/slash/commands 返回命令清单(前端输入框联想)。
// 设计约束:不 eval;命令 handler 为纯 Rust 闭包,签名 (args, &mut ScopeVars) -> String;
// /help 由 execute 特判(避免闭包自引用注册表);未知命令返回 None 由调用方兜底。
pub mod registry;

pub use registry::{parse_args, split_key_value, SlashCommand, SlashCommandMeta, SlashRegistry};
