// 业务服务层
//
// 代际: L2(中层·干 / Orchestration)——业务编排与协议翻译,**日常开发主战场**。
// 判据: 依赖 L1 契约完成业务流程(角色/会话/世界书/注入/设置/密钥/任务/undo),
//       自身不被 L1 感知;是「经验 → 实现」的桥梁。
// 纪律: 不得绕过 L1 契约直接改数据层语义;协议翻译集中在本层;
//       不得依赖 L3(scripts/mcp/plugins/exec)——青层能力经显式接缝注入
//       (先例:task_core::TaskBackend 断开 task_engine→task_service)。
// 子域: task_service/(任务宿主)· task_engine/(六模式引擎)· task_core/(共享契约)·
//       settings_service/ · exec/(命令执行,默认关) · script_authorization_service/(脚本授权门)。
// 详见 docs/契约-架构与数据.md §2.2、§3。
pub mod agent_flow_service;
pub mod agent_session_service;
pub mod agent_subtask_service;
pub mod audio_service;
pub mod cache_diagnostics;
// characters.data_raw 读改写单一入口(防丢更新):世界书/脚本/契约三处共用
pub(crate) mod character_data;
pub mod character_service;
pub mod contract_changelog_service;
// 向量化服务(Phase 3):OpenAI 兼容 /embeddings 客户端 + 余弦/归一化工具
pub mod embedding_service;
// 命令执行抽象层(阶段 C):bash 工具与 Android 执行层共用的单一入口
pub mod exec;
pub mod kaleido_state_service;
// Android Keystore 桥接(JNI):API Key 加密存储,仅 android 目标编译
#[cfg(target_os = "android")]
pub mod keystore_android;
// 共享 JNI 桥基础设施(VM/类缓存),仅 android 目标编译
#[cfg(target_os = "android")]
pub mod jni_bridge;
// Android 原生能力桥接(外链/分享/前台服务保活),仅 android 目标编译
pub mod memory_service;
#[cfg(target_os = "android")]
pub mod native_bridge_android;
pub mod prompt_inject_service;
pub mod prompt_kit;
pub mod quick_reply_service;
pub mod runtime_prompt_service;
pub mod secret_store;
pub mod session_service;
pub mod settings_service;
pub mod skill_service;
pub mod task_service;
// 任务核心契约(批次 B 依赖倒置):执行器终态值类型,先于 task_engine 定义
pub(crate) mod task_core;
// 任务引擎(批次 4 六模式):模式执行器底座 + solo/plan;设计见 docs/功能.md
pub mod task_engine;
pub mod token_service;
// 回退快照(批次 6.1「undo」):写工具执行前逆操作负载落 undo_snapshots 表
// 角色卡脚本授权台账(2026-09-14):后端脚本执行门(fail-closed),补 known-limitations L12
pub mod script_authorization_service;
pub mod undo_service;
pub mod user_script_service;
pub mod variable_apply;
pub mod world_book_service;

/// DB 列表查询失败兜底(2026-08 裸 unwrap 审计):记 warn 并回退空列表。
/// 列表函数本就按 filter_map 丢弃坏行的 best-effort 语义工作,prepare/参数绑定失败
/// (schema 级异常)同样不回传 panic——阻塞线程 panic 会经 JoinError 放大为 500。
pub(crate) fn log_query_failure<T>(op: &str, e: rusqlite::Error) -> Vec<T> {
    tracing::warn!(op = op, error = e.to_string(), "DB 列表查询失败,回退空列表");
    Vec::new()
}

/// 取只读连接失败(连接池耗尽等)时的兜底:记 warn 并回退空列表。
/// 与 `log_query_failure` 同属「best-effort 列表语义」——此类函数本就丢弃坏行、
/// 对 schema 级异常回退空集;阻塞线程 panic 会经 JoinError 放大为 500,故不 panic。
pub(crate) fn log_read_pool_failure<T>(op: &str, e: String) -> Vec<T> {
    tracing::warn!(op = op, error = e, "获取只读连接失败,回退空列表");
    Vec::new()
}
