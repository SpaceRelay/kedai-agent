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
use super::agent_tools_agent::{register_agentend, register_agentgo, register_sleep, register_todo};
use super::agent_tools_read::register_read;
use super::agent_tools_search::register_search;
#[cfg(test)]
use super::agent_tools_shared::random_u64;
use super::agent_tools_shared::register_role;
use super::agent_tools_write::{register_create, register_replace, register_write};

// 保持原公共导出路径:crate::tools::agent_tools::character_file_root(shared 定义)
pub use super::agent_tools_shared::character_file_root;
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
}

impl ToolDeps {
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
}
