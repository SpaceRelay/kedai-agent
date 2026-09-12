// 后台执行引擎:启动/停止入口(run/stop)、非流式 LLM 调用(generate_text)、
// 规划/步骤/汇总(plan_task/generate_step(_with)/summarize_task(_with))、
// 空输出分级重试(generate_step_retry/plan_task_retry/summarize_task_retry)、
// 后台执行主体与收尾(run_task_background/finalize_run)。
// 自 task_service.rs 拆分迁入,纯代码移动,逻辑不变;依赖经 `use super::*` 取自 mod.rs。
use super::*;
// 兄弟模块的自由函数:parse_plan(计划解析)与 persona_style(执行者人设文本)
use super::parse::parse_plan;
use super::prompt::persona_style;

impl TaskService {
    // ===== 执行 =====

    /// 启动后台执行(按 task_mode 分派;legacy 路径行为逐字节不变)。
    /// 重跑会清空旧 plan/result/error 并重新规划。
    /// 每次 run 分配新 token 覆盖旧条目,旧后台任务退出时凭 token 判断自己是否仍是当前执行。
    pub fn run(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let task = self.get(id).ok_or("任务不存在")?;
        if task.status == TaskStatus::Running || task.status == TaskStatus::Planning {
            return Err("任务已在执行中".into());
        }
        // planned = 计划已产出待批准(plan 模式):重入 run 会推翻待批准计划,须 approve 或 stop
        if task.status == TaskStatus::Planned {
            return Err("计划已产出待批准:请调用 approve 批准续跑,或 stop 放弃".into());
        }
        match task.task_mode {
            TaskRunMode::Legacy => {
                self.reset_task(id);
                let (cancel, token) = self.register_cancel(id);
                let deps = self.clone();
                let task_id = id.to_string();
                tokio::spawn(async move {
                    run_task_background(deps, task_id, cancel, token).await;
                });
                Ok(())
            }
            // solo / plan / multi / team / custom:任务引擎(六模式)执行器;
            // 取消与互斥沿用同一 token 机制
            TaskRunMode::Solo
            | TaskRunMode::Plan
            | TaskRunMode::Multi
            | TaskRunMode::Team
            | TaskRunMode::Custom => {
                self.reset_task(id);
                let (cancel, token) = self.register_cancel(id);
                let engine = crate::services::task_engine::TaskEngine::new(
                    self.clone(),
                    self.engine.clone(),
                );
                engine.run_mode(&task, cancel, token);
                Ok(())
            }
        }
    }

    /// 批准计划(plan 模式特有):仅 planned 态合法;给了 plan 就替换落库。
    /// 随后置 planning 并 spawn 续跑:已批准计划(steps)随调用传入,由任务引擎
    /// 逐步骤执行(复用 cancel token 登记,与 run 同款互斥/停止语义);
    /// goal 为「目标 + 已批准计划」组合文本,供各步骤消息作整体上下文。
    pub fn approve(self: &Arc<Self>, id: &str, plan: Option<Vec<TaskStep>>) -> Result<(), String> {
        let task = self.get(id).ok_or("任务不存在")?;
        if task.status != TaskStatus::Planned {
            return Err("仅待批准(planned)状态的任务可批准".into());
        }
        let steps = match plan {
            Some(p) => {
                if p.is_empty() {
                    return Err("批准的计划不能为空数组".into());
                }
                let _ = self.set_plan(id, &p);
                p
            }
            None => {
                if task.plan.is_empty() {
                    return Err("任务尚无计划可批准".into());
                }
                task.plan.clone()
            }
        };
        let _ = self.set_status(id, TaskStatus::Planning);
        // 续跑的整体上下文:目标 + 已批准计划(编号列表)。
        // 2026-09-10 实测修复(F4):原始目标里可能写了步骤数量(如「三步计划」),
        // 而用户经 plan-chat 修订后计划步数已变——显式要求以已批准计划为准,
        // 否则汇总会沿用原始目标的旧步数描述,出现「标题三步、正文两步」的矛盾。
        let mut goal = format!(
            "用户目标:\n{}\n\n已批准的计划(请按计划执行并产出最终结果;该计划可能经用户修订,\
             步骤数量与内容一律以本清单为准,不要沿用目标描述中的步骤数量):\n",
            task.title
        );
        for (i, s) in steps.iter().enumerate() {
            goal.push_str(&format!("{}. {}:{}\n", i + 1, s.name, s.goal));
        }
        let (cancel, token) = self.register_cancel(id);
        let engine =
            crate::services::task_engine::TaskEngine::new(self.clone(), self.engine.clone());
        engine.run_approved(&task, goal, steps, cancel, token);
        Ok(())
    }

    /// 六模式(task_engine)后台执行的成功收尾:仅自己仍是当前执行时写结果终态,
    /// 随后按 token 清理取消条目(与 finalize_run 同款「旧执行让位」语义)。
    /// 首轮产出同时落 assistant 消息(kind=result):任务模式的对话记录区按
    /// user/assistant 逐轮气泡呈现,首轮成果必须是一条完整的 assistant 发言,
    /// 否则用户指令历史里只有 followup 轮次、首轮产出无处可看(实跑问题 1)。
    pub(crate) fn complete_mode_run(
        &self,
        task_id: &str,
        token: u64,
        result: &str,
        status: TaskStatus,
        error: Option<&str>,
    ) {
        if self.is_current_run(task_id, token) {
            let _ = self.set_result_with_error(task_id, result, status, error);
            let text = result.trim();
            if !text.is_empty() {
                let _ = self.add_task_message(task_id, "assistant", "result", text);
            }
        }
        self.remove_cancel_if(task_id, token);
    }

    /// 六模式(task_engine)后台执行的取消/失败收尾:转调既有 finalize_run,语义不变。
    pub(crate) fn finalize_mode_run(
        &self,
        task_id: &str,
        token: u64,
        ended_by_cancel: bool,
        error: Option<&str>,
    ) {
        finalize_run(self, task_id, token, ended_by_cancel, error);
    }

    /// 六模式(task_engine)后台执行到达 planned 终态(plan 模式计划产出轮)的收尾:
    /// 计划清单文本落 result(批次 R1:planned 态 result 语义 = 待批准的计划清单,
    /// 见 set_planned_result;仅自己仍是当前执行时写入,与 complete_mode_run 同款
    /// 「旧执行让位」语义),随后按 token 清理取消条目;后续 approve 会经
    /// register_cancel 开启新执行。
    pub(crate) fn planned_mode_run(&self, task_id: &str, token: u64, result: &str) {
        if self.is_current_run(task_id, token) {
            let _ = self.set_planned_result(task_id, result);
        }
        self.remove_cancel_if(task_id, token);
    }

    /// 终态追加指令(批次 R2a followup;R2b+ 扩 mode):仅 done/partial/error/ended 可追加,
    /// running/planning/planned 由 API 层预检 409(此处复核兜底竞态)。
    /// 用户指令落 task_messages(kind=followup, role=user)→ 任务回 running,
    /// 以「原目标 + 上轮 result + 追加指令」solo 续跑(复用 run_agent_loop 单轮
    /// 工具自循环,不重新规划)→ 产出落 assistant 消息;`mode=append`(默认)以
    /// 「追加 N」段附加进 result,`mode=replace` 用产出整体替换 result(段标「修订 N」),
    /// 以支持「压缩 / 重写 / 改前面」类指令(实测 append 无法表达,压缩后原文仍在)。
    /// 取消/失败语义与 run 对齐:stop 可中断(ended),失败落 error 文本。
    /// 不经 TaskEngine 派发:收尾语义不同(改写 result + 写消息)。
    pub fn followup(
        self: &Arc<Self>,
        id: &str,
        content: &str,
        mode: TaskFollowupMode,
    ) -> Result<(), String> {
        let task = self.get(id).ok_or("任务不存在")?;
        if !matches!(
            task.status,
            TaskStatus::Done | TaskStatus::Partial | TaskStatus::Error | TaskStatus::Ended
        ) {
            return Err(format!(
                "仅终态任务可追加指令(当前:{});执行中请等待或 stop,待批准请走批准/规划对话",
                task.status.as_str()
            ));
        }
        let content = content.trim();
        if content.is_empty() {
            return Err("追加指令不能为空".into());
        }
        // 用户指令先落库(第 N 次追加的 N 含本条:落库后计数即本轮序号)
        self.add_task_message(id, "user", "followup", content)?;
        let followup_no = self.followup_count(id);
        // 续跑上下文:原目标 + 上轮结果 + 追加指令(沿用原人设/世界书,与 solo 同口径)。
        // replace 模式明确告知以本轮产出作为整体新版结果(旧结果仅作改写素材)。
        let mut goal = format!("用户目标:\n{}\n", task.title);
        if !task.result.trim().is_empty() {
            goal.push_str(&format!("\n已产出的结果:\n{}\n", task.result));
        }
        let tail = if mode == TaskFollowupMode::Replace {
            format!(
                "\n用户修订指令(第 {followup_no} 次修订):\n{content}\n\n\
                 请按修订指令重写整份结果:直接输出修订后的完整结果正文,它将整体替换旧结果,\
                 不要在输出中包含「修订」「追加」等字样的说明。"
            )
        } else {
            format!(
                "\n用户追加指令(第 {followup_no} 次追加):\n{content}\n\n\
                 请按追加指令在前述结果基础上继续,直接产出本轮追加的结果正文。"
            )
        };
        goal.push_str(&tail);
        let prev_partial = task.status == TaskStatus::Partial;
        let prev_result = task.result.clone();
        // 先登记新执行再写 running(2026-09-03 并发实测竞态):stop 后旧后台任务
        // 的收尾(finalize_run/complete_mode_run)可能晚到——若先写 running 后登记,
        // 旧收尾凭旧 token 仍是 current 会把 ended 盖回 running;先登记则旧收尾
        // is_current_run 让位不写,新执行的 running 不被覆盖。
        let (cancel, token) = self.register_cancel(id);
        let _ = self.set_status(id, TaskStatus::Running);
        let deps = self.clone();
        let task_id = id.to_string();
        let instruction = content.to_string();
        tokio::spawn(async move {
            run_followup_background(
                deps,
                task_id,
                goal,
                instruction,
                followup_no,
                prev_result,
                prev_partial,
                mode,
                cancel,
                token,
            )
            .await;
        });
        Ok(())
    }

    /// 批准环节规划对话(批次 R2b plan-chat):仅 planned 态;用户反馈落
    /// task_messages(kind=plan_chat)→ 规划器携带(原始目标 + 当前计划 JSON +
    /// 全部 plan_chat 历史)同步修订产出新计划(register_cancel 登记,stop
    /// 可中断;侦察/解析/分级重试与首轮规划同骨架)→ set_plan 替换 +
    /// planned 态 result 计划清单文本同步刷新 + assistant 修订说明落库 →
    /// 重发 approval_required 事件(plan 事件由 set_plan 发射);任务保持
    /// planned,等待用户再次审阅。stop 竞态/新执行覆盖时不写库。
    /// 返回修订后计划(供 API 响应携带,前端可即时刷新)。
    pub async fn plan_chat(
        self: &Arc<Self>,
        id: &str,
        message: &str,
    ) -> Result<Vec<TaskStep>, String> {
        let task = self.get(id).ok_or("任务不存在")?;
        if task.status != TaskStatus::Planned {
            return Err(format!(
                "仅待批准(planned)状态的任务可进行规划对话(当前:{})",
                task.status.as_str()
            ));
        }
        let message = message.trim();
        if message.is_empty() {
            return Err("反馈内容不能为空".into());
        }
        // 历史先于本轮落库读取(本轮反馈单独成段,不重复进历史段)
        let history: Vec<TaskMessageRecord> = self
            .list_task_messages(id)
            .into_iter()
            .filter(|m| m.kind == "plan_chat")
            .collect();
        self.add_task_message(id, "user", "plan_chat", message)?;
        let (cancel, token) = self.register_cancel(id);
        let revised = match plan_revise_retry(self, &task, &history, message, &cancel).await {
            Ok(v) => v,
            Err(e) => {
                self.remove_cancel_if(id, token);
                return Err(e);
            }
        };
        let (steps, out) = revised;
        self.record_usage(id, "planner", None, &out);
        // stop 竞态/新执行覆盖:不写库(任务状态由 stop 路径掌管)
        if *cancel.borrow() || !self.is_current_run(id, token) {
            self.remove_cancel_if(id, token);
            return Err("任务已停止".into());
        }
        let _ = self.set_plan(id, &steps);
        // planned 态 result 语义 = 待批准计划清单:随修订同步刷新(批准区预览即新计划)
        let mut listing = format!("计划已修订,共 {} 步:", steps.len());
        for (i, s) in steps.iter().enumerate() {
            listing.push_str(&format!("\n{}. {}:{}", i + 1, s.name, s.goal));
        }
        let _ = self.set_planned_result(id, &listing);
        // assistant 修订说明落库:共 N 步 + 步骤名清单(截 200 字符防膨胀)
        let names = steps
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("、");
        let names: String = names.chars().take(200).collect();
        let _ = self.add_task_message(
            id,
            "assistant",
            "plan_chat",
            &format!("计划已修订,共 {} 步:{}", steps.len(), names),
        );
        // 重发批准请求:计划已变,需用户重新审阅批准(前端经此事件刷新批准区)
        self.emit_event(
            "approval_required",
            id,
            None,
            Some(TaskStatus::Planned),
            Some("计划已修订,待批准".into()),
        );
        self.remove_cancel_if(id, token);
        Ok(steps)
    }

    /// 停止:标记 ended + 发取消信号 + 结束未完成的子任务。
    /// 不再立即移除取消条目——由后台任务退出时按 token 清理,避免删掉
    /// 「stop 后立即重跑」的新执行条目。
    /// 六模式(multi/team)主 agent 经 agentgo 排出的子 agent 一并结束
    ///(task:{id} 前缀虚拟 session 的内存记录),防 stop 后残留孤儿后台任务。
    pub fn stop(&self, id: &str) -> bool {
        let changed = self.set_status(id, TaskStatus::Ended);
        self.signal_cancel(id);
        for st in self.list_subtasks(id) {
            if st.status == TaskSubtaskStatus::Running || st.status == TaskSubtaskStatus::Pending {
                self.set_subtask_status(&st.id, TaskSubtaskStatus::Ended, None, None);
            }
        }
        self.agent_subtasks
            .end_by_session_prefix(&format!("task:{id}"));
        changed
    }

    // ===== LLM 调用 =====

    /// 非流式生成(批次 R4 起底层改走 generate_stream 逐块旁路):聚合
    /// Token/Reasoning/Usage/Finish 块的返回语义不变;同时 Token 块经
    /// DeltaBatcher 攒批旁路发射 kind=delta 暂态事件(规划/审计/终审/汇总/
    /// custom 无工具步骤的正文增量,与引擎桥 sink 同一攒批出口)。
    /// 记录模型/参数/耗时/结果规模/finish_reason/token 用量到日志,便于任务运行期
    /// 问题定位——尤其是「空输出」:finish_reason=length + reasoning_tokens 顶满
    /// max_tokens 即推理耗尽预算的直接证据(llm_requests 表仅聊天引擎使用,任务模式不走该链路)。
    /// task_id/phase/step_index 用于调用追踪落库(批次 3):本函数是任务侧 LLM 调用
    /// 统一出口,成功/空内容/超时/上游错误均落 task_llm_calls 一行。
    /// tools 非空时下发工具定义并聚合 ToolCall 块到产出(问题②规划器侦察轮用;
    /// 其余调用方传空,出现 ToolCall 块按协议异常记 warn 忽略)。
    /// pub(crate):任务引擎 team/custom 执行器的纯生成步(规划/审计/汇总/无工具步骤)
    /// 复用本统一出口,勿另写 HTTP 调用(批次 4.3b)。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn generate_text(
        &self,
        task_id: &str,
        phase: &str,
        step_index: Option<usize>,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
        max_tokens: u32,
        temperature: f64,
        top_p: f64,
        cancel: watch::Receiver<bool>,
    ) -> Result<TaskGenOutput, String> {
        let params = GenerationParams {
            temperature,
            top_p,
            max_tokens,
            stop: None,
            tools,
            max_tool_rounds: None,
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: None,
        };
        // 工具是否下发(params 随后 move 进 generate_stream,先行记录):未下发而出现
        // ToolCall 块属上游/协议异常(保留既有 warn 语义);已下发则聚合进产出(规划器侦察轮)
        let tools_given = !params.tools.is_empty();
        let started = Instant::now();
        // 逐块通道:connector 边收边发,下方 collect 边收边聚合 + 旁路攒批 delta
        let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel::<LlmStreamChunk>();
        // 总时长看门狗:超时范围包含 connector 读锁获取(该锁与聊天引擎/设置接口共享,
        // 慢网络请求可能持锁排队)与整个流式生成;超时后 future 被 drop,读锁随之释放,
        // chunk_tx 随之 drop,collect 端收 None 正常收尾。
        let call = async {
            let connector = self.connector.read().await;
            let model = connector.model().to_string();
            let connector_type = connector.type_name().to_string();
            let res = connector
                .generate_stream(&messages, params, cancel, chunk_tx)
                .await;
            (res, model, connector_type)
        };
        // delta 攒批旁路(批次 R4):与聚合同一路逐块处理;delta 是暂态事件不落库
        // (纪律例外见 events.rs emit_delta),权威数据以本函数收尾的落库行为准
        let mut batcher = self.delta_batcher(task_id, phase, step_index);
        let collect = async {
            let mut content = String::new();
            let mut finish_reason: Option<String> = None;
            let (mut prompt_tokens, mut completion_tokens, mut reasoning_tokens) =
                (0i64, 0i64, 0i64);
            let mut reasoning_chars = 0usize;
            let mut tool_calls: Vec<ToolCallArgs> = Vec::new();
            while let Some(c) = chunk_rx.recv().await {
                match c {
                    LlmStreamChunk::Token(t) => {
                        batcher.push(&t);
                        content.push_str(&t);
                    }
                    LlmStreamChunk::Reasoning(r) => reasoning_chars += r.chars().count(),
                    LlmStreamChunk::Usage {
                        prompt_tokens: p,
                        completion_tokens: ct,
                        reasoning_tokens: rt,
                        ..
                    } => {
                        prompt_tokens += p;
                        completion_tokens += ct;
                        reasoning_tokens += rt;
                    }
                    LlmStreamChunk::Finish { reason } => finish_reason = Some(reason),
                    // 下发了工具(规划器侦察轮,问题②):ToolCall 块是预期产出,聚合进结果;
                    // 未下发工具时出现 ToolCall 块即上游/协议异常,记 warn 便于定位
                    LlmStreamChunk::ToolCall(call) => {
                        if tools_given {
                            tool_calls.push(call);
                        } else {
                            tracing::warn!("任务模式 LLM 流出现非预期 ToolCall 块(generate_text 未注册工具,已忽略)")
                        }
                    }
                }
            }
            // 流收尾:不足一批的尾段发出(聚合返回值不受攒批影响,全文在 content)
            batcher.flush();
            (
                content,
                finish_reason,
                prompt_tokens,
                completion_tokens,
                reasoning_tokens,
                reasoning_chars,
                tool_calls,
            )
        };
        let (
            timed,
            (
                content,
                finish_reason,
                prompt_tokens,
                completion_tokens,
                reasoning_tokens,
                reasoning_chars,
                tool_calls,
            ),
        ) = tokio::join!(tokio::time::timeout(TASK_LLM_TOTAL_TIMEOUT, call), collect);
        let (res, model, connector_type) = match timed {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    timeout_s = TASK_LLM_TOTAL_TIMEOUT.as_secs(),
                    max_tokens = max_tokens,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "任务模式 LLM 生成超时(看门狗触发)"
                );
                let err = format!(
                    "模型调用超过 {}s 未完成(上游停滞或接口占用),已中止;可重新执行任务",
                    TASK_LLM_TOTAL_TIMEOUT.as_secs()
                );
                // 超时发生在 connector 读锁获取前后,model 未知,记空串
                self.record_llm_call(
                    task_id,
                    phase,
                    step_index,
                    "",
                    &messages,
                    &err,
                    None,
                    started.elapsed(),
                    "error",
                );
                return Err(err);
            }
        };
        if let Err(e) = res {
            self.record_llm_call(
                task_id,
                phase,
                step_index,
                &model,
                &messages,
                &e,
                None,
                started.elapsed(),
                "error",
            );
            return Err(e);
        }
        let out = TaskGenOutput {
            text: content,
            finish_reason,
            prompt_tokens,
            completion_tokens,
            reasoning_tokens,
            reasoning_chars,
            tool_calls,
        };
        // 调用追踪落库先于日志字段构建:log_fields 会 move model,此处只借用。
        // 空内容判定须排除「带 tool_calls 的轮次」(问题②规划器侦察轮:模型请求
        // 工具时正文为空是正常形态,属有效产出;标 empty 会误导调用情况面板)
        let status = if out.text.trim().is_empty() && out.tool_calls.is_empty() {
            "empty"
        } else {
            "ok"
        };
        self.record_llm_call(
            task_id,
            phase,
            step_index,
            &model,
            &messages,
            &out.text,
            Some(&out),
            started.elapsed(),
            status,
        );
        // D-4:字段表由切片改为宏实参,warn/info 两路各写一份(tracing 宏不支持字段表复用)
        if out.text.trim().is_empty() && out.tool_calls.is_empty() {
            // 空内容是任务模式最常见的失败形态,提升为 warn 便于日志扫描定位
            // (带 tool_calls 的侦察轮不算空:工具请求即本轮产出,与落库 status 同口径)
            tracing::warn!(
                model = model.as_str(),
                connector = connector_type.as_str(),
                max_tokens = max_tokens,
                temperature = temperature,
                top_p = top_p,
                chars = out.text.chars().count(),
                finish_reason = out.finish_reason.as_deref().unwrap_or(""),
                prompt_tokens = out.prompt_tokens,
                completion_tokens = out.completion_tokens,
                reasoning_tokens = out.reasoning_tokens,
                reasoning_chars = out.reasoning_chars,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "任务模式 LLM 返回空内容"
            );
        } else {
            tracing::info!(
                model = model.as_str(),
                connector = connector_type.as_str(),
                max_tokens = max_tokens,
                temperature = temperature,
                top_p = top_p,
                chars = out.text.chars().count(),
                finish_reason = out.finish_reason.as_deref().unwrap_or(""),
                prompt_tokens = out.prompt_tokens,
                completion_tokens = out.completion_tokens,
                reasoning_tokens = out.reasoning_tokens,
                reasoning_chars = out.reasoning_chars,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "任务模式 LLM 生成完成"
            );
        }
        Ok(out)
    }

    /// 规划(单次规划尝试,含只读侦察;解析与重试在 plan_task_retry):
    /// 把目标交给规划器,允许先用只读白名单工具(PLANNER_SCOUT_TOOLS,最多
    /// PLANNER_SCOUT_MAX_ROUNDS 轮)收集与目标相关的信息,再产出计划 JSON 数组文本
    /// (问题②,2026-08-31 实测:规划器原先单次 generate_text 无工具,对「测试 agent
    /// 框架能力」这类目标只能凭空编造步骤;plan/legacy 两模式共用本函数,均受益)。
    /// 温度保持低温(JSON 解析稳定是结构约束,非用户可调生成风格),top_p 读取任务模式设置;
    /// max_tokens 由调用方按重试级别递增(推理模型 reasoning 与正文共用预算)。
    /// 仅注入世界书常驻设定作为背景;不注入提示词注入(输出要求会破坏 JSON)与 Agent 系统提示词。
    /// 侦察轮也经 generate_text 统一出口落 task_llm_calls(phase=planner);
    /// 侦察轮调用失败(如半截 tool_call 被判协议损坏)不沉规划——记 warn 回退
    /// 无工具最终轮,与侦察能力缺席时的旧行为等价。
    async fn plan_task(
        &self,
        task_id: &str,
        title: &str,
        character_id: Option<&str>,
        max_tokens: u32,
        cancel: &watch::Receiver<bool>,
    ) -> Result<TaskGenOutput, String> {
        let mut sys = String::from(super::prompt::PLANNER_PROMPT);
        // 世界书为外部文本(WP7):untrusted 边界包裹,内置规划器指令不包裹
        let world = self.world_context(character_id);
        if !world.is_empty() {
            sys.push_str(&format!(
                "\n\n{}",
                crate::services::prompt_kit::untrusted_boundary("world_book", &world)
            ));
        }
        let messages = vec![
            LlmMessage::plain("system", &sys),
            LlmMessage::plain("user", title),
        ];
        self.plan_scout_loop(task_id, character_id, messages, max_tokens, cancel)
            .await
    }

    /// 规划对话修订(批次 R2b plan-chat):system = PLANNER_PROMPT + 世界书 +
    /// 修订指引段(PLANNER_REVISE_GUIDANCE);user = 原始目标 + 当前计划 JSON +
    /// plan_chat 对话历史 + 本轮反馈。历史截断参照 record_llm_call 摘要口径:
    /// 每条内容截 800 字符、历史段整体截 4000(多轮对话体积封底)。
    /// 侦察/落库/取消检查与 plan_task 同口径(共用 plan_scout_loop)。
    async fn plan_revise(
        &self,
        task: &TaskRecord,
        history: &[TaskMessageRecord],
        feedback: &str,
        max_tokens: u32,
        cancel: &watch::Receiver<bool>,
    ) -> Result<TaskGenOutput, String> {
        let mut sys = String::from(super::prompt::PLANNER_PROMPT);
        let world = self.world_context(task.character_id.as_deref());
        if !world.is_empty() {
            sys.push_str(&format!(
                "\n\n{}",
                crate::services::prompt_kit::untrusted_boundary("world_book", &world)
            ));
        }
        // 内置修订指引段(不包裹,与内置规划器指令同口径)
        sys.push_str(&format!("\n\n{}", super::prompt::PLANNER_REVISE_GUIDANCE));

        // 历史段:逐条「用户/规划器: 内容(截 800)」,整体截 4000
        let mut hist = String::new();
        for m in history {
            if !hist.is_empty() {
                hist.push('\n');
            }
            let who = if m.role == "user" {
                "用户"
            } else {
                "规划器"
            };
            let content: String = m.content.chars().take(800).collect();
            hist.push_str(&format!("{who}: {content}"));
        }
        let hist: String = hist.chars().take(4000).collect();

        let plan_json = serde_json::to_string(&task.plan).unwrap_or_else(|_| "[]".into());
        let mut user = format!(
            "原始目标:\n{}\n\n当前计划(JSON):\n{}\n",
            task.title, plan_json
        );
        if !hist.is_empty() {
            user.push_str(&format!("\n此前的修订对话(按时间顺序):\n{hist}\n"));
        }
        user.push_str(&format!(
            "\n本轮反馈:\n{feedback}\n\n请按本轮反馈修订计划,严格只输出修订后的 JSON 数组。"
        ));
        let messages = vec![
            LlmMessage::plain("system", &sys),
            LlmMessage::plain("user", &user),
        ];
        self.plan_scout_loop(
            &task.id,
            task.character_id.as_deref(),
            messages,
            max_tokens,
            cancel,
        )
        .await
    }

    /// 带只读侦察的规划调用循环(plan_task/plan_revise 共用骨架,批次 R2b 抽取):
    /// messages 由调用方组装(首轮规划 = 目标;修订轮 = 目标+当前计划+历史+反馈),
    /// 侦察白名单/轮数上限/回填/取消检查口径不变。
    async fn plan_scout_loop(
        &self,
        task_id: &str,
        character_id: Option<&str>,
        mut messages: Vec<LlmMessage>,
        max_tokens: u32,
        cancel: &watch::Receiver<bool>,
    ) -> Result<TaskGenOutput, String> {
        let settings = self.task_settings();
        // 侦察白名单 ∩ 已注册工具(未注册的环境静默缺席,如裁剪版工具集)
        let defs: Vec<ToolDefinition> = self
            .engine
            .tool_definitions()
            .into_iter()
            .filter(|d| PLANNER_SCOUT_TOOLS.contains(&d.name.as_str()))
            .collect();
        // 虚拟 session_id(task: 前缀,与 run_agent_loop 同口径;不建影子行)
        let tool_ctx = ToolContext {
            session_id: format!("task:{task_id}"),
            character_id: character_id.unwrap_or_default().to_string(),
            agent_depth: 0,
        };
        let mut scout_round = 0usize;
        loop {
            // 侦察轮(带白名单工具)或最终轮(侦察轮用尽/无可用工具 → 不带工具,
            // 强制纯文本产出计划 JSON,契约不变)
            let give_tools = scout_round < PLANNER_SCOUT_MAX_ROUNDS && !defs.is_empty();
            let out = match self
                .generate_text(
                    task_id,
                    "planner",
                    None,
                    messages.clone(),
                    if give_tools { defs.clone() } else { Vec::new() },
                    max_tokens,
                    0.3,
                    settings.default_top_p,
                    cancel.clone(),
                )
                .await
            {
                Ok(o) => o,
                Err(e) => {
                    if give_tools {
                        // 侦察轮失败不沉规划(侦察是增强环节):回退无工具最终轮,
                        // 与旧版单次规划行为等价;失败行已由统一出口落库(status=error)
                        tracing::warn!(error = e, "规划器侦察轮调用失败,回退无工具直接规划");
                        scout_round = PLANNER_SCOUT_MAX_ROUNDS;
                        continue;
                    }
                    return Err(e);
                }
            };
            if out.tool_calls.is_empty() || !give_tools {
                // 模型未请求工具(直接产出计划,零侦察)或已是最终轮:正文 = 计划 JSON
                return Ok(out);
            }
            // 侦察轮入账(2026-09-10 实测修复):此前侦察轮只落 task_llm_calls 不落
            // task_usage,导致 usage_total 与调用明细求和不等(实测 legacy/plan 各少计
            // 800+ prompt token)。最终轮由调用方入账,此处只记非最终侦察轮,避免双记。
            self.record_usage(task_id, "planner", None, &out);
            scout_round += 1;
            // 回填 OpenAI 标准结构:assistant(tool_calls) → 逐条 tool 结果
            // (与 run_tool_loop 回填同构;缺 tool 结果消息时严格后端对孤立
            // tool_calls 会 400,不得用 user 旁白降级)
            messages.push(LlmMessage {
                role: "assistant".into(),
                content: out.text.clone(),
                reasoning_content: None,
                tool_calls: Some(out.tool_calls.clone()),
                tool_call_id: None,
            });
            for call in &out.tool_calls {
                // 白名单双保险:下发定义已是子集,执行时仍逐一核对(防模型幻觉工具名)。
                // 收口到统一裁决入口(批次授权改造):名单内 = custom_authorized 放行,
                // 名单外直接拒绝。任务模式无 UI 授权上下文,不走等待授权分支。
                let listed = PLANNER_SCOUT_TOOLS.contains(&call.name.as_str());
                let decision = self.engine.tool_registry().permissions().decide_with_policy(
                    &call.name,
                    &tool_ctx,
                    self.engine.tool_registry().get(&call.name).is_some(),
                    listed,
                    crate::tools::permissions::AuthorizationMode::Loose,
                    &crate::tools::action_class::classify(
                        &call.name,
                        &call.arguments,
                        self.engine
                            .tool_registry()
                            .origin_of(&call.name)
                            .unwrap_or(crate::tools::action_class::ToolOrigin::Builtin),
                    ),
                    false,
                );
                let output = if listed && decision.allowed {
                    match self
                        .engine
                        .tool_registry()
                        .execute_with_decision(
                            &call.name,
                            &call.arguments,
                            tool_ctx.clone(),
                            &decision,
                        )
                        .await
                    {
                        Ok(r) => r,
                        Err(e) => serde_json::json!({ "error": e }).to_string(),
                    }
                } else {
                    serde_json::json!({ "error": format!("工具 \"{}\" 不在规划器只读侦察白名单,已拒绝", call.name) })
                        .to_string()
                };
                messages.push(LlmMessage {
                    role: "tool".into(),
                    content: output,
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some(call.id.clone()),
                });
            }
            if *cancel.borrow() {
                return Err("任务已停止".into());
            }
        }
    }

    /// 执行单个步骤:注入执行者人设 + 世界书 + 提示词注入 + Agent 系统提示词。
    /// 生成参数读任务模式设置。
    async fn generate_step(
        &self,
        task: &TaskRecord,
        step: &TaskStep,
        step_index: Option<usize>,
        cancel: &watch::Receiver<bool>,
    ) -> Result<TaskGenOutput, String> {
        let settings = self.task_settings();
        self.generate_step_with(
            task,
            step,
            step_index,
            settings.default_max_tokens,
            settings.default_temperature,
            cancel,
        )
        .await
    }

    /// 带生成参数覆盖的步骤执行(空输出重试时提高 max_tokens 或调整温度)。
    async fn generate_step_with(
        &self,
        task: &TaskRecord,
        step: &TaskStep,
        step_index: Option<usize>,
        max_tokens: u32,
        temperature: f64,
        cancel: &watch::Receiver<bool>,
    ) -> Result<TaskGenOutput, String> {
        let settings = self.task_settings();
        let character = task
            .character_id
            .as_deref()
            .and_then(|cid| self.characters.get(cid));

        let mut sys = String::from(super::prompt::EXECUTOR_PROMPT);
        // 外部来源段落(人设/世界书/注入/用户可编辑 Agent 提示词)逐一 untrusted 边界包裹
        // (WP7,与引擎侧同一原语);内置执行者指令不包裹。
        if let Some(c) = &character {
            // 人设精简/完整按任务模式有效设置(task_persona_full,R3a;None/false=精简)
            let style = persona_style(c, settings.task_persona_full);
            if !style.is_empty() {
                sys.push_str(&format!(
                    "\n\n写作风格参考(角色「{}」):\n{}",
                    c.chara_name,
                    crate::services::prompt_kit::untrusted_boundary("character", &style)
                ));
            }
        }
        let world = self.world_context(task.character_id.as_deref());
        if !world.is_empty() {
            sys.push_str(&format!(
                "\n\n{}",
                crate::services::prompt_kit::untrusted_boundary("world_book", &world)
            ));
        }
        let inject = if settings.task_prompt_inject_enabled {
            self.inject_text()
        } else {
            String::new()
        };
        if !inject.is_empty() {
            sys.push_str(&format!(
                "\n\n{}",
                crate::services::prompt_kit::untrusted_boundary("prompt_inject", &inject)
            ));
        }
        // settings 为 for_mode(Task) 合并值;agent_system_prompt 字段类型 RoleplayPromptConfig
        // (RuntimeSettings 共用成员),此处内容已是 task 有效值(覆盖层计算结果),.0 取字符串
        if !settings.agent_system_prompt.0.trim().is_empty() {
            let rendered = self.render_agent_prompt(
                &settings.agent_system_prompt.0,
                character.as_ref(),
                &world,
                &task.title,
            );
            sys.push_str(&format!(
                "\n\n{}",
                crate::services::prompt_kit::untrusted_boundary("agent_prompt", &rendered)
            ));
        }
        let messages = vec![
            LlmMessage::plain("system", &sys),
            LlmMessage::plain("user", &step.goal),
        ];
        self.generate_text(
            &task.id,
            "step",
            step_index,
            messages,
            Vec::new(),
            max_tokens,
            temperature,
            settings.default_top_p,
            cancel.clone(),
        )
        .await
    }

    /// 汇总:综合各步骤结果产出最终成果。注入世界书 + 提示词注入 + Agent 系统提示词。
    /// 生成参数读任务模式设置。
    async fn summarize_task(
        &self,
        task: &TaskRecord,
        plan: &[TaskStep],
        cancel: &watch::Receiver<bool>,
    ) -> Result<TaskGenOutput, String> {
        let settings = self.task_settings();
        self.summarize_task_with(
            task,
            plan,
            settings.default_max_tokens,
            settings.default_temperature,
            cancel,
        )
        .await
    }

    /// 带生成参数覆盖的汇总(空输出重试时提高 max_tokens 或调整温度)。
    async fn summarize_task_with(
        &self,
        task: &TaskRecord,
        plan: &[TaskStep],
        max_tokens: u32,
        temperature: f64,
        cancel: &watch::Receiver<bool>,
    ) -> Result<TaskGenOutput, String> {
        let settings = self.task_settings();
        let character = task
            .character_id
            .as_deref()
            .and_then(|cid| self.characters.get(cid));

        let mut sys = String::from(super::prompt::SUMMARIZER_PROMPT);
        // 外部来源段落 untrusted 边界包裹(WP7,同 generate_step_with);内置汇总者指令不包裹。
        let world = self.world_context(task.character_id.as_deref());
        if !world.is_empty() {
            sys.push_str(&format!(
                "\n\n{}",
                crate::services::prompt_kit::untrusted_boundary("world_book", &world)
            ));
        }
        // 提示词注入默认隔离(2026-09-10 实测修复,同 generate_step_with):
        // 仅显式开启 task_prompt_inject_enabled 才继承 prompt_floors.json
        let inject = if settings.task_prompt_inject_enabled {
            self.inject_text()
        } else {
            String::new()
        };
        if !inject.is_empty() {
            sys.push_str(&format!(
                "\n\n{}",
                crate::services::prompt_kit::untrusted_boundary("prompt_inject", &inject)
            ));
        }
        // 同 generate_step_with:字段内容已是 task 有效值,.0 取字符串
        if !settings.agent_system_prompt.0.trim().is_empty() {
            let rendered = self.render_agent_prompt(
                &settings.agent_system_prompt.0,
                character.as_ref(),
                &world,
                &task.title,
            );
            sys.push_str(&format!(
                "\n\n{}",
                crate::services::prompt_kit::untrusted_boundary("agent_prompt", &rendered)
            ));
        }
        let mut user = format!("用户目标:\n{}\n\n各步骤结果:\n", task.title);
        for (i, s) in plan.iter().enumerate() {
            user.push_str(&format!(
                "{}. {}({}):\n{}\n",
                i + 1,
                s.name,
                s.status.as_str(),
                s.result
            ));
        }
        let messages = vec![
            LlmMessage::plain("system", &sys),
            LlmMessage::plain("user", &user),
        ];
        self.generate_text(
            &task.id,
            "summarize",
            None,
            messages,
            Vec::new(),
            max_tokens,
            temperature,
            settings.default_top_p,
            cancel.clone(),
        )
        .await
    }
}

/// 后台执行退出收尾:仅当自己仍是该任务当前执行时才写终态(旧执行在重跑后让位,
/// 不覆盖新任务状态),并按 token 清理取消条目(不删新执行的条目)。
fn finalize_run(
    deps: &TaskService,
    task_id: &str,
    token: u64,
    ended_by_cancel: bool,
    error: Option<&str>,
) {
    if deps.is_current_run(task_id, token) {
        if ended_by_cancel {
            let _ = deps.set_status(task_id, TaskStatus::Ended);
        } else if let Some(e) = error {
            let _ = deps.set_error(task_id, e);
        }
    }
    deps.remove_cancel_if(task_id, token);
}

/// 空输出分级重试骨架(步骤/汇总共用;WP2-A 收敛两份逐行同构的重试,逻辑不变):
/// 首次产出非空即返回;空输出退避 EMPTY_RETRY_BACKOFF 后按 finish_reason 分级重试一次——
/// finish_reason=length:max_tokens 翻倍(上限 RETRY_MAX_TOKENS_CAP),温度保持默认;
/// 其他原因/无原因:同 max_tokens,temperature 调至 0.7(沿用 mvu.rs 先例)。
/// 仍空返回带 label 与原因的 Err;网络/超时等硬错误不重试直接透传。
/// call 按 (max_tokens, temperature) 发起重试调用,复用同一 cancel,不重建 system 提示词。
/// (plan_task_retry 不共用本骨架:多轮循环 + parse 判定 + 参数演进/日志规则不同。)
async fn retry_if_empty_output<F, Fut>(
    deps: &TaskService,
    task_id: &str,
    phase: &str,
    step_index: Option<usize>,
    label: &str,
    first: TaskGenOutput,
    cancel: &watch::Receiver<bool>,
    call: F,
) -> Result<TaskGenOutput, String>
where
    F: Fn(u32, f64) -> Fut,
    Fut: std::future::Future<Output = Result<TaskGenOutput, String>>,
{
    if !first.text.trim().is_empty() {
        return Ok(first);
    }
    // 首调空输出的那次调用仍应入账(2026-09-10 实测修复):此前只落 task_llm_calls,
    // 重试成功后只记末次 out,首调 token 从 usage_total 丢失。
    deps.record_usage(task_id, phase, step_index, &first);
    tokio::time::sleep(EMPTY_RETRY_BACKOFF).await;
    if *cancel.borrow() {
        return Err("任务已停止".into());
    }
    let settings = deps.task_settings();
    let reason = first.finish_reason.as_deref().unwrap_or("");
    let retried = if reason == "length" {
        let doubled = (settings.default_max_tokens.saturating_mul(2)).min(RETRY_MAX_TOKENS_CAP);
        call(doubled, settings.default_temperature).await
    } else {
        call(settings.default_max_tokens, 0.7).await
    };
    match retried {
        Ok(o) if !o.text.trim().is_empty() => Ok(o),
        Ok(o) => Err(format!(
            "{label}返回空内容(finish_reason={})",
            o.finish_reason.as_deref().unwrap_or("未知")
        )),
        Err(e) => Err(e),
    }
}

/// 生成步骤:空输出按 finish_reason 分级重试一次(仍空返回带原因的 Err)。
/// - finish_reason=length:max_tokens 翻倍(上限 RETRY_MAX_TOKENS_CAP)重试;
/// - 其他原因/无原因:temperature 调至 0.7 重试(沿用 mvu.rs 先例)。
///
/// 重试走 generate_step_with 单参数覆盖变体,复用同一 cancel,不重建 system 提示词。
async fn generate_step_retry(
    deps: &TaskService,
    task: &TaskRecord,
    step: &TaskStep,
    step_index: Option<usize>,
    cancel: &watch::Receiver<bool>,
) -> Result<TaskGenOutput, String> {
    let first = deps.generate_step(task, step, step_index, cancel).await?;
    retry_if_empty_output(
        deps,
        &task.id,
        "step",
        step_index,
        "子任务",
        first,
        cancel,
        |max_tokens, temperature| {
            deps.generate_step_with(task, step, step_index, max_tokens, temperature, cancel)
        },
    )
    .await
}

/// 汇总:与步骤同款的分级重试(空输出按 finish_reason 分别加倍 max_tokens 或调温)。
/// 规划(带解析与分级重试):parse_plan 内含截断打捞;仍失败时按 finish_reason 分级——
/// length/空内容 = 预算被推理 token 耗尽,max_tokens 翻倍(上限 RETRY_MAX_TOKENS_CAP);
/// 纯格式错误 = 同预算重试。网络/超时等硬错误不重试直接抛(与步骤重试同款约定)。
/// 共 PLAN_MAX_ATTEMPTS 次尝试;失败文案带末次错误,便于排障。
/// pub(crate):任务引擎 plan 模式复用(只规划不执行,docs/任务引擎六模式.md)。
pub(crate) async fn plan_task_retry(
    deps: &TaskService,
    task_id: &str,
    title: &str,
    character_id: Option<&str>,
    cancel: &watch::Receiver<bool>,
) -> Result<(Vec<TaskStep>, TaskGenOutput), String> {
    let mut max_tokens = PLAN_INITIAL_MAX_TOKENS;
    let mut last_err = String::from("规划器未产出有效步骤");
    for attempt in 1..=PLAN_MAX_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(EMPTY_RETRY_BACKOFF).await;
            if *cancel.borrow() {
                return Err("任务已停止".into());
            }
        }
        let out = deps
            .plan_task(task_id, title, character_id, max_tokens, cancel)
            .await?;
        match parse_plan(&out.text) {
            Ok(steps) if !steps.is_empty() => return Ok((steps, out)),
            Ok(_) => last_err = "规划器未产出有效步骤".into(),
            Err(e) => last_err = e,
        }
        // 失败 attempt 仍是一次真实调用(已落 task_llm_calls):同步入账 usage,
        // 否则重试场景下 usage_total 少于调用明细求和(2026-09-10 实测修复)。
        deps.record_usage(task_id, "planner", None, &out);
        let reason = out.finish_reason.as_deref().unwrap_or("");
        tracing::warn!(
            attempt = attempt,
            finish_reason = reason,
            max_tokens = max_tokens,
            error = last_err.clone(),
            "任务模式规划输出解析失败,准备重试"
        );
        if reason == "length" || out.text.trim().is_empty() {
            max_tokens = (max_tokens.saturating_mul(2)).min(RETRY_MAX_TOKENS_CAP);
        }
    }
    Err(format!("{last_err}(已重试 {} 次)", PLAN_MAX_ATTEMPTS - 1))
}

/// 规划对话修订(带解析与分级重试):与 plan_task_retry 同款骨架——
/// 截断/空输出按 finish_reason 翻倍 max_tokens(上限 RETRY_MAX_TOKENS_CAP),
/// 纯格式错误同预算重试;共 PLAN_MAX_ATTEMPTS 次;网络/超时硬错误直接抛。
/// 返回修订后步骤与末次调用产出(usage 落库用)。
async fn plan_revise_retry(
    deps: &TaskService,
    task: &TaskRecord,
    history: &[TaskMessageRecord],
    feedback: &str,
    cancel: &watch::Receiver<bool>,
) -> Result<(Vec<TaskStep>, TaskGenOutput), String> {
    let mut max_tokens = PLAN_INITIAL_MAX_TOKENS;
    let mut last_err = String::from("规划器未产出有效修订计划");
    for attempt in 1..=PLAN_MAX_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(EMPTY_RETRY_BACKOFF).await;
            if *cancel.borrow() {
                return Err("任务已停止".into());
            }
        }
        let out = deps
            .plan_revise(task, history, feedback, max_tokens, cancel)
            .await?;
        match parse_plan(&out.text) {
            Ok(steps) if !steps.is_empty() => return Ok((steps, out)),
            Ok(_) => last_err = "规划器未产出有效修订计划".into(),
            Err(e) => last_err = e,
        }
        // 失败 attempt 同步入账 usage(口径同 plan_task_retry;2026-09-10 实测修复)
        deps.record_usage(&task.id, "planner", None, &out);
        let reason = out.finish_reason.as_deref().unwrap_or("");
        tracing::warn!(
            attempt = attempt,
            finish_reason = reason,
            max_tokens = max_tokens,
            error = last_err.clone(),
            "规划对话修订输出解析失败,准备重试"
        );
        if reason == "length" || out.text.trim().is_empty() {
            max_tokens = (max_tokens.saturating_mul(2)).min(RETRY_MAX_TOKENS_CAP);
        }
    }
    Err(format!("{last_err}(已重试 {} 次)", PLAN_MAX_ATTEMPTS - 1))
}

/// 汇总:与步骤同款的分级重试(空输出按 finish_reason 分别加倍 max_tokens 或调温)。
/// pub(crate):任务引擎 plan 批准续跑执行器(ApprovedPlanExecutor)汇总段复用,
/// 对齐 legacy 执行段语义(docs/任务引擎六模式.md)。
pub(crate) async fn summarize_task_retry(
    deps: &TaskService,
    task: &TaskRecord,
    plan: &[TaskStep],
    cancel: &watch::Receiver<bool>,
) -> Result<TaskGenOutput, String> {
    let first = deps.summarize_task(task, plan, cancel).await?;
    retry_if_empty_output(
        deps,
        &task.id,
        "summary",
        None,
        "汇总",
        first,
        cancel,
        |max_tokens, temperature| {
            deps.summarize_task_with(task, plan, max_tokens, temperature, cancel)
        },
    )
    .await
}

/// 后台执行主体:规划 → 逐步执行 → 汇总。
async fn run_task_background(
    deps: Arc<TaskService>,
    task_id: String,
    cancel: watch::Receiver<bool>,
    token: u64,
) {
    let Some(task) = deps.get(&task_id) else {
        deps.remove_cancel_if(&task_id, token);
        return;
    };

    // 1) 规划(带解析与分级重试:截断/空输出翻倍 max_tokens,最多 PLAN_MAX_ATTEMPTS 次)
    let (plan, plan_out) = match plan_task_retry(
        &deps,
        &task_id,
        &task.title,
        task.character_id.as_deref(),
        &cancel,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            finalize_run(&deps, &task_id, token, *cancel.borrow(), Some(&e));
            return;
        }
    };
    deps.record_usage(&task_id, "plan", None, &plan_out);
    if *cancel.borrow() {
        finalize_run(&deps, &task_id, token, true, None);
        return;
    }
    let _ = deps.set_plan(&task_id, &plan);
    let _ = deps.set_status(&task_id, TaskStatus::Running);

    // 2) 逐步执行
    let mut final_plan = plan.clone();
    for (i, step) in plan.iter().enumerate() {
        if *cancel.borrow() {
            finalize_run(&deps, &task_id, token, true, None);
            return;
        }
        final_plan[i].status = TaskStepStatus::Running;
        let _ = deps.set_plan(&task_id, &final_plan);

        let subtask_id = match deps.create_subtask(&task_id, &step.name, &step.goal) {
            Ok(id) => id,
            Err(e) => {
                final_plan[i].status = TaskStepStatus::Error;
                final_plan[i].result = e.clone();
                let _ = deps.set_plan(&task_id, &final_plan);
                continue;
            }
        };

        // 生成步骤:空输出按 finish_reason 分级重试一次(length 加倍 max_tokens,否则调温 0.7),
        // 仍空标 error 且文案带 finish_reason(诊断推理耗尽 vs 内容过滤等不同成因)。
        let gen = generate_step_retry(&deps, &task, step, Some(i), &cancel).await;
        match gen {
            Ok(out) => {
                let text = out.text.trim().to_string();
                final_plan[i].status = TaskStepStatus::Done;
                final_plan[i].result = text.clone();
                deps.set_subtask_status(&subtask_id, TaskSubtaskStatus::Done, Some(&text), None);
                deps.record_usage(&task_id, "step", Some(i), &out);
            }
            Err(e) => {
                if *cancel.borrow() {
                    finalize_run(&deps, &task_id, token, true, None);
                    return;
                }
                final_plan[i].status = TaskStepStatus::Error;
                final_plan[i].result = e.clone();
                deps.set_subtask_status(&subtask_id, TaskSubtaskStatus::Error, None, Some(&e));
            }
        }
        let _ = deps.set_plan(&task_id, &final_plan);
    }

    // 3) 汇总:空输出同样按 finish_reason 分级重试一次。
    if *cancel.borrow() {
        finalize_run(&deps, &task_id, token, true, None);
        return;
    }
    match summarize_task_retry(&deps, &task, &final_plan, &cancel).await {
        Ok(out) => {
            deps.record_usage(&task_id, "summary", None, &out);
            // 正常完成:旧执行在重跑后让位,不覆盖新任务状态。
            // 含 error 步骤时终态为「部分完成」(成果仍产出,但需向用户标示有步骤失败)。
            if deps.is_current_run(&task_id, token) {
                let has_error = final_plan.iter().any(|s| s.status == TaskStepStatus::Error);
                let status = if has_error {
                    TaskStatus::Partial
                } else {
                    TaskStatus::Done
                };
                let _ = deps.set_result(&task_id, out.text.trim(), status);
                // 首轮成果落 assistant 消息(kind=result):与六模式
                // complete_mode_run 同口径,供前端对话记录区逐轮气泡呈现
                // (实跑问题 1:此前首轮产出只进 result,不进 messages)
                let text = out.text.trim();
                if !text.is_empty() {
                    let _ = deps.add_task_message(&task_id, "assistant", "result", text);
                }
            }
        }
        Err(e) => finalize_run(&deps, &task_id, token, *cancel.borrow(), Some(&e)),
    }
    deps.remove_cancel_if(&task_id, token);
}

/// followup 追加指令的后台续跑主体(批次 R2a;R2b+ 扩 mode):solo 单轮 run_agent_loop
///(工具全量,不重新规划;goal 已在入口组装为「原目标 + 上轮 result + 追加指令」)。
/// 成功:usage 落库(phase=agent 与 solo 同口径)→ assistant 消息落库
///(kind=followup)→ `append` 模式以「追加 N」段附加进 result(不覆盖前轮成果,
/// 指令概要截 80 字符),`replace` 模式以「修订 N」段整体替换 result → 终态映射:
/// 原 partial 保持 partial(失败步骤历史不被抹平),done/error/ended 后回 done。
/// 取消/失败与 run_task_background 同款 finalize_run 收尾(stop → ended,
/// 其余 → error 文本落库);is_current_run 守门,旧执行让位语义不变。
#[allow(clippy::too_many_arguments)]
async fn run_followup_background(
    deps: Arc<TaskService>,
    task_id: String,
    goal: String,
    instruction: String,
    followup_no: usize,
    prev_result: String,
    prev_partial: bool,
    mode: TaskFollowupMode,
    cancel: watch::Receiver<bool>,
    token: u64,
) {
    let Some(task) = deps.get(&task_id) else {
        deps.remove_cancel_if(&task_id, token);
        return;
    };
    let call = crate::services::task_engine::solo::AgentLoopCall {
        task_id: task_id.clone(),
        session_id: format!("task:{task_id}"),
        goal,
        settings: deps.task_settings(),
        character_id: task.character_id.clone(),
        phase: "agent",
        step_index: None,
        label: "主 agent".into(),
    };
    let result = crate::services::task_engine::solo::run_agent_loop(
        deps.clone(),
        deps.engine.clone(),
        call,
        cancel.clone(),
    )
    .await;
    match result {
        Ok((text, usage)) => {
            deps.record_usage(
                &task_id,
                "agent",
                None,
                &crate::services::task_engine::executor::usage_as_output(&usage),
            );
            // 取消优先于写结果:stop 后产出不再落库(与 run_task_background 同口径)
            if *cancel.borrow() {
                finalize_run(&deps, &task_id, token, true, None);
                return;
            }
            if deps.is_current_run(&task_id, token) {
                let text = text.trim();
                // assistant 产出落库(WP4 纪律:消息不发射独立事件,详情经
                // 下方 set_result 的 status 事件驱动前端重拉)
                let _ = deps.add_task_message(&task_id, "assistant", "followup", text);
                let brief: String = instruction.chars().take(80).collect();
                let brief = if instruction.chars().count() > 80 {
                    format!("{brief}…")
                } else {
                    brief
                };
                let section = if mode == TaskFollowupMode::Replace {
                    // replace:整体替换,段标「修订 N」,旧 result 不再保留
                    format!("**修订 {followup_no}:**{brief}\n\n{text}")
                } else {
                    format!("**追加 {followup_no}:**{brief}\n\n{text}")
                };
                let new_result = if mode == TaskFollowupMode::Replace {
                    section
                } else {
                    // prev_result 为入口快照:追加期间仅本执行可写 result
                    //(is_current_run 守门;stop 只动状态不动 result),快照即当前
                    let prev = prev_result.trim_end();
                    if prev.is_empty() {
                        section
                    } else {
                        format!("{prev}\n\n---\n\n{section}")
                    }
                };
                let status = if prev_partial {
                    TaskStatus::Partial
                } else {
                    TaskStatus::Done
                };
                let _ = deps.set_result(&task_id, &new_result, status);
            }
            deps.remove_cancel_if(&task_id, token);
        }
        Err(e) => finalize_run(&deps, &task_id, token, *cancel.borrow(), Some(&e)),
    }
}
