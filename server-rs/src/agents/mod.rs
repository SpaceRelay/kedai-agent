// Agent 引擎模块
//
// 代际: L2(中层·干 / Orchestration)——业务编排与协议翻译。
// 判据: 依赖 L1 契约完成「消息构建 → 模型调用 → 工具循环 → 收尾落库」业务流程,
//       自身不被 L1 感知;是日常开发主战场。
// 纪律: 不得绕过 L1 契约直接改数据层语义;不得依赖 L3(scripts/mcp/plugins/exec)。
// 详见 docs/契约-架构与数据.md §2.2。
pub mod engine;
pub mod planner;
pub mod reflector;
pub mod state_machine;
