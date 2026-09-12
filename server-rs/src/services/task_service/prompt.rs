// 提示词组装:任务模式有效设置读取(task_settings)、世界书常驻段落(world_context)、
// 提示词注入文本(inject_text)、Agent 系统提示词占位符渲染(render_agent_prompt)、
// 执行者人设参考文本(persona_style)。
// 自 task_service.rs 拆分迁入,纯代码移动,逻辑不变;依赖经 `use super::*` 取自 mod.rs。
use super::*;

// ===== 三层固定提示词(单一来源)=====
// 规划器/执行者/汇总者内置指令:执行器(executor.rs)与设置预览(api/settings.rs)
// 共用同一份文本,改动只改这里,预览即真实下发。内置指令不经 untrusted 包裹
//(docs/模式提示词边界.md 第三节)。

/// 规划器内置指令:目标 → JSON 步骤数组(严格 JSON,低温保证结构稳定)。
/// 问题②(2026-08-31 实测:模型只写计划不收集信息,凭空编造步骤):允许先用
/// 只读工具侦察(白名单/轮数上限由执行侧 PLANNER_SCOUT_* 保证),再产出计划;
/// 「严格只输出 JSON 数组」约束的是最终计划轮的产出形态,侦察轮的工具调用不算违约。
pub(crate) const PLANNER_PROMPT: &str = "你是任务规划器。把用户目标拆解为 2~5 个可独立执行的具体步骤,每个步骤单一、明确、粒度适中(约 2~5 分钟可完成)。若目标涉及的信息不足,可先调用可用的只读工具(如读取文件/搜索/查询记忆)收集与目标相关的信息,再产出计划。计划须严格只输出 JSON 数组,不要输出任何解释或多余文字。数组元素格式:{\"name\":\"步骤名\",\"goal\":\"该步骤要完成的目标\"}。契约先行纪律:① 每个步骤的 goal 必须写明交付物与可判是的验收判据(能以是/否回答);② 同一交付物只允许一个权威版本,不得规划出会互相覆盖的重复步骤;③ 字段名与口径以本计划为唯一来源,执行方不得自拟字段名。";

/// 执行者内置指令:独立完成单个子任务并直接产出结果
pub(crate) const EXECUTOR_PROMPT: &str = "你是任务执行者,负责独立完成交给你的一个子任务。直接输出该子任务的最终结果:不要复述指令、不要输出计划或元文本、不要模拟对话、不要用标题包裹结果。引用或替换其他 agent 的既有产出时,必须写明取代对象(名称或版本);禁止出现未声明取代对象的「替换旧版/最新版为唯一口径」这类表述。若你的职责是验证/审计,只回填实测结论,不得另立一版交付物。";

/// 规划器修订指引段(批次 R2b plan-chat):追加在 PLANNER_PROMPT 之后,
/// 引导规划器按用户反馈修订已产出计划(输出契约不变:严格 JSON 数组)。
/// 文本含独特词「计划修订指引」:mock 测试据此经 [[reply_if:]] 区分首轮规划
/// 与修订轮(首轮 system 仅 PLANNER_PROMPT,修订轮才携带本段)。
pub(crate) const PLANNER_REVISE_GUIDANCE: &str = "## 计划修订指引\n你正在修订一份已产出的计划。用户会提供原始目标、当前计划(JSON)、与你就该计划的对话记录以及本轮反馈。请按本轮反馈修订计划:未受反馈影响的步骤尽量保持不变;输出契约不变——严格只输出 JSON 数组,不要输出任何解释或多余文字。";

/// 汇总者内置指令:综合各步骤结果产出最终成果
pub(crate) const SUMMARIZER_PROMPT: &str = "你是任务汇总者。下面是用户目标、执行计划与各步骤结果。请输出一份完整、有条理的最终成果,直接呈现结果本身(不要写「汇总如下」「以下是」等元文本)。";

/// team 模式规划器内置指令:目标 → 主 agent 分工拓扑 JSON(批次 4.3b)。
/// 严格 JSON 单一对象:mains 2~4 个主 agent,每主 1~4 个子目标,全局 ≤15 个
///(4×4=16 可超,全局封顶由解析侧截断保证);低温保证结构稳定(与 PLANNER_PROMPT
/// 同口径)。内置指令不经 untrusted 包裹。
pub(crate) const TEAM_PLANNER_PROMPT: &str = "你是团队规划器。把用户目标拆解并归并为 2~4 个主 agent 的分工拓扑:每个主 agent 领 1~4 个子目标(全局子目标总数不超过 15 个),每个子目标单一、明确、可独立执行。严格只输出 JSON 对象,不要输出任何解释或多余文字。格式:{\"mains\":[{\"name\":\"主 agent 分工名\",\"goals\":[{\"name\":\"子目标名\",\"goal\":\"该子目标要完成的具体目标\"}]}]}";

/// team 模式审计员内置指令:收齐各主 agent 产出后做一致性/质量/覆盖度审查
///(批次 4.3b)。严格 JSON 输出:通过=true;发现缺漏时打回指定主 agent
///(1-based 序号;可进一步用 step 指明该主第几个子目标,避免整主重跑造成多版本并存)。
/// 打回最多发生一轮(执行器保证);无打回时该结论进入最终结果的「## 审计结论」段,
/// 有打回时由补做后的终审结论替代(前端按此拆卡)。
/// 内置指令不经 untrusted 包裹。
pub(crate) const TEAM_AUDIT_PROMPT: &str = "你是团队审计员。下面是用户目标与各主 agent 的产出(各子目标以「子目标 N「子目标名」」标注,N 即该主内第几个子目标)。请做一致性(产出之间是否矛盾)、质量(是否达到目标要求)与覆盖度(目标各部分是否都有产出)审查。注意区分「中间稿/草稿」与「最终交付物」:同一交付物出现多个互相矛盾的版本即视为不一致,必须在结论中指出应以哪一版为准。严格只输出 JSON 对象,不要输出任何解释或多余文字。格式:{\"通过\":true或false,\"打回\":[{\"main\":主agent序号(从1开始),\"step\":该主第几个子目标(从1开始,必须与产出里的「子目标 N」编号一致;可省略表示整个主agent),\"instruction\":\"补做指令\"}],\"结论\":\"审查结论文本\"}。仅当确有缺漏且补做可修复时才打回,并尽量定位到具体子目标而非整个主 agent;无问题或问题不可经补做修复时通过=true、打回=[]。";

/// team 模式终审员内置指令:打回补做完成后的最终审查(2026-08 实测修复;
/// 实跑问题 2 改为结构化 JSON,使终审结论能作为终态闸门——旧实现只出自由文本,
/// 无法机器判定,审计不过也能落 done)。
/// 只产出 JSON(不再打回);终审结论进入最终结果的「## 审计结论」段,替代首次审计的
/// 打回原文;通过=false 时任务终态为 partial(不再静默 done)。内置指令不经 untrusted 包裹。
pub(crate) const TEAM_FINAL_AUDIT_PROMPT: &str = "你是团队终审员。下面是用户目标、各主 agent 补做后的最终产出与首轮审计意见。请基于最终产出做最终裁定:目标各部分是否已覆盖、质量是否达标、产出之间是否一致、首轮审计指出的问题是否已解决,以及是否仍存在同一交付物的多个矛盾版本。严格只输出 JSON 对象,不要输出任何解释或多余文字。格式:{\"通过\":true或false,\"结论\":\"终审结论文本\"}。只要仍存在未解决的缺漏、质量问题或产出间矛盾,通过=false 并在结论中说明。";

/// custom 模式反思步骤内置指令:检查上一版产出并输出判定结论(批次 4.3b)。
/// reflect 步骤按契约不携带用户 system_prompt(validate_flow 强制),统一用本内置指令。
pub(crate) const CUSTOM_REFLECT_PROMPT: &str = "你是反思审查员。下面是用户目标与上一版产出。检查:目标要求是否全部满足、有无事实/逻辑错误、是否被截断、有无复述指令或元文本。直接输出审查结论:无问题时输出 PASS 并附一句通过理由;有问题时输出 FAIL 并逐条列出需修复的问题。";

/// custom 模式内部规划步骤内置指令(generates=false 的 direct 步骤,如「理解意图」)。
/// 背景(2026-09-10 六模式实测):此类步骤此前复用 EXECUTOR_PROMPT「直接输出最终结果」,
/// 导致「只做内部规划」的步骤实际吐出完整正文,并被下一步原样回显。
/// 本指令明确「只做分析规划、产出要点、不产出面向用户的正文」,与步骤自身语义一致;
/// 其产出仅作后续步骤的参考上下文,不计入最终成果。内置指令不经 untrusted 包裹。
pub(crate) const TASK_INTERNAL_PLAN_PROMPT: &str = "你是任务内部规划者。请针对下面给出的目标做内部分析与规划:提炼关键要求、约束、需要参考的信息与执行要点,产出简洁的要点清单供后续步骤使用。只输出分析规划要点本身,不要输出面向用户的最终正文、不要模拟对话、不要用标题包裹结果。";

impl TaskService {
    /// 读取任务模式合并后的有效设置(生成参数覆盖项已应用,连接信息共享)。
    /// pub(crate):任务引擎(task_engine)构造 TaskRunContext 设置快照用。
    pub(crate) fn task_settings(&self) -> RuntimeSettings {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .for_mode(AppMode::Task)
    }

    /// 执行者角色卡读取(solo 提示词组装用;无执行者/角色不存在返回 None)。
    pub(crate) fn character_for(&self, character_id: Option<&str>) -> Option<CharacterRecord> {
        character_id.and_then(|cid| self.characters.get(cid))
    }

    /// 世界书常驻条目文本:过滤 enabled && constant 且内容非空,按 position→order→id 排序,
    /// 拼成「世界书设定」段落。无执行者时传空 id 仅取全局世界书。
    /// 过滤/排序/格式化逻辑下沉 prompt_kit::constant_world_text(WP7),与引擎侧共用口径。
    /// pub(crate):任务引擎 solo 提示词组装复用(勿复制实现)。
    pub(crate) fn world_context(&self, character_id: Option<&str>) -> String {
        let cid = character_id.unwrap_or("");
        let entries = self.world_books.collect_entries_for_character(cid);
        crate::services::prompt_kit::constant_world_text(&entries)
    }

    /// 提示词注入文本:简单模式取合成文本;复杂模式取 role=system 的启用楼层内容。
    /// 任务模式无对话历史,楼层 before/after/depth 位置语义不适用,仅注入 system 楼层。
    /// 逻辑下沉 prompt_kit::system_inject_text(WP7)。
    /// pub(crate):任务引擎 solo 提示词组装复用。
    pub(crate) fn inject_text(&self) -> String {
        let guard = self.prompt_inject.lock().unwrap_or_else(|e| e.into_inner());
        crate::services::prompt_kit::system_inject_text(guard.get())
    }

    /// 渲染任务模式 Agent 系统提示词占位符(与角色扮演同款占位符语义):
    /// {{char}}/{{character_name}}/{{character_description}}/{{personality}}/{{scenario}}/
    /// {{world_info}}/{{user}}/{{lastUserMessage}}(任务模式取任务目标)。
    /// 无执行者时角色类占位符一律置空,避免宏原文泄漏进 LLM 上下文。
    /// 替换链下沉 prompt_kit::render_character_placeholders(WP7),模式专属对经 extra 传入。
    /// pub(crate):任务引擎 solo 提示词组装复用。
    pub(crate) fn render_agent_prompt(
        &self,
        prompt: &str,
        character: Option<&CharacterRecord>,
        world_text: &str,
        user_goal: &str,
    ) -> String {
        crate::services::prompt_kit::render_character_placeholders(
            prompt,
            character,
            world_text,
            &[
                ("{{user}}".to_string(), "用户".to_string()),
                ("{{lastUserMessage}}".to_string(), user_goal.to_string()),
            ],
        )
    }
}

/// 从角色卡构建执行者人设参考文本。
/// full=false(精简,默认):人设(description)+ 人格(personality)两段——scenario 与
/// mes_example 文风示例对任务执行大部分无用(实测是 system 膨胀来源之一),精简模式不注入;
/// full=true(完整):现状四段全量(再加 世界观/情境 scenario + 文风示例 mes_example),
/// 与改造前逐字节一致。字段均为 V2 角色卡 data 对象的顶层字段。
/// 任一字段为空则跳过;全部为空返回空串(此时不注入人设)。
/// pub(crate):任务引擎 solo 提示词组装复用(勿复制实现)。
/// 开关口径见 docs/模式提示词边界.md 第一节(task_persona_full,None/false=精简)。
pub(crate) fn persona_style(c: &CharacterRecord, full: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !c.description.trim().is_empty() {
        parts.push(format!("人设:\n{}", c.description.trim()));
    }
    if let Some(raw) = c.data_raw.as_ref() {
        if let Some(p) = raw
            .get("personality")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
        {
            parts.push(format!("人格:\n{}", p.trim()));
        }
        // 完整模式才注入 scenario / mes_example(精简模式跳过,见函数文档)
        if full {
            if let Some(s) = raw
                .get("scenario")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
            {
                parts.push(format!("世界观/情境:\n{}", s.trim()));
            }
            if let Some(m) = raw
                .get("mes_example")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
            {
                parts.push(format!("文风示例:\n{}", m.trim()));
            }
        }
    }
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 四段人设字段各带唯一标记的测试角色卡
    fn mk_character() -> CharacterRecord {
        CharacterRecord {
            id: "c1".into(),
            name: "测试".into(),
            chara_name: "测试".into(),
            description: "DESCRIPTION-MARK 人设正文".into(),
            file_path: "test.json".into(),
            avatar_path: None,
            data_raw: Some(serde_json::json!({
                "personality": "PERSONALITY-MARK 人格",
                "scenario": "SCENARIO-MARK 情境",
                "mes_example": "MES-EXAMPLE-MARK 文风示例"
            })),
            first_mes: None,
            alternate_greetings: None,
            regex_scripts: None,
            card_plugins: None,
            creator: None,
            character_version: None,
            creator_notes: None,
            created_at: "2026-09-02".into(),
        }
    }

    /// 精简模式(R3a 默认):去掉 scenario 与 mes_example,保留 description + personality
    #[test]
    fn persona_style_slim_omits_scenario_and_mes_example() {
        let c = mk_character();
        let slim = persona_style(&c, false);
        assert!(slim.contains("DESCRIPTION-MARK"), "精简应保留人设: {slim}");
        assert!(slim.contains("PERSONALITY-MARK"), "精简应保留人格: {slim}");
        assert!(
            !slim.contains("SCENARIO-MARK"),
            "精简不得含 scenario 内容: {slim}"
        );
        assert!(
            !slim.contains("MES-EXAMPLE-MARK"),
            "精简不得含 mes_example 内容: {slim}"
        );
        assert!(
            !slim.contains("世界观/情境"),
            "精简不得含 scenario 段标题: {slim}"
        );
        assert!(
            !slim.contains("文风示例"),
            "精简不得含 mes_example 段标题: {slim}"
        );
    }

    /// 完整模式(R3a 开关打开):四段全量,与改造前逐字节一致
    #[test]
    fn persona_style_full_keeps_all_sections() {
        let c = mk_character();
        let full = persona_style(&c, true);
        for marker in [
            "DESCRIPTION-MARK",
            "PERSONALITY-MARK",
            "SCENARIO-MARK",
            "MES-EXAMPLE-MARK",
        ] {
            assert!(full.contains(marker), "完整模式应含 {marker}: {full}");
        }
        for title in ["人设:", "人格:", "世界观/情境:", "文风示例:"] {
            assert!(full.contains(title), "完整模式应含段标题 {title}: {full}");
        }
    }

    /// 全字段为空返回空串(两种模式同口径,此时不注入人设)
    #[test]
    fn persona_style_empty_when_all_blank() {
        let mut c = mk_character();
        c.description = String::new();
        c.data_raw = Some(serde_json::json!({}));
        assert!(persona_style(&c, false).is_empty(), "精简全空应返回空串");
        assert!(persona_style(&c, true).is_empty(), "完整全空应返回空串");
    }

    /// 提示词契约加固(审计项 G):mock 钩子 [[empty_if:任务执行者]] /
    /// [[reply_if:任务规划器]] 依赖的独特子串不得丢失;新增契约纪律须就位。
    /// 本用例是硬约束的回归锁:改文案时若删掉这两个子串,集成测试会连锁失败。
    #[test]
    fn builtin_prompts_keep_mock_hook_substrings_and_contract_disciplines() {
        assert!(
            EXECUTOR_PROMPT.contains("任务执行者"),
            "EXECUTOR_PROMPT 必须含「任务执行者」(mock [[empty_if:]] 钩子依赖)"
        );
        assert!(
            PLANNER_PROMPT.contains("任务规划器"),
            "PLANNER_PROMPT 必须含「任务规划器」(mock [[reply_if:]] 钩子依赖)"
        );
        // 规划器:契约先行(交付物 + 可判是验收判据 + 权威版本唯一 + 字段名唯一来源)
        for needle in ["交付物", "验收判据", "权威版本", "唯一来源"] {
            assert!(
                PLANNER_PROMPT.contains(needle),
                "PLANNER_PROMPT 应含契约先行要素「{needle}」: {PLANNER_PROMPT}"
            );
        }
        // 执行者:取代对象声明 + 审计只回填结论
        for needle in ["取代对象", "验证/审计"] {
            assert!(
                EXECUTOR_PROMPT.contains(needle),
                "EXECUTOR_PROMPT 应含纪律「{needle}」: {EXECUTOR_PROMPT}"
            );
        }
        // END 只属于子任务指令(agentgo description),不得进 EXECUTOR_PROMPT 污染用户可见正文
        assert!(
            !EXECUTOR_PROMPT.contains("END"),
            "EXECUTOR_PROMPT 不得含 END 收尾(属子任务指令模板): {EXECUTOR_PROMPT}"
        );
    }
}
