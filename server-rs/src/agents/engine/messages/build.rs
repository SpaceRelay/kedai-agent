// LLM 消息构建:6 层位置拼装(build_llm_messages_with_position)与测试兼容入口
// (build_llm_messages,仅 #[cfg(test)]);空楼层/空注入跳过语义见 MAINTENANCE.md,不变。
// (自 messages.rs 拆分迁入,纯代码移动,逻辑不变;本次再按职责细拆:
//  步骤派生 → steps.rs,位置拼装/前缀稳定/槽位布局回归测试 → build_tests.rs)
// 可见性说明:原 messages.rs 中 pub(super)(= 对 engine 可见)的导出条目在此改为
// pub(in crate::agents::engine),供 messages/mod.rs 以相同可见性再导出,范围不变。
use crate::agents::engine::worldbook::WorldInjection;
use crate::models::types::LlmMessage;
use crate::parsing::assistant::AssistantVars;
use crate::parsing::macros::{expand_macros, MacroCtx};
use crate::services::prompt_inject_service::{FloorRole, InjectMode, PromptInjectConfig};
// 防注入包裹原语已下沉 services::prompt_kit(WP7),与任务模式共用同一实现
use crate::services::prompt_kit::untrusted_boundary;
use std::collections::HashMap;

/// 构建发送给 LLM 的消息数组(6 层规范落地的核心拼装点)。
/// 位置5(头部)= 系统提示词/主 agent 提示词;位置4 = 简单注入 + 复杂模式楼层;
/// 位置3 = 角色卡 + 世界书常态(constant);位置2 = 历史上下文;位置1 = 世界书激发(触发);
/// 位置0(尾部)= 预设尾部提示词。物理布局:
///   - system 消息 = 位置5 主提示词({{char}}/{{personality}}/{{scenario}}/{{world_info}} 宏展开)
///     + 位置4 系统角色楼层 + 位置3 角色卡 personality/scenario 与世界书常态(经宏/{{world_info}})
///   - 位置4 非系统角色楼层(user/assistant)紧随 system,按 order 升序
///   - 位置2 历史上下文原样
///   - 位置1 世界书激发 + 位置0 预设尾部追加到最新用户消息尾部(缓存友好:system+早期历史稳定)
///
/// 参数:world_constant = 位置3 世界书常态文本;world_triggered = 位置1 世界书激发文本;
/// preset_tail = 位置0 预设尾部提示词(空 = 禁用)。宏展开共享同一 mctx,
/// 保证 {{setvar}} 在前面的文本写入、{{getvar}} 在后面的文本能读到(时序与 ST prompt_order 一致)。
/// 返回 (messages, protected_tail):protected_tail 为拼入 system 末尾的注入文本/楼层块
/// 字符数(无注入为 0),供 trim_to_context 截断时优先保留(注入不能先于角色设定被切掉)。
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn build_llm_messages(
    character_name: &str,
    character_description: &str,
    personality: &str,
    scenario: &str,
    world_text: Option<&str>,
    history: &[(String, String)],
    custom_prompt: Option<&str>,
    inject: Option<&PromptInjectConfig>,
    vars: &mut HashMap<String, String>,
    assistant_vars: &mut AssistantVars,
) -> (Vec<LlmMessage>, usize) {
    // 兼容入口:旧单一世界书视为常态组(位置3,role=system);无尾部注入(测试与旧调用路径不变)
    // 测试用空作用域上下文(None 语义,行为与旧一致)
    let constant: Vec<WorldInjection> = world_text
        .map(|t| {
            vec![WorldInjection {
                role: "system".to_string(),
                text: t.to_string(),
            }]
        })
        .unwrap_or_default();
    build_llm_messages_with_position(
        character_name,
        character_description,
        personality,
        scenario,
        &constant,
        &[],
        history,
        custom_prompt,
        inject,
        None,
        "user",
        None,
        "user",
        vars,
        assistant_vars,
        None,
    )
}
/// build_llm_messages 的 6 层位置版本(位置定义见 build_llm_messages 文档)。
/// 世界书常驻(role=system)并入 system;激发与预设尾部追加到最新用户消息尾部
/// (system + 早期历史前缀保持稳定 → 前缀缓存命中率显著提升,缓存友好)。
/// 各条目角色可自由选择:system 角色在尾部消息位置钳制为 user。
/// reflect_advice = 反思失败建议(位置0,自动生成的改进建议;注入到位置1 激发之后、
/// 预设尾部之前,不再作为最末尾内容);reflect_advice_role = user / assistant(system 钳制为 user)。
#[allow(clippy::too_many_arguments)]
pub(in crate::agents::engine) fn build_llm_messages_with_position(
    character_name: &str,
    character_description: &str,
    personality: &str,
    scenario: &str,
    world_constant: &[WorldInjection],
    world_triggered: &[WorldInjection],
    history: &[(String, String)],
    custom_prompt: Option<&str>,
    inject: Option<&PromptInjectConfig>,
    preset_tail: Option<&str>,
    preset_tail_role: &str,
    reflect_advice: Option<&str>,
    reflect_advice_role: &str,
    vars: &mut HashMap<String, String>,
    assistant_vars: &mut AssistantVars,
    // 7 作用域变量(计划二);None = 无作用域上下文(宏/EJS 走旧行为)
    scopes: Option<&mut crate::parsing::scopes::ScopeVars>,
) -> (Vec<LlmMessage>, usize) {
    let mut messages = Vec::new();
    // 角色卡字段属于不可信素材。只包裹宏展开后的替换值，不改写原始角色扮演文本。
    let bounded_description =
        untrusted_boundary("character_card.description", character_description);
    let bounded_personality = untrusted_boundary("character_card.personality", personality);
    let bounded_scenario = untrusted_boundary("character_card.scenario", scenario);
    // 宏上下文:自定义系统提示词 / 简单合成 / 楼层 / 世界书共享同一 vars,按序展开,
    // 保证 {{setvar}} 在前面的文本写入、{{getvar}} 在后面的文本能读到(时序与 ST prompt_order 一致)。
    let mut mctx = MacroCtx {
        character_name,
        character_description: &bounded_description,
        user_name: "用户",
        user_input: "",
        personality: &bounded_personality,
        scenario: &bounded_scenario,
        history,
        vars,
        assistant_vars: Some(assistant_vars),
        scopes,
    };
    // 无用户消息时(空历史/仅 assistant 历史):位置1 激发与位置0 预设尾部没有可追加的
    // user 消息,退化为并入 system 兜底(避免凭空新增 user 消息干扰对话流)。
    let has_user = history.iter().any(|(r, _)| r == "user");
    // 位置3 常驻 system 角色 → 并入 system 文本;无 user 时激发/预设尾部也并入兜底
    let sys_world: String = if has_user {
        world_constant
            .iter()
            .filter(|i| i.role == "system")
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    } else {
        let mut parts: Vec<String> = Vec::new();
        for inj in world_constant.iter().chain(world_triggered.iter()) {
            parts.push(inj.text.clone());
        }
        // 反思失败建议(位置0):激发之后、预设尾部之前
        if let Some(a) = reflect_advice {
            if !a.trim().is_empty() {
                parts.push(a.to_string());
            }
        }
        if let Some(t) = preset_tail {
            parts.push(t.to_string());
        }
        parts.join("\n\n")
    };
    let mut sys = match custom_prompt {
        Some(tpl) if !tpl.trim().is_empty() => {
            // {{world_info}} 宏层不认,先替换为位置3 世界书常驻文本;{{character_name}}/{{character_description}}
            // /{{char}}/{{user}}/{{random}}/{{getvar}} 等由宏系统统一展开
            let bounded_world = untrusted_boundary("world_book", &sys_world);
            let pre = tpl.replace("{{world_info}}", &bounded_world);
            expand_macros(&pre, &mut mctx)
        }
        _ => {
            // 内置默认模板(用户清空自定义系统提示词时兜底):文学创作定位,融合「可待」预设精华
            // (创作总纲/不转述/独立角色/基础文风/模块化剧情),并保留 kedai 变量更新协议。
            let mut s = format!(
                "你是角色「{character_name}」的扮演者与文学创作者,与用户进行沉浸式角色扮演 / 文学创作。"
            );
            if !character_description.is_empty() {
                s.push_str(&format!(
                    "\n\n{}",
                    untrusted_boundary(
                        "character_card",
                        &format!("背景设定:\n{character_description}")
                    )
                ));
            }
            if !sys_world.is_empty() {
                s.push_str(&format!(
                    "\n\n{}",
                    untrusted_boundary("world_book", &sys_world)
                ));
            }
            s.push_str(
                "\n\n【创作总纲】\n\
                 本会话定位为文学创作系统:你的正文是小说文本而非聊天记录,以\"能否被称为一段好小说\"为最低验收标准。描写应当可朗读、可回味、经得起推敲。\n\n\
                 【角色扮演规则】\n\
                 1. 视角与口吻:视角遵循系统提示词与用户要求(用户未指定时,默认以角色的视角与口吻叙述);不要出现旁白标题、「以上是回复」「作为AI」等元文本;角色设定与世界观保持一致。\n\
                 2. 用户指令优先:用户提出的字数、风格、视角、情节走向要求必须服从;用户提问必须在本轮正面回答,不允许回避。\n\
                 3. 不转述:用户输入的动作与对话视为已经发生,不得复述或引用,直接从其后无缝衔接继续创作新的剧情。\n\
                 4. 主角与独立角色:用户所操控角色是剧情主角,但所有角色都是独立的人,有自己的价值观、思考方式与喜好,不会无条件依附用户,好感度不会因小事凭空上涨或下降。\n\
                 5. 推进节奏:一次输出不得把当前事件直接推进至末尾,应当适当拆分事件、合理安排节奏,在正文末尾为用户留下可互动的窗口。\n\n\
                 【写作要求】\n\
                 1. 句式:长短句合理搭配;段落之间空一行;适当插入短段,不得连续堆叠对话;各段长度合理交错。\n\
                 2. 描写:区分周围描写(环境)与聚焦描写(重点物),合理穿插;相同环境描写不得重复提及;拒绝\"自然式\"滥用与过度精确化的机械描写;比喻须贴切,不把物比作与其无关的事物。\n\
                 3. 对话:口语化,话题连贯不重复;所有说出口的对话必须用中文双引号\"...\"包裹。\n\
                 4. 规避:禁止先否后肯句式(不是……而是……)、动物比喻、元评论(用括号解释正文)、夸张化描写;非必要不描写回忆,尤其不得复述与上文类似的回忆。\n\
                 5. 开头结尾:每次输出的开头与结尾都应当新颖,不得与上一次输出类似或重复,不得强行升华。\n\
                 6. 不得以任何方式描述任何角色的具体年龄。\n\
                 7. 角色对话必须符合其性格与对话示例,禁止刻板印象化、指导式、刻薄式、油腻式语言;该爆发时爆发,该平静时平静。\n\
                 8. 逻辑一致:严格遵守时间/对话/行为/变量/剧情的逻辑;角色不得知晓不该知道的设定信息,不得出现\"根据XX的设定\"这类作者视角发言。\n\n\
                 【剧情结构(模块化)】\n\
                 每次输出将剧情分为三个剧情模块和一个结尾模块,模块之间以过渡段承转;模块类型在【环境/推进/插入】中选择,同类型不得连续出现三次;模块与结尾的格式、内容不得与上一次输出相似或雷同;禁止在正文中对模块或过渡部分做任何标注。\n\n\
                 【输出纪律】\n\
                 一次只输出角色回应本身;若需要分段,使用空行,不使用 Markdown 标题。",
            );
            s
        }
    };
    // 拼入 system 尾部的注入文本总长度(截断保护用)
    let mut protected_tail: usize = 0;

    // 位置4 简单模式:合成四项注入文本,宏展开后拼入 system 末尾
    if let Some(cfg) = inject {
        if cfg.mode == InjectMode::Simple {
            let text = cfg.simple_inject_text();
            if !text.is_empty() {
                let expanded = expand_macros(&text, &mut mctx);
                protected_tail += expanded.chars().count();
                sys.push_str(&format!("\n\n{expanded}"));
            }
        }
    }

    // 位置4 复杂模式:楼层按 order 顺序展开(共享 mctx),统一归位位置4——
    // role=system 的内容拼入系统提示词;user/assistant 角色紧随 system 按 order 排
    // (废弃旧 before/after/depth 历史内散插;旧配置 position 字段仅兼容解析,不再参与注入)。
    let mut floors_in_chat: Vec<(String, String)> = Vec::new();
    if let Some(cfg) = inject {
        if cfg.mode == InjectMode::Complex {
            for floor in cfg.enabled_floors_sorted() {
                let content = expand_macros(&floor.content, &mut mctx);
                if matches!(floor.role, FloorRole::System) {
                    // system 角色始终进系统提示词(避免对话中间夹 system 消息)
                    protected_tail += content.chars().count();
                    sys.push_str(&format!("\n\n{content}"));
                } else {
                    floors_in_chat.push((floor.role.as_str().to_string(), content));
                }
            }
            // 禁词库自省提示:所有模式生效(简单模式由 simple_inject_text 追加,
            // 复杂模式在此追加;deep/agent/custom 另由引擎收尾工具替换兜底)。
            let banned_hint = cfg.simple.banned_words_hint();
            if !banned_hint.is_empty() {
                let expanded = expand_macros(&banned_hint, &mut mctx);
                protected_tail += expanded.chars().count();
                sys.push_str(&format!("\n\n{expanded}"));
            }
        }
    }

    messages.push(LlmMessage::plain("system", &sys));
    // 位置4 非系统角色楼层:紧随 system,保持楼层 order 顺序
    // (空内容跳过:导入 ST 预设时纯 {{addvar}} 累积宏楼层展开为空,避免产生空 user/assistant 消息)
    for (role, content) in &floors_in_chat {
        if !content.trim().is_empty() {
            messages.push(LlmMessage::plain(role, content));
        }
    }
    // 位置3 常驻非系统角色(role=user/assistant):作独立消息紧跟位置4(按注入顺序)
    if has_user {
        for inj in world_constant.iter().filter(|i| i.role != "system") {
            let expanded = expand_macros(&inj.text, &mut mctx);
            if !expanded.trim().is_empty() {
                messages.push(LlmMessage::plain(
                    &inj.role,
                    &untrusted_boundary("world_book", &expanded),
                ));
            }
        }
    }

    // 位置2 历史上下文 + 位置1 世界书激发 + 位置0 预设尾部
    if let Some(idx) = history.iter().rposition(|(r, _)| r == "user") {
        // 尾部角色:system 在尾部消息位置钳制为 user(尾部无 system 消息概念)
        let tail_role = if preset_tail_role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        // 位置1 user 激发(含 system 钳制为 user)+ 位置0 反思建议 + 位置0 预设尾部(user)→ 拼最新 user 消息尾部
        let mut user_tail: Vec<String> = Vec::new();
        for inj in world_triggered.iter().filter(|i| i.role != "assistant") {
            let expanded = expand_macros(&inj.text, &mut mctx);
            if !expanded.trim().is_empty() {
                user_tail.push(expanded);
            }
        }
        // 反思失败建议(位置0):自动生成的改进建议,注入在激发之后、预设尾部之前。
        // 按 reflect_advice_role 选边:user 并入最新 user 消息尾部;assistant 走下方 assistant_tail。
        if reflect_advice_role != "assistant" {
            if let Some(a) = reflect_advice {
                if !a.trim().is_empty() {
                    user_tail.push(a.to_string());
                }
            }
        }
        if let Some(t) = preset_tail {
            if tail_role == "user" {
                let expanded = expand_macros(t, &mut mctx);
                if !expanded.trim().is_empty() {
                    user_tail.push(untrusted_boundary("world_book", &expanded));
                }
            }
        }
        let user_tail_text = if user_tail.is_empty() {
            None
        } else {
            Some(user_tail.join("\n\n"))
        };
        // 位置1 assistant 激发 + 位置0 反思建议 + 位置0 预设尾部(assistant)→ 独立 assistant 消息(最新 user 之后)
        let mut assistant_tail: Vec<String> = Vec::new();
        for inj in world_triggered.iter().filter(|i| i.role == "assistant") {
            let expanded = expand_macros(&inj.text, &mut mctx);
            if !expanded.trim().is_empty() {
                assistant_tail.push(expanded);
            }
        }
        // 反思失败建议(位置0,assistant 角色):激发之后、预设尾部之前
        if reflect_advice_role == "assistant" {
            if let Some(a) = reflect_advice {
                if !a.trim().is_empty() {
                    assistant_tail.push(a.to_string());
                }
            }
        }
        if let Some(t) = preset_tail {
            if tail_role == "assistant" {
                let expanded = expand_macros(t, &mut mctx);
                if !expanded.trim().is_empty() {
                    assistant_tail.push(untrusted_boundary("world_book", &expanded));
                }
            }
        }
        for (i, (role, content)) in history.iter().enumerate() {
            // history 中的 system 消息与既有行为一致跳过
            if role == "system" {
                continue;
            }
            let mut final_content = content.clone();
            if i == idx {
                if let Some(t) = &user_tail_text {
                    final_content.push_str(&format!("\n\n{t}"));
                }
            }
            messages.push(LlmMessage::plain(role, &final_content));
            // assistant 尾部注入紧跟最新 user 消息之后(位置1 → 位置0 顺序)
            if i == idx {
                for at in &assistant_tail {
                    messages.push(LlmMessage::plain("assistant", at));
                }
            }
        }
    } else {
        // 无 user 消息:激发与预设尾部已并入 system,历史原样输出
        for (role, content) in history.iter() {
            if role == "system" {
                continue;
            }
            messages.push(LlmMessage::plain(role, content));
        }
    }
    (messages, protected_tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::prompt_inject_service::{FloorPosition, PromptFloor};

    /// build_llm_messages:world_text 注入到 system 内
    #[test]
    fn build_messages_injects_world_text() {
        let mut vars = HashMap::new();
        let (msgs, _) = build_llm_messages(
            "芽衣",
            "兔族少女。",
            "",
            "",
            Some("世界书设定:\n[地点]\n图书馆。"),
            &[],
            None,
            None,
            &mut vars,
            &mut AssistantVars::new(),
        );
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].role == "system");
        assert!(msgs[0].content.contains("世界书设定:"));
        assert!(msgs[0].content.contains("图书馆。"));
        assert!(msgs[0].content.contains("兔族少女。"));
    }
    /// build_llm_messages:自定义提示词模板替换占位符,世界书经 {{world_info}} 注入
    #[test]
    fn custom_prompt_placeholders_replaced() {
        let tpl = "你是{{character_name}},{{character_description}},以下是世界书:\n{{world_info}}\n自定义要求。";
        let mut vars = HashMap::new();
        let (msgs, _) = build_llm_messages(
            "芽衣",
            "兔族少女。",
            "",
            "",
            Some("世界书设定:\n[地点]\n图书馆。"),
            &[],
            Some(tpl),
            None,
            &mut vars,
            &mut AssistantVars::new(),
        );
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.contains("你是芽衣"));
        assert!(msgs[0].content.contains("兔族少女。"));
        assert!(msgs[0].content.contains("图书馆。"));
        assert!(msgs[0].content.contains("自定义要求。"));
        // 自定义提示词不再拼接内置默认要求
        assert!(!msgs[0].content.contains("始终以角色身份回复"));
    }
    /// build_llm_messages:自定义系统提示词同样走宏系统(宏在提示词中持续工作)
    #[test]
    fn custom_prompt_expands_macros() {
        // {{char}}/{{random}}/{{setvar}}/{{getvar}} 应全部展开;时序:setvar 在 getvar 前生效
        let tpl =
            "你是{{char}},随机值{{random:5,5}},{{setvar::地点::酒馆}}当前地点={{getvar::地点}}";
        let mut vars = HashMap::new();
        let (msgs, _) = build_llm_messages(
            "芽衣",
            "兔族少女。",
            "",
            "",
            None,
            &[],
            Some(tpl),
            None,
            &mut vars,
            &mut AssistantVars::new(),
        );
        let sys = &msgs[0].content;
        assert!(sys.contains("你是芽衣"), "{{char}} 应展开:{sys}");
        assert!(sys.contains("随机值5"), "{{random:5,5}} 应展开为 5:{sys}");
        assert!(
            sys.contains("当前地点=酒馆"),
            "setvar/getvar 时序错误:{sys}"
        );
        // 与楼层共享 vars:setvar 副作用写入 vars,供持久化
        assert_eq!(vars.get("地点").map(|s| s.as_str()), Some("酒馆"));
        // {{world_info}} 依旧由调用方替换后进入
        let tpl2 = "世界书:\n{{world_info}}";
        let (msgs2, _) = build_llm_messages(
            "芽衣",
            "",
            "",
            "",
            Some("世界书设定:图书馆。"),
            &[],
            Some(tpl2),
            None,
            &mut vars,
            &mut AssistantVars::new(),
        );
        assert!(msgs2[0].content.contains("图书馆。"));
    }
    /// build_llm_messages:空自定义提示词回退内置默认
    #[test]
    fn empty_custom_prompt_falls_back() {
        let mut vars = HashMap::new();
        let (msgs, _) = build_llm_messages(
            "芽衣",
            "兔族少女。",
            "",
            "",
            None,
            &[],
            Some("  "),
            None,
            &mut vars,
            &mut AssistantVars::new(),
        );
        assert!(
            msgs[0].content.contains("文学创作系统"),
            "内置默认应含创作总纲: {}",
            msgs[0].content
        );
    }
    /// build_llm_messages:简单模式合成文本拼入 system;楼层按位置插入历史
    #[test]
    fn build_messages_injects_simple_and_floors() {
        let mut vars = HashMap::new();
        let mut cfg = PromptInjectConfig {
            mode: InjectMode::Simple,
            ..PromptInjectConfig::default()
        };
        cfg.simple.word_count_enabled = true;
        cfg.simple.word_count = 99;
        let history = vec![("user".to_string(), "你好".to_string())];
        let (msgs, _) = build_llm_messages(
            "芽衣",
            "兔族少女。",
            "",
            "",
            None,
            &history,
            None,
            Some(&cfg),
            &mut vars,
            &mut AssistantVars::new(),
        );
        assert_eq!(msgs.len(), 2);
        assert!(
            msgs[0].content.contains("99 字"),
            "msgs[0]: {}",
            msgs[0].content
        );
        assert!(
            msgs[0].content.contains("文学创作系统"),
            "内置默认应含创作总纲: {}",
            msgs[0].content
        );

        // 复杂模式:楼层统一归位位置4 —— system 角色进 system 尾部,user/assistant 紧随 system 按 order 排
        // (旧 before/after/depth 历史内散插已废弃;position 字段仅兼容解析,不再参与注入)
        cfg.mode = InjectMode::Complex;
        cfg.simple.word_count_enabled = false;
        cfg.floors = vec![
            PromptFloor {
                id: "f1".into(),
                name: "开".into(),
                content: "{{char}}的开场".into(),
                role: FloorRole::User,
                position: FloorPosition::Before,
                depth: 0,
                enabled: true,
                order: 0,
            },
            PromptFloor {
                id: "f2".into(),
                name: "变".into(),
                content: "{{setvar::地点::酒馆}}地点={{getvar::地点}}".into(),
                role: FloorRole::System,
                position: FloorPosition::System,
                depth: 0,
                enabled: true,
                order: 1,
            },
            PromptFloor {
                id: "f3".into(),
                name: "尾".into(),
                content: "尾{{getvar::地点}}".into(),
                role: FloorRole::Assistant,
                position: FloorPosition::After,
                depth: 0,
                enabled: true,
                order: 2,
            },
        ];
        let (msgs, _) = build_llm_messages(
            "芽衣",
            "兔族少女。",
            "",
            "",
            None,
            &history,
            None,
            Some(&cfg),
            &mut vars,
            &mut AssistantVars::new(),
        );
        // system(含 f2 宏)+ f1(user 楼层)+ f3(assistant 楼层)+ 历史用户消息
        assert_eq!(msgs.len(), 4, "消息序列: {msgs:?}");
        assert!(
            msgs[0].content.contains("地点=酒馆"),
            "system 宏: {}",
            msgs[0].content
        );
        assert_eq!(msgs[1].role, "user");
        assert_eq!(msgs[1].content, "芽衣的开场");
        assert_eq!(msgs[2].role, "assistant");
        assert_eq!(msgs[2].content, "尾酒馆");
        assert_eq!(msgs[3].role, "user");
        assert_eq!(msgs[3].content, "你好");
        // setvar 写入了 vars,可被持久化
        assert_eq!(vars.get("地点").map(|s| s.as_str()), Some("酒馆"));
    }
}
