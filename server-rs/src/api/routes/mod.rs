// 分域路由表:build_router 按域 merge 组装,路由集合与原 api/mod.rs 单链一致
// (每个子模块暴露一个 `*_routes() -> Router<Arc<AppState>>`,state 在最终 merge 后统一 with_state)
pub(crate) mod agent;
pub(crate) mod chat;
pub(crate) mod content;
pub(crate) mod misc;
pub(crate) mod settings;
