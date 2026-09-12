// Agent 强化工具集(中层 L3 青层工具域):聚合入口 + 共享定义。
// 按功能域拆分到同目录子模块(见 tools/mod.rs):
//   agent_tools_shared  共享件:路径安全校验/文件区读写/RNG/role 工具
//   agent_tools_read    read      读取世界书/角色提示词/skill/文件/子任务结果(支持同步多次查询)
//   agent_tools_write   write/replace/create 写入/修改/创建气泡与文件区文件
//   agent_tools_agent   agentgo/agentend/sleep/todo 子智能体编排与计划表
//   agent_tools_search  search    联网搜索(DuckDuckGo 默认,端点可配置;SSRF 防护共用)
// 本文件仅保留:ToolDeps 依赖集合、register_agent_tools 聚合入口、测试。
// 外部路径兼容:crate::tools::agent_tools::{ToolDeps, register_agent_tools,
// character_file_root, resolve_public_http_url} 均经本文件/pub use 保持可用。
use crate::connectors::Connector;
use crate::services::agent_session_service::AgentSessionService;
use crate::services::agent_subtask_service::AgentSubtaskService;
use crate::services::character_service::CharacterService;
use crate::services::session_service::SessionService;
use crate::services::settings_service::RuntimeSettings;
use crate::services::skill_service::SkillService;
use crate::services::world_book_service::WorldBookService;
use crate::tools::registry::ToolRegistry;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

// 各功能域注册函数(register_agent_tools 聚合调用;测试经 use super::* 复用)
use super::agent_tools_agent::{
    register_agentend, register_agentgo, register_sleep, register_todo,
};
use super::agent_tools_read::register_read;
use super::agent_tools_search::register_search;
#[cfg(test)]
use super::agent_tools_shared::random_u64;
use super::agent_tools_shared::register_role;
use super::agent_tools_write::{register_create, register_replace, register_write};

// 保持原公共导出路径:crate::tools::agent_tools::character_file_root(shared 定义)
pub use super::agent_tools_shared::character_file_root;
// 批次 6.1:undo_service 恢复快照时复用同一路径安全规则(模块本身私有,经本文件重导出)
pub(crate) use super::agent_tools_shared::safe_rel_path;
// 保持原公共导出路径:crate::tools::agent_tools::resolve_public_http_url(search 定义)
// 被 api/resource.rs 以 `use crate::tools::agent_tools::resolve_public_http_url` 引用;
// 原可见性即为 pub(crate),故用 pub(crate) use re-export(crate 内可达)。
pub(crate) use super::agent_tools_search::resolve_public_http_url;

/// 工具依赖集合(注册时注入;启动时构造一次)
pub struct ToolDeps {
    pub sessions: Arc<SessionService>,
    pub characters: Arc<CharacterService>,
    pub world_books: Arc<WorldBookService>,
    pub agent_sessions: Arc<AgentSessionService>,
    pub subtasks: Arc<AgentSubtaskService>,
    pub skills: Arc<SkillService>,
    pub settings: Arc<Mutex<RuntimeSettings>>,
    pub connector: Arc<RwLock<Connector>>,
    pub data_dir: PathBuf,
    /// 跨会话记忆蒸馏(落地项 2):memory_write / memory_read 共用
    pub memory: Arc<crate::services::memory_service::MemoryService>,
    /// 聊天引擎(批次 4.3b 子 agent 工具化:run_subtask 经 run_tool_loop 跑白名单
    /// 工具循环)。构造时序:ToolDeps 先于 AgentEngine 创建(工具注册在引擎之前),
    /// 故用 OnceLock + Weak 由 app_state 在引擎构造后注入,防循环引用;
    /// 未注入(单测等)时子任务回退纯生成路径。
    pub engine: std::sync::OnceLock<std::sync::Weak<crate::agents::engine::AgentEngine>>,
    /// 任务服务(批次 4.3b:task: 前缀虚拟 session 的子 agent 进度经任务事件桥
    /// 发 agent_status,并落 task_llm_calls/task_usage;Weak 防循环,缺省 = 非任务模式)
    pub tasks: std::sync::OnceLock<std::sync::Weak<crate::services::task_service::TaskService>>,
}

impl ToolDeps {
    /// 运行期设置快照:lock 后立即 clone 返回,锁中毒时 into_inner 恢复取值。
    /// 快照语义:不留锁跨 await —— 工具处理器多为 async,散点 .lock() 易把
    /// 锁守卫带进 await 点;统一经本方法取独立副本(与 AppState/AgentEngine 同名同语义)。
    pub fn settings_snapshot(&self) -> RuntimeSettings {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 测试用空依赖(临时目录 + mock 连接器)
    #[cfg(test)]
    fn dummy_for_test() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kedai-tool-test-{}-{}",
            std::process::id(),
            random_u64()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let db = Arc::new(crate::models::db::Db::open(&dir.join("t.db"), &dir).unwrap());
        ToolDeps {
            sessions: Arc::new(SessionService::new(db.clone())),
            characters: Arc::new(CharacterService::new(db.clone(), dir.clone())),
            world_books: Arc::new(WorldBookService::new(db.clone())),
            agent_sessions: Arc::new(AgentSessionService::new(db.clone())),
            subtasks: Arc::new(AgentSubtaskService::new(db.clone())),
            skills: Arc::new(SkillService::new(db.clone())),
            settings: Arc::new(Mutex::new(RuntimeSettings::from_config(
                &crate::config::AppConfig::from_env(),
            ))),
            connector: Arc::new(RwLock::new(Connector::Mock(
                crate::connectors::mock::MockConnector::new(),
            ))),
            data_dir: dir,
            memory: Arc::new(crate::services::memory_service::MemoryService::new(db)),
            // 测试缺省不注入引擎/任务服务:子任务走纯生成回退路径
            engine: std::sync::OnceLock::new(),
            tasks: std::sync::OnceLock::new(),
        }
    }
}

/// 注册全部强化工具
pub fn register_agent_tools(registry: &ToolRegistry, deps: Arc<ToolDeps>) {
    register_read(registry, deps.clone());
    register_role(registry, deps.clone());
    register_write(registry, deps.clone());
    register_replace(registry, deps.clone());
    register_create(registry, deps.clone());
    register_agentgo(registry, deps.clone());
    register_sleep(registry, deps.clone());
    register_agentend(registry, deps.clone());
    register_todo(registry, deps.clone());
    register_search(registry, deps.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    // 测试专用可见性:拆分后私有辅助函数移入子模块,经显式 use 引入(断言不变)
    use crate::models::types::ToolContext;
    use crate::tools::agent_tools_search::{
        decode_html, is_public_ip, normalize_search_url, parse_search_html, percent_decode,
        percent_encode, strip_html,
    };
    use crate::tools::agent_tools_shared::safe_rel_path;
    use serde_json::Value;

    #[test]
    fn test_safe_rel_path() {
        assert_eq!(safe_rel_path("a/b/c.txt").unwrap(), "a/b/c.txt");
        assert_eq!(safe_rel_path("a\\b\\c.txt").unwrap(), "a/b/c.txt");
        assert!(safe_rel_path("../x").is_err());
        assert!(safe_rel_path("a/../../x").is_err());
        assert!(safe_rel_path("/abs").is_err());
        assert!(safe_rel_path("").is_err());
        assert!(safe_rel_path("./a//b").unwrap() == "a/b");
    }

    #[test]
    fn test_percent_roundtrip() {
        let s = "kedai 是什么？";
        let enc = percent_encode(s);
        assert_eq!(percent_decode(&enc), s);
        assert_eq!(percent_encode("abc-_.~"), "abc-_.~");
    }

    #[test]
    fn test_decode_html_and_strip() {
        assert_eq!(decode_html("a &amp; b &lt;c&gt;"), "a & b <c>");
        assert_eq!(strip_html("<b>标题</b> &amp; 更多"), "标题 & 更多");
    }

    #[test]
    fn test_normalize_search_url() {
        assert_eq!(
            normalize_search_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fx&rut=123"),
            "https://example.com/x"
        );
        assert_eq!(
            normalize_search_url("//example.com/a"),
            "https://example.com/a"
        );
        assert_eq!(
            normalize_search_url("https://example.com"),
            "https://example.com"
        );
    }

    #[tokio::test]
    async fn search_ssrf_rejects_local_and_metadata_without_network_request() {
        for url in [
            "http://127.0.0.1:3001/search",
            "http://169.254.169.254/latest/meta-data/",
            "file:///etc/passwd",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:169.254.169.254]/",
        ] {
            assert!(resolve_public_http_url(url).await.is_err(), "应拒绝 {url}");
        }
    }

    #[test]
    fn public_ip_classifier_rejects_private_link_local_and_multicast() {
        for ip in [
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "0.0.0.0",
            "224.0.0.1",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "应拒绝 {ip}");
        }
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn test_parse_search_html() {
        let html = r#"<div class="result results_links">
            <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage">示例标题</a>
            <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage">这是摘要 <b>加粗</b></a>
        </div>"#;
        let results = parse_search_html(html, 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["title"], "示例标题");
        assert_eq!(results[0]["url"], "https://example.com/page");
        assert_eq!(results[0]["snippet"], "这是摘要 加粗");
    }

    #[tokio::test]
    async fn test_role_tool() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        let reg = ToolRegistry::new();
        register_role(&reg, deps);
        let out = reg
            .execute(
                "role",
                r#"{"sides":6,"count":4}"#,
                ToolContext {
                    session_id: "s".into(),
                    character_id: "c".into(),
                    agent_depth: 0,
                },
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let rolls = v["rolls"].as_array().unwrap();
        assert_eq!(rolls.len(), 4);
        for r in rolls {
            let n = r.as_i64().unwrap();
            assert!((1..=6).contains(&n));
        }
        let total: i64 = rolls.iter().map(|r| r.as_i64().unwrap()).sum();
        assert_eq!(v["total"].as_i64().unwrap(), total);
        // 范围模式
        let out2 = reg
            .execute(
                "role",
                r#"{"min":10,"max":20,"count":2}"#,
                ToolContext {
                    session_id: "s".into(),
                    character_id: "c".into(),
                    agent_depth: 0,
                },
            )
            .await
            .unwrap();
        let v2: Value = serde_json::from_str(&out2).unwrap();
        for r in v2["rolls"].as_array().unwrap() {
            assert!((10..=20).contains(&r.as_i64().unwrap()));
        }
    }

    /// write 文件 / create 目录与文件 / read 读回 / 重复创建报错 / 目录穿越被拒
    #[tokio::test]
    async fn test_write_create_read_files() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        let reg = ToolRegistry::new();
        register_write(&reg, deps.clone());
        register_read(&reg, deps.clone());
        register_create(&reg, deps.clone());
        let ctx = ToolContext {
            session_id: "s".into(),
            character_id: "c".into(),
            agent_depth: 0,
        };
        for tool in ["write", "create"] {
            reg.permissions()
                .authorize(tool, "session", &ctx.session_id)
                .unwrap();
        }

        // write 文件(自动建父目录)
        let out = reg
            .execute(
                "write",
                r##"{"target":"file","path":"笔记/plan.md","content":"# 计划"}"##,
                ctx.clone(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["path"], "笔记/plan.md");

        // create 目录
        let out2 = reg
            .execute(
                "create",
                r#"{"items":[{"type":"dir","path":"素材"}]}"#,
                ctx.clone(),
            )
            .await
            .unwrap();
        let v2: Value = serde_json::from_str(&out2).unwrap();
        assert_eq!(v2["results"][0]["ok"], true);

        // create 重复目录/文件 → 单项报错但整体成功
        let out3 = reg.execute("create", r#"{"items":[{"type":"dir","path":"素材"},{"type":"file","path":"笔记/plan.md","content":"x"}]}"#, ctx.clone()).await.unwrap();
        let v3: Value = serde_json::from_str(&out3).unwrap();
        assert_eq!(v3["results"].as_array().unwrap().len(), 2);
        assert_eq!(v3["results"][0]["ok"], false);
        assert_eq!(v3["results"][1]["ok"], false);

        // read 读回文件
        let out4 = reg
            .execute(
                "read",
                r#"{"queries":[{"type":"file","name":"笔记/plan.md"}]}"#,
                ctx.clone(),
            )
            .await
            .unwrap();
        let v4: Value = serde_json::from_str(&out4).unwrap();
        assert_eq!(v4["results"][0]["content"], "# 计划");

        // 目录穿越:write 直接报错,create 单项失败,均未写出
        let out5 = reg
            .execute(
                "write",
                r#"{"target":"file","path":"../escape.txt","content":"x"}"#,
                ctx.clone(),
            )
            .await;
        assert!(out5.is_err());
        let out6 = reg
            .execute(
                "create",
                r#"{"items":[{"type":"file","path":"/abs.txt","content":"x"}]}"#,
                ctx.clone(),
            )
            .await
            .unwrap();
        let v6: Value = serde_json::from_str(&out6).unwrap();
        assert_eq!(v6["results"][0]["ok"], false);
        assert!(!std::env::temp_dir().join("escape.txt").exists());
        assert!(!character_file_root(&deps, "c")
            .join("..")
            .join("..")
            .join("escape.txt")
            .exists());
    }

    /// write 气泡 → replace 同步多次操作(append/prepend/replace/delete)
    #[tokio::test]
    async fn test_write_and_replace_bubble() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        let reg = ToolRegistry::new();
        register_write(&reg, deps.clone());
        register_replace(&reg, deps.clone());
        deps.characters.seed_default_character();
        let session = deps
            .sessions
            .create(crate::services::character_service::BUILTIN_SYSTEM_ID, None)
            .unwrap();
        let ctx = ToolContext {
            session_id: session.id.clone(),
            character_id: crate::services::character_service::BUILTIN_SYSTEM_ID.into(),
            agent_depth: 0,
        };
        for tool in ["write", "replace"] {
            reg.permissions()
                .authorize(tool, "session", &ctx.session_id)
                .unwrap();
        }

        // write 气泡
        let out = reg
            .execute(
                "write",
                r#"{"target":"bubble","role":"assistant","content":"你好"}"#,
                ctx.clone(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], true);
        let mid = v["message_id"].as_i64().unwrap();

        // 一次两个操作:append + prepend
        let r = reg
            .execute(
                "replace",
                &format!(r#"{{"operations":[{{"target":"bubble","id":{mid},"action":"append","content":"世界"}},{{"target":"bubble","id":{mid},"action":"prepend","content":"[开场]"}}]}}"#),
                ctx.clone(),
            )
            .await
            .unwrap();
        let rv: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(rv["results"].as_array().unwrap().len(), 2);
        let m = deps.sessions.get_message(&ctx.session_id, mid).unwrap();
        assert_eq!(m.content, "[开场]\n你好\n世界");

        // replace 子串
        let r2 = reg
            .execute("replace", &format!(r#"{{"operations":[{{"target":"bubble","id":{mid},"action":"replace","search":"你好","content":"嗨"}}]}}"#), ctx.clone())
            .await
            .unwrap();
        let rv2: Value = serde_json::from_str(&r2).unwrap();
        assert_eq!(rv2["results"][0]["ok"], true);
        assert_eq!(
            deps.sessions
                .get_message(&ctx.session_id, mid)
                .unwrap()
                .content,
            "[开场]\n嗨\n世界"
        );

        // 找不到子串 → 单项失败不 panic
        let r3 = reg
            .execute("replace", &format!(r#"{{"operations":[{{"target":"bubble","id":{mid},"action":"replace","search":"不存在","content":"x"}}]}}"#), ctx.clone())
            .await
            .unwrap();
        let rv3: Value = serde_json::from_str(&r3).unwrap();
        assert_eq!(rv3["results"][0]["ok"], false);

        // delete 整个气泡
        let r4 = reg
            .execute(
                "replace",
                &format!(
                    r#"{{"operations":[{{"target":"bubble","id":{mid},"action":"delete"}}]}}"#
                ),
                ctx.clone(),
            )
            .await
            .unwrap();
        let rv4: Value = serde_json::from_str(&r4).unwrap();
        assert_eq!(rv4["results"][0]["ok"], true);
        assert!(deps.sessions.get_message(&ctx.session_id, mid).is_none());
    }

    /// replace 文件多次操作 + 最终内容验证
    #[tokio::test]
    async fn test_replace_file_ops() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        let reg = ToolRegistry::new();
        register_write(&reg, deps.clone());
        register_replace(&reg, deps.clone());
        register_read(&reg, deps.clone());
        let ctx = ToolContext {
            session_id: "s".into(),
            character_id: "c".into(),
            agent_depth: 0,
        };
        for tool in ["write", "replace"] {
            reg.permissions()
                .authorize(tool, "session", &ctx.session_id)
                .unwrap();
        }

        reg.execute(
            "write",
            r#"{"target":"file","path":"log.txt","content":"第一行"}"#,
            ctx.clone(),
        )
        .await
        .unwrap();

        let r = reg
            .execute(
                "replace",
                r#"{"operations":[{"target":"file","path":"log.txt","action":"append","content":"第二行"},{"target":"file","path":"log.txt","action":"prepend","content":"标题"}]}"#,
                ctx.clone(),
            )
            .await
            .unwrap();
        let rv: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(rv["results"].as_array().unwrap().len(), 2);
        assert_eq!(rv["results"][0]["ok"], true);
        assert_eq!(rv["results"][1]["ok"], true);

        // 子串替换
        let r2 = reg
            .execute("replace", r#"{"operations":[{"target":"file","path":"log.txt","action":"replace","search":"第一行","content":"第一行(改)"}]}"#, ctx.clone())
            .await
            .unwrap();
        let rv2: Value = serde_json::from_str(&r2).unwrap();
        assert_eq!(rv2["results"][0]["ok"], true);

        // read 验证最终内容
        let out = reg
            .execute(
                "read",
                r#"{"queries":[{"type":"file","name":"log.txt"}]}"#,
                ctx.clone(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let content = v["results"][0]["content"].as_str().unwrap();
        assert!(content.contains("标题"));
        assert!(content.contains("第一行(改)"));
        assert!(content.contains("第二行"));

        // 不存在时默认失败,且不得静默创建。
        let missing = reg.execute(
            "replace",
            r#"{"operations":[{"target":"file","path":"missing.txt","action":"append","content":"x"}]}"#,
            ctx.clone(),
        ).await.unwrap();
        let missing: Value = serde_json::from_str(&missing).unwrap();
        assert_eq!(missing["results"][0]["ok"], false);
        assert!(!character_file_root(&deps, &ctx.character_id)
            .join("missing.txt")
            .exists());

        // 只有显式 create_if_missing 才允许创建。
        let created = reg.execute(
            "replace",
            r#"{"operations":[{"target":"file","path":"missing.txt","action":"append","content":"x","create_if_missing":true}]}"#,
            ctx.clone(),
        ).await.unwrap();
        let created: Value = serde_json::from_str(&created).unwrap();
        assert_eq!(created["results"][0]["ok"], true);
    }

    /// replace action=remove:真正删除文件;delete 仍是清空内容(文件保留)。
    /// 删除不存在的文件报错而非静默成功;bubble 不支持 remove。
    #[tokio::test]
    async fn test_replace_remove_file_ops() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        let reg = ToolRegistry::new();
        register_write(&reg, deps.clone());
        register_replace(&reg, deps.clone());
        register_read(&reg, deps.clone());
        let ctx = ToolContext {
            session_id: "s".into(),
            character_id: "c".into(),
            agent_depth: 0,
        };
        for tool in ["write", "replace"] {
            reg.permissions()
                .authorize(tool, "session", &ctx.session_id)
                .unwrap();
        }
        let root = character_file_root(&deps, &ctx.character_id);

        reg.execute(
            "write",
            r#"{"target":"file","path":"keep.txt","content":"内容"}"#,
            ctx.clone(),
        )
        .await
        .unwrap();

        // delete:清空内容,文件必须仍存在
        let cleared = reg
            .execute(
                "replace",
                r#"{"operations":[{"target":"file","path":"keep.txt","action":"delete"}]}"#,
                ctx.clone(),
            )
            .await
            .unwrap();
        let cleared: Value = serde_json::from_str(&cleared).unwrap();
        assert_eq!(cleared["results"][0]["ok"], true);
        assert!(
            root.join("keep.txt").exists(),
            "delete 只清空内容,不应删除文件"
        );
        assert_eq!(std::fs::read_to_string(root.join("keep.txt")).unwrap(), "");

        // remove:文件本身被删除
        let removed = reg
            .execute(
                "replace",
                r#"{"operations":[{"target":"file","path":"keep.txt","action":"remove"}]}"#,
                ctx.clone(),
            )
            .await
            .unwrap();
        let removed: Value = serde_json::from_str(&removed).unwrap();
        assert_eq!(removed["results"][0]["ok"], true);
        assert!(!root.join("keep.txt").exists(), "remove 应删除文件本身");

        // 重复 remove:明确报错,不静默成功
        let again = reg
            .execute(
                "replace",
                r#"{"operations":[{"target":"file","path":"keep.txt","action":"remove"}]}"#,
                ctx.clone(),
            )
            .await
            .unwrap();
        let again: Value = serde_json::from_str(&again).unwrap();
        assert_eq!(again["results"][0]["ok"], false);
        assert!(again["results"][0]["error"]
            .as_str()
            .unwrap()
            .contains("不存在"));

        // bubble 不支持 remove
        let bubble = reg
            .execute(
                "replace",
                r#"{"operations":[{"target":"bubble","id":1,"action":"remove"}]}"#,
                ctx.clone(),
            )
            .await
            .unwrap();
        let bubble: Value = serde_json::from_str(&bubble).unwrap();
        assert_eq!(bubble["results"][0]["ok"], false);
        assert!(bubble["results"][0]["error"]
            .as_str()
            .unwrap()
            .contains("仅支持 target=file"));
    }

    // ==================== 子智能体调度守卫(落地项 3) ====================

    /// 深度守卫:agent_depth 达到上限时拒绝派发,提示主智能体直接处理
    #[tokio::test]
    async fn agentgo_rejects_when_depth_exceeds_limit() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        // 默认 subagent_max_depth = 2;模拟深度 2 的嵌套上下文
        {
            let mut s = deps.settings.lock().unwrap_or_else(|e| e.into_inner());
            s.subagent_max_depth = 2;
        }
        let reg = ToolRegistry::new();
        register_agentgo(&reg, deps.clone());
        // agentgo 为敏感工具:先授权会话,避免权限层先拒导致守卫逻辑未走到;
        // 子任务表 session_id 外键指向 sessions,先造真实会话行
        deps.characters.seed_default_character();
        let session = deps
            .sessions
            .create(crate::services::character_service::BUILTIN_SYSTEM_ID, None)
            .unwrap();
        reg.permissions()
            .authorize("agentgo", "session", &session.id)
            .unwrap();
        let ctx = ToolContext {
            session_id: session.id.clone(),
            character_id: "c".into(),
            agent_depth: 2,
        };
        let err = reg
            .execute(
                "agentgo",
                r#"{"tasks":[{"name":"t","instruction":"i"}]}"#,
                ctx.clone(),
            )
            .await
            .unwrap_err();
        assert!(
            err.contains("子智能体嵌套超过 2 层"),
            "深度守卫应拒绝: {err}"
        );
        // 深度 1(第二层)仍可派发
        let ctx1 = ToolContext {
            agent_depth: 1,
            ..ctx
        };
        let ok = reg
            .execute(
                "agentgo",
                r#"{"tasks":[{"name":"t","instruction":"i"}]}"#,
                ctx1,
            )
            .await;
        assert!(ok.is_ok(), "深度 1 不应被拒: {:?}", ok.err());
    }

    /// 并发守卫:会话内 running 子任务数加本次派发超过上限时拒绝,不创建新任务
    #[tokio::test]
    async fn agentgo_rejects_when_concurrency_exhausted() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        {
            let mut s = deps.settings.lock().unwrap_or_else(|e| e.into_inner());
            s.subagent_max_concurrency = 2;
        }
        // 预置 2 个 running 子任务占满并发额度(先造角色与会话行满足外键)
        deps.characters.seed_default_character();
        let session = deps
            .sessions
            .create(crate::services::character_service::BUILTIN_SYSTEM_ID, None)
            .unwrap();
        let sid = session.id.as_str();
        let t1 = deps.subtasks.create(sid, "c", "占位1", "i").unwrap();
        let t2 = deps.subtasks.create(sid, "c", "占位2", "i").unwrap();
        deps.subtasks.set_running(&t1.id);
        deps.subtasks.set_running(&t2.id);

        let reg = ToolRegistry::new();
        register_agentgo(&reg, deps.clone());
        // 敏感工具先授权会话,守卫断言才能命中并发上限逻辑
        reg.permissions()
            .authorize("agentgo", "session", sid)
            .unwrap();
        let ctx = ToolContext {
            session_id: sid.to_string(),
            character_id: "c".into(),
            agent_depth: 0,
        };
        let err = reg
            .execute(
                "agentgo",
                r#"{"tasks":[{"name":"t","instruction":"i"}]}"#,
                ctx,
            )
            .await
            .unwrap_err();
        assert!(err.contains("子智能体并发已满 2"), "并发守卫应拒绝: {err}");
        // 守卫拒绝时不创建新任务
        assert_eq!(deps.subtasks.list_by_session(sid).len(), 2);
    }

    /// 结果截断:超 subagent_result_max_chars 的子任务结果保留前 N 字符并带原长尾注
    #[tokio::test]
    async fn subtask_result_truncates_with_marker() {
        use crate::tools::agent_tools_agent::truncate_subtask_result_for_test;
        let deps = Arc::new(ToolDeps::dummy_for_test());
        {
            let mut s = deps.settings.lock().unwrap_or_else(|e| e.into_inner());
            s.subagent_result_max_chars = 10;
        }
        let long = "字".repeat(25);
        let out = truncate_subtask_result_for_test(&deps, &long);
        assert!(
            out.contains("[子智能体结果已截断,原长 25 字符]"),
            "应带原长尾注: {out}"
        );
        // 保留前 10 字符,截断在字符边界(首行恰为 max_chars 个字符)
        assert!(out.starts_with(&"字".repeat(10)), "应保留前 10 字符: {out}");
        assert_eq!(
            out.lines().next().map(|l| l.chars().count()),
            Some(10),
            "首行应恰为 max_chars 个字符"
        );
        // 未超长:原样返回,不带尾注
        let short = "短结果";
        assert_eq!(truncate_subtask_result_for_test(&deps, short), short);
    }

    // ==================== 任务编排契约加固(审计 A/B/C/E/F) ====================

    /// 造一个满足 agent_subtasks FK 的真实会话,并授权敏感工具
    fn mk_authorized_session(
        deps: &Arc<ToolDeps>,
        reg: &ToolRegistry,
        tools: &[&str],
    ) -> ToolContext {
        deps.characters.seed_default_character();
        let session = deps
            .sessions
            .create(crate::services::character_service::BUILTIN_SYSTEM_ID, None)
            .unwrap();
        for t in tools {
            reg.permissions()
                .authorize(t, "session", &session.id)
                .unwrap();
        }
        ToolContext {
            session_id: session.id,
            character_id: crate::services::character_service::BUILTIN_SYSTEM_ID.into(),
            agent_depth: 0,
        }
    }

    /// 审计 A1/A2:混合批(1 合法 + 1 空 instruction)→ Ok,合法项被创建,非法项进 rejected
    #[tokio::test]
    async fn agentgo_mixed_batch_dispatches_valid_and_rejects_invalid() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        let reg = ToolRegistry::new();
        register_agentgo(&reg, deps.clone());
        let ctx = mk_authorized_session(&deps, &reg, &["agentgo"]);

        let out = reg
            .execute(
                "agentgo",
                r#"{"tasks":[{"name":"合法","instruction":"做点什么"},{"name":"空指令","instruction":""}]}"#,
                ctx.clone(),
            )
            .await
            .expect("混合批应整体成功(部分派发)");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["tasks"].as_array().unwrap().len(), 1, "合法项应派出: {v}");
        assert_eq!(
            v["rejected"].as_array().unwrap().len(),
            1,
            "非法项应被拒: {v}"
        );
        assert_eq!(v["rejected"][0]["index"], 1, "index 应为入参 0-based 下标");
        assert!(v["rejected"][0]["reject_reason"]
            .as_str()
            .unwrap()
            .contains("instruction 为空"));
        // 合法项确实被创建(而非孤儿:返回里也要能查到)
        let created = deps.subtasks.list_by_session(&ctx.session_id);
        assert_eq!(created.len(), 1, "仅合法项落库: {created:?}");
        assert_eq!(created[0].name, "合法");
        assert_eq!(created[0].instruction, "做点什么");
    }

    /// 审计 A2:纯空白 instruction(" ")按 trim 语义被拒,不再当合法任务
    #[tokio::test]
    async fn agentgo_blank_whitespace_instruction_is_rejected() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        let reg = ToolRegistry::new();
        register_agentgo(&reg, deps.clone());
        let ctx = mk_authorized_session(&deps, &reg, &["agentgo"]);

        let err = reg
            .execute(
                "agentgo",
                r#"{"tasks":[{"name":"空白","instruction":"   "}]}"#,
                ctx.clone(),
            )
            .await
            .expect_err("纯空白项应被拒且无合法项 → 整批 Err");
        assert!(err.contains("未派出任何子任务"), "错误应说明全废: {err}");
        assert!(err.contains("instruction 为空"), "错误应含逐项原因: {err}");
        assert!(
            deps.subtasks.list_by_session(&ctx.session_id).is_empty(),
            "空白项不得创建"
        );
    }

    /// 审计 A:全废批 → Err,错误文案含逐项原因(便于模型自纠)
    #[tokio::test]
    async fn agentgo_all_invalid_returns_error_with_per_item_reasons() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        let reg = ToolRegistry::new();
        register_agentgo(&reg, deps.clone());
        let ctx = mk_authorized_session(&deps, &reg, &["agentgo"]);

        let err = reg
            .execute(
                "agentgo",
                r#"{"tasks":[{"name":"","instruction":""},{"name":"无指令","instruction":" "}]}"#,
                ctx.clone(),
            )
            .await
            .expect_err("全废批应 Err");
        assert!(err.contains("#0"), "应带 0-based 下标 #0: {err}");
        assert!(err.contains("#1"), "应带 0-based 下标 #1: {err}");
        assert!(err.contains("name 为空"), "应含 name 原因: {err}");
        assert!(
            err.contains("instruction 为空"),
            "应含 instruction 原因: {err}"
        );
        assert!(
            deps.subtasks.list_by_session(&ctx.session_id).is_empty(),
            "全废批不得创建任何子任务"
        );
    }

    /// 审计 C:read(type=subtask) 三键命中(id / 精确 name / 子串)+ 无命中和多命中语义
    #[tokio::test]
    async fn read_subtask_matches_by_id_name_and_substring() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        deps.characters.seed_default_character();
        let session = deps
            .sessions
            .create(crate::services::character_service::BUILTIN_SYSTEM_ID, None)
            .unwrap();
        let sid = session.id.as_str();
        let a = deps
            .subtasks
            .create(sid, "c", "alpha-task", "指令A")
            .unwrap();
        let b = deps
            .subtasks
            .create(sid, "c", "beta-task", "指令B")
            .unwrap();
        let reg = ToolRegistry::new();
        register_read(&reg, deps.clone());
        let ctx = ToolContext {
            session_id: session.id.clone(),
            character_id: "c".into(),
            agent_depth: 0,
        };

        let call = |name: String| {
            let reg = &reg;
            let ctx = ctx.clone();
            async move {
                let args = serde_json::json!({ "queries": [{ "type": "subtask", "name": name }] });
                let raw = reg.execute("read", &args.to_string(), ctx).await.unwrap();
                let v: Value = serde_json::from_str(&raw).unwrap();
                v["results"][0]["content"].as_str().unwrap().to_string()
            }
        };

        // 1) 按 id:matched_by=id,且同时含 id 与 name(双向别名)
        let by_id = call(a.id.clone()).await;
        let v: Value = serde_json::from_str(&by_id).expect("按 id 命中应返回 JSON");
        assert_eq!(v["matched_by"], "id");
        assert_eq!(v["id"].as_str(), Some(a.id.as_str()));
        assert_eq!(v["name"], "alpha-task");

        // 2) 按精确 name:matched_by=name
        let by_name = call("beta-task".into()).await;
        let v: Value = serde_json::from_str(&by_name).unwrap();
        assert_eq!(v["matched_by"], "name");
        assert_eq!(v["id"].as_str(), Some(b.id.as_str()));

        // 3) 按子串(大小写不敏感):matched_by=name_like
        let by_like = call("ALPHA".into()).await;
        let v: Value = serde_json::from_str(&by_like).unwrap();
        assert_eq!(v["matched_by"], "name_like");
        assert_eq!(v["id"].as_str(), Some(a.id.as_str()));

        // 4) 无命中:报错列出可用候选 id 与 name
        let miss = call("不存在的东西".into()).await;
        assert!(miss.contains("未命中"), "应报无命中: {miss}");
        assert!(
            miss.contains(&a.id) && miss.contains("beta-task"),
            "应列候选: {miss}"
        );

        // 5) 多命中:返回候选清单,不随便挑一条
        let many = call("task".into()).await;
        let v: Value = serde_json::from_str(&many).expect("多命中应返回候选清单");
        assert_eq!(v["matched_by"], "ambiguous");
        assert_eq!(
            v["candidates"].as_array().unwrap().len(),
            2,
            "应列 2 条候选: {v}"
        );
    }

    /// 审计 E:todo 在任务模式派生 session 下能看到同任务全部子任务,且显式标注 tool_calls 不可用
    #[tokio::test]
    async fn todo_in_task_mode_sees_sibling_subtasks_and_flags_tool_calls_unavailable() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        // 子任务挂在 task:t1(走内存覆盖层);todo 从派生 session task:t1:sub:x 查询
        let _s1 = deps
            .subtasks
            .create("task:t1", "c", "兄弟一", "指令一")
            .unwrap();
        let _s2 = deps
            .subtasks
            .create("task:t1:main:0", "c", "兄弟二", "指令二")
            .unwrap();
        let _other = deps
            .subtasks
            .create("task:t2", "c", "别任务", "指令")
            .unwrap();
        let reg = ToolRegistry::new();
        register_todo(&reg, deps.clone());
        let ctx = ToolContext {
            session_id: "task:t1:sub:x".into(),
            character_id: "c".into(),
            agent_depth: 0,
        };
        let raw = reg.execute("todo", "{}", ctx).await.unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        let subs = v["subtasks"].as_array().unwrap();
        let names: Vec<&str> = subs.iter().filter_map(|s| s["name"].as_str()).collect();
        assert!(names.contains(&"兄弟一"), "应见 task:t1 下子任务: {v}");
        assert!(
            names.contains(&"兄弟二"),
            "应见 :main: 派生子任务的兄弟: {v}"
        );
        assert!(!names.contains(&"别任务"), "不得混入其他任务: {v}");
        // tool_calls 恒空是任务模式虚拟 session 的结构性限制,必须显式标注
        assert_eq!(v["tool_calls_available"], false, "应标注不可用: {v}");
        assert!(
            v["tool_calls_note"].as_str().unwrap().contains("不可用"),
            "应带 note: {v}"
        );
    }

    /// 审计 F:agentend 对活跃任务 interrupted=true;对已结束任务再调 existed=true 但 interrupted=false
    #[tokio::test]
    async fn agentend_reports_interrupted_only_for_active_task() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        let reg = ToolRegistry::new();
        register_agentend(&reg, deps.clone());
        let ctx = mk_authorized_session(&deps, &reg, &["agentend"]);
        let t = deps
            .subtasks
            .create(&ctx.session_id, "c", "活任务", "指令")
            .unwrap();

        let raw = reg
            .execute(
                "agentend",
                &format!(r#"{{"task_ids":["{}"]}}"#, t.id),
                ctx.clone(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["results"][0]["existed"], true);
        assert_eq!(v["results"][0]["prior_status"], "pending");
        assert_eq!(
            v["results"][0]["interrupted"], true,
            "活跃任务应记为本次中断: {v}"
        );

        // 再调一次:已 ended,本次不是真实中断
        let raw2 = reg
            .execute(
                "agentend",
                &format!(r#"{{"task_ids":["{}"]}}"#, t.id),
                ctx.clone(),
            )
            .await
            .unwrap();
        let v2: Value = serde_json::from_str(&raw2).unwrap();
        assert_eq!(v2["results"][0]["existed"], true);
        assert_eq!(v2["results"][0]["prior_status"], "ended");
        assert_eq!(
            v2["results"][0]["interrupted"], false,
            "已 ended 任务不得再报本次中断: {v2}"
        );
    }

    /// 审计 B(纯函数):截断即失败、空内容带 finish_reason、自然完成仍 done
    #[test]
    fn subtask_verdict_classifies_truncation_as_failure() {
        use crate::tools::agent_tools_agent::classify_subtask_result_for_test as classify;
        let (kind, err) = classify("半截正文", Some("length"), 512);
        assert_eq!(kind, "truncated", "length 截断应判失败");
        assert!(
            err.contains("截断") && err.contains("length") && err.contains("512"),
            "错误文案: {err}"
        );
        let (kind, err) = classify("", Some("length"), 512);
        assert_eq!(kind, "empty");
        assert!(
            err.contains("finish_reason=length"),
            "空内容应带 finish_reason: {err}"
        );
        let (kind, _) = classify("完整正文", Some("stop"), 512);
        assert_eq!(kind, "done", "自然完成应 done");
        let (kind, _) = classify("纯生成正文", None, 512);
        assert_eq!(kind, "done", "无 finish_reason(纯生成回退)维持原行为");
    }

    /// 审计 B(落库):set_failed 状态 error 且保留截断正文
    #[tokio::test]
    async fn set_failed_keeps_truncated_body_and_marks_error() {
        let deps = Arc::new(ToolDeps::dummy_for_test());
        let t = deps.subtasks.create("task:t1", "c", "子", "指令").unwrap();
        let rec = deps
            .subtasks
            .set_failed(
                &t.id,
                "半截正文",
                "子任务输出被截断(finish_reason=length,输出上限 512 token)",
            )
            .expect("set_failed 应命中记录");
        assert_eq!(rec.status, "error");
        assert_eq!(rec.result, "半截正文", "截断正文必须保留");
        assert!(rec.error.contains("截断"), "error 应含定性: {}", rec.error);
    }
}
