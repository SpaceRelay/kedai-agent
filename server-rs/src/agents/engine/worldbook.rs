// 世界书注入:条目匹配与分组(常态·激发)、状态块构建与随机注入概率
use super::*;

/// 世界书条目匹配并拼接注入文本。
/// 规则:
///   - constant=true 的条目始终注入
///   - 非 constant 条目:enabled 前提下按 keys / keys_secondary(副关键词)/ regex 命中;
///     keys 命中按条目 depth 限制扫描最近 N 条用户消息(0 = 全部历史,缺省 4)
///   - case_sensitive=true 时大小写敏感匹配
///   - use_probability 时按 probability% 随机决定命中后是否注入
///   - 注入顺序:entries 已按 position → order → id 排序,输出保持该顺序
///
/// 每条命中/常驻条目的 content 先经酒馆助手渲染(EJS 模板 + {{format_message_variable}} 等)。
/// 单条世界书注入(6 层规范):role 为注入消息角色
#[derive(Debug, Clone)]
pub(crate) struct WorldInjection {
    pub role: String,
    pub text: String,
}

/// 世界书收集结果(6 层规范):按注入位置分为两组——
/// constant(常态,位置3)与 triggered(激发,位置1),每组内每条带注入角色
/// (条目显式 role 优先;缺省:常驻 → system,激发 → user)。
#[derive(Debug, Default)]
pub(crate) struct WorldText {
    /// 常态条目(位置3):role=system 拼入 system 文本;role=user/assistant 作独立消息紧跟 system
    pub constant: Vec<WorldInjection>,
    /// 激发条目(位置1):role=user 追加最新用户消息尾部;role=assistant 作独立消息;
    /// role=system 在尾部钳制为 user
    pub triggered: Vec<WorldInjection>,
}

impl WorldInjection {
    /// 解析条目注入角色:显式 role 优先;缺省 constant → system,激发 → user
    fn from_entry(e: &crate::parsing::world_book::WorldEntry, default_role: &str) -> String {
        e.role.as_deref().unwrap_or(default_role).to_string()
    }
}

/// 分组建世界书文本:常态(constant=true → 位置3)与激发(触发命中 → 位置1)各一组。
/// 每组内部保持 entries 传入顺序(collect_entries 已按 position→order→id 排序)。
/// 兼容入口:空渲染上下文(无 getwi/getqr 等外部数据,仅变量树)。
pub(crate) fn collect_world_text_grouped(
    entries: &[crate::parsing::world_book::WorldEntry],
    history: &[crate::models::types::MessageRecord],
    assistant_vars: &mut AssistantVars,
) -> WorldText {
    let mut ctx = crate::parsing::assistant::RenderCtx::new(assistant_vars);
    collect_world_text_grouped_with(entries, history, &mut ctx)
}

/// 分组建世界书文本(带渲染上下文,engine 注入链路用):
/// 每条目先执行 @@ 装饰器(if/unless 决定是否注入,var/set 写入变量树),
/// 再经 ctx 渲染(EJS 内建 getwi/getqr/getChatMessage 等真实读取);
/// 渲染登记的注入清单留在 ctx.data.injected 供 engine 消费。
pub(crate) fn collect_world_text_grouped_with(
    entries: &[crate::parsing::world_book::WorldEntry],
    history: &[crate::models::types::MessageRecord],
    ctx: &mut crate::parsing::assistant::RenderCtx<'_>,
) -> WorldText {
    use crate::parsing::assistant::{apply_entry_decorators, render_assistant_content_with};
    let mut world = WorldText::default();
    if entries.is_empty() {
        return world;
    }
    // 全部用户消息(从旧到新),按条目 depth 截取尾部;缺省扫描深度 4
    let all_user: Vec<String> = history
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| m.content.clone())
        .collect();

    let mut constant_parts: Vec<WorldInjection> = Vec::new();
    let mut triggered_parts: Vec<WorldInjection> = Vec::new();
    for e in entries {
        if !e.enabled {
            continue;
        }
        // @@ 装饰器(if/unless/var/set):决定是否注入并执行变量写入(渲染前)
        if !apply_entry_decorators(e, ctx) {
            continue;
        }
        if e.constant {
            // 酒馆助手渲染(EJS/宏/状态占位符),渲染后为空不注入
            let rendered = render_assistant_content_with(&e.content, ctx);
            if !rendered.trim().is_empty() {
                constant_parts.push(WorldInjection {
                    role: WorldInjection::from_entry(e, "system"),
                    // keep_empty_bracket=true:保持存量卡的逐字节行为(空 comment 也出 `[]`)。
                    // 差异化约定与单点实现见 services::prompt_kit::world_entry_text。
                    text: crate::services::prompt_kit::world_entry_text(
                        &e.comment, &rendered, true,
                    ),
                });
            }
            continue;
        }
        // 激发判定(「窗口→匹配→概率」三步已收敛到 prompt_kit::triggered_entry_hits):
        // 本场景只扫 **user 消息**,未声明 scan_depth 时兜底用条目 depth(存量卡行为)。
        // 概率 roll 统一走 prompt_kit::world_entry_roll(收敛前用本模块的 simple_roll,
        // 与 generate-raw 侧的 subsec_nanos 不同源,现已统一)。
        if !crate::services::prompt_kit::triggered_entry_hits(
            e,
            &all_user,
            e.depth,
            crate::services::prompt_kit::world_entry_roll(),
        ) {
            continue;
        }
        if !e.content.trim().is_empty() {
            let rendered = render_assistant_content_with(&e.content, ctx);
            if !rendered.trim().is_empty() {
                triggered_parts.push(WorldInjection {
                    role: WorldInjection::from_entry(e, "user"),
                    text: crate::services::prompt_kit::world_entry_text(
                        &e.comment, &rendered, true,
                    ),
                });
            }
        }
    }
    world.constant = constant_parts;
    world.triggered = triggered_parts;
    world
}

/// 兼容合并视图(旧行为:常态与激发混为一段文本;运行时请用 collect_world_text_grouped
/// 分组注入,本函数仅测试用)
#[cfg(test)]
fn collect_world_text(
    entries: &[crate::parsing::world_book::WorldEntry],
    history: &[crate::models::types::MessageRecord],
    assistant_vars: &mut AssistantVars,
) -> Option<String> {
    let g = collect_world_text_grouped(entries, history, assistant_vars);
    let mut parts: Vec<String> = Vec::new();
    if !g.constant.is_empty() {
        parts.push(
            g.constant
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
    }
    if !g.triggered.is_empty() {
        parts.push(
            g.triggered
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("世界书设定:\n{}", parts.join("\n\n")))
    }
}

/// 变量树状态块文本(供 make_state_block_with_contract / 测试使用)
fn state_block_text(vars: &AssistantVars) -> String {
    format!(
        "[当前状态]\n{}\n\n[状态更新协议]\n状态变化时必须在本轮输出 <UpdateVariable> 块\
         (JSONPatch 数组,支持 replace / delta / addvar / remove,路径对应当前状态的键);\
         无变化则不要输出。示例:\n\
         <UpdateVariable><JSONPatch>[{{\"op\":\"replace\",\"path\":\"/hp\",\"value\":88}}]</JSONPatch></UpdateVariable>\n\
         除该块外不要输出任何自定义标签。",
        vars.to_json()
    )
}

/// 契约驱动的状态块注入(P5):契约存在时状态文本经 build_state_prompt 裁剪
/// (dueFields + observe,只暴露到期字段与依赖,替代整树 to_json —— G4 增量裁剪),
/// 且本轮无到期字段时不再注入状态块(变量本轮不更新,正文也不承载状态)。
/// 无契约(存量卡)走整树兼容路径(文档 D-2)。
pub(super) fn make_state_block_with_contract(
    vars: &AssistantVars,
    role: &str,
    contract: Option<&crate::contracts::Contract>,
    turn_id: u64,
) -> Option<WorldInjection> {
    if vars.is_empty() {
        return None;
    }
    let text = match contract {
        Some(c) => {
            let (state_text, _due) =
                crate::contracts::build_state_prompt(Some(c), vars.tree(), turn_id);
            if state_text.is_empty() {
                // 本轮无到期字段:变量不更新,正文也不注入状态块
                return None;
            }
            format!(
                "[当前状态]\n{state_text}\n\n[状态更新协议]\n状态变化时必须在本轮输出 <UpdateVariable> 块\
                 (JSONPatch 数组,支持 replace / delta / addvar / remove,路径对应当前状态的键);\
                 无变化则不要输出。示例:\n\
                 <UpdateVariable><JSONPatch>[{{\"op\":\"replace\",\"path\":\"/hp\",\"value\":88}}]</JSONPatch></UpdateVariable>\n\
                 除该块外不要输出任何自定义标签。"
            )
        }
        // 无契约(存量卡):整树兼容路径,与历史 make_state_block 行为逐字一致
        None => state_block_text(vars),
    };
    Some(WorldInjection {
        role: role.to_string(),
        text,
    })
}

/// 兼容视图(测试用):状态块并入既有世界书文本末尾
#[cfg(test)]
fn append_state_block(world_text: Option<String>, vars: &AssistantVars) -> Option<String> {
    if vars.is_empty() {
        return world_text;
    }
    let state_block = state_block_text(vars);
    Some(match world_text {
        Some(existing) => format!("{existing}\n\n{state_block}"),
        None => state_block,
    })
}

/// 注:本文件原有的 `simple_roll()` 已于 2026-09-14 移除——激发概率的 roll 来源统一收敛到
/// `crate::services::prompt_kit::world_entry_roll()`(此前与 generate-raw 侧各自实现,
/// 来源不同导致同轮行为不可复现地分叉)。若需随机 roll,请调用该单点函数,勿再写一份。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsing::world_book::WorldEntry;

    fn entry(
        comment: &str,
        keys: Vec<&str>,
        content: &str,
        constant: bool,
        enabled: bool,
    ) -> WorldEntry {
        WorldEntry {
            id: 0,
            comment: comment.to_string(),
            keys: keys.into_iter().map(|s| s.to_string()).collect(),
            keys_secondary: Vec::new(),
            regex: None,
            use_regex: false,
            content: content.to_string(),
            constant,
            enabled,
            position: 0,
            depth: 4,
            scan_depth: None,
            order: 100,
            case_sensitive: false,
            sticky: 0,
            cooldown: 0,
            probability: 100,
            use_probability: false,
            role: None,
            decorators: Vec::new(),
        }
    }

    /// 构造正则条目
    fn regex_entry(comment: &str, regex: &str, content: &str, enabled: bool) -> WorldEntry {
        WorldEntry {
            id: 0,
            comment: comment.to_string(),
            keys: Vec::new(),
            keys_secondary: Vec::new(),
            regex: Some(regex.to_string()),
            use_regex: true,
            content: content.to_string(),
            constant: false,
            enabled,
            position: 0,
            depth: 4,
            scan_depth: None,
            order: 100,
            case_sensitive: false,
            sticky: 0,
            cooldown: 0,
            probability: 100,
            use_probability: false,
            role: None,
            decorators: Vec::new(),
        }
    }

    fn message(role: &str, content: &str) -> MessageRecord {
        message_with(role, content, json!({}))
    }

    /// 带自定义 extra 的消息
    fn message_with(role: &str, content: &str, extra: Value) -> MessageRecord {
        MessageRecord {
            id: 0,
            session_id: "s".into(),
            role: role.into(),
            content: content.into(),
            extra,
            created_at: "".into(),
        }
    }

    /// constant=true 始终注入,即使 key 未命中
    #[test]
    fn constant_entries_always_inject() {
        let entries = vec![entry(
            "世界观",
            vec!["兽人"],
            "这是一个兽人社会。",
            true,
            true,
        )];
        let history = vec![message("user", "你好")];
        let text = collect_world_text(&entries, &history, &mut AssistantVars::new()).unwrap();
        assert!(text.contains("兽人社会"));
        assert!(text.starts_with("世界书设定:"));
    }

    /// 非 constant 条目按 key 命中注入(大小写不敏感)
    #[test]
    fn keyword_entries_inject_on_match() {
        let entries = vec![entry("地点", vec!["图书馆"], "图书馆很安静。", false, true)];
        let history = vec![message("user", "我在图 书 馆里看书")];
        // 不含连续子串 → 不命中
        assert!(collect_world_text(&entries, &history, &mut AssistantVars::new()).is_none());
        let history2 = vec![message("user", "我在图书馆里看书")];
        let text = collect_world_text(&entries, &history2, &mut AssistantVars::new()).unwrap();
        assert!(text.contains("图书馆很安静"));
    }

    /// 概率门控只作用于触发条目:constant 条目即使 use_probability=true/probability=0
    /// 也必须注入(2026-09-13 批次 3 核实并锁定:常驻条目进 system 前缀,若参与概率
    /// 会每轮扰动缓存前缀;实现上概率判定位于 constant 分支之后,本测试锁死该不变式)。
    #[test]
    fn constant_entries_ignore_probability() {
        let mut e = entry("世界观", vec![], "常驻设定", true, true);
        e.use_probability = true;
        e.probability = 0; // 0 = 永不注入;若常驻条目参与概率,本断言即失败
        let history = vec![message("user", "你好")];
        let text = collect_world_text(&[e], &history, &mut AssistantVars::new())
            .expect("常驻条目不参与概率,必须注入");
        assert!(text.contains("常驻设定"));
    }

    /// 触发条目参与概率:probability=0 时命中也不注入(证明门控确实作用于触发条目,
    /// 与上一条共同锁定「概率只影响尾部注入、不影响 system 前缀」)
    #[test]
    fn triggered_entries_respect_probability() {
        let mut e = entry("地点", vec!["图书馆"], "图书馆很安静", false, true);
        e.use_probability = true;
        e.probability = 0;
        let history = vec![message("user", "我在图书馆")];
        assert!(
            collect_world_text(&[e], &history, &mut AssistantVars::new()).is_none(),
            "触发条目 probability=0 应被概率门控拦下"
        );
    }

    /// 未启用 / 非 constant 且无 key / 命中与否的多种组合
    #[test]
    fn disabled_and_keyless_entries_skipped() {
        let entries = vec![
            entry("禁用", vec!["图书馆"], "不应出现", false, false),
            entry("无key", vec![], "不应出现", false, true),
            entry("未命中", vec!["火车站"], "不应出现", false, true),
        ];
        let history = vec![message("user", "我在图书馆")];
        assert!(collect_world_text(&entries, &history, &mut AssistantVars::new()).is_none());
    }

    /// 多条目命中按注释分隔拼接
    #[test]
    fn multiple_matches_joined() {
        let entries = vec![
            entry("地点", vec!["图书馆"], "图书馆安静。", false, true),
            entry("人物", vec!["芽衣"], "芽衣是兔族少女。", true, true),
        ];
        let history = vec![message("user", "我在图书馆遇见芽衣")];
        let text = collect_world_text(&entries, &history, &mut AssistantVars::new()).unwrap();
        assert!(text.contains("图书馆安静"));
        assert!(text.contains("芽衣是兔族少女"));
        assert!(text.matches("[地点]").count() == 1);
    }

    /// 正则条目按模式命中注入
    #[test]
    fn regex_entries_inject_on_match() {
        let entries = vec![regex_entry(
            "日期",
            r"\d{4}-\d{2}-\d{2}",
            "日期相关设定",
            true,
        )];
        // 命中
        let history = vec![message("user", "今天是 2026-08-06,天气不错")];
        let text = collect_world_text(&entries, &history, &mut AssistantVars::new()).unwrap();
        assert!(text.contains("日期相关设定"));
        // 未命中
        let history2 = vec![message("user", "今天没有日期格式内容")];
        assert!(collect_world_text(&entries, &history2, &mut AssistantVars::new()).is_none());
    }

    /// 无效正则不命中;禁用的正则条目跳过
    #[test]
    fn invalid_or_disabled_regex_skipped() {
        let entries = vec![
            regex_entry("坏正则", "([", "不应出现", true),
            regex_entry("禁用", r"abc", "不应出现", false),
        ];
        let history = vec![message("user", "abc 内容")];
        assert!(collect_world_text(&entries, &history, &mut AssistantVars::new()).is_none());
    }

    /// 空 entries → None(不注入)
    #[test]
    fn empty_entries_none() {
        assert!(collect_world_text(&[], &[], &mut AssistantVars::new()).is_none());
    }

    /// 仅扫描用户消息,assistant 回复中的词不触发
    #[test]
    fn assistant_text_does_not_trigger() {
        let entries = vec![entry("地点", vec!["图书馆"], "图书馆安静。", false, true)];
        let history = vec![
            message("assistant", "这里是图书馆,很安静"),
            message("user", "你好"),
        ];
        // assistant 内容含 key,但不应触发
        assert!(collect_world_text(&entries, &history, &mut AssistantVars::new()).is_none());
    }

    /// 扫描窗口兜底:条目未声明 scan_depth 时沿用条目 depth 作窗口
    /// (存量卡行为不变;ST 语义上 depth 是插入深度,此处为兼容保留,见
    /// parsing::world_book::scan_window_len)
    #[test]
    fn depth_limits_scan_window() {
        let mut e = entry("地点", vec!["图书馆"], "图书馆安静。", false, true);
        e.depth = 1;
        let history = vec![
            message("user", "上次去过图书馆"),
            message("user", "今天在家看书"),
        ];
        // 窗口 1 只扫最近 1 条用户消息("今天在家看书"),图书馆不命中
        assert!(collect_world_text(&[e.clone()], &history, &mut AssistantVars::new()).is_none());
        // 窗口 0 = 全部历史,命中
        e.depth = 0;
        assert!(collect_world_text(&[e], &history, &mut AssistantVars::new()).is_some());
    }

    /// 回归:角色卡把 depth 写在 extensions 内时,它作为窗口兜底须按该值生效。
    /// 赛马娘卡的【开局】/【怪奇IF】条目作者设定 depth=2,而首楼界面替换后选项文案
    /// 会继续留在历史里;旧实现把 depth 当缺省 4,使这些条目超出作者窗口后仍命中,
    /// 污染提示词(实测:4 条消息窗口下误注入)。
    #[test]
    fn extensions_depth_limits_scan_window() {
        let raw = serde_json::json!({ "entries": [
            { "uid": 1, "comment": "【开局】野史大学习", "keys": ["野史大学习"],
              "content": "开局正文", "constant": false, "disable": false,
              "extensions": { "depth": 2 } }
        ] });
        let entries = crate::parsing::world_book::collect_entries(&raw);
        assert_eq!(entries[0].depth, 2, "extensions.depth 应被解析");
        assert_eq!(entries[0].scan_depth, None, "该卡未声明 scan_depth");

        // 关键词只在第 1 条,其后 3 条无关 → 共 4 条用户消息
        let history = vec![
            message("user", "【开场场景】野史大学习"),
            message("user", "无关消息一"),
            message("user", "无关消息二"),
            message("user", "无关消息三"),
        ];
        // 兜底窗口 2 只扫最近 2 条,关键词落在窗口外 → 不命中(旧实现缺省 4 会误命中)
        assert!(
            collect_world_text(&entries, &history, &mut AssistantVars::new()).is_none(),
            "extensions.depth=2 兜底窗口不应扫到 4 条窗口外的关键词"
        );
        // 关键词回到窗口内(最后一条)则命中
        let near = vec![
            message("user", "无关消息一"),
            message("user", "无关消息二"),
            message("user", "【开场场景】野史大学习"),
        ];
        assert!(collect_world_text(&entries, &near, &mut AssistantVars::new()).is_some());
    }

    /// 条目声明 scanDepth 时以它为准,depth 兜底不再参与 —— 吸血鬼卡格式条目
    /// extensions.depth=1 但 (ST 语义)扫描窗口应由 scanDepth 决定;
    /// 声明 scanDepth=0(全部历史)时必须扫到历史里的触发词。
    #[test]
    fn scan_depth_wins_over_depth_fallback() {
        let raw = serde_json::json!({ "entries": [
            { "uid": 1, "comment": "格式规范", "keys": ["system log"],
              "content": "仅输出一个 JSON", "constant": false, "disable": false,
              "extensions": { "depth": 1, "scan_depth": 0 } }
        ] });
        let entries = crate::parsing::world_book::collect_entries(&raw);
        let history = vec![
            message("user", "/* system log(IGNORE the line): */ 开场"),
            message("user", "无关消息一"),
            message("user", "无关消息二"),
        ];
        // depth=1 若当窗口会漏掉首条;scanDepth=0 应扫全部 → 命中
        assert!(
            collect_world_text(&entries, &history, &mut AssistantVars::new()).is_some(),
            "scanDepth=0 应覆盖 depth=1 兜底,扫全部历史"
        );
    }

    /// 副关键词(keysecondary)与主关键词并列命中
    #[test]
    fn secondary_keys_match() {
        let mut e = entry("地点", vec!["图书馆"], "图书馆安静。", false, true);
        e.keys_secondary = vec!["书库".to_string()];
        let history = vec![message("user", "今天去了书库")];
        assert!(collect_world_text(&[e], &history, &mut AssistantVars::new()).is_some());
    }

    /// 大小写敏感:case_sensitive=true 时需精确匹配
    #[test]
    fn case_sensitive_matching() {
        let mut e = entry("地点", vec!["Library"], "图书馆安静。", false, true);
        let history_lower = vec![message("user", "went to library")];
        // 默认不敏感 → 命中
        assert!(
            collect_world_text(&[e.clone()], &history_lower, &mut AssistantVars::new()).is_some()
        );
        e.case_sensitive = true;
        // 敏感 → 小写 library 不命中大写 Library
        assert!(
            collect_world_text(&[e.clone()], &history_lower, &mut AssistantVars::new()).is_none()
        );
        let history_cap = vec![message("user", "went to Library")];
        assert!(collect_world_text(&[e], &history_cap, &mut AssistantVars::new()).is_some());
    }

    /// 概率:use_probability + probability 控制命中后是否注入
    #[test]
    fn probability_gates_injection() {
        let mut e = entry("地点", vec!["图书馆"], "图书馆安静。", false, true);
        let history = vec![message("user", "我在图书馆")];
        // 未开概率 → 恒注入
        e.use_probability = false;
        assert!(collect_world_text(&[e.clone()], &history, &mut AssistantVars::new()).is_some());
        // 开概率 0% → 永不注入
        e.use_probability = true;
        e.probability = 0;
        assert!(collect_world_text(&[e.clone()], &history, &mut AssistantVars::new()).is_none());
        // 开概率 100% → 恒注入(roll 0-99 < 100 恒真)
        e.probability = 100;
        assert!(collect_world_text(&[e], &history, &mut AssistantVars::new()).is_some());
    }

    /// 注入顺序:position 升序 → order 升序(同 position 按 order 排序)
    #[test]
    fn entries_sorted_by_position_then_order() {
        // 通过 collect_entries 从 JSON 构造,验证排序(position→order→id)
        let raw = serde_json::json!({
            "entries": [
                { "uid": 1, "comment": "低序", "keys": ["图书馆"], "content": "低序内容", "constant": false, "disable": false, "position": 0, "order": 300 },
                { "uid": 2, "comment": "高序", "keys": ["图书馆"], "content": "高序内容", "constant": false, "disable": false, "position": 0, "order": 100 },
                { "uid": 3, "comment": "靠后", "keys": ["图书馆"], "content": "靠后内容", "constant": false, "disable": false, "position": 2 }
            ]
        });
        let entries = crate::parsing::world_book::collect_entries(&raw);
        let comments: Vec<&str> = entries.iter().map(|e| e.comment.as_str()).collect();
        assert_eq!(
            comments,
            vec!["高序", "低序", "靠后"],
            "应按 position→order 排序,实际 {comments:?}"
        );
        let history = vec![message("user", "我在图书馆")];
        let text = collect_world_text(&entries, &history, &mut AssistantVars::new()).unwrap();
        assert!(
            text.find("高序内容").unwrap() < text.find("低序内容").unwrap(),
            "同 position 按 order 升序注入"
        );
    }

    /// append_state_block:空变量树原样返回;有变量树时注入状态 JSON 与更新协议
    #[test]
    fn append_state_block_injects_state_and_protocol() {
        // 空树 → 不变
        let empty = AssistantVars::new();
        assert!(append_state_block(None, &empty).is_none());
        assert_eq!(
            append_state_block(Some("世界书设定:x".into()), &empty).as_deref(),
            Some("世界书设定:x")
        );

        // 有树 + 原 world_text 为空 → 仅状态块
        let vars = AssistantVars::from_value(serde_json::json!({"hp": 100, "好感": {"值": 50}}));
        let only = append_state_block(None, &vars).unwrap();
        assert!(only.contains("[当前状态]"), "{only}");
        assert!(only.contains("\"hp\":100"), "{only}");
        assert!(
            only.contains("[{\"op\":\"replace\",\"path\":\"/hp\",\"value\":88}]"),
            "应含 UpdateVariable 示例:{only}"
        );
        assert!(
            only.contains("<UpdateVariable>") && only.contains("</UpdateVariable>"),
            "{only}"
        );

        // 有树 + 原 world_text → 状态块追加在末尾
        let both = append_state_block(Some("世界书设定:\n[地点] 图书馆".into()), &vars).unwrap();
        assert!(both.starts_with("世界书设定:"), "{both}");
        assert!(
            both.contains("[当前状态]") && both.contains("</UpdateVariable>"),
            "{both}"
        );
        assert!(
            both.find("[当前状态]").unwrap() > both.find("图书馆").unwrap(),
            "状态块应在世界书之后:{both}"
        );
    }
}
