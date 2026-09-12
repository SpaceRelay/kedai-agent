// 业务服务层
pub mod agent_flow_service;
pub mod agent_session_service;
pub mod agent_subtask_service;
pub mod audio_service;
pub mod cache_diagnostics;
pub mod character_service;
pub mod contract_changelog_service;
// 向量化服务(Phase 3):OpenAI 兼容 /embeddings 客户端 + 余弦/归一化工具
pub mod embedding_service;
pub mod kaleido_state_service;
// Android Keystore 桥接(JNI):API Key 加密存储,仅 android 目标编译
#[cfg(target_os = "android")]
pub mod keystore_android;
// 共享 JNI 桥基础设施(VM/类缓存),仅 android 目标编译
#[cfg(target_os = "android")]
pub mod jni_bridge;
// Android 原生能力桥接(外链/分享/前台服务保活),仅 android 目标编译
#[cfg(target_os = "android")]
pub mod native_bridge_android;
pub mod memory_service;
pub mod prompt_inject_service;
pub mod prompt_kit;
pub mod quick_reply_service;
pub mod runtime_prompt_service;
pub mod secret_store;
pub mod session_service;
pub mod settings_service;
pub mod skill_service;
pub mod task_service;
// 任务引擎(批次 4 六模式):模式执行器底座 + solo/plan;设计见 docs/任务引擎六模式.md
pub mod task_engine;
pub mod token_service;
// 回退快照(批次 6.1「undo」):写工具执行前逆操作负载落 undo_snapshots 表
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
