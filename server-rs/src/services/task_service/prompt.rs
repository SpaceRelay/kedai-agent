// 提示词组装:任务模式有效设置读取(task_settings)、世界书常驻段落(world_context)、
// 提示词注入文本(inject_text)、Agent 系统提示词占位符渲染(render_agent_prompt)、
// 执行者人设参考文本(persona_style)。
// 自 task_service.rs 拆分迁入,纯代码移动,逻辑不变;依赖经 `use super::*` 取自 mod.rs。
use super::*;

// ===== 三层固定提示词(单一来源)=====
// 规划器/执行者/汇总者内置指令:执行器(executor.rs)与设置预览(api/settings.rs)
// 共用同一份文本,改动只改这里,预览即真实下发。内置指令不经 untrusted 包裹
//(docs/契约-协议与配置.md 第三节)。
// 批次 B.3 依赖倒置:常量本体已机械搬迁至 task_core::prompt_consts
//(使 task_engine 侧可直接引用而不经 task_service);此处按名再导出宿主侧
// 既有使用者所需的三层固定提示词(executor.rs / api/settings.rs / 本文件),
// 调用方零改动。team/custom 专属常量由 task_engine 侧直接从 task_core 引用。
pub(crate) use crate::services::task_core::prompt_consts::{
    EXECUTOR_PROMPT, PLANNER_PROMPT, PLANNER_REVISE_GUIDANCE, SUMMARIZER_PROMPT,
};

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

    /// 执行者 system 提示词组装(单一实现):内置执行者指令 → 人设 → 世界书 →
    /// 提示词注入 → 用户可编辑 Agent 提示词,外部来源段落逐一 untrusted 边界包裹,
    /// 内置指令不包裹(WP7 纪律)。
    /// 调用方:legacy 步骤生成(task_service::generate_step_with)与任务引擎主 agent
    /// 循环(task_engine::solo::run_agent_loop)——两处此前各有一份逐行同构实现,
    /// 差异仅在角色取用方式(characters.get vs character_for,本实现统一经
    /// character_for,两者等价),合并后消除漂移风险。
    /// `user_goal` 供 {{lastUserMessage}} 占位符渲染(步骤生成传任务目标,主 agent 传 goal)。
    pub(crate) fn assemble_executor_system_prompt(
        &self,
        settings: &RuntimeSettings,
        character_id: Option<&str>,
        user_goal: &str,
    ) -> String {
        let character = self.character_for(character_id);

        let mut sys = String::from(EXECUTOR_PROMPT);
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
        let world = self.world_context(character_id);
        if !world.is_empty() {
            sys.push_str(&format!(
                "\n\n{}",
                crate::services::prompt_kit::untrusted_boundary("world_book", &world)
            ));
        }
        // 提示词注入默认隔离(2026-09-10 实测修复):solo/multi/team 主 agent 与 followup
        // 续跑共用本函数;仅显式开启 task_prompt_inject_enabled 才继承 prompt_floors.json
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
                user_goal,
            );
            sys.push_str(&format!(
                "\n\n{}",
                crate::services::prompt_kit::untrusted_boundary("agent_prompt", &rendered)
            ));
        }
        sys
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
/// 开关口径见 docs/契约-协议与配置.md 第一节(task_persona_full,None/false=精简)。
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
