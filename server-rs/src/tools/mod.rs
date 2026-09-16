// 工具系统:注册表 + 内置工具(calculator / censor / memory / agent 强化工具集)
//
// 代际: L2(中层·干 / Orchestration)——**2026-09-14 由 L3 修正为 L2**。
// 判据: ① P3 隔离性不满足——本目录内无沙箱/无设置开关/无实验隔离,注册表是 L2 骨干设施;
//       ② P5 复用度命中——被 agents/services/api 共 32 处复用,属核心必需设施
//       (它失败则整个应用不可用,与 L3「失败必须被隔离」语义相反)。
// 纪律: 可依赖 L1/L2;不得依赖 L3(scripts/mcp/plugins/exec)与 entry。
// 真 L3 隔离能力在相邻模块:scripts/(rquickjs 沙箱)、mcp/(默认关)、plugins/(受控求值器)、
// services/exec/(默认关)。详见 docs/契约-架构与数据.md §2.5 末段与晋升台账。
pub mod action_class;
pub mod agent_tools;
pub mod bash;
// agent 强化工具集拆分(按功能域分文件;agent_tools.rs 为聚合入口)。
// 代际归属见本文件头部:tools/ 整体为 L2(2026-09-14 修正),不再是「青层工具域」。
mod agent_tools_agent;
mod agent_tools_read;
mod agent_tools_search;
mod agent_tools_shared;
mod agent_tools_write;
pub mod calculator;
pub mod censor;
pub mod command_risk;
pub mod memory;
pub mod multistep;
pub mod permissions;
pub mod registry;
pub mod revise;
pub mod tool_sets;
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
    memory::register_memory_tools(registry, deps.sessions.clone(), deps.memory.clone());
    variables::register_update_variables_tool(registry, deps.sessions.clone());
    censor::register_censor_tool(registry);
    revise::register_revise_passage_tool(registry);
    // 命令执行(阶段 B):危险工具,任务模式默认策略不下发;授权与命令级风险确认见
    // tools/command_risk.rs 与 tools/permissions.rs(破坏性/提权命令任何模式都不自动放行)。
    bash::register_bash_tool(
        registry,
        deps.db.clone(),
        deps.settings.clone(),
        deps.data_dir.clone(),
    );
    agent_tools::register_agent_tools(registry, deps);
}
