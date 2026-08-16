// Agent 引擎核心编排服务(与 Node 版 agent.service.ts + executor.ts 对齐)
use crate::agents::planner::{
    extract_expression, looks_like_calculation, make_custom_plan, make_plan,
};
use crate::agents::reflector::{parse_reflect_verdict, reflect, ReflectionResult};
use crate::agents::state_machine::{AgentState, StateMachine};
use crate::connectors::Connector;
use crate::models::db::Db;
use crate::models::types::{
    AgentSessionRecord, GenerationParams, LlmMessage, LlmStreamChunk, MessageRecord, Plan,
    PlanStep, SseEvent, TokenUsage, ToolCallArgs, ToolContext, ToolDefinition,
};
use crate::parsing::assistant::{
    collect_generate_entries_with, collect_init_vars, parse_patch_array, parse_update_variable,
    render_assistant_content_with, AssistantVars, CharacterCtx, PatchOp, PresetPromptCtx,
    RenderCtx, RenderCtxData,
};
use crate::parsing::macros::{expand_macros, MacroCtx};
use crate::services::agent_session_service::AgentSessionService;
use crate::services::character_service::CharacterService;
use crate::services::prompt_inject_service::{
    FloorRole, InjectMode, PromptInjectConfig, PromptInjectService,
};
use crate::services::quick_reply_service::QuickReplyService;
use crate::services::runtime_prompt_service::RuntimePromptService;
use crate::services::session_service::SessionService;
use crate::services::settings_service::RuntimeSettings;
use crate::services::token_service::TokenService;
use crate::services::user_script_service::UserScriptService;
use crate::services::world_book_service::WorldBookService;
use crate::tools::registry::ToolRegistry;
use crate::utils::logger;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch, RwLock};

pub(super) mod executor;
pub(super) mod messages;
pub(super) mod mvu;
pub(super) mod reflector_integration;
pub(crate) mod worldbook;
pub(super) mod compaction;

use self::compaction::{
    compaction_split, compaction_system_prompt, compaction_user_text, project_history,
    should_auto_compact, upto_message_id, ProjectedHistory,
};

use self::executor::{execute_generation, maybe_run_tool, run_tool_loop};
use self::messages::{
    apply_inject_insertions, build_llm_messages_with_position, inject_reflect_advice,
    parse_inject_insertion, retreat_to_generating_step, step_params_for, trim_to_context,
    with_step_prompt, InjectInsertion, InjectAt,
};
use self::mvu::{apply_mvu_patches, generate_mvu_status, strip_status_bar_tag};
use self::reflector_integration::{generate_reflect_advice, reflect_with_tools};
use self::worldbook::{
    collect_world_text_grouped_with, make_state_block_with_contract, WorldInjection,
};
use crate::parsing::world_book::WorldEntry;

/// 中止标志(watch channel;true = 已请求中断)
pub struct AbortFlag {
    tx: watch::Sender<bool>,
}

impl AbortFlag {
    fn new() -> (Arc<AbortFlag>, watch::Receiver<bool>) {
        let (tx, rx) = watch::channel(false);
        (Arc::new(AbortFlag { tx }), rx)
    }

    pub fn abort(&self) {
        let _ = self.tx.send(true);
    }
}

#[derive(Debug, Clone)]
pub struct AgentRunRequest {
    pub session_id: String,
    pub character_id: String,
    pub user_input: String,
    pub mode: String, // fast | deep | agent | custom
    pub params: GenerationParams,
    /// 上下文窗口上限(token):历史超出后按时间裁剪(最旧优先丢弃)
    pub max_context_tokens: Option<u32>,
    /// 自定义流程步骤快照(custom 模式;路由层已校验并过滤启用步骤)
    pub flow: Option<Vec<PlanStep>>,
    /// 重生成锚点(阶段六 6f):非 None 时收尾原地更新该 assistant 消息行
    /// (旧内容并入 extra.swipes 版本数组),而不是新增一条消息。
    pub regenerate_assistant_id: Option<i64>,
}

/// 运行中的句柄(runs map 的值)
struct RunHandle {
    run_id: uuid::Uuid,
    flag: Arc<AbortFlag>,
    active: bool,
}

/// run_body 内共享可变状态的聚合(供阶段拆分函数按字段借用,避免长参数列表)。
/// 各字段均为 &mut,调用方在同一函数内可对结构体的不同字段做 disjoint borrow
/// (如构建消息时同时可变借用 session_vars 与 assistant_vars)。
struct RunContext<'a> {
    assistant_vars: &'a mut AssistantVars,
    session_vars: &'a mut HashMap<String, String>,
    /// 7 作用域变量(计划二):渲染/宏展开的作用域读写目标;收尾统一落库。
    /// Arc<Mutex> 共享:阶段三 3b 脚本执行(EvalBridge)经 clone 共享同一容器,
    /// 脚本对 global/character/script 等作用域的写回随收尾 take_others 一并落库。
    scopes: Arc<Mutex<crate::parsing::scopes::ScopeVars>>,
    llm_messages: &'a mut Vec<LlmMessage>,
    total_usage: &'a mut TokenUsage,
}

/// 阶段 2「上下文收集」的只读产物:字符卡/历史/世界书/设置快照等,
/// 供消息构建、步骤循环与收尾使用(不持有可变引用,可跨函数存活)。
struct CollectedCtx {
    chara_name: String,
    chara_desc: String,
    personality: String,
    scenario: String,
    history: Vec<MessageRecord>,
    history_tuples: Vec<(String, String)>,
    /// 历史压缩摘要(可空):存在时拼入 system 作为早期历史回顾,替代被压缩的原文段
    history_summary: Option<String>,
    custom_prompt: Option<String>,
    inject_snapshot: PromptInjectConfig,
    reflect_prompt: String,
    reflect_advice_supplement: String,
    reflect_advice_role: String,
    preset_tail: Option<String>,
    preset_tail_role: String,
    initial_vars_tree: Value,
    world_constant: Vec<WorldInjection>,
    world_triggered: Vec<WorldInjection>,
    inject_insertions: Vec<(InjectInsertion, String)>,
    generate_before: Vec<String>,
    generate_after: Vec<String>,
}

pub struct AgentEngine {
    pub connector: Arc<RwLock<Connector>>,
    current_model: Mutex<String>,
    characters: Arc<CharacterService>,
    sessions: Arc<SessionService>,
    agent_sessions: Arc<AgentSessionService>,
    world_books: Arc<WorldBookService>,
    tool_registry: Arc<ToolRegistry>,
    token_service: Arc<Mutex<TokenService>>,
    /// 运行时设置(agent 系统提示词、搜索端点等)
    settings: Arc<Mutex<RuntimeSettings>>,
    /// 提示词注入配置(简单模式 + 楼层系统)
    prompt_inject: Arc<Mutex<PromptInjectService>>,
    /// 快速回复(Quick Replies):getqr 的渲染数据源
    quick_replies: Arc<QuickReplyService>,
    /// 与 settings API 共用的 DATA_DIR 运行时提示词文件服务
    runtime_prompt: Arc<RuntimePromptService>,
    /// 用户脚本服务(阶段三 3b-3):角色卡 extensions.tavern_helper 脚本树读取
    user_scripts: Arc<UserScriptService>,
    /// slash 命令注册表(阶段四 4a):脚本 triggerSlash 与 API 命令清单共用
    slash: Arc<crate::slash::SlashRegistry>,
    /// SQLite 句柄(Token 累计统计)
    db: Arc<Db>,
    /// 契约注册表(character_id → Contract):与多步工具/API 写路径共享同一实例
    /// (AppState 构造注入),保证「改卡 → invalidate → 下轮重提取」的缓存一致性。
    pub(crate) contract_registry: Arc<crate::contracts::ContractRegistry>,
    /// 契约运行态服务(P5):收尾把 KaleidoState/changelog 提交到 SQLite
    kaleido_state: Arc<crate::services::kaleido_state_service::KaleidoStateService>,
    runs: Mutex<HashMap<String, RunHandle>>,
}

/// 构造 SSE Step 事件(自定义流程步骤附带 index/total 进度;其余模式不携带)
fn step_evt(
    step: &str,
    detail: Option<String>,
    index: Option<usize>,
    total: Option<usize>,
) -> SseEvent {
    SseEvent::Step {
        step: step.into(),
        detail,
        index,
        total,
    }
}

/// 在正文中定位 UpdateVariable 标签起始位置(大小写不敏感,容错 <updatevariable> 等变体)。
/// 返回标签名首字符的字节下标;找不到返回 None。
/// 用字节窗口匹配:TAG 全 ASCII,与中文等多字节字符(字节 ≥0x80)不会重叠,
/// 匹配起点必落在字符边界,避免按字节切片多字节字符导致 panic。
fn find_update_variable_tag(content: &str) -> Option<usize> {
    const TAG: &str = "updatevariable";
    let tag = TAG.as_bytes();
    content
        .as_bytes()
        .windows(tag.len())
        .position(|w| w.eq_ignore_ascii_case(tag))
        .filter(|&p| content.is_char_boundary(p))
}

/// 把正文替换为修正后文本,同时保留 <UpdateVariable> 补丁块。
/// 以标签名 "UpdateVariable" 为锚点定位块起始(大小写不敏感),向前取最近 '<'
/// 作为标签头;块及之后内容原样保留,块前视为正文被替换。避免 censor 修正误伤
/// JSONPatch 中的变量键/值。
fn rebuild_content_keeping_blocks(content: &str, new_body: &str) -> String {
    match find_update_variable_tag(content) {
        Some(i) => {
            let tag_start = content[..i].rfind('<').unwrap_or(i);
            format!("{new_body}{}", &content[tag_start..])
        }
        None => new_body.to_string(),
    }
}

/// 顶层生成错误分类:返回 (code, retryable)。
/// 网络/超时/上游 429/5xx 视为可重试;鉴权/参数/工具错误不可重试。
fn classify_engine_error(msg: &str) -> (String, bool) {
    let m = msg.to_lowercase();
    let retryable = m.contains("超时")
        || m.contains("timeout")
        || m.contains("429")
        || m.contains("500")
        || m.contains("502")
        || m.contains("503")
        || m.contains("504")
        || m.contains("connection")
        || m.contains("connect")
        || m.contains("eof")
        || m.contains("broken pipe");
    let code = if m.contains("超时") || m.contains("timeout") {
        "request_timeout"
    } else if m.contains("429") || m.contains("rate limit") {
        "rate_limited"
    } else if m.contains("401")
        || m.contains("403")
        || m.contains("apikey")
        || m.contains("api key")
    {
        "auth_failed"
    } else if retryable {
        "upstream_error"
    } else {
        "generation_failed"
    };
    (code.to_string(), retryable)
}

/// 发送 SSE 事件;中断时返回 Err("已中断");
/// 客户端断开(tx.send 失败)时置位 abort,使后续流程按中断处理
async fn send_event(
    event: SseEvent,
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
) -> Result<(), String> {
    if *abort.borrow() {
        return Err("已中断".to_string());
    }
    match tx.send(event).await {
        Ok(_) => Ok(()),
        Err(_) => {
            flag.abort();
            Err("已中断".to_string())
        }
    }
}

/// 检查中断
fn check_aborted(abort: &watch::Receiver<bool>) -> Result<(), String> {
    if *abort.borrow() {
        Err("已中断".to_string())
    } else {
        Ok(())
    }
}

/// 生成反思失败建议并拼为注入文本(位置0 内容,自动而非用户决定):
/// 调用 LLM 产出 ≤200 token 的针对性改进建议(失败/空则仅保留用户补充说明),
/// 可选的用户补充说明附加在其后;建议与补充均为空时返回 None(不注入)。
/// 生成产生的 usage 累加进 total_usage。注入边(user/assistant)由构建期
/// reflect_advice_role 决定,与本函数无关。
async fn build_reflect_advice(
    engine: &AgentEngine,
    reason: &str,
    user_input: &str,
    draft: &str,
    supplement: &str,
    abort: &watch::Receiver<bool>,
    total_usage: &mut TokenUsage,
) -> Option<String> {
    let mut text = String::new();
    if let Some((advice, u)) =
        generate_reflect_advice(engine, reason, user_input, draft, abort).await
    {
        total_usage.prompt_tokens += u.prompt_tokens;
        total_usage.completion_tokens += u.completion_tokens;
        total_usage.total_tokens += u.total_tokens;
        total_usage.prompt_cache_hit_tokens += u.prompt_cache_hit_tokens;
        text.push_str("[反思反馈]\n");
        text.push_str(advice.trim());
    }
    let supplement = supplement.trim();
    if !supplement.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n[补充要求]\n");
        }
        text.push_str(supplement);
    }
    (!text.is_empty()).then_some(text)
}

impl AgentEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        connector: Arc<RwLock<Connector>>,
        characters: Arc<CharacterService>,
        sessions: Arc<SessionService>,
        agent_sessions: Arc<AgentSessionService>,
        world_books: Arc<WorldBookService>,
        tool_registry: Arc<ToolRegistry>,
        settings: Arc<Mutex<RuntimeSettings>>,
        prompt_inject: Arc<Mutex<PromptInjectService>>,
        quick_replies: Arc<QuickReplyService>,
        runtime_prompt: Arc<RuntimePromptService>,
        db: Arc<Db>,
        initial_model: String,
        user_scripts: Arc<UserScriptService>,
        slash: Arc<crate::slash::SlashRegistry>,
        contract_registry: Arc<crate::contracts::ContractRegistry>,
        kaleido_state: Arc<crate::services::kaleido_state_service::KaleidoStateService>,
    ) -> Self {
        AgentEngine {
            connector,
            current_model: Mutex::new(initial_model),
            characters,
            sessions,
            agent_sessions,
            world_books,
            tool_registry,
            token_service: Arc::new(Mutex::new(TokenService::new())),
            settings,
            prompt_inject,
            quick_replies,
            runtime_prompt,
            db,
            runs: Mutex::new(HashMap::new()),
            contract_registry,
            kaleido_state,
            user_scripts,
            slash,
        }
    }

    /// 惰性加载角色卡契约(角色卡 extensions.nlkaleido 优先,世界书条目兜底)。
    /// 未命中契约(存量卡)返回 None,引擎走既有兼容层路径(文档 D-2)。
    /// 缓存由共享 ContractRegistry 管理;作者改卡后经 API 写路径 invalidate 失效。
    pub fn load_character_contract(&self, character_id: &str) -> Option<crate::contracts::Contract> {
        self.contract_registry.load(character_id)
    }

    /// 失效角色契约缓存(角色卡/世界书写路径调用;供 API 层转发)。
    pub fn invalidate_character_contract(&self, character_id: &str) {
        self.contract_registry.invalidate(character_id);
    }

    /// 当前生效模型(settings 切换模型后立即生效)
    pub fn model(&self) -> String {
        self.current_model.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 运行时切换模型(更新 connector 内的 model;异步避免阻塞 runtime)
    pub async fn switch_model(&self, model: &str) {
        let mut c = self.connector.write().await;
        *c = crate::connectors::with_model(&c, model);
        *self.current_model.lock().unwrap_or_else(|e| e.into_inner()) = model.to_string();
    }

    pub fn is_active(&self, session_id: &str) -> bool {
        self.runs
            .lock().unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .map(|r| r.active)
            .unwrap_or(false)
    }

    pub fn stop(&self, session_id: &str) {
        if let Some(run) = self.runs.lock().unwrap_or_else(|e| e.into_inner()).get(session_id) {
            run.flag.abort();
            logger::agent_step(session_id, "interrupt", Some("用户请求停止生成"));
        }
    }

    /// 主流程:全量运行 Agent,事件经 tx 推送(与 Node 版 run 对齐)。
    /// 正常完成时返回 Some((assistant_content, total_usage, 酒馆助手变量快照));
    /// content 已剥离 <UpdateVariable> 块;快照为 Some 时表示本轮回合更新过变量。
    /// 中断/出错返回 None。
    pub async fn run(
        &self,
        req: AgentRunRequest,
        tx: mpsc::Sender<SseEvent>,
    ) -> Option<(String, TokenUsage, Option<Value>, Option<String>)> {
        let session_id = req.session_id.clone();
        let user_input = req.user_input.clone();

        // 抢占:若该会话正在生成,先中止旧的
        {
            let runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = runs.get(&session_id) {
                if existing.active {
                    existing.flag.abort();
                }
            }
        }

        let run_id = uuid::Uuid::new_v4();
        let (flag, abort_rx) = AbortFlag::new();
        {
            let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
            runs.insert(
                session_id.clone(),
                RunHandle {
                    run_id,
                    flag: flag.clone(),
                    active: true,
                },
            );
        }

        // 初始化 Agent 会话:清理旧残留再创建
        self.agent_sessions.delete_by_session(&session_id);
        let agent_session = match self.agent_sessions.create(&session_id, &req.mode) {
            Ok(a) => a,
            Err(e) => {
                logger::error("Agent 会话初始化失败", &[("error", Value::String(e))]);
                self.finish_run(&session_id, run_id);
                return None;
            }
        };

        let mut state_machine = StateMachine::new(&session_id);
        let mut total_usage = TokenUsage::default();
        let mut final_out: Option<(String, TokenUsage, Option<Value>, Option<String>)> = None;

        let run_body = async {
            // ===== 1. 规划阶段 =====
            let plan = self
                .plan_phase(
                    &mut state_machine,
                    &agent_session,
                    &req,
                    &user_input,
                    &tx,
                    &abort_rx,
                    &flag,
                    &session_id,
                )
                .await?;

            // ===== 2. 上下文收集与消息构建 =====
            // 压缩决策先于上下文收集:摘要一旦生成即落库,collect_context 读取时会投影生效。
            self.maybe_compact(&session_id, &tx, &abort_rx).await;
            // 共享可变状态聚合进 RunContext,供收集/构建/步骤循环/收尾按字段借用
            let mut assistant_vars = self.sessions.load_assistant_vars(&session_id);
            let mut session_vars = self.sessions.load_session_vars(&session_id);
            // 7 作用域变量(计划二):character/preset/message 作用域数据按需懒加载,
            // 渲染/宏展开的作用域读写目标;收尾统一落库(见 collect_context 与下方收尾)。
            // Arc<Mutex> 共享容器:脚本执行(3b)克隆同一引用读写变量。
            let scopes = Arc::new(Mutex::new(crate::parsing::scopes::ScopeVars::new()));
            let character_id = req.character_id.clone();
            if let Some(c) = self.characters.get(&character_id) {
                // character 作用域种子:优先 scope_variables 持久化值,否则从角色卡
                // data_raw.extensions.variables 提取(V2 角色卡规范字段;写不落回 data_raw)
                let seed = self
                    .sessions
                    .load_scope_variables("character", &c.id)
                    .or_else(|| {
                        c.data_raw
                            .as_ref()
                            .and_then(|raw| raw.get("extensions"))
                            .and_then(|ext| ext.get("variables"))
                            .cloned()
                    });
                {
                    let mut s = scopes.lock().unwrap_or_else(|e| e.into_inner());
                    s.set_character_scope_id(Some(c.id.clone()));
                    if let Some(seed) = seed {
                        s.with_scope(crate::parsing::scopes::Scope::Character, &c.id, seed);
                    }
                }
            }
            // global 作用域:阶段三 3b 脚本跨轮累积读写的持久层(scope_variables 表,
            // scope_id 恒为空),启动时加载进共享容器;收尾 take_others 整树落库回写。
            if let Some(g) = self.sessions.load_scope_variables("global", "") {
                scopes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .with_scope(crate::parsing::scopes::Scope::Global, "", g);
            }
            let mut llm_messages = Vec::new();
            let mut rctx = RunContext {
                assistant_vars: &mut assistant_vars,
                session_vars: &mut session_vars,
                scopes: scopes.clone(),
                llm_messages: &mut llm_messages,
                total_usage: &mut total_usage,
            };
            let ctx_data = self.collect_context(&req, &session_id, &mut rctx);
            self.finalize_messages(&req, &session_id, &ctx_data, &mut rctx);

            let tool_ctx = ToolContext {
                session_id: session_id.clone(),
                character_id: req.character_id.clone(),
            };

            // ===== 2. 执行阶段 =====
            let (content, custom_vars_snapshot, custom_contract_entries, custom_contract_pending) = self
                .step_loop(
                    &req,
                    &ctx_data,
                    &plan,
                    &mut state_machine,
                    &agent_session,
                    &user_input,
                    &tool_ctx,
                    &tx,
                    &abort_rx,
                    &flag,
                    &session_id,
                    &run_id,
                    &mut rctx,
                )
                .await?;

            // ===== 3. 收尾 =====
            if *abort_rx.borrow() {
                let _ = state_machine.transition(AgentState::Interrupted, &session_id);
                let _ = self.agent_sessions.update(
                    &agent_session.id,
                    Some("interrupted"),
                    None,
                    None,
                    None,
                );
                send_event(SseEvent::Interrupted, &tx, &abort_rx, &flag).await?;
                logger::agent_step(&session_id, "interrupted", Some("生成被中止"));
            } else {
                let _ = state_machine.transition(AgentState::Finished, &session_id);
                let _ = self.agent_sessions.update(
                    &agent_session.id,
                    Some("finished"),
                    None,
                    None,
                    None,
                );
                // 酒馆助手输出协议:解析 <UpdateVariable> 块 → 应用补丁 → 树持久化
                // → SSE Vars 事件推送最新树;显示/存储用剥离后的内容(历史干净)。
                // custom 模式已在步骤内即时应用(content 已剥离),此处仅需快照落库;
                // 其余模式在此原子应用(反思重试丢弃内容时补丁不提前生效,保持整轮原子)。
                let (clean_content, patches) = parse_update_variable(&content);
                // <StatusBar> 是两步生成的输出协议标签(状态栏文本单独落库),不进入正文渲染/存储
                let mut clean_content = strip_status_bar_tag(&clean_content);
                // 禁词库工具兜底(deep/agent/custom):输出含禁词时,引擎收尾调 censor_text 工具
                // 做同义替换(替换后同时作用于落库与 Finish.content,前端 finish 覆盖流式文本)。
                // fast 模式不带工具,仅靠 simple_inject_text 注入的自省提示词预防。
                if req.mode != "fast" && !clean_content.trim().is_empty() {
                    let inject = self.prompt_inject.lock().unwrap_or_else(|e| e.into_inner()).get().clone();
                    if inject.simple.banned_words_enabled {
                        // 从禁词提示词(新格式)或词条表(旧格式)提取禁用词列表
                        let words: Vec<String> = inject.simple.banned_words_extract();
                        let entries: Vec<serde_json::Value> = words
                            .into_iter()
                            .map(|w| json!({ "word": w, "replacement": "" }))
                            .collect();
                        if !entries.is_empty() {
                            let ctx = ToolContext {
                                session_id: session_id.clone(),
                                character_id: req.character_id.clone(),
                            };
                            let args = json!({ "text": clean_content, "entries": entries });
                            match self
                                .tool_registry
                                .execute("censor_text", &args.to_string(), ctx)
                                .await
                            {
                                Ok(censored) => {
                                    if !censored.trim().is_empty() {
                                        clean_content = censored;
                                    }
                                }
                                Err(e) => {
                                    logger::warn(
                                        "禁词替换工具调用失败,保留原文",
                                        &[("error", Value::String(e))],
                                    );
                                }
                            }
                        }
                    }
                }
                let mut vars_snapshot = custom_vars_snapshot.clone();
                let mut status_bar: Option<String> = None;
                // P5 契约运行态:收尾统一加载契约一次(正文路径与两步路径共用);
                // 无契约时所有门控/留痕路径退化为原行为(零变化)。
                let contract = self.load_character_contract(&req.character_id);
                // P5:custom 模式已在步骤内即时门控应用,其产物并入收尾统一提交;
                // 其余模式(deep/fast/agent)在此收尾集中门控正文补丁。
                let mut contract_entries: Vec<crate::contracts::ChangelogEntry> =
                    custom_contract_entries;
                let mut contract_pending: Vec<crate::contracts::PatchOp> = custom_contract_pending;
                if vars_snapshot.is_none() {
                    // P5:有契约时正文补丁先经契约门控(未声明/越权拒绝、低置信 pending)
                    let gated = crate::contracts::gate_assistant_patches_detailed(
                        contract.as_ref(),
                        &patches,
                        "agent",
                    );
                    if gated.rejected.len() > 0 || gated.pending.len() > 0 {
                        logger::warn(
                            "契约门控过滤了部分正文变量补丁",
                            &[
                                ("rejected", json!(gated.rejected.len())),
                                ("pending", json!(gated.pending.len())),
                            ],
                        );
                    }
                    contract_pending.extend(gated.pending);
                    if contract.is_some() && !gated.applied.is_empty() {
                        let tree_before = rctx.assistant_vars.tree().clone();
                        vars_snapshot = apply_mvu_patches(
                            self,
                            &session_id,
                            &mut *rctx.assistant_vars,
                            &gated.applied,
                            &tx,
                            &abort_rx,
                            &flag,
                        )
                        .await;
                        if vars_snapshot.is_some() {
                            contract_entries.extend(crate::contracts::entries_from_applied(
                                &tree_before,
                                rctx.assistant_vars.tree(),
                                ctx_data.history.len() as u64,
                                &gated.applied_ops,
                                crate::contracts::ChangelogSource::Agent,
                            ));
                        }
                    } else if contract.is_none() {
                        vars_snapshot = apply_mvu_patches(
                            self,
                            &session_id,
                            &mut *rctx.assistant_vars,
                            &gated.applied,
                            &tx,
                            &abort_rx,
                            &flag,
                        )
                        .await;
                    }
                }
                // 两步生成:角色卡有变量树时,正文之外总是追加一次「变量更新 + 状态栏」专用调用
                // (用户方案:正文一次、变量+状态栏一次)。输入仅含精简状态、输出协议与正文,
                // 输出 <UpdateVariable> 补丁块(增量,基于已含部分更新的当前状态)与
                // <StatusBar> 状态栏文本(前端插入对话气泡);失败则降级为仅正文。
                if !rctx.assistant_vars.is_empty() {
                    match generate_mvu_status(
                        self,
                        &session_id,
                        &ctx_data.history,
                        &clean_content,
                        &ctx_data.initial_vars_tree,
                        &mut *rctx.assistant_vars,
                        &req.params,
                        &tx,
                        &abort_rx,
                        &flag,
                        contract.as_ref(),
                        ctx_data.history.len() as u64,
                    )
                    .await
                    {
                        Ok((snap, bar, entries, pending)) => {
                            if snap.is_some() {
                                vars_snapshot = snap;
                            }
                            status_bar = bar;
                            contract_entries.extend(entries);
                            contract_pending.extend(pending);
                        }
                        Err(e) => {
                            logger::warn("变量+状态栏生成失败", &[("error", Value::String(e))])
                        }
                    }
                }
                // 落库 assistant 消息必须在 Finish 事件发出之前完成:前端收到 finish 后立即
                // loadHistory 刷新,若消息尚未落库会出现最后一条消息消失、变量树被旧历史回滚。
                // (落库责任原在 chat.rs 的 spawn 后台任务,存在竞态;移到引擎收尾保证顺序)
                if !clean_content.is_empty() || vars_snapshot.is_some() || status_bar.is_some() {
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    let mut extra = json!({
                        "ts": ts,
                        "completion_tokens": rctx.total_usage.completion_tokens,
                        "total_tokens": rctx.total_usage.total_tokens,
                    });
                    if let Some(tree) = vars_snapshot.clone() {
                        extra["mvu"] = json!({ "stat_data": tree });
                    }
                    if let Some(bar) = status_bar.clone() {
                        extra["status_bar"] = json!(bar);
                    }
                    // 阶段六 6f:重生成锚点 → 原地更新原 assistant 消息行(swipes 追加,
                    // id 稳定);首次生成(无锚点)走既有 add_message 新增一行。
                    let stored = if let Some(regenerate_id) = req.regenerate_assistant_id {
                        self.upsert_regenerated_message(
                            &session_id,
                            regenerate_id,
                            &clean_content,
                            &mut extra,
                            ts,
                        )
                    } else {
                        self.sessions
                            .add_message(&session_id, "assistant", &clean_content, extra.clone())
                    };
                    match stored {
                        Ok(rec) => {
                            // 7 作用域(计划二):message 作用域镜像(与 extra.mvu 双写,
                            // 读时 scope_variables 优先、extra.mvu 兜底;会话级串行,引擎为唯一写者)
                            if let Some(tsv) = vars_snapshot.clone() {
                                rctx.scopes
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .with_scope(
                                        crate::parsing::scopes::Scope::Message,
                                        &rec.id.to_string(),
                                        tsv,
                                    );
                            }
                        }
                        Err(e) => {
                            logger::warn("assistant 消息落库失败", &[("error", Value::String(e))]);
                        }
                    }
                    // 阶段三 3b-3:角色卡后端脚本执行(消息生成完成后)。
                    // 读取角色卡 extensions.tavern_helper 脚本树,串行执行启用脚本;
                    // 脚本经 TavernHelper 兼容桥对 global/character/script 等作用域的
                    // 写回进入 rctx.scopes,由下方 take_others 一并落库。失败仅记日志。
                    self.run_character_scripts(&character_id, &rctx.scopes).await;
                    // 其余作用域(global/character/preset/script/extension)整树落库
                    let others = rctx
                        .scopes
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take_others();
                    if !others.is_empty() {
                        let entries: Vec<(String, String, String)> = others
                            .into_iter()
                            .map(|(s, id, raw)| (s.as_str().to_string(), id, raw))
                            .collect();
                        self.sessions.save_scope_variables_batch(entries);
                    }
                }
                // P5 契约运行态提交(kaleido_state/kaleido_changelog):仅在角色有契约时执行;
                // 失败仅记日志(「生成结果不受影响」),不改变 vars_snapshot/正文内容。
                // meta 读取失败按空 meta 起步(不 ? 中断生成);旧 contract_version 不调和,
                // 直接覆盖写入。
                if let Some(contract) = &contract {
                    let turn_id = ctx_data.history.len() as u64;
                    let meta_init = match self.kaleido_state.load_meta(&session_id) {
                        Ok(Some((_, meta))) => meta,
                        Ok(None) => crate::contracts::KaleidoMeta::default(),
                        Err(e) => {
                            logger::warn(
                                "契约运行态 meta 读取失败,按空 meta 继续",
                                &[("error", Value::String(e))],
                            );
                            crate::contracts::KaleidoMeta::default()
                        }
                    };
                    let mut meta = meta_init;
                    meta.last_turn_id = turn_id;
                    meta.last_contract_version = contract.version;
                    // P6:pending 经 merge_pending 合并——本轮成功应用的 path 消费旧
                    // pending、同指纹去重、超 MAX_PENDING 丢最旧(防低置信 op 重复膨胀)。
                    let applied_paths: Vec<&str> =
                        contract_entries.iter().map(|e| e.path.as_str()).collect();
                    meta.pending = crate::contracts::merge_pending(
                        &meta.pending,
                        &contract_pending,
                        &applied_paths,
                        turn_id,
                    );
                    for entry in &contract_entries {
                        meta.confidence.insert(entry.path.clone(), entry.confidence);
                    }
                    let stat_data = rctx.assistant_vars.tree().clone();
                    let mut entries = contract_entries;
                    if let Err(e) = self.kaleido_state.commit_turn(
                        &session_id,
                        contract.version,
                        &stat_data,
                        &meta,
                        &mut entries,
                    ) {
                        logger::warn(
                            "契约运行态提交失败(生成结果不受影响)",
                            &[
                                ("session_id", Value::String(session_id.clone())),
                                ("error", Value::String(e)),
                            ],
                        );
                    }
                }
                send_event(
                    SseEvent::Finish {
                        usage: total_usage.clone(),
                        content: clean_content.clone(),
                    },
                    &tx,
                    &abort_rx,
                    &flag,
                )
                .await?;
                // 接收统计(ST-Prompt-Template 兼容):LAST_RECEIVE_TOKENS / LAST_RECEIVE_CHARS。
                // 记入会话宏变量,下一轮模板可用 {{getvar::LAST_RECEIVE_TOKENS}} 读取。
                self.record_receive_stats(&session_id, total_usage.completion_tokens, &clean_content);
                // Token 累计统计:会话 + 全局
                self.record_usage(&session_id, &total_usage);
                logger::agent_step(
                    &session_id,
                    "finish",
                    Some(&format!("total_tokens={}", total_usage.total_tokens)),
                );
                // 正常完成:返回内容、usage 与状态栏,供路由层落库 assistant 消息
                final_out = Some((
                    clean_content,
                    total_usage.clone(),
                    vars_snapshot,
                    status_bar,
                ));
            }
            Ok::<(), String>(())
        };

        let result = run_body.await;
        match result {
            Ok(_) => {}
            Err(e) => {
                if *abort_rx.borrow() {
                    // 中断或客户端断开
                    let _ = state_machine.transition(AgentState::Interrupted, &session_id);
                    let _ = self.agent_sessions.update(
                        &agent_session.id,
                        Some("interrupted"),
                        None,
                        None,
                        None,
                    );
                    let _ = tx.send(SseEvent::Interrupted).await;
                } else {
                    let _ = state_machine.transition(AgentState::Error, &session_id);
                    let _ = self.agent_sessions.update(
                        &agent_session.id,
                        Some("error"),
                        None,
                        None,
                        None,
                    );
                    let _ = tx
                        .send(step_evt("执行出错", Some(e.clone()), None, None))
                        .await;
                    // 错误终态:发 Error 事件,不再用「空内容 finish」伪装正常结束。
                    // 前端据此展示错误并给出可重试提示。
                    let (code, retryable) = classify_engine_error(&e);
                    let _ = tx
                        .send(SseEvent::Error {
                            code,
                            message: e.clone(),
                            retryable,
                        })
                        .await;
                    logger::agent_step(&session_id, "error", Some(&e));
                }
            }
        }
        self.finish_run(&session_id, run_id);
        // LLM 请求快照保留策略(第四点·主题 A):每次 run 结束裁剪,仅保留最近 50 条。
        if let Err(e) = self.sessions.prune_llm_requests(&session_id, 50) {
            logger::warn("LLM 请求快照裁剪失败", &[("error", Value::String(e))]);
        }
        final_out
    }

    /// 阶段 2「上下文收集」:装载字符卡/历史/世界书/变量树与设置快照,产出只读上下文
    /// 清除会话的压缩摘要(撤销压缩,恢复完整原文历史)。返回是否确有摘要被清除。
    pub fn clear_compaction(&self, session_id: &str) -> Result<bool, String> {
        let had = self.sessions.get_compaction(session_id).is_some();
        self.sessions.delete_compaction(session_id)?;
        Ok(had)
    }

    /// 手动压缩(独立端点 /api/chat/compact 调用):对会话较早历史做一次摘要压缩。
    /// 原文消息不删、摘要 upsert 到 session_compactions(可逆);返回是否实际压缩。
    /// mode=off 时拒绝;历史不足 KEEP_RECENT_MESSAGES 条时返回 Ok(false) 无需压缩。
    pub async fn compact_session(&self, session_id: &str) -> Result<bool, String> {
        let mode = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .compaction_mode
            .clone();
        if mode == "off" {
            return Err("上下文压缩模式为 off,请先在设置中改为 manual 或 auto".into());
        }
        let history = self.sessions.get_messages(session_id);
        let Some(to_compact) = compaction_split(&history) else {
            return Ok(false);
        };
        let Some(upto) = upto_message_id(&history, to_compact) else {
            return Ok(false);
        };
        let (abort, abort_rx) = AbortFlag::new();
        let segment = &history[..to_compact];
        let messages = vec![
            LlmMessage::plain("system", compaction_system_prompt()),
            LlmMessage::plain("user", &compaction_user_text(segment)),
        ];
        let params = GenerationParams {
            temperature: 0.3,
            top_p: 1.0,
            max_tokens: 512,
            stop: None,
            tools: Vec::new(),
            max_tool_rounds: None,
            tool_choice: crate::models::types::ToolChoice::Auto,
            parallel_tool_calls: None,
        };
        let (summary, _usage) = self.generate_text(&messages, params, abort_rx).await?;
        let summary = summary.trim().to_string();
        if summary.is_empty() {
            return Err("摘要生成返回空内容".into());
        }
        self.sessions
            .save_compaction(session_id, upto, &summary, &self.model())?;
        drop(abort);
        Ok(true)
    }

    /// auto 压缩决策(生成主流程内):mode=auto 且历史 token 达到阈值时触发摘要。
    /// mode=manual 由前端独立端点 /api/chat/compact 触发,不经本方法。
    /// 摘要经 generate_text 非流式生成,失败仅告警并跳过,不阻塞生成主流程。
    async fn maybe_compact(
        &self,
        session_id: &str,
        tx: &mpsc::Sender<SseEvent>,
        abort: &watch::Receiver<bool>,
    ) {
        let (mode, threshold, max_context) = {
            let s = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            (
                s.compaction_mode.clone(),
                s.compaction_threshold,
                s.max_context_tokens,
            )
        };
        if mode != "auto" {
            return;
        }
        let history = self.sessions.get_messages(session_id);
        if history.is_empty() {
            return;
        }
        let tokens = {
            let mut ts = self.token_service.lock().unwrap_or_else(|e| e.into_inner());
            let mut total: i64 = 0;
            for m in &history {
                total += ts.count_tokens(&m.content, &self.model()) + 4;
            }
            total + 2
        };
        if !should_auto_compact(tokens, max_context, threshold) {
            return;
        }
        let Some(to_compact) = compaction_split(&history) else {
            return;
        };
        let Some(upto) = upto_message_id(&history, to_compact) else {
            return;
        };
        let _ = tx
            .send(step_evt(
                "压缩历史中…",
                Some("正在把较早对话压成摘要,原文仍保留可恢复".to_string()),
                None,
                None,
            ))
            .await;
        let segment = &history[..to_compact];
        let messages = vec![
            LlmMessage::plain("system", compaction_system_prompt()),
            LlmMessage::plain("user", &compaction_user_text(segment)),
        ];
        let params = GenerationParams {
            temperature: 0.3,
            top_p: 1.0,
            max_tokens: 512,
            stop: None,
            tools: Vec::new(),
            max_tool_rounds: None,
            tool_choice: crate::models::types::ToolChoice::Auto,
            parallel_tool_calls: None,
        };
        match self.generate_text(&messages, params, abort.clone()).await {
            Ok((summary, _usage)) => {
                let summary = summary.trim().to_string();
                if !summary.is_empty() {
                    if let Err(e) =
                        self.sessions
                            .save_compaction(session_id, upto, &summary, &self.model())
                    {
                        logger::warn(
                            "压缩摘要落库失败",
                            &[("error", Value::String(e))],
                        );
                    }
                }
            }
            Err(e) => {
                logger::warn(
                    "压缩摘要生成失败,本轮跳过压缩",
                    &[("error", Value::String(e))],
                );
            }
        }
    }

    /// CollectedCtx 供消息构建、步骤循环与收尾使用;变量树初始化/渲染副作用就地生效
    /// (rctx.assistant_vars 为可变,条目分离与状态块注入同步更新)。
    /// 对应 run_body 内「构建 LLM 消息」段的收集部分;L2 中层定位:消息构建前的数据装配。
    fn collect_context(
        &self,
        req: &AgentRunRequest,
        session_id: &str,
        rctx: &mut RunContext<'_>,
    ) -> CollectedCtx {
        // 构建 LLM 消息(系统提示 + 历史;与 Node 版一致,history 不带 extra → system 全部跳过)
        // 开场白(first_mes)由 create_session/ensure_session 作为首条 assistant 消息写入会话,
        // 历史中天然包含,不再重复注入 system(避免同一段文本出现两次)
        let character = self.characters.get(&req.character_id);
        let history = self.sessions.get_messages(session_id);
        // 历史压缩投影:读已存在的摘要(若曾压缩过),模型可见历史 = 摘要 + 截止点之后的原文。
        // 原文 history 保持不变,继续供 EJS 渲染与世界书分组读取完整历史。
        let projected: ProjectedHistory = project_history(&history, self.sessions.get_compaction(session_id));
        // 角色名/描述(供消息构建与自定义流程步骤提示词的宏上下文)
        let chara_name = character
            .as_ref()
            .map(|c| c.chara_name.as_str())
            .unwrap_or("角色")
            .to_string();
        let chara_desc = character
            .as_ref()
            .map(|c| c.description.as_str())
            .unwrap_or("")
            .to_string();
        // 世界书注入:角色内嵌 character_book + 独立世界书(绑定角色或全局启用)
        let mut entries = Vec::new();
        if let Some(raw) = character.as_ref().and_then(|c| c.data_raw.as_ref()) {
            entries.extend(crate::parsing::world_book::character_book_entries(raw));
        }
        entries.extend(
            self.world_books
                .collect_entries_for_character(&req.character_id),
        );
        // 酒馆助手变量树:会话级持久化;空则从 [InitVar] 条目初始化并落库
        if rctx.assistant_vars.is_empty() {
            *rctx.assistant_vars = collect_init_vars(&entries);
            // P8 契约 default 填充:InitVar 没写/老卡无 InitVar 时,契约
            // updateRules 声明的 default 兜底补齐(已有值不动);无契约零变化。
            // 仅会话初始化时执行一次;契约后续版本变更不回填 default,
            // 与 contract_version 不调和的现状一致,留给 P9+ 调和机制。
            if let Some(contract) = self.contract_registry.load(&req.character_id).as_ref() {
                let mut tree = rctx.assistant_vars.tree().clone();
                crate::contracts::apply_contract_defaults(contract, &mut tree);
                *rctx.assistant_vars =
                    crate::parsing::assistant::AssistantVars::from_value(tree);
            }
            if !rctx.assistant_vars.is_empty() {
                if let Err(e) = self.sessions.save_assistant_vars(session_id, rctx.assistant_vars) {
                    logger::warn(
                        "初始变量树落库失败",
                        &[
                            ("session_id", Value::String(session_id.to_string())),
                            ("error", Value::String(e)),
                        ],
                    );
                }
            }
        }
        // 7 作用域(计划二):chat 树/扁平层镜像同步(回合边界),此后渲染/宏展开经
        // scopes 读合并视图时能读到最新树;回合内 chat 树写仍以 assistant_vars 为权威
        {
            let mut s = rctx.scopes.lock().unwrap_or_else(|e| e.into_inner());
            s.sync_chat_tree(rctx.assistant_vars);
            s.sync_chat_flat(rctx.session_vars);
        }
        // 提示词注入配置快照(简单模式 + 楼层;getpreset 的预设数据源)
        let inject_snapshot = self
            .prompt_inject
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get()
            .clone();
        // getpreset/getPresetPrompt 的预设楼层数据(启用的楼层,按配置顺序)
        let preset_prompts: Vec<PresetPromptCtx> = inject_snapshot
            .floors
            .iter()
            .filter(|f| f.enabled)
            .map(|f| PresetPromptCtx {
                name: f.name.clone(),
                content: f.content.clone(),
            })
            .collect();
        // getqr/getQuickReply 的快速回复库(启用的条目)
        let quick_replies = self.quick_replies.render_lib();
        // 渲染上下文(ST-Prompt-Template 兼容):EJS 内建读取类函数(getwi/getchar/getqr/
        // getChatMessage/injectPrompt 等)经此读写;渲染副作用(injectPrompt 登记)收集后并入提示词流。
        let character_ctx: Option<CharacterCtx> = character.as_ref().map(|c| {
            let raw = c.data_raw.as_ref();
            CharacterCtx {
                name: c.chara_name.clone(),
                description: c.description.clone(),
                personality: raw
                    .and_then(|r| r.get("personality"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                scenario: raw
                    .and_then(|r| r.get("scenario"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                avatar_url: raw
                    .and_then(|r| r.get("avatar"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                mes_example: raw
                    .and_then(|r| r.get("mes_example"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                first_mes: raw
                    .and_then(|r| r.get("first_mes"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }
        });
        // 渲染上下文持有 scopes 只读引用(宏 getvar/get_*_variable 经合并视图读取);
        // guard 须活过 render_ctx 使用期,渲染结束后显式释放再继续 lock(见下方 drop)
        let mut scopes_guard = rctx.scopes.lock().unwrap_or_else(|e| e.into_inner());
        let mut render_ctx = RenderCtx {
            vars: rctx.assistant_vars,
            data: RenderCtxData {
                character: character_ctx.as_ref(),
                world_entries: Some(&entries),
                history: Some(&history),
                quick_replies: Some(&quick_replies),
                preset_prompts: Some(&preset_prompts),
                scopes: Some(&mut *scopes_guard),
                injected: Vec::new(),
            },
        };
        // 世界书条目分离:注入标签条目([GENERATE]/[RENDER]/[InitialVariables]/@INJECT)
        // 单独处理,其余走普通注入链路。渲染在分离阶段完成(变量树已初始化)。
        let (generate_entries, mut normal_entries) =
            collect_generate_entries_with(&entries, &mut render_ctx);
        // @INJECT 精确消息插入条目分离:comment 含 @INJECT 的条目(须 disabled 才生效,
        // 与 ST 原版语义一致)内容渲染后按位置插入消息数组,不进普通世界书注入。
        let mut inject_insertions: Vec<(InjectInsertion, String)> = Vec::new();
        let mut world_entries: Vec<WorldEntry> = Vec::new();
        for e in normal_entries.drain(..) {
            if let Some(spec) = parse_inject_insertion(&e.comment) {
                if !e.enabled {
                    let rendered = render_assistant_content_with(&e.content, &mut render_ctx);
                    if !rendered.trim().is_empty() {
                        inject_insertions.push((spec, rendered));
                    }
                }
                continue;
            }
            world_entries.push(e);
        }
        // 世界书分组:常态(位置3,并入 system 提示词)+ 激发(位置1,追加最新用户消息尾部)
        // @@ 装饰器(if/unless/var/set)与 injectPrompt 登记在分组阶段一并处理
        let mut world = collect_world_text_grouped_with(&world_entries, &history, &mut render_ctx);
        // GENERATE 注入分类:BEFORE 拼 system 开头、AFTER 拼 system 末尾;
        // GENERATE:idx / GENERATE:REGEX 转 @INJECT 插入消息数组。
        // RENDER:BEFORE / RENDER:AFTER 与 GENERATE 同路并入 system 首/尾(酒馆原版
        // RENDER 仅影响显示渲染、不影响生成;kedai 无独立显示渲染管道,故并入生成注入,
        // 语义差异见 plan6-ecosystem-devtools.md 6a 实施记录,前端显示渲染留扩展位)。
        let mut generate_before: Vec<String> = Vec::new();
        let mut generate_after: Vec<String> = Vec::new();
        for ge in &generate_entries {
            match &ge.tag {
                crate::parsing::assistant::InjectTag::GenerateBefore
                | crate::parsing::assistant::InjectTag::RenderBefore => {
                    generate_before.push(ge.rendered.clone());
                }
                crate::parsing::assistant::InjectTag::GenerateAfter
                | crate::parsing::assistant::InjectTag::RenderAfter => {
                    generate_after.push(ge.rendered.clone());
                }
                crate::parsing::assistant::InjectTag::GenerateIndex { idx, before } => {
                    // 第 idx 条消息(0-based,非 system)的开头/结尾 = 该位置前/后插入
                    let pos = if *before { *idx as i64 } else { *idx as i64 + 1 };
                    inject_insertions.push((
                        InjectInsertion::Pos {
                            pos,
                            role: "user".into(),
                        },
                        ge.rendered.clone(),
                    ));
                }
                crate::parsing::assistant::InjectTag::GenerateRegex { pattern, .. } => {
                    // 匹配到的消息之后插入(该消息的回应素材)
                    inject_insertions.push((
                        InjectInsertion::Regex {
                            pattern: pattern.clone(),
                            at: InjectAt::After,
                            role: "user".into(),
                        },
                        ge.rendered.clone(),
                    ));
                }
                _ => {
                    // InitialVariables 由 init 阶段处理(collect_generate_entries_with 已分流,
                    // 不会进 generate_entries);保留分支作防御,静默跳过不影响其余注入。
                }
            }
        }
        // injectPrompt 登记(ST-Prompt-Template):模板内 injectPrompt(key, prompt, order?, sticky?, uid?)
        // 登记的注入提示词,按 order/position 排序后并入 generate_after(system 尾部,计入 protected_tail)。
        let mut injected_prompts: Vec<_> = std::mem::take(&mut render_ctx.data.injected);
        injected_prompts.sort_by_key(|p| (p.order, p.position));
        for p in injected_prompts {
            if !p.prompt.trim().is_empty() {
                generate_after.push(p.prompt);
            }
        }
        // 释放渲染上下文(解除对 rctx.assistant_vars 的可变借用,后续阶段可再借用)
        drop(render_ctx);
        drop(scopes_guard);
        // 渲染副作用(世界书条目/装饰器 EJS 写树)同步进 scopes chat 镜像,
        // 供消息构建宏展开读合并视图时读到最新树
        rctx.scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sync_chat_tree(rctx.assistant_vars);
        // 本轮初始变量树(两步生成状态栏 diff 的基准:对比本轮开始与结束时)
        let initial_vars_tree = rctx.assistant_vars.tree().clone();
        // mvu 变量状态注入位置(system / user_tail,缓存友好模式见 build_llm_messages_with_position)
        let mvu_vars_position = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .mvu_vars_position
            .clone();
        // 变量树自动注入:角色卡/世界书未提供 {{format_message_variable}} 状态条目时,
        // 模型看不到任何状态 → 不会输出 <UpdateVariable>。此处把 stat_data 与更新协议
        // 作为一条状态注入并入世界书(状态块位置跟随 mvu_vars_position:system → 常态组
        // system 角色进 system;user_tail → 激发组 user 角色进最新 user 消息尾部,
        // 变量更新只改变最后一条 user 消息,system + 早期历史前缀保持稳定 → 前缀缓存友好)。
        let state_role = if mvu_vars_position == "user_tail" {
            "user"
        } else {
            "system"
        };
        // P5:契约驱动的状态块(存在契约时按 dueFields 裁剪;无契约走整树兼容层)。
        // turn_id 以历史消息数为近似轮次(每轮追加两条消息,与 generate_mvu_status 同源)。
        let contract = self.load_character_contract(&req.character_id);
        if let Some(block) = make_state_block_with_contract(
            rctx.assistant_vars,
            state_role,
            contract.as_ref(),
            history.len() as u64,
        ) {
            if mvu_vars_position == "user_tail" {
                world.triggered.push(block);
            } else {
                world.constant.push(block);
            }
        }
        let history_tuples: Vec<(String, String)> = projected.tuples;
        let history_summary = projected.summary;
        // 自定义 Agent 系统提示词(设置里编辑;为空则用内置默认)
        let settings_snapshot = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .agent_system_prompt
            .clone();
        let custom_prompt = if settings_snapshot.trim().is_empty() {
            None
        } else {
            Some(settings_snapshot)
        };
        // 反思提示词(空 = 机械规则检查;非空 = 反思步骤调用 LLM 判定)
        let reflect_prompt = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reflect_prompt
            .clone();
        // 反思失败建议的补充说明(可选;主体建议由引擎自动生成,见 reflect_integration):
        // 反思未通过放弃重试时,附在自动建议之后;注入角色(user/assistant;system 钳制为 user)
        let reflect_advice_supplement = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reflect_advice_prompt
            .clone();
        let reflect_advice_role = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reflect_advice_role
            .clone();
        // 提示词注入:配置快照已提前获取(inject_snapshot);角色卡个性/情景(供宏)
        let (personality, scenario) = match character
            .as_ref()
            .and_then(|c| c.data_raw.as_ref())
        {
            Some(raw) => (
                raw.get("personality")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                raw.get("scenario")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            None => (String::new(), String::new()),
        };
        // 位置0 预设尾部提示词与注入角色(空 = 禁用;system 在尾部钳制为 user)
        let preset_tail_snapshot = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .preset_tail_prompt
            .clone();
        let preset_tail = if preset_tail_snapshot.trim().is_empty() {
            None
        } else {
            Some(preset_tail_snapshot)
        };
        let preset_tail_role = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .preset_tail_role
            .clone();
        CollectedCtx {
            chara_name,
            chara_desc,
            personality,
            scenario,
            history,
            history_tuples,
            history_summary,
            custom_prompt,
            inject_snapshot,
            reflect_prompt,
            reflect_advice_supplement,
            reflect_advice_role,
            preset_tail,
            preset_tail_role,
            initial_vars_tree,
            world_constant: world.constant,
            world_triggered: world.triggered,
            inject_insertions,
            generate_before,
            generate_after,
        }
    }

    /// 阶段 2「消息构建收尾」:基于收集的上下文组装 LLM 消息(系统提示 + 历史)、
    /// 注入运行时主提示词与 GENERATE/@INJECT 条目、按上下文窗口裁剪并统计发送侧
    /// 用量,最后把发送统计写入会话宏变量。结果写入 rctx.llm_messages。
    /// 对应 run_body 内「构建 LLM 消息」段的构建部分;L2 中层定位:上下文 → 可下发消息的转换。
    fn finalize_messages(
        &self,
        req: &AgentRunRequest,
        session_id: &str,
        ctx: &CollectedCtx,
        rctx: &mut RunContext<'_>,
    ) {
        // 反思失败建议(位置0):本轮初始构建时恒为空(未失败/未生成),后续失败才注入
        let mut scopes_guard = rctx.scopes.lock().unwrap_or_else(|e| e.into_inner());
        let (messages, mut protected_tail) = build_llm_messages_with_position(
            &ctx.chara_name,
            &ctx.chara_desc,
            &ctx.personality,
            &ctx.scenario,
            &ctx.world_constant,
            &ctx.world_triggered,
            &ctx.history_tuples,
            ctx.custom_prompt.as_deref(),
            Some(&ctx.inject_snapshot),
            ctx.preset_tail.as_deref(),
            &ctx.preset_tail_role,
            None,
            &ctx.reflect_advice_role,
            rctx.session_vars,
            rctx.assistant_vars,
            Some(&mut *scopes_guard),
        );
        drop(scopes_guard);
        // 会话变量(session_vars 表):宏 {{setvar}}/{{addvar}} 写入、{{getvar}} 读取;
        // 展开过程中可能产生新变量,构建完成后写回持久化
        self.sessions.save_session_vars(session_id, rctx.session_vars);
        let mut llm_messages = messages;
        // 运行时主 Agent 提示词(AGENTS_RUNTIME.md)注入到 system 消息开头,
        // 作为最高层约定(角色定位/创作原则/工具使用原则/输出纪律),其余内容随其后。
        // 宏 {{char}} 等已由 build 阶段对 system 展开,此处为外层拼接,不再二次展开。
        match self.runtime_prompt.read_optional() {
            Ok(Some(rt)) => {
                // 替换角色/用户宏为实际值(其余宏已在 build 阶段对 system 展开,此处仅做角色替换)
                let rt = rt
                    .replace("{{char}}", &ctx.chara_name)
                    .replace("{{character_name}}", &ctx.chara_name)
                    .replace("{{user}}", "用户");
                if let Some(s0) = llm_messages.first_mut() {
                    if s0.role == "system" {
                        s0.content = format!("{}\n\n{}", rt.trim(), s0.content);
                    }
                }
            }
            Ok(None) => {}
            Err(error) => logger::warn(
                "运行时主 Agent 提示词读取失败，已回退其余提示词层",
                &[("error", Value::String(error))],
            ),
        }
        // 历史压缩摘要:拼入 system 作为早期历史回顾(替代被压缩的原文段)。
        // 摘要一旦生成即固定(前缀缓存稳定);位于角色设定之后、GENERATE 注入之前。
        if let Some(summary) = &ctx.history_summary {
            let summary = summary.trim();
            if !summary.is_empty() {
                if let Some(s0) = llm_messages.first_mut() {
                    if s0.role == "system" {
                        s0.content.push_str(&format!("\n\n【早期对话摘要】\n{summary}"));
                    }
                }
            }
        }
        // GENERATE 注入(ST-Prompt-Template 兼容):BEFORE 拼到 system 开头(角色内容之前,
        // 运行时主提示词之后,保持系统契约首位);AFTER 拼到 system 末尾(计入 protected_tail,
        // 防上下文裁剪先于注入被切掉)。
        if !ctx.generate_before.is_empty() || !ctx.generate_after.is_empty() {
            if let Some(s0) = llm_messages.first_mut() {
                if s0.role == "system" {
                    if !ctx.generate_after.is_empty() {
                        let tail = ctx.generate_after.join("\n\n");
                        protected_tail += tail.chars().count();
                        s0.content.push_str(&format!("\n\n{tail}"));
                    }
                    if !ctx.generate_before.is_empty() {
                        let head = ctx.generate_before.join("\n\n");
                        s0.content = format!("{head}\n\n{}", s0.content);
                    }
                }
            }
        }
        // @INJECT / GENERATE:idx / GENERATE:REGEX 精确消息插入:在上下文裁剪之前应用
        // (位置以构建期消息数组为准;裁剪后再插入会被切掉或定位漂移)。
        if !ctx.inject_insertions.is_empty() {
            apply_inject_insertions(&mut llm_messages, &ctx.inject_insertions);
        }
        // 按上下文窗口裁剪历史(始终保留角色系统提示;从最旧消息起丢弃;
        // system 超限时优先保留尾部注入块 protected_tail)
        {
            let mut ts = self.token_service.lock().unwrap_or_else(|e| e.into_inner());
            trim_to_context(
                &mut llm_messages,
                req.max_context_tokens,
                &mut ts,
                &self.model(),
                protected_tail,
            );
        }
        rctx.total_usage.context_tokens = {
            let mut ts = self.token_service.lock().unwrap_or_else(|e| e.into_inner());
            ts.count_message_tokens(&llm_messages, &self.model())
        };
        // 发送统计(ST-Prompt-Template 兼容):LAST_SEND_TOKENS / LAST_SEND_CHARS 记入
        // 会话宏变量,供模板 `{{getvar::LAST_SEND_TOKENS}}` 读取。写入宏表而非变量树,
        // 避免污染 {{format_message_variable}} 的状态输出(原版即「不参与树渲染的特殊变量」)。
        self.record_send_stats(
            rctx.session_vars,
            &llm_messages,
            rctx.total_usage.context_tokens,
            session_id,
        );
        *rctx.llm_messages = llm_messages;
    }

    /// 阶段 3「步骤循环」:按计划顺序执行每个步骤(反思判定 / 工具循环 / 生成),
    /// 维护 attempt 与反思重试计数、custom 模式变量基线、mergeUsage 累计。
    /// 返回 (content, custom_vars_snapshot);中断时提前结束循环。
    /// 对应 run_body 内「2. 执行阶段」段;L2 中层定位:计划 → 生成内容的推进循环。
    #[allow(clippy::too_many_arguments)]
    async fn step_loop(
        &self,
        req: &AgentRunRequest,
        ctx: &CollectedCtx,
        plan: &Plan,
        state_machine: &mut StateMachine,
        agent_session: &AgentSessionRecord,
        user_input: &str,
        tool_ctx: &ToolContext,
        tx: &mpsc::Sender<SseEvent>,
        abort: &watch::Receiver<bool>,
        flag: &AbortFlag,
        session_id: &str,
        run_id: &uuid::Uuid,
        rctx: &mut RunContext<'_>,
    ) -> Result<
        (
            String,
            Option<Value>,
            Vec<crate::contracts::ChangelogEntry>,
            Vec<crate::contracts::PatchOp>,
        ),
        String,
    > {
        let mut content = String::new();
        // custom 模式:每步生成后立即应用的变量树快照(收尾据此落库 extra.mvu,
        // 避免内容已剥离后无法再解析出补丁)
        let mut custom_vars_snapshot: Option<Value> = None;
        // P5:custom 模式各步即时应用产生的契约 changelog 条目与 pending(收尾统一
        // commit 到 kaleido_state/kaleido_changelog;无契约时两者恒空)
        let mut custom_contract_entries: Vec<crate::contracts::ChangelogEntry> = Vec::new();
        let mut custom_contract_pending: Vec<crate::contracts::PatchOp> = Vec::new();
        // custom 模式:各生成步骤开始前的变量树基线——反思回退重生成时恢复基线,
        // 避免 delta 等非幂等补丁被重复应用导致数值翻倍(回退步骤已应用的补丁随内容一并丢弃)
        let mut step_vars_baseline: HashMap<usize, AssistantVars> = HashMap::new();
        let mut attempt: usize = 0;
        // 反思重试独立计数(兜底防护:即使回退路径存在缺陷,反思重试也有硬上限,绝不无限循环)
        let mut reflect_retries: usize = 0;
        let max_attempts = if req.mode == "deep" || req.mode == "agent" || req.mode == "custom" {
            3
        } else {
            1
        };
        let mut tool_triggered = false;

        let mut idx = 0usize;
        // 反思重试回退时,丢弃上一个 direct 步骤里工具循环追加的中间消息
        let base_len = rctx.llm_messages.len();
        while idx < plan.steps.len() {
            check_aborted(abort)?;
            let step = &plan.steps[idx];
            // 自定义流程步骤进度(SSE Step 事件的 index/total;其余模式不携带)
            let progress = if req.mode == "custom" {
                (Some(idx + 1), Some(plan.steps.len()))
            } else {
                (None, None)
            };

            if step.action == "reflect" {
                // ===== 反思阶段 =====
                // 简单模式字数要求(如「输出约 1200 字」):机械规则按此校验明显偏短,
                // 与 LLM 反思提示词(用户可配)互补;未启用时无字数约束
                let min_chars = if ctx.inject_snapshot.mode == InjectMode::Simple
                    && ctx.inject_snapshot.simple.word_count_enabled
                    && ctx.inject_snapshot.simple.word_count > 0
                {
                    Some(ctx.inject_snapshot.simple.word_count as usize)
                } else {
                    None
                };
                state_machine.transition(AgentState::Reflecting, session_id)?;
                self.agent_sessions
                    .update(&agent_session.id, Some("reflecting"), None, None, None)
                    .map_err(|e| e.to_string())?;
                send_event(
                    step_evt(
                        "反思中…",
                        Some("检查输出质量与连贯性".into()),
                        progress.0,
                        progress.1,
                    ),
                    tx,
                    abort,
                    flag,
                )
                .await?;
                // 反思判定前剥离 mvu <UpdateVariable> 块:变量补丁不参与质量检查,
                // 含补丁时视为有效输出(纯变量更新响应不再被规则 1/3 误判而白重试)。
                let (mut reflect_text, reflect_patches) = parse_update_variable(&content);
                let has_updates = !reflect_patches.is_empty();
                // 反思加强:禁词检测与工具修正。正文(剥离补丁块后)含启用禁词时,
                // 直接调用 censor_text 修改工具做删除/同义替换,而非仅靠提示词约束或
                // 重生成(模型重生成可能再次写出同一批禁词)。修正只作用于正文,
                // <UpdateVariable> 补丁块原样保留,避免误伤 JSONPatch 中的变量键/值。
                // fast 模式不带工具,维持既有「仅自省提示预防」策略。
                if req.mode != "fast" && !reflect_text.trim().is_empty() {
                    let simple = &ctx.inject_snapshot.simple;
                    if simple.banned_words_enabled {
                        let words = simple.banned_words_extract();
                        let hits: Vec<&String> = words
                            .iter()
                            .filter(|w| !w.trim().is_empty() && reflect_text.contains(w.trim()))
                            .collect();
                        if !hits.is_empty() {
                            let entries: Vec<serde_json::Value> = hits
                                .iter()
                                .map(|w| json!({ "word": w.trim(), "replacement": "" }))
                                .collect();
                            let cctx = ToolContext {
                                session_id: session_id.to_string(),
                                character_id: req.character_id.clone(),
                            };
                            let args = json!({ "text": reflect_text, "entries": entries });
                            match self
                                .tool_registry
                                .execute("censor_text", &args.to_string(), cctx)
                                .await
                            {
                                Ok(censored)
                                    if !censored.trim().is_empty() && censored != reflect_text =>
                                {
                                    let words_text = hits
                                        .iter()
                                        .map(|w| w.trim())
                                        .collect::<Vec<_>>()
                                        .join("、");
                                    content = rebuild_content_keeping_blocks(&content, &censored);
                                    reflect_text = censored;
                                    send_event(
                                        step_evt(
                                            "禁词修正",
                                            Some(format!(
                                                "检测到禁词「{words_text}」,已用 censor_text 修正正文"
                                            )),
                                            progress.0,
                                            progress.1,
                                        ),
                                        tx,
                                        abort,
                                        flag,
                                    )
                                    .await?;
                                    logger::agent_step(
                                        session_id,
                                        "censor",
                                        Some(&format!("反思修正禁词:{words_text}")),
                                    );
                                }
                                Ok(_) => {}
                                Err(e) => logger::warn(
                                    "反思禁词修正失败,保留原文",
                                    &[("error", Value::String(e))],
                                ),
                            }
                        }
                    }
                }
                // 反思双轨:配置了反思提示词且正文非空 → 调用 LLM 按提示词判定
                // (输出无法解析/调用失败回退机械规则,反思路径永远可判定、有界);
                // 未配置或纯变量更新(正文为空)→ 机械规则(含补丁放行)。
                // 第四点·阶段 C:反思模型带文本修正工具(censor_text / revise_passage),
                // 发现禁词/不合理段落时自主定点修正正文,修正结果写回 content。
                let verdict = if !ctx.reflect_prompt.trim().is_empty()
                    && !reflect_text.trim().is_empty()
                {
                    let reflect_tool_ctx = ToolContext {
                        session_id: session_id.to_string(),
                        character_id: req.character_id.clone(),
                    };
                    match reflect_with_tools(
                        self,
                        &ctx.reflect_prompt,
                        user_input,
                        &reflect_text,
                        &reflect_tool_ctx,
                        abort,
                    )
                    .await
                    {
                        Some((v, revised, u)) => {
                            rctx.total_usage.prompt_tokens += u.prompt_tokens;
                            rctx.total_usage.completion_tokens += u.completion_tokens;
                            rctx.total_usage.total_tokens += u.total_tokens;
                            rctx.total_usage.prompt_cache_hit_tokens += u.prompt_cache_hit_tokens;
                            // 模型自主修正的正文写回(保留 <UpdateVariable> 补丁块,
                            // 与既有禁词修正的 rebuild_content_keeping_blocks 同语义)
                            if let Some(revised_text) = revised {
                                let trimmed = revised_text.trim().to_string();
                                if !trimmed.is_empty() && trimmed != reflect_text {
                                    content = rebuild_content_keeping_blocks(&content, &trimmed);
                                    reflect_text = trimmed;
                                    send_event(
                                        step_evt(
                                            "反思修正",
                                            Some("反思模型已定点修正正文段落".into()),
                                            progress.0,
                                            progress.1,
                                        ),
                                        tx,
                                        abort,
                                        flag,
                                    )
                                    .await?;
                                }
                            }
                            v
                        }
                        None => reflect(
                            &reflect_text,
                            user_input,
                            attempt,
                            max_attempts,
                            has_updates,
                            min_chars,
                        ),
                    }
                } else {
                    reflect(
                        &reflect_text,
                        user_input,
                        attempt,
                        max_attempts,
                        has_updates,
                        min_chars,
                    )
                };
                logger::agent_step(session_id, "reflect", Some(&verdict.reason));
                if verdict.passed {
                    send_event(
                        step_evt(
                            "反思通过",
                            Some(verdict.reason.clone()),
                            progress.0,
                            progress.1,
                        ),
                        tx,
                        abort,
                        flag,
                    )
                    .await?;
                    idx += 1;
                    continue;
                }
                send_event(
                    step_evt(
                        "反思未通过",
                        Some(verdict.reason.clone()),
                        progress.0,
                        progress.1,
                    ),
                    tx,
                    abort,
                    flag,
                )
                .await?;
                // 放弃条件:retry_action=stop(规则在 attempt 达上限时返回 stop)或反思重试达硬上限。
                // 不再检查 attempt >= max_attempts:多生成步骤流程(如 custom 多步)中 attempt 被
                // 生成步骤累计,反思重试次数应独立由 reflect_retries 限制(有界性不变)。
                if verdict.retry_action == Some("stop") || reflect_retries >= max_attempts {
                    // 达到重试上限:放弃反思,继续后续步骤(保证任何路径下有界)。
                    // 反思失败建议(位置0):由引擎自动生成(≤200 token)并注入到位置1 激发之后、
                    // 预设尾部之前;生成失败时仅存可选的用户补充说明。角色沿用配置(user/assistant)。
                    if let Some(a) = build_reflect_advice(
                        self,
                        &verdict.reason,
                        user_input,
                        &reflect_text,
                        &ctx.reflect_advice_supplement,
                        abort,
                        rctx.total_usage,
                    )
                    .await
                    {
                        // 同一 run 生效:注入位置0(预设尾部之前),后续步骤生成可见
                        inject_reflect_advice(
                            rctx.llm_messages,
                            &a,
                            &ctx.reflect_advice_role,
                            ctx.preset_tail.as_deref(),
                        );
                    }
                    idx += 1;
                    continue;
                }
                reflect_retries += 1;
                send_event(
                    step_evt(
                        "重新生成",
                        Some(format!("第 {}/{} 次重试", reflect_retries, max_attempts)),
                        progress.0,
                        progress.1,
                    ),
                    tx,
                    abort,
                    flag,
                )
                .await?;
                // 回退到上一个会生成内容的 direct 步骤重新生成。
                // 旧实现 while idx > 0 && plan.steps[idx - 1].action != "direct" 有缺陷:
                // plan 中反思步骤前一个 direct 步骤恒为「理解意图」(generates=false,不生成、
                // 不递增 attempt),条件不成立导致 idx 原地打转 → attempt 永不递增 →
                // 反思失败进入无限紧密循环(日志佐证:同一会话每毫秒一条 reflect)。
                match retreat_to_generating_step(&plan.steps, idx) {
                    Some(target) => {
                        // custom 模式即时应用语义:回退重生成会丢弃该步骤起的内容,
                        // 一并回滚其已应用的变量补丁(基线恢复 + Vars 事件同步前端);
                        // 否则 delta 补丁会被重生成重复应用,变量值翻倍。
                        if req.mode == "custom" {
                            if let Some(baseline) = step_vars_baseline.get(&target).cloned() {
                                *rctx.assistant_vars = baseline;
                                custom_vars_snapshot = None;
                                let tree = rctx.assistant_vars.tree().clone();
                                let _ = send_event(
                                    SseEvent::Vars { stat_data: tree },
                                    tx,
                                    abort,
                                    flag,
                                )
                                .await;
                                step_vars_baseline.retain(|&k, _| k <= target);
                            }
                        }
                        idx = target;
                    }
                    None => {
                        // 找不到可回退的生成步骤(理论不可达):放弃反思继续。
                        // 同样自动生成反思失败建议(位置0,激发之后、预设尾部之前)
                        if let Some(a) = build_reflect_advice(
                            self,
                            &verdict.reason,
                            user_input,
                            &reflect_text,
                            &ctx.reflect_advice_supplement,
                            abort,
                            rctx.total_usage,
                        )
                        .await
                        {
                            // 同一 run 生效:注入位置0(预设尾部之前),后续步骤生成可见
                            inject_reflect_advice(
                                rctx.llm_messages,
                                &a,
                                &ctx.reflect_advice_role,
                                ctx.preset_tail.as_deref(),
                            );
                        }
                        idx += 1;
                        continue;
                    }
                }
                // 丢弃上一个 direct 步骤的工具调用中间消息,干净地重新生成
                rctx.llm_messages.truncate(base_len);
                continue;
            }

            // ===== 执行步骤(direct / tool)=====
            state_machine.transition(AgentState::Executing, session_id)?;
            self.agent_sessions
                .update(
                    &agent_session.id,
                    Some("executing"),
                    None,
                    Some((idx + 1) as i64),
                    None,
                )
                .map_err(|e| e.to_string())?;
            send_event(
                step_evt("执行中…", Some(step.goal.clone()), progress.0, progress.1),
                tx,
                abort,
                flag,
            )
            .await?;

            // 非 AGENT/CUSTOM 模式:工具步骤与计算启发式保留原行为
            if req.mode != "agent" && req.mode != "custom" {
                if step.generates == Some(false) {
                    maybe_run_tool(
                        self,
                        state_machine,
                        &mut tool_triggered,
                        &agent_session.id,
                        session_id,
                        user_input,
                        tool_ctx,
                        rctx.llm_messages,
                        tx,
                        abort,
                        flag,
                    )
                    .await?;
                    idx += 1;
                    continue;
                }

                maybe_run_tool(
                    self,
                    state_machine,
                    &mut tool_triggered,
                    &agent_session.id,
                    session_id,
                    user_input,
                    tool_ctx,
                    rctx.llm_messages,
                    tx,
                    abort,
                    flag,
                )
                .await?;
            } else if step.generates == Some(false) {
                // AGENT/CUSTOM 模式:计划中的「理解意图」类步骤不生成,直接进入下一步;
                // 是否调用工具完全由模型通过 function calling 自主决定(或按步骤工具配置)
                idx += 1;
                continue;
            }
            attempt += 1;
            // custom 模式:记录该生成步骤开始前的变量树基线(反思回退时恢复用)
            if req.mode == "custom" {
                step_vars_baseline.insert(idx, rctx.assistant_vars.clone());
            }
            // 步骤级消息视图(agent/deep/custom 统一):步骤 system_prompt 宏展开后以
            // 「[本步指令]」追加到 system 末尾;agent 模式额外在末尾注入动态工具指南。
            // 独立视图不污染共享 llm_messages,反思回退后按步骤重建、无中间消息残留。
            // 计划二:展开前同步 scopes 镜像,保证 getvar/get_*_variable 读到本步骤
            // 之前的 setvar 副作用(步骤循环内多次展开,回合边界同步不够)。
            {
                let mut s = rctx.scopes.lock().unwrap_or_else(|e| e.into_inner());
                s.sync_chat_tree(rctx.assistant_vars);
                s.sync_chat_flat(rctx.session_vars);
            }
            let mut step_msgs = rctx.llm_messages.clone();
            if step
                .system_prompt
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
            {
            {
                let mut scopes_guard = rctx.scopes.lock().unwrap_or_else(|e| e.into_inner());
                let mut mctx = MacroCtx {
                    character_name: &ctx.chara_name,
                    character_description: &ctx.chara_desc,
                    user_name: "用户",
                    user_input,
                    personality: &ctx.personality,
                    scenario: &ctx.scenario,
                    history: &ctx.history_tuples,
                    vars: &mut *rctx.session_vars,
                    assistant_vars: Some(&mut *rctx.assistant_vars),
                    scopes: Some(&mut *scopes_guard),
                };
                step_msgs = with_step_prompt(rctx.llm_messages, step, &mut mctx);
            }
            }
            // 先计算本步骤实际下发的工具,再按 effective tools 注入指南。
            // Custom 的 null/[]/白名单语义只影响能力与永久授权,不能只做可见性过滤。
            let mut step_params = step_params_for(&req.params, step, &self.tool_registry);
            if req.mode == "agent" && step.tools.is_none() && step_params.tools.is_empty() {
                step_params.tools = req.params.tools.clone();
            }
            if matches!(req.mode.as_str(), "agent" | "custom") && !step_params.tools.is_empty() {
                if let Some(s) = step_msgs.first_mut() {
                    if s.role == "system" {
                        s.content.push_str(&format!(
                            "\n\n【可用工具】\n{}",
                            self.tool_registry.tool_guidance_for(&step_params.tools)
                        ));
                    }
                }
            }
            send_event(
                step_evt(
                    "生成中…",
                    Some(format!("第 {} 次尝试", attempt)),
                    progress.0,
                    progress.1,
                ),
                tx,
                abort,
                flag,
            )
            .await?;
            // AGENT 模式:完整 function calling 工具循环;custom 模式:按步骤派生参数
            // (温度/输出上限/工具白名单);三者统一使用上方构造的步骤级消息视图 step_msgs
            // (含 [本步指令] 与 agent 模式的动态工具指南),反思回退后按步骤重建。
            let result = if req.mode == "agent" {
                // agent 兼容旧行为:planner 未配步骤 tools 时使用请求级工具。
                let effective = step_params.clone();
                let whitelist = step.tools.as_deref();
                run_tool_loop(
                    self,
                    state_machine,
                    agent_session,
                    session_id,
                    &mut step_msgs,
                    &effective,
                    tool_ctx,
                    tx,
                    abort,
                    flag,
                    rctx.total_usage,
                    &run_id.to_string(),
                    whitelist,
                )
                .await?
            } else if req.mode == "custom" {
                let r = if step_params.tools.is_empty() {
                    execute_generation(self, session_id, &run_id.to_string(), &step_msgs, &step_params, tx, abort, flag).await?
                } else {
                    // custom 模式白名单:步骤配置了 tools 白名单时,名单内工具自动放行
                    let whitelist = step.tools.as_deref();
                    run_tool_loop(
                        self,
                        state_machine,
                        agent_session,
                        session_id,
                        &mut step_msgs,
                        &step_params,
                        tool_ctx,
                        tx,
                        abort,
                        flag,
                        rctx.total_usage,
                        &run_id.to_string(),
                        whitelist,
                    )
                    .await?
                };
                // 步骤提示词展开可能产生新变量:执行后写回持久化(与构建阶段一致)
                self.sessions.save_session_vars(session_id, rctx.session_vars);
                r
            } else {
                // deep / fast:应用步骤级消息视图([本步指令])与步骤级温度/输出上限(无工具)
                execute_generation(self, session_id, &run_id.to_string(), &step_msgs, &step_params, tx, abort, flag).await?
            };
            if result.interrupted {
                break;
            }
            // update_variables 通过工具注册表直接持久化当前会话变量树;工具循环结束后
            // 必须同步回引擎内存副本,否则后续文本协议或状态栏生成会用旧树覆盖工具结果。
            if req.mode == "custom" && !step_params.tools.is_empty() {
                let persisted_vars = self.sessions.load_assistant_vars(session_id);
                if persisted_vars.tree() != rctx.assistant_vars.tree() {
                    *rctx.assistant_vars = persisted_vars;
                    custom_vars_snapshot = Some(rctx.assistant_vars.tree().clone());
                }
            }
            content = result.content;
            // custom 模式:每步生成后立即解析并应用 mvu <UpdateVariable> 补丁。
            // 中间步骤的内容不保留(循环内被覆盖、不在收尾解析),延迟应用会丢;
            // 即时语义与 MagVarUpdate 原版一致(反思回退时已应用的补丁不回滚)。
            // P5:有契约时补丁先经契约门控(与收尾正文路径同构),被拒/低置信仅计
            // 数告警,applied 生效并生成 changelog 条目(收尾统一 commit)。
            if req.mode == "custom" {
                let (clean, patches) = parse_update_variable(&content);
                content = clean;
                let contract = self.load_character_contract(&req.character_id);
                let gated = crate::contracts::gate_assistant_patches_detailed(
                    contract.as_ref(),
                    &patches,
                    "agent",
                );
                if gated.rejected.len() > 0 || gated.pending.len() > 0 {
                    logger::warn(
                        "契约门控过滤了部分自定义模式正文补丁",
                        &[
                            ("rejected", json!(gated.rejected.len())),
                            ("pending", json!(gated.pending.len())),
                        ],
                    );
                }
                custom_contract_pending.extend(gated.pending);
                if contract.is_some() && !gated.applied.is_empty() {
                    let tree_before = rctx.assistant_vars.tree().clone();
                    if let Some(tree) = apply_mvu_patches(
                        self,
                        session_id,
                        rctx.assistant_vars,
                        &gated.applied,
                        tx,
                        abort,
                        flag,
                    )
                    .await
                    {
                        custom_vars_snapshot = Some(tree.clone());
                        custom_contract_entries.extend(
                            crate::contracts::entries_from_applied(
                                &tree_before,
                                &tree,
                                ctx.history.len() as u64,
                                &gated.applied_ops,
                                crate::contracts::ChangelogSource::Agent,
                            ),
                        );
                    }
                } else {
                    if let Some(tree) = apply_mvu_patches(
                        self,
                        session_id,
                        rctx.assistant_vars,
                        &gated.applied,
                        tx,
                        abort,
                        flag,
                    )
                    .await
                    {
                        custom_vars_snapshot = Some(tree);
                    }
                }
            }
            // mergeUsage:累加三个字段;context_tokens 固定为该轮上下文值
            rctx.total_usage.prompt_tokens += result.usage.prompt_tokens;
            rctx.total_usage.completion_tokens += result.usage.completion_tokens;
            rctx.total_usage.total_tokens += result.usage.total_tokens;
            rctx.total_usage.prompt_cache_hit_tokens += result.usage.prompt_cache_hit_tokens;
            idx += 1;
        }
        Ok((content, custom_vars_snapshot, custom_contract_entries, custom_contract_pending))
    }

    /// 规划阶段:状态机切入 Planning,更新 agent 会话状态为 planning,构建计划
    /// (custom 按配置步骤序列,其余模式按输入与模式生成),把 plan_goals 落库,
    /// 推送「计划中…」SSE 事件并检查中断。
    /// 对应 run_body 内「1. 规划阶段」段;L2 中层定位:引擎单阶段的编排封装。
    async fn plan_phase(
        &self,
        state_machine: &mut StateMachine,
        agent_session: &AgentSessionRecord,
        req: &AgentRunRequest,
        user_input: &str,
        tx: &mpsc::Sender<SseEvent>,
        abort: &watch::Receiver<bool>,
        flag: &AbortFlag,
        session_id: &str,
    ) -> Result<Plan, String> {
        state_machine.transition(AgentState::Planning, session_id)?;
        self.agent_sessions
            .update(&agent_session.id, Some("planning"), None, None, None)
            .map_err(|e| e.to_string())?;
        // custom 模式:按用户配置的步骤序列构建计划(路由层已校验;此处对配置被外部
        // 手改的情况兜底,失败直接中止本轮)
        let plan = if req.mode == "custom" {
            match make_custom_plan(req.flow.as_deref().unwrap_or(&[])) {
                Ok(p) => p,
                Err(e) => return Err(e),
            }
        } else {
            make_plan(user_input, &req.mode)
        };
        let plan_goals: Vec<String> = plan
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {}", i + 1, s.goal))
            .collect();
        self.agent_sessions
            .update(&agent_session.id, None, Some(&plan_goals), Some(0), None)
            .map_err(|e| e.to_string())?;
        send_event(
            step_evt("计划中…", Some(plan.summary.clone()), None, None),
            tx,
            abort,
            flag,
        )
        .await?;
        logger::agent_step(session_id, "plan", Some(&plan.summary));
        check_aborted(abort)?;
        Ok(plan)
    }

    /// 发送统计(ST-Prompt-Template 兼容):LAST_SEND_TOKENS / LAST_SEND_CHARS 写入会话
    /// 宏变量并落库,供模板 `{{getvar::LAST_SEND_TOKENS}}` 读取。写入宏表而非变量树,
    /// 避免污染 {{format_message_variable}} 的状态输出(原版即「不参与树渲染的特殊变量」)。
    /// 对应 run_body 内「发送统计」段;L2 中层定位:SSE 链路旁路的状态落库封装。
    fn record_send_stats(
        &self,
        session_vars: &mut HashMap<String, String>,
        llm_messages: &[LlmMessage],
        context_tokens: i64,
        session_id: &str,
    ) {
        let send_chars: usize = llm_messages.iter().map(|m| m.content.chars().count()).sum();
        session_vars.insert("LAST_SEND_TOKENS".to_string(), context_tokens.to_string());
        session_vars.insert("LAST_SEND_CHARS".to_string(), send_chars.to_string());
        self.sessions.save_session_vars(session_id, session_vars);
    }

    /// 接收统计(ST-Prompt-Template 兼容):LAST_RECEIVE_TOKENS / LAST_RECEIVE_CHARS 写入
    /// 会话宏变量并落库,下一轮模板可用 {{getvar::LAST_RECEIVE_TOKENS}} 读取。
    /// 先重新加载最新宏表再写回,与运行时序保持一致。
    /// 对应 run_body 内「接收统计」段;L2 中层定位:SSE 链路旁路的状态落库封装。
    fn record_receive_stats(&self, session_id: &str, completion_tokens: i64, clean_content: &str) {
        let mut vars = self.sessions.load_session_vars(session_id);
        vars.insert(
            "LAST_RECEIVE_TOKENS".to_string(),
            completion_tokens.to_string(),
        );
        vars.insert(
            "LAST_RECEIVE_CHARS".to_string(),
            clean_content.chars().count().to_string(),
        );
        self.sessions.save_session_vars(session_id, &vars);
    }

    /// 阶段六 6f:重生成时原地更新原 assistant 消息行,不新增行。
    /// 旧内容并入 extra.swipes 版本数组(含激活版本,元素 {swipe_id, content, ts}),
    /// swipe_id 指向新版本;与最后一条版本相同则不重复追加(避免重复生成同一文本)。
    /// 返回更新后的消息记录;锚点不存在或更新失败返回 Err。
    fn upsert_regenerated_message(
        &self,
        session_id: &str,
        regenerate_id: i64,
        content: &str,
        extra: &mut Value,
        ts: i64,
    ) -> Result<MessageRecord, String> {
        let original = self
            .sessions
            .get_message(session_id, regenerate_id)
            .ok_or_else(|| "重生成锚点消息不存在".to_string())?;
        // 已有版本数组则沿用(可能来自更早的重生成),否则以原内容为首版本
        let mut swipes: Vec<Value> = match original.extra.get("swipes").and_then(|s| s.as_array()) {
            Some(arr) => arr.clone(),
            None => vec![json!({ "swipe_id": 0, "content": original.content, "ts": ts })],
        };
        let last_content = swipes
            .last()
            .and_then(|s| s.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        if last_content != content {
            swipes.push(json!({ "swipe_id": swipes.len(), "content": content, "ts": ts }));
        }
        let swipe_id = swipes.len() - 1;
        extra["swipes"] = json!(swipes);
        extra["swipe_id"] = json!(swipe_id);
        self.sessions
            .update_message_full(session_id, regenerate_id, content, extra.clone())
            .ok_or_else(|| "重生成消息更新失败".to_string())
    }

    /// 阶段六 6g-1:非流式静默生成(复刻 generate_reflect_advice 模式)。
    /// 供后端脚本 TavernHelper.generate 与外部调用;不入聊天记录、不推 SSE。
    pub async fn generate_text(
        &self,
        messages: &[LlmMessage],
        params: GenerationParams,
        abort: watch::Receiver<bool>,
    ) -> Result<(String, TokenUsage), String> {
        if *abort.borrow() {
            return Err("生成已中断".into());
        }
        let connector = self.connector.read().await;
        let chunks = connector.generate(messages, params, abort).await?;
        drop(connector);
        let mut out = String::new();
        let mut usage = TokenUsage::default();
        for chunk in chunks {
            match chunk {
                LlmStreamChunk::Token(t) => out.push_str(&t),
                LlmStreamChunk::Usage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    prompt_cache_hit_tokens,
                } => {
                    usage.prompt_tokens += prompt_tokens;
                    usage.completion_tokens += completion_tokens;
                    usage.total_tokens += total_tokens;
                    usage.prompt_cache_hit_tokens += prompt_cache_hit_tokens;
                }
                _ => {}
            }
        }
        if out.trim().is_empty() {
            Err("生成返回空内容".into())
        } else {
            Ok((out, usage))
        }
    }

    /// 阶段六 6g-1:构建 generate 处理器(捕获 connector 的 Arc clone,不引用引擎)。
    /// 同步签名,内部经 Handle::current().block_on 完成异步生成(调用方须位于
    /// tokio 运行时线程内,如 run_character_scripts 的 spawn_blocking)。
    fn make_generate_handler(&self) -> Arc<crate::scripts::bridge::GenerateHandler> {
        let connector = self.connector.clone();
        Arc::new(move |messages, params, abort| {
            let connector = connector.clone();
            tokio::runtime::Handle::current()
                .block_on(async move {
                    let conn = connector.read().await;
                    let chunks = conn.generate(&messages, params, abort).await?;
                    drop(conn);
                    let mut out = String::new();
                    let mut usage = TokenUsage::default();
                    for chunk in chunks {
                        match chunk {
                            LlmStreamChunk::Token(t) => out.push_str(&t),
                            LlmStreamChunk::Usage {
                                prompt_tokens,
                                completion_tokens,
                                total_tokens,
                                prompt_cache_hit_tokens,
                            } => {
                                usage.prompt_tokens += prompt_tokens;
                                usage.completion_tokens += completion_tokens;
                                usage.total_tokens += total_tokens;
                                usage.prompt_cache_hit_tokens += prompt_cache_hit_tokens;
                            }
                            _ => {}
                        }
                    }
                    Ok((out, usage))
                })
        })
    }

    /// 阶段六 6g-2:构建导入处理器(捕获各 service 的 Arc clone,绕过 HTTP 直接调用)。
    /// 五类导入:character/worldbook/preset/chat/regex(暂不支持)。
    fn make_import_handler(&self) -> Arc<crate::scripts::bridge::ImportHandler> {
        let characters = self.characters.clone();
        let world_books = self.world_books.clone();
        let prompt_inject = self.prompt_inject.clone();
        let sessions = self.sessions.clone();
        Arc::new(
            move |kind: String, filename: String, content: String, session_id: String| {
                match kind.as_str() {
                    "character" => {
                        let rec = characters.upload(content.as_bytes(), &filename)?;
                        Ok(format!("已导入角色:{}", rec.id))
                    }
                    "worldbook" => {
                        let rec = world_books.upload(content.as_bytes(), &filename, None)?;
                        Ok(format!("已导入世界书:{}", rec.id))
                    }
                    "preset" => {
                        let floors = crate::parsing::preset::parse_st_preset(&content)?;
                        let mut svc = prompt_inject.lock().unwrap_or_else(|e| e.into_inner());
                        let mut cfg = svc.get().clone();
                        cfg.floors = floors.clone();
                        svc.set(cfg)?;
                        Ok(format!("已导入预设:{} 个楼层", floors.len()))
                    }
                    "chat" => {
                        if session_id.trim().is_empty() {
                            return Err("importRawChat 需指定会话(sessionId)".to_string());
                        }
                        let messages: Vec<crate::models::types::StMessage> =
                            serde_json::from_str(&content)
                                .map_err(|e| format!("解析聊天消息失败: {e}"))?;
                        let n = sessions.import_chat(&session_id, messages)?;
                        Ok(format!("已导入会话:{n} 条消息"))
                    }
                    "regex" => Err("importRawTavernRegex 暂不支持".to_string()),
                    other => Err(format!("未知导入类型:{other}")),
                }
            },
        )
    }

    /// 阶段三 3b-3:执行后端脚本(消息生成完成后)。
    /// 依次收集「全局脚本」与「角色卡脚本」的启用脚本,串行执行;
    /// 脚本经 TavernHelper 兼容桥写回共享 scopes(global/character/script 等),
    /// 由调用方收尾 take_others 统一落库。执行失败仅记日志,不影响主流程。
    async fn run_character_scripts(
        &self,
        character_id: &str,
        scopes: &Arc<Mutex<crate::parsing::scopes::ScopeVars>>,
    ) {
        // 全局脚本(scope=global,owner 恒为空)
        let mut all: Vec<crate::scripts::loader::LoadedScript> = Vec::new();
        if let Ok(g) = self.user_scripts.get_tree("global", "") {
            all.extend(crate::scripts::loader::collect_enabled_scripts(&g));
        }
        // 角色卡脚本(含旧字段迁移;角色不存在/无脚本树 → 跳过)
        if let Ok(t) = self.user_scripts.get_character(character_id) {
            all.extend(crate::scripts::loader::collect_enabled_scripts(&t));
        }
        if all.is_empty() {
            return;
        }
        let opts = crate::scripts::runtime::EvalOptions::default();
        // 阶段六 6g:生成/导入处理器在循环外构建一次(捕获最小依赖:connector 与各
        // service 的 Arc clone),所有脚本共享同一份接线。
        let generate_handler = self.make_generate_handler();
        let import_handler = self.make_import_handler();
        for script in all {
            // 每次执行新建 EvalBridge(共享 scopes + 角色 id);运行时为同步阻塞,
            // 经 spawn_blocking 隔离,避免阻塞 tokio 运行时(quickjs 非线程安全,
            // 每次执行独立 Runtime,天然无跨线程共享)。
            let bridge = crate::scripts::bridge::EvalBridge::new(scopes.clone(), script.clone())
                .with_character(character_id.to_string())
                .with_registry(self.slash.clone())
                .with_generate(generate_handler.clone())
                .with_imports(import_handler.clone());
            let source = script.content.clone();
            let opts = opts.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                crate::scripts::runtime::eval_with_bridge(&source, &opts, &bridge)
            })
            .await
            .unwrap_or_else(|_| {
                crate::scripts::runtime::EvalOutcome::Error("脚本执行任务被取消".into())
            });
            if let crate::scripts::runtime::EvalOutcome::Error(msg) = outcome {
                logger::warn(
                    "角色脚本执行失败",
                    &[
                        ("character_id", Value::String(character_id.to_string())),
                        ("script", Value::String(script.name)),
                        ("error", Value::String(msg)),
                    ],
                );
            }
        }
    }

    /// Token 累计统计落库:会话级 session_usage 累加 + 全局 global_usage 累加,
    /// 与 Node 版 usage 持久化语义一致。
    /// 对应 run_body 内「Token 累计统计」段;L2 中层定位:落库侧的单一职责封装。
    fn record_usage(&self, session_id: &str, total_usage: &TokenUsage) {
        let total = total_usage.prompt_tokens + total_usage.completion_tokens;
        let now = crate::models::db::now_iso();
        let conn = self.db.conn();
        let _ = conn.execute(
            "INSERT INTO session_usage (session_id, total_prompt, total_completion, total_tokens, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id) DO UPDATE SET
               total_prompt = total_prompt + ?2,
               total_completion = total_completion + ?3,
               total_tokens = total_tokens + ?4,
               updated_at = ?5",
            rusqlite::params![
                session_id,
                total_usage.prompt_tokens,
                total_usage.completion_tokens,
                total,
                now
            ],
        );
        let _ = conn.execute(
            "UPDATE global_usage SET
               total_prompt = total_prompt + ?1,
               total_completion = total_completion + ?2,
               total_tokens = total_tokens + ?3,
               updated_at = ?4
             WHERE id = 1",
            rusqlite::params![
                total_usage.prompt_tokens,
                total_usage.completion_tokens,
                total,
                now
            ],
        );
    }

    fn finish_run(&self, session_id: &str, run_id: uuid::Uuid) {
        finish_run_generation(&mut self.runs.lock().unwrap_or_else(|e| e.into_inner()), session_id, run_id);
    }
}

fn finish_run_generation(
    runs: &mut HashMap<String, RunHandle>,
    session_id: &str,
    run_id: uuid::Uuid,
) {
    if runs.get(session_id).is_some_and(|run| run.run_id == run_id) {
        runs.remove(session_id);
    }
}

#[cfg(test)]
mod run_generation_tests {
    use super::*;

    /// censor 修正后的正文替换,<UpdateVariable> 补丁块必须原样保留(大小写变体同样容错),
    /// 避免误伤 JSONPatch 中的变量键/值。
    #[test]
    fn rebuild_content_keeping_blocks_preserves_update_block() {
        let content = "正文含禁词示例。\n\n<UpdateVariable><JSONPatch>[{\"op\":\"replace\",\"path\":\"stat_data.世界.时间\",\"value\":[\"14:30\",\"初始\"]}]</JSONPatch></UpdateVariable>";
        let rebuilt = rebuild_content_keeping_blocks(content, "修正后的正文。");
        assert_eq!(
            rebuilt,
            "修正后的正文。<UpdateVariable><JSONPatch>[{\"op\":\"replace\",\"path\":\"stat_data.世界.时间\",\"value\":[\"14:30\",\"初始\"]}]</JSONPatch></UpdateVariable>"
        );
        assert!(!rebuilt.contains("禁词"), "正文应被替换:{rebuilt}");
    }

    #[test]
    fn rebuild_content_keeping_blocks_handles_lowercase_and_no_block() {
        // 小写变体标签:仍应保留块
        let lower = "正文。<updatevariable><JSONPatch>[{\"op\":\"add\"}]</JSONPatch></updatevariable>";
        let rebuilt = rebuild_content_keeping_blocks(lower, "新正文。");
        assert!(rebuilt.starts_with("新正文。"));
        assert!(rebuilt.contains("<updatevariable>"), "{rebuilt}");
        // 无补丁块:整体替换
        let plain = rebuild_content_keeping_blocks("旧正文禁词。", "新正文。");
        assert_eq!(plain, "新正文。");
    }

    #[test]
    fn old_run_finishing_does_not_remove_new_generation() {
        let session_id = "same-session";
        let old_run_id = uuid::Uuid::new_v4();
        let new_run_id = uuid::Uuid::new_v4();
        let (new_flag, _) = AbortFlag::new();
        let mut runs = HashMap::from([(
            session_id.to_string(),
            RunHandle {
                run_id: new_run_id,
                flag: new_flag,
                active: true,
            },
        )]);

        finish_run_generation(&mut runs, session_id, old_run_id);

        assert!(runs
            .get(session_id)
            .is_some_and(|run| run.run_id == new_run_id && run.active));
        finish_run_generation(&mut runs, session_id, new_run_id);
        assert!(!runs.contains_key(session_id));
    }
}
