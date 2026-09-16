// 上下文收集与消息构建收尾:collect_context(装载字符卡/历史/世界书/变量树与设置快照,
// 产出只读 CollectedCtx)与 finalize_messages(组装 LLM 消息、注入运行时主提示词与
// GENERATE/@INJECT 条目、按上下文窗口裁剪并统计发送侧用量)。
// (自 engine/mod.rs 拆分迁入,纯代码移动,逻辑不变)
// 可见性说明:两个方法与 CollectedCtx 原为 engine/mod.rs 内的私有条目,此处改为
// pub(in crate::agents::engine),可见范围与拆分前完全一致(引擎模块树内),未放宽。
use crate::agents::engine::compaction::{
    project_history, should_snip, snip_tuples, ProjectedHistory,
};
use crate::agents::engine::worldbook::{
    collect_world_text_grouped_with, make_state_block_with_contract, WorldInjection,
};
use crate::agents::engine::{AgentEngine, AgentRunRequest, RunContext};
use crate::models::types::MessageRecord;
use crate::parsing::assistant::{
    collect_generate_entries_with, collect_init_vars, render_assistant_content_with, CharacterCtx,
    PresetPromptCtx, RenderCtx, RenderCtxData,
};
use crate::parsing::world_book::WorldEntry;
use crate::services::prompt_inject_service::PromptInjectConfig;
use serde_json::Value;

use super::{
    append_memory_notice, apply_inject_insertions, build_llm_messages_with_position,
    insert_memory_slot, insert_recall_slot, insert_summary_slot, parse_inject_insertion,
    trim_to_context, InjectAt, InjectInsertion,
};

/// 阶段 2「上下文收集」的只读产物:字符卡/历史/世界书/设置快照等,
/// 供消息构建、步骤循环与收尾使用(不持有可变引用,可跨函数存活)。
pub(in crate::agents::engine) struct CollectedCtx {
    pub(in crate::agents::engine) chara_name: String,
    pub(in crate::agents::engine) chara_desc: String,
    pub(in crate::agents::engine) personality: String,
    pub(in crate::agents::engine) scenario: String,
    pub(in crate::agents::engine) history: Vec<MessageRecord>,
    pub(in crate::agents::engine) history_tuples: Vec<(String, String)>,
    /// 历史压缩摘要(可空):存在时拼入 system 作为早期历史回顾,替代被压缩的原文段
    pub(in crate::agents::engine) history_summary: Option<String>,
    pub(in crate::agents::engine) custom_prompt: Option<String>,
    pub(in crate::agents::engine) inject_snapshot: PromptInjectConfig,
    pub(in crate::agents::engine) reflect_prompt: String,
    pub(in crate::agents::engine) reflect_advice_supplement: String,
    pub(in crate::agents::engine) reflect_advice_role: String,
    pub(in crate::agents::engine) preset_tail: Option<String>,
    pub(in crate::agents::engine) preset_tail_role: String,
    pub(in crate::agents::engine) initial_vars_tree: Value,
    pub(in crate::agents::engine) world_constant: Vec<WorldInjection>,
    pub(in crate::agents::engine) world_triggered: Vec<WorldInjection>,
    pub(in crate::agents::engine) inject_insertions: Vec<(InjectInsertion, String)>,
    pub(in crate::agents::engine) generate_before: Vec<String>,
    pub(in crate::agents::engine) generate_after: Vec<String>,
}

impl AgentEngine {
    /// CollectedCtx 供消息构建、步骤循环与收尾使用;变量树初始化/渲染副作用就地生效
    /// (rctx.assistant_vars 为可变,条目分离与状态块注入同步更新)。
    /// 对应 run_body 内「构建 LLM 消息」段的收集部分;L2 中层定位:消息构建前的数据装配。
    pub(in crate::agents::engine) async fn collect_context(
        &self,
        req: &AgentRunRequest,
        session_id: &str,
        rctx: &mut RunContext<'_>,
    ) -> CollectedCtx {
        // 构建 LLM 消息(系统提示 + 历史;与 Node 版一致,history 不带 extra → system 全部跳过)
        // 开场白(first_mes)由 create_session/ensure_session 作为首条 assistant 消息写入会话,
        // 历史中天然包含,不再重复注入 system(避免同一段文本出现两次)
        // 同步 SQLite 读取(角色/历史/压缩摘要)合并挪进阻塞线程池(DB 并发改造)
        let (character, history, compaction) = {
            let characters = self.characters.clone();
            let sessions = self.sessions.clone();
            let character_id = req.character_id.clone();
            let sid = session_id.to_string();
            tokio::task::spawn_blocking(move || {
                let character = characters.get(&character_id);
                let history = sessions.get_messages(&sid);
                let compaction = sessions.get_compaction(&sid);
                (character, history, compaction)
            })
            .await
            .unwrap_or((None, Vec::new(), None))
        };
        // 历史压缩投影:读已存在的摘要(若曾压缩过),模型可见历史 = 摘要 + 截止点之后的原文。
        // 原文 history 保持不变,继续供 EJS 渲染与世界书分组读取完整历史。
        let projected: ProjectedHistory = project_history(&history, compaction);
        // snip 零成本裁剪档(缓存感知管线):auto 模式且历史 token 达 SNIP_THRESHOLD(0.6,
        // 先于 LLM 摘要档 0.8)时,把投影中陈旧的超长消息替换为占位符(尾部 2 条原文保留、
        // 错误特征保留)。只影响模型可见投影,不改数据库原文,与可逆投影设计一致。
        let (snip_mode, snip_bytes, snip_max_context) = {
            // 设置快照:不留锁跨 await
            let s = self.settings_snapshot();
            (
                s.compaction_mode.clone(),
                s.compaction_snip_bytes,
                s.max_context_tokens,
            )
        };
        let tuples = if snip_mode == "auto" && snip_bytes > 0 && !projected.tuples.is_empty() {
            let history_tokens = {
                let mut ts = self.token_service.lock().unwrap_or_else(|e| e.into_inner());
                let mut total: i64 = 0;
                for m in &history {
                    total += ts.count_tokens(&m.content, &self.model()) + 4;
                }
                total + 2
            };
            if should_snip(history_tokens, snip_max_context) {
                snip_tuples(&projected.tuples, snip_bytes as usize)
            } else {
                projected.tuples
            }
        } else {
            projected.tuples
        };
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
        // 世界书条目读取(同步 SQLite)挪进阻塞线程池(DB 并发改造)
        let world_entries_for_char = {
            let world_books = self.world_books.clone();
            let character_id = req.character_id.clone();
            tokio::task::spawn_blocking(move || {
                world_books.collect_entries_for_character(&character_id)
            })
            .await
            .unwrap_or_default()
        };
        entries.extend(world_entries_for_char);
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
                *rctx.assistant_vars = crate::parsing::assistant::AssistantVars::from_value(tree);
            }
            if !rctx.assistant_vars.is_empty() {
                // 初始变量树落库(同步 SQLite 写)挪进阻塞线程池(DB 并发改造)
                let save_result = {
                    let sessions = self.sessions.clone();
                    let sid = session_id.to_string();
                    let vars = rctx.assistant_vars.clone();
                    tokio::task::spawn_blocking(move || sessions.save_assistant_vars(&sid, &vars))
                        .await
                };
                if let Ok(Err(e)) | Err(e) = save_result.map_err(|e| e.to_string()) {
                    tracing::warn!(
                        session_id = session_id.to_string(),
                        error = e,
                        "初始变量树落库失败"
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
                    let pos = if *before {
                        *idx as i64
                    } else {
                        *idx as i64 + 1
                    };
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
        // 设置快照(快照语义:不留锁跨 await):本阶段要连续读 mvu 注入位置、
        // 自定义系统提示词、反思提示词、预设尾部等多个设置字段,取一次快照逐字段读,
        // 避免逐字段重复加锁。
        let settings_snap = self.settings_snapshot();
        // mvu 变量状态注入位置(system / user_tail,缓存友好模式见 build_llm_messages_with_position)
        let mvu_vars_position = settings_snap.mvu_vars_position.clone();
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
        // 模型可见历史 = 投影(摘要截止点之后)经 snip 零成本裁剪后的视图;
        // 原文 history(完整)继续供世界书/EJS 等读取
        let history_tuples: Vec<(String, String)> = tuples;
        let history_summary = projected.summary;
        // 自定义 Agent 系统提示词(设置里编辑;为空则用内置默认)
        // 扁平字段类型为 RoleplayPromptConfig(WP7 模式隔离):roleplay 权威值,.0 取字符串
        let agent_system_prompt = settings_snap.agent_system_prompt.0.clone();
        let custom_prompt = if agent_system_prompt.trim().is_empty() {
            None
        } else {
            Some(agent_system_prompt)
        };
        // 反思提示词(空 = 机械规则检查;非空 = 反思步骤调用 LLM 判定)
        let reflect_prompt = settings_snap.reflect_prompt.clone();
        // 反思失败建议的补充说明(可选;主体建议由引擎自动生成,见 reflect_integration):
        // 反思未通过放弃重试时,附在自动建议之后;注入角色(user/assistant;system 钳制为 user)
        let reflect_advice_supplement = settings_snap.reflect_advice_prompt.clone();
        let reflect_advice_role = settings_snap.reflect_advice_role.clone();
        // 提示词注入:配置快照已提前获取(inject_snapshot);角色卡个性/情景(供宏)
        let (personality, scenario) = match character.as_ref().and_then(|c| c.data_raw.as_ref()) {
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
        let preset_tail_snapshot = settings_snap.preset_tail_prompt.clone();
        let preset_tail = if preset_tail_snapshot.trim().is_empty() {
            None
        } else {
            Some(preset_tail_snapshot)
        };
        let preset_tail_role = settings_snap.preset_tail_role.clone();
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
    /// 返回本轮注入记忆槽的条目 id(供响应后 touch 衰减回写;未注入为空)。
    pub(in crate::agents::engine) fn finalize_messages(
        &self,
        req: &AgentRunRequest,
        session_id: &str,
        ctx: &CollectedCtx,
        rctx: &mut RunContext<'_>,
        recall_query_vec: Option<&[f32]>,
    ) -> Vec<i64> {
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
        //
        // 同步落库让出 async worker(2026-09-16 性能批次 P-7):本函数由 async 的
        // `run` 直接调用,而 run 经 `tokio::spawn` 跑在 worker 上(api/chat.rs:329)。
        // 在此直接发同步 SQLite 写会卡住整个 worker(唯一写连接 + busy_timeout 最长 5s
        // + fsync),连带该 worker 上排队的其他请求停摆——这正是 `collect_context`
        // 早已用 spawn_blocking 规避、而此处漏掉的一处,属纪律不统一。
        // 用 `park_worker`:让出调度核心,语义与 spawn_blocking 等价而调用点保持同步
        // (顺序不变是本函数的硬要求:变量必须在本轮消息构建之后、下发之前落库)。
        crate::utils::blocking::park_worker(|| {
            self.sessions
                .save_session_vars(session_id, rctx.session_vars);
        });
        let mut llm_messages = messages;
        // 运行时主 Agent 提示词(AGENTS_RUNTIME.md)注入到 system 消息开头,
        // 作为最高层约定(角色定位/创作原则/工具使用原则/输出纪律),其余内容随其后。
        // 宏 {{char}} 等已由 build 阶段对 system 展开,此处为外层拼接,不再二次展开。
        // 读取已按指纹缓存(P-4),命中时只剩 metadata();未命中仍会读盘,故一并让出。
        let runtime_prompt =
            crate::utils::blocking::park_worker(|| self.runtime_prompt.read_optional());
        match runtime_prompt {
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
            Err(error) => tracing::warn!(
                error = error,
                "运行时主 Agent 提示词读取失败，已回退其余提示词层"
            ),
        }
        // 历史压缩摘要:独立 system 消息槽(缓存感知管线·改造 A),插在首个 system
        // 之后、其余消息之前——摘要更新只改写摘要槽自身,不再改写 system 锚点,
        // system 与早期历史的前缀缓存得以保留。分层固定为
        // 「system(静态)→ 摘要槽(半静态,增量追加)→ 尾部历史(只追加)」。
        if let Some(summary) = &ctx.history_summary {
            insert_summary_slot(&mut llm_messages, summary);
        }
        // 跨会话记忆槽(落地项 2 + 升级工作流 B2):摘要槽之后、历史之前注入精选记忆。
        // inject_limit=0 等价关闭;character_id 为空或无选中记忆不插槽(与现状一致)。
        // 记忆集合未变时槽内容逐字节稳定(select_for_injection 排序键确定性),
        // touch 衰减回写延迟到响应主体生成之后,不影响本轮已构建内容。
        // B2 通道 2:按当前用户输入检索召回,作为尾部独立 system 消息追加(条数上限
        // RECALL_LIMIT,且只注入通道 1 未包含的条目);字符预算超限时截断并在槽内
        // 加一行显式提示(不静默丢弃)。
        let mut memory_touched: Vec<i64> = Vec::new();
        let settings = self.settings_snapshot();
        let inject_limit = settings.memory_inject_limit as usize;
        let char_budget = settings.memory_inject_char_budget as usize;
        if inject_limit > 0 && !req.character_id.trim().is_empty() {
            // 记忆读取(全量 list + 按 id 批量取向量)是同步 SQLite 读,同样让出 worker
            // (2026-09-16 性能批次 P-7:与上方 save_session_vars 同因)。
            let (entries, entry_vecs) = crate::utils::blocking::park_worker(|| {
                let entries = self.memory.list(&req.character_id);
                let entry_vecs: std::collections::HashMap<i64, Vec<f32>> = match recall_query_vec {
                    Some(_) => {
                        let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
                        self.memory.get_vectors(&ids)
                    }
                    None => std::collections::HashMap::new(),
                };
                (entries, entry_vecs)
            });
            let picked =
                crate::services::memory_service::select_for_injection(&entries, inject_limit);
            let picked_ids: Vec<i64> = picked.iter().map(|e| e.id).collect();
            // 通道 1+2 合计字符预算:先按序收通道 1,再收通道 2,超预算截断
            let mut used = 0usize;
            let mut truncated = false;
            let mut contents: Vec<String> = Vec::new();
            for e in &picked {
                if char_budget > 0 && used + e.content.chars().count() > char_budget {
                    truncated = true;
                    break;
                }
                used += e.content.chars().count();
                contents.push(e.content.clone());
            }
            // 通道 2:预算剩余时召回(预算 0 = 不限制;截断后即停止,避免槽无限增长)
            // Phase 3:查询向量由调用方(async 上下文)预先算好传入;
            // 有向量则「向量×0.7 + Jaccard×0.3」混合打分,无则纯 Jaccard。
            let mut recall_contents: Vec<String> = Vec::new();
            let mut recall_ids: Vec<i64> = Vec::new();
            if !truncated {
                // entry_vecs 已在上方与 entries 同批取出(P-7:避免在 worker 上同步读库)
                let recalled = crate::services::memory_service::select_recall_hybrid(
                    &entries,
                    &req.user_input,
                    crate::services::memory_service::RECALL_LIMIT,
                    &picked_ids,
                    recall_query_vec,
                    &entry_vecs,
                );
                for e in recalled {
                    if char_budget > 0 && used + e.content.chars().count() > char_budget {
                        truncated = true;
                        break;
                    }
                    used += e.content.chars().count();
                    recall_contents.push(e.content.clone());
                    recall_ids.push(e.id);
                }
            }
            let notice = truncated.then_some("(记忆条目因字符预算超限被截断,未全部注入)");
            let mut memory_slot_inserted = false;
            if insert_memory_slot(&mut llm_messages, &contents) {
                memory_slot_inserted = true;
                memory_touched = picked.iter().take(contents.len()).map(|e| e.id).collect();
            }
            // 通道 2 尾部追加:召回命中且通道 1 未包含的条目
            if insert_recall_slot(&mut llm_messages, &recall_contents, notice) {
                memory_touched.extend(recall_ids);
            } else if truncated && memory_slot_inserted {
                // 通道 2 无可注入条目但通道 1 被截断:提示兜底写入记忆槽,不静默丢弃
                append_memory_notice(&mut llm_messages, notice.unwrap_or_default());
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
        memory_touched
    }
}
