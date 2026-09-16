// 数据模型:类型 + SQLite
//
// 代际: L1(老层·稳 / Anchored Core)——数据契约层。
// 判据: SQLite 表结构与全部数据契约类型;形状变更即破坏既有用户数据。
// 纪律: 零上层依赖;新增表/列必须配幂等迁移(见 migration/)。
// 详见 docs/契约-架构与数据.md §2.2。
//
pub mod db;
/// 上游/传输错误分类词汇（错误码 + 可重试语义 + HTTP 状态/传输形态纯映射）。
///
/// **L1 契约层**：被 L2 的 `connectors/`（边界处把 reqwest 错误与 HTTP 状态映射为
/// 分类）与 L2 的 `agents/engine`（消费分类决定 SSE 错误终态）共同引用。
/// 下沉理由与边界见该模块头注释及 `docs/契约-架构与数据.md` §2.2。
pub mod llm_error;
/// 工具策略领域词汇（授权档位 / 工具风险 / 命令风险）。
///
/// **L1 契约层**：被 L2 的 `services/`（设置、任务工具策略、审计）与 L3 的 `tools/`
/// 共同引用。下沉理由与边界见本文件头部注释及 `docs/契约-架构与数据.md` §2.2。
pub mod tool_policy;
pub mod types;
