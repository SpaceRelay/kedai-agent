// slash 命令系统(阶段四 4a):注册表 + 内置命令 + 参数解析。
//
// 代际: L2(中层·干 / Orchestration)。
// 判据: 被 API 与脚本桥两条链路共用(命令语义的单一来源);
//       2026-09-14 补登记——此前未列入任何代际清单(分层叙事的静默失效点)。
// 纪律: 可被 API(L2)与 scripts(L3)经注册回调方式使用;
//       现状 `scripts → slash` 已登记为 class=wiring(兼容桥的显式接线)。
//
// 服务两条链路:
//   - 脚本侧:TavernHelper.triggerSlash → EvalBridge.trigger_slash → registry.execute;
//   - API 侧:GET /api/slash/commands 返回命令清单(前端输入框联想)。
pub mod registry;

pub use registry::{parse_args, split_key_value, SlashCommand, SlashCommandMeta, SlashRegistry};
