// Agent 引擎核心编排服务(与 Node 版 agent.service.ts + executor.ts 对齐)
// 模块地图(巨型文件拆分后):
//   本文件        Engine 结构体、装配方法与 run 编排入口
//   types.rs      共享小类型:AbortFlag / AgentRunRequest / RunHandle / RunContext
//   util.rs       辅助小函数:SSE 事件构造与发送、错误分类、变量标签查找与内容重建
//   plan.rs       规划阶段(plan_phase)
//   messages/     LLM 消息构建(build)、@INJECT/反思建议/槽位插入(inject)、上下文裁剪(trim)、
//                 上下文收集与消息构建收尾(context: collect_context/finalize_messages)
//   run_loop.rs   主状态机循环 step_loop(步骤执行 / 反思回退 / custom 即时补丁)
//   run_scripts.rs 角色脚本执行(run_character_scripts 与 generate/import 处理器)
//   run_finish.rs 收尾落库(收/发统计、swipes 重生成更新、record_usage、run 登记清理)
//   executor.rs / mvu.rs / worldbook.rs / compaction.rs / reflector_integration.rs 见各自文件头
//                 (generate_text 在 executor 尾;压缩引擎方法在 compaction 尾;
//                  build_reflect_advice 在 reflector_integration 尾)
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
use crate::parsing::assistant::{parse_patch_array, parse_update_variable, AssistantVars, PatchOp};
use crate::parsing::macros::MacroCtx;
use crate::services::agent_session_service::AgentSessionService;
use crate::services::character_service::CharacterService;
use crate::services::prompt_inject_service::{InjectMode, PromptInjectService};
use crate::services::quick_reply_service::QuickReplyService;
use crate::services::runtime_prompt_service::RuntimePromptService;
use crate::services::session_service::SessionService;
use crate::services::settings_service::RuntimeSettings;
use crate::services::token_service::TokenService;
use crate::services::user_script_service::UserScriptService;
use crate::services::world_book_service::WorldBookService;
use crate::tools::registry::ToolRegistry;
use crate::utils::logging;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch, RwLock};

// executor 提为 pub(crate):任务引擎(services/task_engine)复用
// execute_generation/run_tool_loop(docs/任务引擎六模式.md 第三节)
pub(super) mod compaction;
pub(crate) mod executor;
pub(super) mod messages;
pub(super) mod mvu;
pub(super) mod plan;
pub(super) mod reflector_integration;
pub(crate) mod worldbook;
// 按职责拆分的子模块(纯代码移动):主状态机循环 / 角色脚本执行 / 收尾落库
pub(super) mod run_finish;
#[cfg(test)]
mod run_generation_tests;
pub(super) mod run_loop;
pub(super) mod run_scripts;
pub(super) mod types;
pub(super) mod util;

use self::executor::{execute_generation, maybe_run_tool, run_tool_loop};
use self::messages::{
    inject_reflect_advice, retreat_to_generating_step, step_params_for, trim_tool_history,
    with_step_prompt, CollectedCtx, TOOL_HISTORY_SUMMARY_PREFIX,
};
use self::mvu::{apply_mvu_patches, generate_mvu_status, strip_status_bar_tag};
use self::reflector_integration::{build_reflect_advice, reflect_with_tools};
pub use self::types::{AbortFlag, AgentRunRequest};
use self::types::{RunContext, RunHandle};
use self::util::{
    check_aborted, classify_engine_error, rebuild_content_keeping_blocks, send_event, step_evt,
};

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
    /// 跨会话记忆蒸馏(落地项 2):记忆槽注入与 touch 衰减回写
    memory: Arc<crate::services::memory_service::MemoryService>,
    /// 技能库(落地项 3 渐进披露):system 注入「name:description」紧凑清单
    skills: Arc<crate::services::skill_service::SkillService>,
    /// 契约注册表(character_id → Contract):与多步工具/API 写路径共享同一实例
    /// (AppState 构造注入),保证「改卡 → invalidate → 下轮重提取」的缓存一致性。
    pub(crate) contract_registry: Arc<crate::contracts::ContractRegistry>,
    /// 契约运行态服务(P5):收尾把 KaleidoState/changelog 提交到 SQLite
    kaleido_state: Arc<crate::services::kaleido_state_service::KaleidoStateService>,
    /// 脚本 generate 调度请求的发送端(优化项 B-2):首次执行角色脚本时惰性
    /// 建立常驻调度任务(装配期 AppState::new 是同步函数,不保证有 runtime 可
    /// tokio::spawn;run_character_scripts 为 async,执行时必在 runtime 内)。
    /// None = 尚未建立。
    generate_dispatch: Mutex<Option<mpsc::Sender<run_scripts::GenerateRequest>>>,
    runs: Mutex<HashMap<String, RunHandle>>,
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
        memory: Arc<crate::services::memory_service::MemoryService>,
        skills: Arc<crate::services::skill_service::SkillService>,
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
            memory,
            skills,
            generate_dispatch: Mutex::new(None),
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
    pub fn load_character_contract(
        &self,
        character_id: &str,
    ) -> Option<crate::contracts::Contract> {
        self.contract_registry.load(character_id)
    }

    /// 失效角色契约缓存(角色卡/世界书写路径调用;供 API 层转发)。
    pub fn invalidate_character_contract(&self, character_id: &str) {
        self.contract_registry.invalidate(character_id);
    }

    /// 当前生效模型(settings 切换模型后立即生效)
    pub fn model(&self) -> String {
        self.current_model
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 运行期设置快照:lock 后立即 clone 返回,锁中毒时 into_inner 恢复取值。
    /// 快照语义:不留锁跨 await —— 调用方拿到独立副本,锁在本函数内即释放;
    /// 引擎内各阶段(上下文收集/工具循环/压缩决策)一律经本方法读设置,
    /// 不再散点 .lock()。写路径由 API 层 settings_update 事务串行化后替换内存值。
    pub fn settings_snapshot(&self) -> RuntimeSettings {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 记忆召回用的查询向量(Phase 3):embedding 未启用/未配置/调用失败一律返回 None,
    /// 调用方据此降级为纯 Jaccard 召回——向量化是增强而非必需路径,绝不阻断对话。
    async fn recall_query_vector(&self, req: &AgentRunRequest) -> Option<Vec<f32>> {
        let settings = self.settings_snapshot();
        if !settings.embedding_enabled || req.user_input.trim().is_empty() {
            return None;
        }
        let svc = crate::services::embedding_service::EmbeddingService::new();
        match svc.embed_one(&settings, &req.user_input).await {
            Ok(v) if !v.is_empty() => Some(v),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(
                    error = e.to_string(),
                    "记忆召回查询向量生成失败,降级为纯 Jaccard"
                );
                None
            }
        }
    }

    /// 工具注册表全量定义(与聊天 agent 模式 GenerationParams.tools 同一来源;
    /// 任务引擎 solo 模式构建工具清单用,docs/任务引擎六模式.md 第三节)
    pub(crate) fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tool_registry.list_definitions()
    }

    /// 工具注册表句柄(pub(crate):任务模式规划器只读侦察循环执行白名单工具用,
    /// 问题②;与聊天引擎/任务执行器同一注册表,权限模型唯一——侦察循环经
    /// execute_with_decision 预放行白名单内只读工具,绕开 UI 授权等待)
    pub(crate) fn tool_registry(&self) -> Arc<ToolRegistry> {
        self.tool_registry.clone()
    }

    /// 运行时切换模型(更新 connector 内的 model;异步避免阻塞 runtime)
    pub async fn switch_model(&self, model: &str) {
        let mut c = self.connector.write().await;
        *c = crate::connectors::with_model(&c, model);
        *self.current_model.lock().unwrap_or_else(|e| e.into_inner()) = model.to_string();
    }

    pub fn is_active(&self, session_id: &str) -> bool {
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .map(|r| r.active)
            .unwrap_or(false)
    }

    pub fn stop(&self, session_id: &str) {
        if let Some(run) = self
            .runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
        {
            run.flag.abort();
            logging::agent_step(session_id, "interrupt", Some("用户请求停止生成"));
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
                tracing::error!(error = e, "Agent 会话初始化失败");
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
                scopes.lock().unwrap_or_else(|e| e.into_inner()).with_scope(
                    crate::parsing::scopes::Scope::Global,
                    "",
                    g,
                );
            }
            let mut llm_messages = Vec::new();
            let mut rctx = RunContext {
                assistant_vars: &mut assistant_vars,
                session_vars: &mut session_vars,
                scopes: scopes.clone(),
                llm_messages: &mut llm_messages,
                total_usage: &mut total_usage,
            };
            let ctx_data = self.collect_context(&req, &session_id, &mut rctx).await;
            // 记忆召回查询向量(Phase 3):在 async 上下文算好,传入同步的 finalize_messages。
            // embedding 未启用或调用失败返回 None → 召回自动降级为纯 Jaccard。
            let recall_query_vec = self.recall_query_vector(&req).await;
            let memory_touched = self.finalize_messages(
                &req,
                &session_id,
                &ctx_data,
                &mut rctx,
                recall_query_vec.as_deref(),
            );

            let tool_ctx = ToolContext {
                session_id: session_id.clone(),
                character_id: req.character_id.clone(),
                agent_depth: 0,
            };

            // ===== 2. 执行阶段 =====
            let (content, custom_vars_snapshot, custom_contract_entries, custom_contract_pending) =
                self.step_loop(
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

            // 记忆使用计数回写(落地项 2):生成主体已完成,本轮已注入内容不受影响;
            // 只动 usage_count/last_usage,为下一轮精选衰减提供数据。失败仅告警。
            if !memory_touched.is_empty() {
                if let Err(e) = self.memory.touch(&memory_touched) {
                    tracing::warn!(error = e, "记忆使用计数回写失败");
                }
            }

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
                logging::agent_step(&session_id, "interrupted", Some("生成被中止"));
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
                    let inject = self
                        .prompt_inject
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get()
                        .clone();
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
                                agent_depth: 0,
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
                                    tracing::warn!(error = e, "禁词替换工具调用失败,保留原文");
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
                    if !gated.rejected.is_empty() || !gated.pending.is_empty() {
                        tracing::warn!(
                            rejected = gated.rejected.len(),
                            pending = gated.pending.len(),
                            "契约门控过滤了部分正文变量补丁"
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
                            tracing::warn!(error = e, "变量+状态栏生成失败")
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
                        self.sessions.add_message(
                            &session_id,
                            "assistant",
                            &clean_content,
                            extra.clone(),
                        )
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
                            tracing::warn!(error = e, "assistant 消息落库失败");
                        }
                    }
                    // 阶段三 3b-3:角色卡后端脚本执行(消息生成完成后)。
                    // 读取角色卡 extensions.tavern_helper 脚本树,串行执行启用脚本;
                    // 脚本经 TavernHelper 兼容桥对 global/character/script 等作用域的
                    // 写回进入 rctx.scopes,由下方 take_others 一并落库。失败仅记日志。
                    self.run_character_scripts(&character_id, &rctx.scopes)
                        .await;
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
                            tracing::warn!(error = e, "契约运行态 meta 读取失败,按空 meta 继续");
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
                        tracing::warn!(
                            session_id = session_id.clone(),
                            error = e,
                            "契约运行态提交失败(生成结果不受影响)"
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
                self.record_receive_stats(
                    &session_id,
                    total_usage.completion_tokens,
                    &clean_content,
                );
                // Token 累计统计:会话 + 全局
                self.record_usage(&session_id, &total_usage).await;
                logging::agent_step(
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
                    logging::agent_step(&session_id, "error", Some(&e));
                }
            }
        }
        self.finish_run(&session_id, run_id);
        // LLM 请求快照保留策略(第四点·主题 A):每次 run 结束裁剪,仅保留最近 50 条。
        if let Err(e) = self.sessions.prune_llm_requests(&session_id, 50) {
            tracing::warn!(error = e, "LLM 请求快照裁剪失败");
        }
        final_out
    }
}
