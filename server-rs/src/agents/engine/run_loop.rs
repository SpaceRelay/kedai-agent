// 主状态机循环:step_loop(规划步骤的 direct/tool/reflect 执行、反思回退重生成、
// custom 模式即时补丁应用)。自 engine/mod.rs 拆分迁入,纯代码移动,逻辑不变。
// 依赖经 `use super::*` 取自 engine/mod.rs(与 executor/mvu 等子模块同一约定)。
use super::*;

impl AgentEngine {
    /// 阶段 3「步骤循环」:按计划顺序执行每个步骤(反思判定 / 工具循环 / 生成),
    /// 维护 attempt 与反思重试计数、custom 模式变量基线、mergeUsage 累计。
    /// 返回 (content, custom_vars_snapshot);中断时提前结束循环。
    /// 对应 run_body 内「2. 执行阶段」段;L2 中层定位:计划 → 生成内容的推进循环。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn step_loop(
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
        EngineError,
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
                                agent_depth: 0,
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
                                    logging::agent_step(
                                        session_id,
                                        "censor",
                                        Some(&format!("反思修正禁词:{words_text}")),
                                    );
                                }
                                Ok(_) => {}
                                Err(e) => tracing::warn!(error = e, "反思禁词修正失败,保留原文"),
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
                        agent_depth: 0,
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
                            rctx.total_usage.prompt_cache_miss_tokens += u.prompt_cache_miss_tokens;
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
                logging::agent_step(session_id, "reflect", Some(&verdict.reason));
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
                                let _ =
                                    send_event(SseEvent::Vars { stat_data: tree }, tx, abort, flag)
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
                        // 技能渐进披露(落地项 3):system 只注入「技能名:一句话用途」紧凑清单,
                        // 正文按需 read(type=skill) 加载;清单按 name 排序,跨轮字节稳定(前缀缓存)。
                        // skill_progressive_disclosure = false 时回退旧行为(完全不注入清单)。
                        // (设置快照:不留锁跨 await)
                        if self.settings_snapshot().skill_progressive_disclosure {
                            let manifest = crate::services::skill_service::skill_manifest(
                                &self.skills.list(true),
                            );
                            if !manifest.is_empty() {
                                s.content.push_str(&format!(
                                    "\n\n【可用技能】(需要时用 read 工具 type=skill 按名读取正文)\n{manifest}"
                                ));
                            }
                        }
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
                // 聊天路径保留「未放行即等待授权」语义(有 UI 授权上下文)。
                let effective = step_params.clone();
                let gate = match step.tools.as_deref() {
                    // planner 显式配了步骤白名单:名单内自动放行,名单外回到等待授权
                    Some(list) if !list.is_empty() => crate::agents::engine::executor::ToolGate {
                        whitelist: Some(list),
                        no_ui_authorization: false,
                    },
                    _ => crate::agents::engine::executor::ToolGate::wait(),
                };
                run_tool_loop(
                    self,
                    state_machine,
                    // 聊天路径恒有 agent_sessions 行,包 Some(行为不变;任务模式传 None)
                    Some(agent_session),
                    session_id,
                    &mut step_msgs,
                    &effective,
                    tool_ctx,
                    tx,
                    abort,
                    flag,
                    rctx.total_usage,
                    &run_id.to_string(),
                    gate,
                )
                .await?
            } else if req.mode == "custom" {
                let r = if step_params.tools.is_empty() {
                    execute_generation(
                        self,
                        session_id,
                        &run_id.to_string(),
                        &step_msgs,
                        &step_params,
                        tx,
                        abort,
                        flag,
                    )
                    .await?
                } else {
                    // custom 模式白名单:步骤配置了 tools 白名单时,名单内工具自动放行;
                    // 名单外工具回到等待授权(聊天有 UI 授权上下文)
                    let gate = match step.tools.as_deref() {
                        Some(list) if !list.is_empty() => {
                            crate::agents::engine::executor::ToolGate {
                                whitelist: Some(list),
                                no_ui_authorization: false,
                            }
                        }
                        _ => crate::agents::engine::executor::ToolGate::wait(),
                    };
                    run_tool_loop(
                        self,
                        state_machine,
                        // 聊天路径恒有 agent_sessions 行,包 Some(行为不变;任务模式传 None)
                        Some(agent_session),
                        session_id,
                        &mut step_msgs,
                        &step_params,
                        tool_ctx,
                        tx,
                        abort,
                        flag,
                        rctx.total_usage,
                        &run_id.to_string(),
                        gate,
                    )
                    .await?
                };
                // 步骤提示词展开可能产生新变量:执行后写回持久化(与构建阶段一致)
                self.sessions
                    .save_session_vars(session_id, rctx.session_vars);
                r
            } else {
                // deep / fast:应用步骤级消息视图([本步指令])与步骤级温度/输出上限(无工具)
                execute_generation(
                    self,
                    session_id,
                    &run_id.to_string(),
                    &step_msgs,
                    &step_params,
                    tx,
                    abort,
                    flag,
                )
                .await?
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
            // finish_reason 与 content 同生命周期:逐轮覆盖,收尾即为「最终采纳那一步」
            // 的上游结束原因(可观测性问题①;聊天截断提示依此判定)。
            rctx.last_finish_reason = result.finish_reason.clone();
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
                if !gated.rejected.is_empty() || !gated.pending.is_empty() {
                    tracing::warn!(
                        rejected = gated.rejected.len(),
                        pending = gated.pending.len(),
                        "契约门控过滤了部分自定义模式正文补丁"
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
                        custom_contract_entries.extend(crate::contracts::entries_from_applied(
                            &tree_before,
                            &tree,
                            ctx.history.len() as u64,
                            &gated.applied_ops,
                            crate::contracts::ChangelogSource::Agent,
                        ));
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
            rctx.total_usage.prompt_cache_miss_tokens += result.usage.prompt_cache_miss_tokens;
            idx += 1;
        }
        Ok((
            content,
            custom_vars_snapshot,
            custom_contract_entries,
            custom_contract_pending,
        ))
    }
}
