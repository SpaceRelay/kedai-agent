// 多步模式工具:get_state / apply_patch(§0.1-A / §10.1 多步 agent 工具循环)。
//
// 复用 ToolRegistry 的权限/超时/截断管线(30s 超时、64KB 截断、危险工具需授权)。
// get_state 只读契约裁剪后的状态;apply_patch 经契约门控(replace/delta/remove/move,
// unknown_field/not_owner 拒绝)。契约经共享 ContractRegistry 加载(与引擎同缓存,
// 改卡后由 API 写路径 invalidate 失效)。这两个工具不进入 agent 正文模式的默认下发
// 列表(chat.rs 过滤),仅供多步驱动器/自定义流程白名单引用。
use crate::contracts::ContractRegistry;
use crate::models::types::{ToolContext, ToolDefinition};
use crate::services::kaleido_state_service::KaleidoStateService;
use crate::services::session_service::SessionService;
use crate::tools::registry::{ToolExecutor, ToolRegistry};
use serde_json::{json, Value};
use std::sync::Arc;

/// 注册多步模式两个工具。
pub fn register_multistep_tools(
    registry: &ToolRegistry,
    sessions: Arc<SessionService>,
    contracts: Arc<ContractRegistry>,
    kaleido: Arc<KaleidoStateService>,
) {
    // get_state:只读契约裁剪后的状态
    let get_state_exec: ToolExecutor = {
        let sessions = sessions.clone();
        let contracts = contracts.clone();
        Arc::new(move |_args: Value, ctx: ToolContext| {
            let sessions = sessions.clone();
            let contracts = contracts.clone();
            Box::pin(async move {
                let vars = sessions.load_assistant_vars(&ctx.session_id);
                if vars.is_empty() {
                    return Ok("{}".to_string());
                }
                // turn_id 用消息数推算,与引擎 len-as-turn 启发式同源
                // (engine collect_context 与 generate_mvu_status 调用处均传 history.len())。
                let turn_id = sessions.get_messages(&ctx.session_id).len() as u64;
                // 契约存在 → 按 dueFields + observe 裁剪(只暴露到期字段与依赖);
                // 无契约 → 整树 JSON(存量卡兼容)。
                let contract = contracts.load(&ctx.character_id);
                let (text, _due) =
                    crate::contracts::build_state_prompt(contract.as_ref(), vars.tree(), turn_id);
                Ok(if text.is_empty() {
                    "{}".to_string()
                } else {
                    text
                })
            })
        })
    };
    registry.register(
        ToolDefinition {
            name: "get_state".into(),
            description: "读取当前 stat_data 变量树中本轮相关的字段值(只读,不修改任何状态)。返回 JSON 对象(path → value),供你决定下一步更新。".into(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
        },
        get_state_exec,
    );

    // apply_patch:经契约门控应用补丁并持久化;P6 起契约生效时逐 op 留痕
    // (kaleido_changelog)并维护 meta.pending(去重/上限),响应携带熔断哈希
    // (rejection_hashes)供多步驱动器接入 multi_step 熔断。
    // P7:核心管线抽到 VariableApplyService,与 HTTP POST /api/variable/update
    // 共用同一契约引擎出口(单库无分叉);本执行器只做参数校验与响应序列化。
    let apply_patch_exec: ToolExecutor = {
        let sessions = sessions.clone();
        let contracts = contracts.clone();
        let kaleido = kaleido.clone();
        Arc::new(move |args: Value, ctx: ToolContext| {
            let sessions = sessions.clone();
            let contracts = contracts.clone();
            let kaleido = kaleido.clone();
            Box::pin(async move {
                let items = args
                    .get("patches")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "apply_patch.patches 必须是非空数组".to_string())?;
                if items.is_empty() {
                    return Err("apply_patch.patches 不能为空".to_string());
                }
                let apply = crate::services::variable_apply::VariableApplyService::new(
                    sessions, contracts, kaleido,
                );
                let outcome = apply.apply(&ctx.session_id, &ctx.character_id, items, "agent")?;
                // 全部被拦(拒绝/低置信)是工具级失败:LLM 需要明确信号停止重试,
                // 低置信提议已入 meta.pending 等待后续自纠(共享层保证)。
                if !outcome.ok {
                    return Err(format!("没有可应用的操作({})", outcome.warnings.join(",")));
                }
                let mut resp = json!({
                    "ok": true,
                    "stat_data": outcome.tree
                });
                if !outcome.warnings.is_empty() {
                    resp["warnings"] = json!(outcome.warnings);
                }
                // 熔断指纹:被拒(带真实原因)/低置信 op 的哈希列表,供多步驱动器接入
                // multi_step::record_and_check(§10.1 近 N 步同指纹 ≥3 次熔断)。
                if !outcome.breaker_hashes.is_empty() {
                    resp["breaker_hashes"] = json!(outcome.breaker_hashes);
                }
                Ok(resp.to_string())
            })
        })
    };
    registry.register(
        ToolDefinition {
            name: "apply_patch".into(),
            description: "把本轮确定的变量更新应用到 stat_data 并立即持久化。传入 JSON Patch 数组,支持 replace/delta/remove/move。被契约拒绝的操作会返回错误信息。".into(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "patches": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                            "op": { "type": "string", "enum": ["replace", "delta", "remove", "move"] },
                            "path": { "type": "string", "minLength": 1 },
                            "value": {},
                            "from": { "type": "string", "minLength": 1 },
                            "confidence": { "type": "string", "enum": ["low", "medium", "high"], "description": "本条更新的置信度;低置信不写入,进入待复核队列" }
                            },
                            "required": ["op", "path"]
                        }
                    }
                },
                "required": ["patches"]
            }),
        },
        apply_patch_exec,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::character_service::CharacterService;
    use crate::services::world_book_service::WorldBookService;

    /// 测试依赖:会话服务 + 契约注册表 + 自持角色服务(建种子角色用)。
    struct Fixture {
        sessions: Arc<SessionService>,
        registry: Arc<ContractRegistry>,
        characters: Arc<CharacterService>,
        kaleido: Arc<KaleidoStateService>,
    }

    fn fixture() -> Fixture {
        let dir = std::env::temp_dir().join(format!("kedai-multistep-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(crate::models::db::Db::open(&dir.join("t.db"), &dir).unwrap());
        let sessions = Arc::new(SessionService::new(db.clone()));
        let characters = Arc::new(CharacterService::new(db.clone(), dir));
        let world_books = Arc::new(WorldBookService::new(db.clone()));
        let kaleido = Arc::new(KaleidoStateService::new(db.clone()));
        let registry = Arc::new(ContractRegistry::new(characters.clone(), world_books));
        Fixture {
            sessions,
            registry,
            characters,
            kaleido,
        }
    }

    /// 建真实角色+会话并返回会话 id:session_assistant_vars 与 sessions 均有 FK 约束
    /// (指向 characters/sessions),测试必须用真实记录,否则写入被 FK 静默拒绝。
    fn make_session(f: &Fixture) -> String {
        f.characters.seed_default_character();
        let cid = f.characters.list()[0].id.clone();
        f.sessions.ensure_session(&cid).unwrap().id
    }

    fn ctx(session_id: &str, character_id: &str) -> ToolContext {
        ToolContext {
            session_id: session_id.into(),
            character_id: character_id.into(),
        }
    }

    /// get_state:无变量树返回 {};apply_patch 无契约放行并持久化。
    #[tokio::test]
    async fn multistep_tools_roundtrip_without_contract() {
        let f = fixture();
        let sid = make_session(&f);
        let cid = f.characters.list()[0].id.clone();
        let tools = ToolRegistry::new();
        register_multistep_tools(&tools, f.sessions.clone(), f.registry.clone(), f.kaleido.clone());
        tools
            .permissions()
            .authorize("get_state", "session", &sid)
            .unwrap();
        tools
            .permissions()
            .authorize("apply_patch", "session", &sid)
            .unwrap();

        // 无变量树:get_state 返回 {}
        let out = tools
            .execute("get_state", "{}", ctx(&sid, &cid))
            .await
            .unwrap();
        assert_eq!(out, "{}");

        // apply_patch 无契约放行,写入并持久化
        let args = json!({ "patches": [{ "op": "replace", "path": "好感度", "value": 5 }] });
        let out = tools
            .execute("apply_patch", &args.to_string(), ctx(&sid, &cid))
            .await
            .unwrap();
        assert!(out.contains("\"ok\":true"), "out: {out}");
        let vars = f.sessions.load_assistant_vars(&sid);
        assert_eq!(vars.get_value("好感度"), Some(&json!(5)));
    }

    /// apply_patch 参数校验:空数组/无效 op 报错。
    #[tokio::test]
    async fn apply_patch_rejects_invalid_arguments() {
        let f = fixture();
        let sid = make_session(&f);
        let cid = f.characters.list()[0].id.clone();
        let tools = ToolRegistry::new();
        register_multistep_tools(&tools, f.sessions.clone(), f.registry.clone(), f.kaleido.clone());
        tools
            .permissions()
            .authorize("apply_patch", "session", &sid)
            .unwrap();

        let err = tools
            .execute("apply_patch", r#"{"patches": []}"#, ctx(&sid, &cid))
            .await
            .unwrap_err();
        assert!(err.contains("不能为空"), "err: {err}");

        let err = tools
            .execute(
                "apply_patch",
                r#"{"patches": [{"op": "delta", "path": "x", "value": "abc"}]}"#,
                ctx(&sid, &cid),
            )
            .await
            .unwrap_err();
        assert!(err.contains("没有有效操作") || err.contains("数字"), "err: {err}");
    }

    /// apply_patch 契约留痕(P6):混合 applied/pending/rejected 三类 op——
    /// 声明字段高置信写入并落 changelog;低置信入 meta.pending 不写树;
    /// 未声明字段被拒;响应携带 breaker_hashes。第二轮覆盖同 path 低置信
    /// 提议后 pending 被消费。
    #[tokio::test]
    async fn apply_patch_contract_logging_and_pending() {
        let f = fixture();
        let sid = make_session(&f);
        let cid = f.characters.list()[0].id.clone();
        // 契约声明两个字段(世界.年分 未声明,供拒绝断言);minConfidence=medium:
        // low 置信进 pending,medium/high 写入(缺省阈值为 low 不会拦)
        let contract = serde_json::json!({
            "version": 1,
            "id": "multistep-test-contract",
            "schema": { "properties": {} },
            "guardrails": { "minConfidence": "medium" },
            "updateRules": {
                "心之所向.好感度": {
                    "path": "心之所向.好感度", "type": "number",
                    "updateMode": "every_turn", "display": true
                },
                "心之所向.信任度": {
                    "path": "心之所向.信任度", "type": "number",
                    "updateMode": "every_turn", "display": true
                }
            }
        });
        f.characters.set_embedded_contract(&cid, Some(&contract));

        // 预置变量树(apply_patch 前的世界态)
        let mut vars = f.sessions.load_assistant_vars(&sid);
        vars.apply_patches(&[
            crate::parsing::assistant::PatchOp::Replace {
                path: "心之所向.好感度".into(),
                value: json!(0),
                reason: None,
            },
            crate::parsing::assistant::PatchOp::Replace {
                path: "心之所向.信任度".into(),
                value: json!(0),
                reason: None,
            },
            crate::parsing::assistant::PatchOp::Replace {
                path: "世界.年分".into(),
                value: json!(2024),
                reason: None,
            },
        ])
        .unwrap();
        f.sessions.save_assistant_vars(&sid, &vars).unwrap();

        let tools = ToolRegistry::new();
        register_multistep_tools(&tools, f.sessions.clone(), f.registry.clone(), f.kaleido.clone());
        tools
            .permissions()
            .authorize("apply_patch", "session", &sid)
            .unwrap();

        // 第一轮:高置信写入 + 低置信 pending + 未声明拒绝
        let args = serde_json::json!({
            "patches": [
                { "op": "replace", "path": "心之所向.好感度", "value": 5 },
                { "op": "replace", "path": "心之所向.信任度", "value": 3, "confidence": "low" },
                { "op": "replace", "path": "世界.年分", "value": 2025 }
            ]
        });
        let out = tools
            .execute("apply_patch", &args.to_string(), ctx(&sid, &cid))
            .await
            .unwrap();
        let resp: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(resp["ok"], json!(true));
        // warnings 覆盖拒绝与低置信各一条;熔断哈希 = 1 拒绝 + 1 pending
        assert_eq!(resp["warnings"].as_array().map(Vec::len), Some(2), "resp: {resp}");
        assert_eq!(resp["breaker_hashes"].as_array().map(Vec::len), Some(2));

        // 树:高置信写入生效;pending 未写;未声明保持原值
        let vars = f.sessions.load_assistant_vars(&sid);
        assert_eq!(vars.get_value("心之所向.好感度"), Some(&json!(5)));
        assert_eq!(vars.get_value("心之所向.信任度"), Some(&json!(0)));
        assert_eq!(vars.get_value("世界.年分"), Some(&json!(2024)));

        // 留痕:仅高置信 op 进 changelog,old/new/seq 正确
        let entries = f.kaleido.list_entries(&sid, 10).unwrap();
        assert_eq!(entries.len(), 1, "entries: {entries:?}");
        assert_eq!(entries[0].path, "心之所向.好感度");
        assert_eq!(entries[0].old, json!(0));
        assert_eq!(entries[0].new, json!(5));
        assert!(entries[0].seq > 0);

        // meta:低置信 op 入队;契约版本推进(last_turn_id=消息数,空会话为 0 合法)
        let (version, meta) = f.kaleido.load_meta(&sid).unwrap().expect("kaleido_state 行存在");
        assert_eq!(version, 1);
        assert_eq!(meta.last_turn_id, 0);
        assert_eq!(meta.pending.len(), 1, "pending: {:?}", meta.pending);
        assert_eq!(meta.pending[0].op.path, "心之所向.信任度");

        // 第二轮:高置信覆盖信任度 → pending 被消费、留痕累计
        let args = json!({
            "patches": [{ "op": "replace", "path": "心之所向.信任度", "value": 4 }]
        });
        let out = tools
            .execute("apply_patch", &args.to_string(), ctx(&sid, &cid))
            .await
            .unwrap();
        let resp: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(resp["ok"], json!(true));
        assert!(resp.get("breaker_hashes").is_none(), "本轮无拒绝/低置信");

        let vars = f.sessions.load_assistant_vars(&sid);
        assert_eq!(vars.get_value("心之所向.信任度"), Some(&json!(4)));
        let entries = f.kaleido.list_entries(&sid, 10).unwrap();
        assert_eq!(entries.len(), 2);
        let (_, meta) = f.kaleido.load_meta(&sid).unwrap().unwrap();
        assert!(meta.pending.is_empty(), "同 path 成功应用应消费 pending: {:?}", meta.pending);

        // 第三轮:非法 confidence 值(大小写错误)不得静默升级为 High 绕过门控,
        // 按低置信处理进 pending(宁可错杀);唯一 op 被拦时工具整体报错
        let args = json!({
            "patches": [{ "op": "replace", "path": "心之所向.好感度", "value": 9, "confidence": "Low" }]
        });
        let err = tools
            .execute("apply_patch", &args.to_string(), ctx(&sid, &cid))
            .await
            .unwrap_err();
        assert!(err.contains("低置信"), "err: {err}");
        let vars = f.sessions.load_assistant_vars(&sid);
        assert_eq!(vars.get_value("心之所向.好感度"), Some(&json!(5)), "非法置信度不应写入");
        let (_, meta) = f.kaleido.load_meta(&sid).unwrap().unwrap();
        assert_eq!(meta.pending.len(), 1, "非法置信度按 Low 入队: {:?}", meta.pending);
    }
}
