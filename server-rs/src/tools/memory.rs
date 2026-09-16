// memory 工具(跨会话记忆蒸馏·落地项 2):写入落 memory_entries 表(kind='tool',
// character_id 取当前会话角色),与蒸馏/手动添加共用同一精选与衰减策略。
// 旧版曾以会话内 system 消息(extra.kind=memory)承载:该路径有存量数据兼容负担,
// 保留读取兼容(memory_read 合并旧会话消息记忆),新写入一律走新表、不再产生旧行。
use crate::models::types::ToolContext;
use crate::services::memory_service::MemoryService;
use crate::services::session_service::SessionService;
use serde_json::{json, Value};
use std::sync::Arc;

/// 注册 memory_read / memory_write 到 registry
pub fn register_memory_tools(
    registry: &crate::tools::registry::ToolRegistry,
    sessions: Arc<SessionService>,
    memory: Arc<MemoryService>,
) {
    // memory_read:无参数,返回本角色全部长期记忆(新表) + 旧会话消息记忆(兼容)
    let sessions_read = sessions.clone();
    let memory_read = memory.clone();
    registry.register(
        crate::models::types::ToolDefinition {
            name: "memory_read".into(),
            description: "读取本角色的跨会话长期记忆".into(),
            parameters: json!({ "type": "object", "properties": {} }),
        },
        Arc::new(move |_args: Value, ctx: ToolContext| {
            let sessions_read = sessions_read.clone();
            let memory_read = memory_read.clone();
            Box::pin(async move {
                // 新表:按精选排序(pinned→usage→recency→id)取前 MEMORY_READ_LIMIT 条,
                // 与注入策略一致;旧表全量返回会随记忆库增长无界膨胀
                let entries = memory_read.list(&ctx.character_id);
                let picked = crate::services::memory_service::select_for_injection(
                    &entries,
                    crate::services::memory_service::MEMORY_READ_LIMIT,
                );
                let mut facts: Vec<String> = picked.iter().map(|e| e.content.clone()).collect();
                // 旧路径兼容:会话内 system 消息(extra.kind=memory)是历史版本的
                // 记忆存储,合并返回避免存量记忆丢失;新写入不再产生该类消息。
                let legacy: Vec<String> = sessions_read
                    .get_messages(&ctx.session_id)
                    .into_iter()
                    .filter(|m| {
                        m.role == "system"
                            && m.extra.get("kind").and_then(|v| v.as_str()) == Some("memory")
                    })
                    .map(|m| m.content)
                    .collect();
                facts.extend(legacy);
                Ok(json!({ "facts": facts }).to_string())
            })
        }),
    );

    // memory_write:参数 fact(string),写入一条跨会话记忆(kind='tool',按会话角色归属)
    let memory_write = memory;
    registry.register(
        crate::models::types::ToolDefinition {
            name: "memory_write".into(),
            description: "写入一条本角色的跨会话长期记忆".into(),
            parameters: json!({
                "type": "object",
                "properties": { "fact": { "type": "string" } },
                "required": ["fact"]
            }),
        },
        Arc::new(move |args: Value, ctx: ToolContext| {
            let memory_write = memory_write.clone();
            Box::pin(async move {
                let fact = args
                    .get("fact")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if fact.trim().is_empty() {
                    return Err("缺少 fact 参数".into());
                }
                let entry =
                    memory_write.insert(&ctx.character_id, Some(&ctx.session_id), "tool", &fact)?;
                // Phase 3:向量化开启时为工具写入的记忆补向量(失败静默,不阻断工具返回)
                if let Some(settings) = memory_write.settings_snapshot() {
                    if settings.embedding_enabled {
                        let svc = crate::services::embedding_service::EmbeddingService::new();
                        match svc.embed_one(&settings, &entry.content).await {
                            Ok(v) if !v.is_empty() => {
                                let memory_id = entry.id;
                                let svc2 = memory_write.clone();
                                let _ = tokio::task::spawn_blocking(move || {
                                    svc2.upsert_vector(memory_id, &v)
                                })
                                .await;
                            }
                            Ok(_) => {}
                            Err(e) => tracing::warn!(
                                error = e.to_string(),
                                memory_id = entry.id,
                                "记忆工具写入后生成向量失败"
                            ),
                        }
                    }
                }
                Ok(json!({ "ok": true, "stored": fact }).to_string())
            })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::db::Db;
    use crate::models::types::ToolContext;
    use crate::tools::registry::ToolRegistry;
    use crate::utils::test_support::TempDataDir;
    use serde_json::json;

    /// 返回 (守卫, ...):解构绑定按**逆序**析构,守卫在前才活到最后(见 test_support 模块头)
    fn setup() -> (
        TempDataDir,
        ToolRegistry,
        Arc<MemoryService>,
        Arc<SessionService>,
    ) {
        let dir = TempDataDir::new("memory-tool");
        let db = Arc::new(Db::open(&dir.join("kedai.db"), &dir).unwrap());
        let memory = Arc::new(MemoryService::new(db.clone()));
        let sessions = Arc::new(SessionService::new(db));
        // 播种 character + session(messages 表 FK 指向 sessions)
        {
            let conn = rusqlite::Connection::open(dir.join("kedai.db")).unwrap();
            conn.execute(
                "INSERT INTO characters (id, name, chara_name, description, file_path, data_raw, created_at)
                 VALUES ('charM', 'm', 'm', '', '', '{}', '')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, character_id, title, created_at, updated_at)
                 VALUES ('sessM', 'charM', 't', '', '')",
                [],
            )
            .unwrap();
        }
        let registry = ToolRegistry::new();
        register_memory_tools(&registry, sessions.clone(), memory.clone());
        (dir, registry, memory, sessions)
    }

    /// memory 工具写入落 memory_entries(kind='tool',按会话角色归属),
    /// 不再产生旧版会话消息;读取合并新表与旧会话消息记忆。
    #[tokio::test]
    async fn memory_write_lands_in_entries_and_read_merges_legacy() {
        let (_dir, registry, memory, sessions) = setup();
        let ctx = ToolContext {
            session_id: "sessM".into(),
            character_id: "charM".into(),
            agent_depth: 0,
        };

        // 写入:落新表(memory_write 为危险级工具,测试以已裁决放行路径执行)
        let decision = crate::tools::permissions::PermissionDecision {
            allowed: true,
            risk: crate::tools::permissions::ToolRisk::Dangerous,
            reason: "测试放行".into(),
        };
        let out = registry
            .execute_with_decision(
                "memory_write",
                r#"{"fact":"用户讨厌香菜"}"#,
                ctx.clone(),
                &decision,
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], json!(true));
        let entries = memory.list("charM");
        assert_eq!(
            entries.len(),
            1,
            "memory_write 应落 memory_entries: {entries:?}"
        );
        assert_eq!(entries[0].kind, "tool");
        assert_eq!(entries[0].content, "用户讨厌香菜");
        assert_eq!(entries[0].source_session_id.as_deref(), Some("sessM"));
        // 新路径不再写会话消息
        let msgs = sessions.get_messages("sessM");
        assert!(
            !msgs
                .iter()
                .any(|m| m.extra.get("kind").and_then(|k| k.as_str()) == Some("memory")),
            "新写入不应再产生旧版会话消息记忆"
        );

        // 旧版数据兼容:会话内已有 system+kind=memory 消息 → 读取时与新表合并
        sessions
            .add_message(
                "sessM",
                "system",
                "旧版记忆:用户养了一只猫",
                json!({ "kind": "memory" }),
            )
            .unwrap();
        let out = registry.execute("memory_read", "{}", ctx).await.unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let facts = v["facts"].as_array().unwrap();
        assert!(
            facts.iter().any(|f| f == "用户讨厌香菜"),
            "读取应含新表记忆: {facts:?}"
        );
        assert!(
            facts.iter().any(|f| f == "旧版记忆:用户养了一只猫"),
            "读取应合并旧版会话消息记忆: {facts:?}"
        );

        // 空 fact 拒绝
        assert!(registry
            .execute(
                "memory_write",
                r#"{"fact":"  "}"#,
                ToolContext {
                    session_id: "sessM".into(),
                    character_id: "charM".into(),
                    agent_depth: 0,
                }
            )
            .await
            .is_err());

        drop(registry);
    }
}
