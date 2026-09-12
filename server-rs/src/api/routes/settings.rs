// 设置 / 跨会话记忆 / Token / 诊断路由(自 api/mod.rs build_router 迁入)
use crate::api::app_state::AppState;
use crate::api::{diagnostics, memory, repo_index, settings, tokens};
use axum::routing::{get, post, put};
use axum::Router;
use std::sync::Arc;

pub(crate) fn settings_routes() -> Router<Arc<AppState>> {
    Router::new()
        // 设置
        .route(
            "/api/settings",
            get(settings::get_settings).put(settings::update_settings),
        )
        .route("/api/settings/connect", post(settings::connect))
        .route(
            "/api/settings/embedding/test",
            post(settings::test_embedding),
        )
        .route(
            "/api/settings/refresh-models",
            post(settings::refresh_models),
        )
        .route("/api/settings/models", get(settings::models))
        .route("/api/settings/info", get(settings::info))
        .route(
            "/api/settings/model",
            put(settings::switch_model).get(settings::get_model),
        )
        .route(
            "/api/settings/agent-prompt",
            get(settings::get_agent_prompt).put(settings::save_agent_prompt),
        )
        .route(
            "/api/settings/prompt-preview",
            get(settings::prompt_preview),
        )
        // 跨会话记忆蒸馏(落地项 2):蒸馏 / 列表 / 检索 / 手动添加 / 归档清理 / 编辑 / 删除
        .route("/api/memory/distill", post(memory::distill))
        .route("/api/memory/search", get(memory::search))
        .route("/api/memory/prune", post(memory::prune))
        // 向量索引(Phase 3):状态查询 + 手动重建
        .route(
            "/api/memory/embedding-status",
            get(memory::embedding_status),
        )
        .route(
            "/api/memory/rebuild-embeddings",
            post(memory::rebuild_embeddings),
        )
        .route("/api/memory", get(memory::list).post(memory::create))
        .route(
            "/api/memory/{id}",
            axum::routing::patch(memory::update).delete(memory::delete),
        )
        // Token 统计
        .route("/api/token/count", post(tokens::count))
        .route("/api/token/session-total", get(tokens::session_total))
        .route("/api/token/global-total", get(tokens::global_total))
        // 缓存诊断(缓存感知管线):命中率/费用节省/四级水位
        .route("/api/diagnostics/cache", get(diagnostics::cache))
        // 仓库索引(开发期工具):读取 .kedai-index/index.json 供前端面板浏览
        .route("/api/repo-index", get(repo_index::get_repo_index))
}
