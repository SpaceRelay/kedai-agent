// memory 工具(与 Node 版 tools/memory.ts 对齐):以 system+memory 消息承载
use crate::models::types::ToolContext;
use crate::services::session_service::SessionService;
use serde_json::{json, Value};
use std::sync::Arc;

/// 注册 memory_read / memory_write 到 registry
pub fn register_memory_tools(
    registry: &crate::tools::registry::ToolRegistry,
    sessions: Arc<SessionService>,
) {
    // memory_read:无参数,返回全部记忆事实
    let sessions_read = sessions.clone();
    registry.register(
        crate::models::types::ToolDefinition {
            name: "memory_read".into(),
            description: "读取本会话长期记忆".into(),
            parameters: json!({ "type": "object", "properties": {} }),
        },
        Arc::new(move |_args: Value, ctx: ToolContext| {
            let sessions_read = sessions_read.clone();
            Box::pin(async move {
                let msgs = sessions_read.get_messages(&ctx.session_id);
                let facts: Vec<String> = msgs
                    .into_iter()
                    .filter(|m| {
                        m.role == "system"
                            && m.extra.get("kind").and_then(|v| v.as_str()) == Some("memory")
                    })
                    .map(|m| m.content)
                    .collect();
                Ok(json!({ "facts": facts }).to_string())
            })
        }),
    );

    // memory_write:参数 fact(string),写入一条记忆
    let sessions_write = sessions.clone();
    registry.register(
        crate::models::types::ToolDefinition {
            name: "memory_write".into(),
            description: "写入一条本会话长期记忆".into(),
            parameters: json!({
                "type": "object",
                "properties": { "fact": { "type": "string" } },
                "required": ["fact"]
            }),
        },
        Arc::new(move |args: Value, ctx: ToolContext| {
            let sessions_write = sessions_write.clone();
            Box::pin(async move {
                let fact = args
                    .get("fact")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if fact.trim().is_empty() {
                    return Err("缺少 fact 参数".into());
                }
                sessions_write.add_message(
                    &ctx.session_id,
                    "system",
                    &fact,
                    json!({ "kind": "memory" }),
                )?;
                Ok(json!({ "ok": true, "stored": fact }).to_string())
            })
        }),
    );
}
