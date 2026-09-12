// 世界书 / 契约 / 契约历史 / 变量引擎 / 提示词注入 / 快速回复路由(自 api/mod.rs build_router 迁入)
use crate::api::app_state::AppState;
use crate::api::{contract_history, contracts, kaleido, prompt_inject, quick_replies, world_books};
use axum::routing::{get, post, put};
use axum::Router;
use std::sync::Arc;

pub(crate) fn content_routes() -> Router<Arc<AppState>> {
    Router::new()
        // 世界书
        .route("/api/world-books", get(world_books::list))
        .route("/api/world-books/upload", post(world_books::upload))
        .route(
            "/api/world-books/auto-assign-check",
            get(world_books::auto_assign_check),
        )
        .route(
            "/api/world-books/{id}",
            put(world_books::update).delete(world_books::delete),
        )
        .route(
            "/api/world-books/{id}/entries",
            get(world_books::entries)
                .put(world_books::save_entries)
                .post(world_books::add_entry),
        )
        // 角色卡内嵌世界书(character_book)条目编辑
        .route(
            "/api/characters/{id}/world-entries",
            get(world_books::character_entries).put(world_books::save_character_entries),
        )
        // 契约编辑(P6 面板):读/写/移除角色卡内嵌契约(extensions.nlkaleido)
        .route(
            "/api/characters/{id}/contract",
            get(contracts::get)
                .put(contracts::put)
                .delete(contracts::delete),
        )
        // 契约变更历史与回滚(阶段 C):历史列表(最新在前)+ 按 seq 回滚
        .route(
            "/api/characters/{id}/contract/history",
            get(contract_history::list),
        )
        .route(
            "/api/characters/{id}/contract/rollback",
            post(contract_history::rollback),
        )
        // 契约引擎 HTTP 出口(P7 方案 A):外部工具与 ST 前端调用同一引擎,
        // 与多步工具 apply_patch 共用 VariableApplyService(单库无分叉)
        .route("/api/variable/update", post(kaleido::update))
        .route("/api/variable/state", get(kaleido::get_state))
        .route("/api/variable/changelog", get(kaleido::changelog))
        // 提示词注入(简单模式 + 楼层系统 + 酒馆预设导入)
        .route(
            "/api/prompt-inject",
            get(prompt_inject::get_prompt_inject).put(prompt_inject::update_prompt_inject),
        )
        .route("/api/prompt-inject/import", post(prompt_inject::import))
        // 快速回复(Quick Replies):getqr 的渲染数据源 + 管理 CRUD
        .route(
            "/api/quick-replies",
            get(quick_replies::list).post(quick_replies::create),
        )
        .route(
            "/api/quick-replies/{id}",
            put(quick_replies::update).delete(quick_replies::delete),
        )
}
