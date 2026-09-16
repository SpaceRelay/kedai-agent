// 角色卡 V2 解析 + 世界书解析 + 正则脚本解析 + 酒馆宏 + 酒馆预设导入 + 酒馆助手插件兼容
//
// 代际: L1(老层·稳 / Anchored Core)——兼容契约层。
// 判据: 承载 SillyTavern 生态兼容契约(角色卡 V2/V3、世界书、酒馆助手协议),
//       形状变更即破坏既有用户数据或外部生态;且本层**不依赖任何上层**。
// 纪律: 不可变契约优先;改动须先写失败测试 + 声明兼容影响 + 过评审。
//       禁止 `use crate::services / agents / api / tools`(规则 D/J 硬门禁)。
// 详见 docs/契约-架构与数据.md §2.2(判据程序)与 §3(允许出边表)。
// 注: assistant/ejs/(自研 JS 解释器)已**冻结**——只接受安全修复,新模板能力走 scripts/runtime.rs。
pub mod assistant;
pub mod character_card;
pub mod macros;
pub mod preset;
pub mod regex_script;
pub mod scopes;
pub mod world_book;
pub mod world_book_convert;
