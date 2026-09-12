// 会话 / 消息 / 变量 / 聊天(SSE)路由(自 api/mod.rs build_router 迁入)
use crate::api::app_state::AppState;
use crate::api::{agent, chat, sessions, undo, variables};
use axum::routing::{get, post, put};
use axum::Router;
use std::sync::Arc;

pub(crate) fn chat_routes() -> Router<Arc<AppState>> {
    Router::new()
        // 会话 / 消息
        .route(
            "/api/chat/sessions",
            get(sessions::list_sessions).post(sessions::create_session),
        )
        .route(
            "/api/chat/sessions/{id}",
            axum::routing::delete(sessions::delete_session),
        )
        .route(
            "/api/chat/sessions/{id}/truncate",
            post(sessions::truncate_messages),
        )
        .route("/api/chat/sessions/{id}/regreet", post(sessions::regreet))
        .route(
            "/api/chat/sessions/{id}/assistant-vars",
            axum::routing::put(sessions::save_assistant_vars),
        )
        .route("/api/chat/history", get(sessions::history))
        .route(
            "/api/chat/messages/{id}",
            put(sessions::update_message).delete(sessions::delete_message),
        )
        .route(
            "/api/chat/messages/{id}/variables",
            axum::routing::patch(sessions::save_variables),
        )
        // 阶段六 6f:切换消息 swipe 版本(extra.swipes 数组)
        .route(
            "/api/chat/messages/{id}/swipe",
            post(sessions::swipe_message),
        )
        // 7 作用域变量(计划二):GET 读整树 / PUT 整树覆写 / PATCH JSON Patch 子集
        .route(
            "/api/variables",
            get(variables::get_variables)
                .put(variables::put_variables)
                .patch(variables::patch_variables),
        )
        .route("/api/chat/init-vars", get(sessions::init_vars))
        // 角色扮演 Agent 记录只读恢复(实跑问题 4):切会话/重启后恢复工具调用记录
        .route(
            "/api/chat/sessions/{id}/agent/trace",
            get(agent::session_trace),
        )
        .route("/api/chat/clear", post(sessions::clear))
        // 回退快照(批次 6.1「undo」):会话快照列表 / 按快照恢复
        .route("/api/chat/sessions/{id}/undo", get(undo::list_snapshots))
        .route("/api/undo/{id}/restore", post(undo::restore_snapshot))
        // 聊天(SSE)
        .route("/api/chat/send", post(chat::send))
        // 角色卡资源页作者脚本的自由生成桥(吸血鬼卡开场白等;一次性、非流式、不写会话)
        .route("/api/chat/generate-raw", post(chat::generate_raw))
        .route("/api/chat/stop", post(chat::stop))
        .route("/api/chat/compact", post(chat::compact))
        .route("/api/chat/compact/clear", post(chat::clear_compact))
}
