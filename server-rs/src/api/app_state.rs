// 应用共享状态(axum State):汇聚 DB、服务、连接器、引擎、工具
use crate::agents::engine::AgentEngine;
use crate::config::AppConfig;
use crate::mcp::McpManager;
use crate::models::db::Db;
use crate::services::agent_flow_service::AgentFlowService;
use crate::services::agent_session_service::AgentSessionService;
use crate::services::agent_subtask_service::AgentSubtaskService;
use crate::services::audio_service::AudioService;
use crate::services::character_service::CharacterService;
use crate::services::contract_changelog_service::ContractChangelogService;
use crate::services::kaleido_state_service::KaleidoStateService;
use crate::services::memory_service::MemoryService;
use crate::services::prompt_inject_service::PromptInjectService;
use crate::services::quick_reply_service::QuickReplyService;
use crate::services::runtime_prompt_service::RuntimePromptService;
use crate::services::session_service::SessionService;
use crate::services::settings_service::RuntimeSettings;
use crate::services::skill_service::SkillService;
use crate::services::task_service::TaskService;
use crate::services::token_service::TokenService;
use crate::services::undo_service::UndoService;
use crate::services::user_script_service::UserScriptService;
use crate::services::world_book_service::WorldBookService;
use crate::tools::agent_tools::ToolDeps;
use crate::tools::permissions::ToolPermissionManager;
use crate::tools::registry::ToolRegistry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{watch, Mutex as AsyncMutex, RwLock};

pub struct AppState {
    pub config: AppConfig,
    pub engine: Arc<AgentEngine>,
    pub characters: Arc<CharacterService>,
    /// 契约变更历史(阶段 C):append-only 审计 + 回滚源
    pub contract_changelog: Arc<ContractChangelogService>,
    /// 契约注册表(引擎与多步工具共享同一实例;P7 起 HTTP 出口共用)
    pub contract_registry: Arc<crate::contracts::ContractRegistry>,
    /// 契约运行态(P5):KaleidoState 会话级事实源 + 领域 changelog
    pub kaleido_state: Arc<KaleidoStateService>,
    pub sessions: Arc<SessionService>,
    pub agent_sessions: Arc<AgentSessionService>,
    pub world_books: Arc<WorldBookService>,
    pub tool_registry: Arc<ToolRegistry>,
    pub token_service: Arc<Mutex<TokenService>>,
    /// SQLite 句柄(Token 累计统计)
    pub db: Arc<Db>,
    /// 技能库(提示词技能)
    pub skills: Arc<SkillService>,
    /// 子智能体任务(agentgo/agentend)
    pub agent_subtasks: Arc<AgentSubtaskService>,
    /// 当前生效模型(与 engine 内保持一致)
    pub model: Arc<Mutex<String>>,
    /// 运行期设置(API 连接 + 生成参数),前端可编辑并持久化到 data/settings.json
    pub settings: Arc<Mutex<RuntimeSettings>>,
    /// 设置更新事务锁:串行化读取、校验、持久化和内存替换，避免部分更新互相覆盖。
    pub settings_update: Arc<AsyncMutex<()>>,
    /// 提示词注入配置(简单模式 + 楼层系统),持久化到 data/prompt_floors.json
    pub prompt_inject: Arc<Mutex<PromptInjectService>>,
    /// 音频播放器状态(bgm/ambient 双通道),持久化到 data/audio.json
    pub audio: Arc<Mutex<AudioService>>,
    /// 快速回复(Quick Replies):getqr 的渲染数据源,SQLite 持久化
    pub quick_replies: Arc<QuickReplyService>,
    /// 用户脚本(ScriptTree,阶段三):global 存 SQLite,character 存角色卡 extensions.tavern_helper
    pub user_scripts: Arc<UserScriptService>,
    /// 跨会话记忆蒸馏(落地项 2):按角色维度共享的长期记忆条目
    pub memory: Arc<MemoryService>,
    /// 回退快照(批次 6.1「undo」):写工具逆操作快照的暂存/列表/恢复
    pub undo: Arc<UndoService>,
    /// MCP stdio 客户端(批次 6.2,L3 隔离;默认关):mcp_enabled=true 时由
    /// run_server 在 AppState::new 之后、serve 之前调 start_mcp 装配;
    /// 未启用时保持空管理器(零进程、零注册)。
    pub mcp: Arc<McpManager>,
    /// 任务模式(task 工作台):任务主表 + 子任务 + 后台执行引擎
    pub tasks: Arc<TaskService>,
    /// slash 命令注册表(阶段四 4a):脚本 triggerSlash 与 GET /api/slash/commands 共用
    pub slash: Arc<crate::slash::SlashRegistry>,
    /// 运行时主 Agent 提示词文件服务；Engine 与 settings API 共用同一实例和 DATA_DIR 路径。
    pub runtime_prompt: Arc<RuntimePromptService>,
    /// 自定义 Agent 执行流程(custom 模式),持久化到 data/agent_flows.json
    pub flow: Arc<Mutex<AgentFlowService>>,
    /// chat/send 在进入异步引擎前的原子占位及取消标志,stop 可取消消息写入/启动前请求。
    pub pending_runs: Arc<Mutex<HashMap<String, watch::Sender<bool>>>>,
    /// bootstrap 令牌桶限频(每对端 IP 一窗口计数),防本地恶意网页/脚本高频枚举 token。
    pub bootstrap_limiter: Arc<Mutex<HashMap<String, (std::time::Instant, u32)>>>,
}

impl AppState {
    pub fn new(config: AppConfig) -> Result<Arc<AppState>, String> {
        let db = Arc::new(Db::open(
            &config.data_dir.join("kedai.db"),
            &config.data_dir,
        )?);

        let characters = Arc::new(CharacterService::new(db.clone(), config.data_dir.clone()));
        let contract_changelog = Arc::new(ContractChangelogService::new(db.clone()));
        let kaleido_state = Arc::new(KaleidoStateService::new(db.clone()));
        let sessions = Arc::new(SessionService::new(db.clone()));
        let agent_sessions = Arc::new(AgentSessionService::new(db.clone()));
        let world_books = Arc::new(WorldBookService::new(db.clone()));
        let skills = Arc::new(SkillService::new(db.clone()));
        // Skill 库加载情况(供启动器/控制台确认;与下方工具插件日志保持一致)
        {
            let all = skills.list(false);
            let enabled = all.iter().filter(|s| s.enabled).count();
            if !all.is_empty() {
                eprintln!("[Skill] 已加载 {} 个(启用 {} 个)", all.len(), enabled);
            } else {
                eprintln!("[Skill] 技能库为空,可通过 POST /api/skills 导入");
            }
        }
        let agent_subtasks = Arc::new(AgentSubtaskService::new(db.clone()));
        // 跨会话记忆蒸馏(落地项 2):记忆槽注入 / memory 工具写入 / 蒸馏 API 共用
        let memory = Arc::new(MemoryService::new(db.clone()));
        // 快速回复(getqr 渲染数据源)
        let quick_replies = Arc::new(QuickReplyService::new(db.clone()));
        // 用户脚本(ScriptTree,阶段三)
        let user_scripts = Arc::new(UserScriptService::new(db.clone()));
        // slash 命令注册表(阶段四 4a):注册内置命令;engine 与 API 共享同一实例
        let slash = crate::slash::SlashRegistry::new();

        // 注入默认「系统助手」角色(无提示词)
        characters.seed_default_character();

        // 运行期设置:优先 data/settings.json,否则回退环境配置。
        // 构造期尚无并发,先持有值再入 Mutex,避免「刚创建即加锁读出」的往返
        let loaded_settings = RuntimeSettings::load(&config.data_dir, &config);

        // 构建连接器:优先使用运行期设置(settings.json)中的 Base URL / Key / 模型,
        // 否则用户保存的 API 配置在重启后会丢失,界面模型显示回退为 .env 默认值。
        let (base_url, api_key, initial_model) = (
            loaded_settings.openai_base_url.clone(),
            loaded_settings.openai_api_key.clone(),
            loaded_settings.model.clone(),
        );
        let settings = Arc::new(Mutex::new(loaded_settings));
        // 记忆服务接入运行期设置(淘汰容量/字符预算阈值来源;OnceLock 幂等注入)
        memory.attach_settings(settings.clone());
        // 关键:已保存非空 API 配置时,即使环境变量 CONNECTOR=mock(演示模式)也自动
        // 使用 openai-compatible,否则用户「退出演示模式」后一旦重启又回到 mock,
        // 设置里填的 API 配置永远不生效(与 PUT /settings 的自动切换逻辑保持一致)。
        let connector_type = if !base_url.trim().is_empty() && !api_key.trim().is_empty() {
            "openai-compatible"
        } else {
            &config.connector
        };
        let connector =
            crate::connectors::build_connector(connector_type, &base_url, &api_key, &initial_model);
        let connector = Arc::new(RwLock::new(connector));

        // 工具注册(calculator / memory / agent 强化工具集)
        let tool_registry = Arc::new(ToolRegistry::with_permissions(ToolPermissionManager::load(
            config.data_dir.join("tool_permissions.json"),
        )));
        let deps = Arc::new(ToolDeps {
            sessions: sessions.clone(),
            characters: characters.clone(),
            world_books: world_books.clone(),
            agent_sessions: agent_sessions.clone(),
            subtasks: agent_subtasks.clone(),
            skills: skills.clone(),
            settings: settings.clone(),
            connector: connector.clone(),
            data_dir: config.data_dir.clone(),
            memory: memory.clone(),
            // engine/tasks 尚不存在(构造顺序在其后),由下方 OnceLock 注入(批次 4.3b)
            engine: std::sync::OnceLock::new(),
            tasks: std::sync::OnceLock::new(),
        });
        crate::tools::register_builtin_tools(&tool_registry, deps.clone());

        // 启动清理:移除指向已不存在会话的孤儿授权(会话可能在历史版本中被删除而
        // 未清理授权;失败仅告警,不影响启动)。
        {
            let existing: std::collections::HashSet<String> = sessions
                .list_all()
                .iter()
                .map(|s| s.id.clone())
                .collect();
            match tool_registry
                .permissions()
                .prune_orphan_session_grants(&existing)
            {
                Ok(0) => {}
                Ok(n) => tracing::info!(removed = n, "清理孤儿会话授权"),
                Err(e) => tracing::warn!(error = e, "孤儿会话授权清理失败"),
            }
        }

        // 回退快照(批次 6.1「undo」):与 SessionService 共用同一 Arc<Db> 句柄
        // (ToolDeps 上没有 db 连接池,直接同源构造是最小侵入路径);注入注册表供
        // run_tool 两段式收口(执行前快照/成功 commit/失败 discard),API 经 state.undo 访问。
        let undo = Arc::new(UndoService::new(
            db.clone(),
            sessions.clone(),
            memory.clone(),
            settings.clone(),
            config.data_dir.clone(),
        ));
        tool_registry.set_undo(undo.clone());

        // 契约注册表:引擎与多步工具共享同一实例(缓存一致;改卡后写路径 invalidate)
        let contract_registry = Arc::new(crate::contracts::ContractRegistry::new(
            characters.clone(),
            world_books.clone(),
        ));
        // P4:多步模式工具(get_state/apply_patch)。契约经共享 registry(与引擎同缓存);
        // 不进入 agent 正文模式的默认下发列表(chat.rs 过滤)。P6:apply_patch 契约生效时
        // 经 kaleido 服务留痕(changelog + meta.pending 维护)。
        crate::tools::multistep::register_multistep_tools(
            &tool_registry,
            sessions.clone(),
            contract_registry.clone(),
            kaleido_state.clone(),
        );

        // 工具插件加载:data/plugins/tools/*.json(白名单脚本)
        {
            let loader = crate::plugins::ToolPluginLoader::new(
                config.data_dir.join("plugins").join("tools"),
            );
            let (count, errors) = loader.load_all(&tool_registry);
            if !errors.is_empty() {
                eprintln!("[工具插件] 加载部分失败: {}", errors.join("; "));
            }
            if count > 0 {
                eprintln!("[工具插件] 已加载 {count} 个");
            }
        }

        // 提示词注入配置(简单模式 + 楼层系统);先于 engine 创建以便注入引擎
        let prompt_inject = Arc::new(Mutex::new(PromptInjectService::new(
            config.data_dir.clone(),
        )));

        // 音频播放器状态(bgm/ambient 双通道);仅 URL 播放,播放列表/设置持久化到 data/audio.json
        let audio = Arc::new(Mutex::new(AudioService::new(config.data_dir.clone())));

        // 运行时主 Agent 提示词固定落在 DATA_DIR。旧版项目根文件仅作一次性兼容迁移来源；
        // 运行路径不依赖编译期 CARGO_MANIFEST_DIR。
        let legacy_runtime_prompt = std::env::var_os("KEDAI_LEGACY_RUNTIME_PROMPT")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.join("AGENTS_RUNTIME.md"))
            });
        let runtime_prompt = Arc::new(match std::env::var_os("KEDAI_RUNTIME_PROMPT_DIR") {
            // 显式指定目录:只从该目录读,不回退内置默认(测试隔离与自定义部署)
            Some(dir) => RuntimePromptService::with_dir(
                std::path::PathBuf::from(dir),
                legacy_runtime_prompt,
            ),
            // 默认:DATA_DIR;文件与旧文件都缺失时回退内置默认提示词
            None => RuntimePromptService::new(config.data_dir.clone(), legacy_runtime_prompt),
        });

        // 自定义 Agent 执行流程(custom 模式);保存时按当前已注册工具校验白名单
        let registered_tools = tool_registry
            .list_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        let flow = Arc::new(Mutex::new(AgentFlowService::new(
            config.data_dir.clone(),
            registered_tools,
        )));

        // 聊天引擎:先于 TaskService 构造(engine 不依赖 tasks,无循环;
        // 批次 4.2 起 TaskService 注入 Arc<AgentEngine> 供六模式执行器复用工具循环)
        let engine = Arc::new(AgentEngine::new(
            connector.clone(),
            characters.clone(),
            sessions.clone(),
            agent_sessions.clone(),
            world_books.clone(),
            tool_registry.clone(),
            settings.clone(),
            prompt_inject.clone(),
            quick_replies.clone(),
            runtime_prompt.clone(),
            db.clone(),
            initial_model.clone(),
            user_scripts.clone(),
            slash.clone(),
            contract_registry.clone(),
            kaleido_state.clone(),
            memory.clone(),
            skills.clone(),
        ));
        let engine_model = engine.model();
        // 批次 4.3b:引擎弱引用注入 ToolDeps(子 agent 工具化经 run_tool_loop 跑
        // 白名单工具循环);OnceLock 仅此处 set 一次,set 失败说明重复装配(不应发生)
        let _ = deps.engine.set(Arc::downgrade(&engine));

        // 任务模式(task 工作台):复用 connector/characters/db + 聊天引擎(六模式)
        let tasks = Arc::new(TaskService::new(
            db.clone(),
            characters.clone(),
            connector.clone(),
            settings.clone(),
            world_books.clone(),
            prompt_inject.clone(),
            engine.clone(),
            flow.clone(),
            agent_subtasks.clone(),
        ));
        // 任务服务弱引用注入 ToolDeps(任务模式子 agent 的事件桥/调用追踪/usage 落库)
        let _ = deps.tasks.set(Arc::downgrade(&tasks));

        let token_service = Arc::new(Mutex::new(TokenService::new()));

        Ok(Arc::new(AppState {
            config,
            contract_registry,
            engine,
            characters,
            contract_changelog,
            kaleido_state,
            sessions,
            agent_sessions,
            world_books,
            tool_registry,
            token_service,
            db,
            skills,
            agent_subtasks,
            model: Arc::new(Mutex::new(engine_model)),
            settings,
            settings_update: Arc::new(AsyncMutex::new(())),
            prompt_inject,
            audio,
            quick_replies,
            user_scripts,
            memory,
            undo,
            // MCP 默认空管理器;AppState::new 是同步函数,进程装配(异步握手)
            // 由 run_server 在 serve 之前调 start_mcp 完成(批次 6.2)
            mcp: Arc::new(McpManager::empty()),
            slash,
            runtime_prompt,
            flow,
            tasks,
            pending_runs: Arc::new(Mutex::new(HashMap::new())),
            bootstrap_limiter: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    /// 运行期设置快照:lock 后立即 clone 返回,锁中毒时 into_inner 恢复取值。
    /// 快照语义:不留锁跨 await —— 调用方拿到独立副本,锁在本函数内即释放,
    /// async 读路径一律走本方法而不是散点 .lock();写路径仍由 settings.rs 的
    /// settings_update 事务锁串行化后直接替换内存值。
    pub fn settings_snapshot(&self) -> RuntimeSettings {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 失效契约缓存(角色卡写路径调用:契约来源之一变化)。
    pub fn invalidate_contracts_for_character(&self, character_id: &str) {
        self.engine.invalidate_character_contract(character_id);
    }

    /// 清空全部契约缓存(世界书写路径调用:全局世界书可能影响任意角色的契约来源)。
    pub fn clear_all_contracts(&self) {
        self.engine.contract_registry.clear();
    }

    /// 在阻塞线程池执行只读 DB 闭包(2026-08 DB 并发改造:把同步 rusqlite 调用
    /// 移出 tokio worker,避免阻塞事件循环)。闭包收到只读池连接;
    /// 闭包内亦可调用持 Db 的 services 只读方法(它们各自从池取连接,互不冲突)。
    pub async fn db_read<T, F>(&self, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> Result<T, String> + Send + 'static,
    {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.read()?;
            f(&conn)
        })
        .await
        .map_err(|e| format!("只读任务执行失败: {e}"))?
    }

    /// 在阻塞线程池执行写 DB 闭包。注意:闭包执行期间持有唯一写连接,
    /// 不得再调用会重新获取写锁的 services 写方法(Mutex 非重入,会自死锁);
    /// services 写方法请用 db_call(连接由服务内部按需获取)。
    pub async fn db_write<T, F>(&self, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> Result<T, String> + Send + 'static,
    {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.write();
            f(&conn)
        })
        .await
        .map_err(|e| format!("写库任务执行失败: {e}"))?
    }

    /// 装配 MCP stdio 服务器(批次 6.2,L3 隔离):mcp_enabled=false 时完全跳过
    /// (零进程、零注册)。读扁平权威设置,不按模式合并——MCP 是进程级全局能力,
    /// 不随请求模式切换;v1 仅启动时装配,PUT 改 mcp_* 后重启生效。
    /// 单台失败仅记 warn 并禁用该台,不 panic、不阻断启动;由 run_server 在 serve 之前调用。
    pub async fn start_mcp(&self) {
        // 快照语义:不留锁跨 await(读开关与服务器清单用同一快照,避免锁守卫进入异步装配)
        let snapshot = self.settings_snapshot();
        if !snapshot.mcp_enabled {
            return;
        }
        let n_before = self.tool_registry.list_definitions().len();
        self.mcp.start(&snapshot, &self.tool_registry).await;
        let servers = self.mcp.server_count();
        if servers > 0 {
            let added = self.tool_registry.list_definitions().len() - n_before;
            eprintln!("[MCP] 已装配 {servers} 台服务器,注册 {added} 个工具(mcp_ 前缀)");
        }
    }

    /// 在阻塞线程池执行任意持 Db 的 services 同步调用(读或写不限;
    /// 连接由服务方法内部按需获取,故无 db_write 的重入死锁约束)。
    pub async fn db_call<T, F>(&self, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|e| format!("DB 任务执行失败: {e}"))
    }
}
