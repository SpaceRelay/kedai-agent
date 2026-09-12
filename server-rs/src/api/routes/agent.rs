// Agent / 工具授权 / 自定义执行流程路由(自 api/mod.rs build_router 迁入)
use crate::api::app_state::AppState;
use crate::api::{agent, agent_flows, tool_permissions};
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

pub(crate) fn agent_routes() -> Router<Arc<AppState>> {
    Router::new()
        // Agent
        .route("/api/agent/plan", post(agent::plan))
        .route("/api/agent/execute", post(agent::execute))
        .route("/api/agent/interrupt", post(agent::interrupt))
        .route(
            "/api/agent/tool-permissions",
            get(tool_permissions::list)
                .post(tool_permissions::authorize)
                .delete(tool_permissions::revoke),
        )
        .route(
            "/api/agent/tool-permissions/resolve",
            post(tool_permissions::resolve),
        )
        // 自定义 Agent 执行流程(custom 模式):流程库 + 当前选择 + 增删
        .route(
            "/api/agent-flows",
            get(agent_flows::get_agent_flows).put(agent_flows::update_agent_flows),
        )
        .route(
            "/api/agent-flows/select",
            post(agent_flows::select_agent_flow),
        )
        .route(
            "/api/agent-flows/{id}",
            axum::routing::delete(agent_flows::delete_agent_flow),
        )
}
