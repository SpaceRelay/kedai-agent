// 工具系统:注册表 + 内置工具(calculator / censor / memory / agent 强化工具集)
pub mod agent_tools;
// agent 强化工具集拆分(中层 L3 青层工具域;按功能域分文件,agent_tools.rs 为聚合入口)
mod agent_tools_agent;
mod agent_tools_read;
mod agent_tools_search;
mod agent_tools_shared;
mod agent_tools_write;
pub mod calculator;
pub mod censor;
pub mod memory;
pub mod multistep;
pub mod permissions;
pub mod registry;
pub mod revise;
pub mod variables;

use agent_tools::ToolDeps;
use registry::ToolRegistry;
use std::sync::Arc;

/// 启动时注册全部内置工具(calculator / censor_text / memory_read / memory_write / agent 强化工具集)
pub fn register_builtin_tools(registry: &ToolRegistry, deps: Arc<ToolDeps>) {
    // calculator
    registry.register(
        crate::models::types::ToolDefinition {
            name: "calculator".into(),
            description: "执行四则运算(白名单解析,安全)".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "expression": { "type": "string", "description": "如 12*34" } },
                "required": ["expression"]
            }),
        },
        Arc::new(
            |args: serde_json::Value, _ctx: crate::models::types::ToolContext| {
                Box::pin(async move {
                    let expr = args
                        .get("expression")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let result = calculator::calculate(&expr)?;
                    Ok(result.to_string())
                })
            },
        ),
    );
    memory::register_memory_tools(registry, deps.sessions.clone());
    variables::register_update_variables_tool(registry, deps.sessions.clone());
    censor::register_censor_tool(registry);
    revise::register_revise_passage_tool(registry);
    agent_tools::register_agent_tools(registry, deps);
}
