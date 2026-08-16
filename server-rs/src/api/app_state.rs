// 应用共享状态(axum State):汇聚 DB、服务、连接器、引擎、工具
use crate::agents::engine::AgentEngine;
use crate::config::AppConfig;
use crate::models::db::Db;
use crate::services::agent_flow_service::AgentFlowService;
use crate::services::agent_session_service::AgentSessionService;
use crate::services::agent_subtask_service::AgentSubtaskService;
use crate::services::audio_service::AudioService;
use crate::services::character_service::CharacterService;
use crate::services::contract_changelog_service::ContractChangelogService;
use crate::services::kaleido_state_service::KaleidoStateService;
use crate::services::prompt_inject_service::PromptInjectService;
use crate::services::quick_reply_service::QuickReplyService;
use crate::services::runtime_prompt_service::RuntimePromptService;
use crate::services::session_service::SessionService;
use crate::services::settings_service::RuntimeSettings;
use crate::services::skill_service::SkillService;
use crate::services::token_service::TokenService;
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
        // 快速回复(getqr 渲染数据源)
        let quick_replies = Arc::new(QuickReplyService::new(db.clone()));
        // 用户脚本(ScriptTree,阶段三)
        let user_scripts = Arc::new(UserScriptService::new(db.clone()));
        // slash 命令注册表(阶段四 4a):注册内置命令;engine 与 API 共享同一实例
        let slash = crate::slash::SlashRegistry::new();

        // 注入默认「系统助手」角色(无提示词)
        characters.seed_default_character();

        // 运行期设置:优先 data/settings.json,否则回退环境配置
        let settings = Arc::new(Mutex::new(RuntimeSettings::load(&config.data_dir, &config)));

        // 构建连接器:优先使用运行期设置(settings.json)中的 Base URL / Key / 模型,
        // 否则用户保存的 API 配置在重启后会丢失,界面模型显示回退为 .env 默认值。
        let (base_url, api_key, initial_model) = {
            let s = settings.lock().unwrap_or_else(|e| e.into_inner());
            (
                s.openai_base_url.clone(),
                s.openai_api_key.clone(),
                s.model.clone(),
            )
        };
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
        });
        crate::tools::register_builtin_tools(&tool_registry, deps);

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
        let runtime_prompt = Arc::new(RuntimePromptService::new(
            config.data_dir.clone(),
            legacy_runtime_prompt,
        ));

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
        ));
        let engine_model = engine.model();

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
            slash,
            runtime_prompt,
            flow,
            pending_runs: Arc::new(Mutex::new(HashMap::new())),
            bootstrap_limiter: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    /// 失效契约缓存(角色卡写路径调用:契约来源之一变化)。
    pub fn invalidate_contracts_for_character(&self, character_id: &str) {
        self.engine.invalidate_character_contract(character_id);
    }

    /// 清空全部契约缓存(世界书写路径调用:全局世界书可能影响任意角色的契约来源)。
    pub fn clear_all_contracts(&self) {
        self.engine.contract_registry.clear();
    }
}
