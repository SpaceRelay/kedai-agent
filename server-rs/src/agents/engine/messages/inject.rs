// 消息插入:反思失败建议注入、@INJECT 精确消息插入(ST-Prompt-Template 兼容)、
// 摘要槽/记忆槽插入(缓存感知管线分层)
// (自 messages.rs 拆分迁入,纯代码移动,逻辑不变)
// 可见性说明:原 messages.rs 中 pub(super)(= 对 engine 可见)的导出条目在此改为
// pub(in crate::agents::engine),供 messages/mod.rs 以相同可见性再导出,范围不变;
// 仅 messages 模块内部使用的槽位标记保持 pub(super)(= 对 messages 可见)。
use crate::models::types::LlmMessage;

/// 将反思失败建议注入消息数组的位置0(预设尾部之前),供同一 run 内后续步骤生成生效。
/// 优先级:1 包含预设尾部标记的消息 → 插到标记前;2 最新 user 消息尾部追加;
/// 3 无 user(仅 assistant 历史等)→ 追加一条 user 消息。
/// role=user 时直接并入/追加 user;role=assistant 时:
/// - user 消息内无预设尾部标记 → 紧跟该 user 之后插一条独立 assistant 消息
/// - 预设尾部标记位于 user 消息内部(位置0 user 预设尾部)→ 把建议插到该 user 消息内标记之前
///   (保证「激发 → 反思反馈 → 预设尾部」的整体顺序;独立 assistant 预设尾部消息视为消息标记走 1)
pub(in crate::agents::engine) fn inject_reflect_advice(
    llm_messages: &mut Vec<LlmMessage>,
    advice: &str,
    role: &str,
    preset_tail: Option<&str>,
) {
    if advice.trim().is_empty() {
        return;
    }
    // 1 定位预设尾部标记:独立消息内含完整标记 → 建议作为独立消息插到它之前;
    //   标记位于 user 消息内部(位置0 user 预设尾部并入)→ 落入下方 2 的嵌入分支
    if let Some(t) = preset_tail.filter(|t| !t.trim().is_empty()) {
        if let Some(pos) = llm_messages
            .iter()
            .position(|m| m.role != "user" && m.content.contains(t.trim()))
        {
            let target_role = if role == "assistant" {
                "assistant"
            } else {
                "user"
            };
            llm_messages.insert(pos, LlmMessage::plain(target_role, advice));
            return;
        }
        // 标记位于 user 消息内部:嵌入该 user 内标记之前(保证 激发 → 建议 → 预设尾部 顺序)
        if let Some(pos) = llm_messages
            .iter()
            .position(|m| m.role == "user" && m.content.contains(t.trim()))
        {
            // contains 已保证该子串存在,但防御性处理(未来谓词变化不应引入 panic)
            if let Some(at) = llm_messages[pos].content.find(t.trim()) {
                llm_messages[pos]
                    .content
                    .insert_str(at, &format!("[反思反馈]\n{advice}\n\n"));
            }
            return;
        }
    }
    // 2 定位最新 user 消息
    if let Some(pos) = llm_messages.iter().rposition(|m| m.role == "user") {
        if role == "assistant" {
            // 预设尾部被并入该 user 消息内部(位置0 user 预设尾部)时,建议插入标记之前;
            // 否则紧跟该 user 之后插独立 assistant 消息
            let marker_at = preset_tail
                .filter(|t| !t.trim().is_empty())
                .and_then(|t| llm_messages[pos].content.find(t.trim()));
            match marker_at {
                Some(at) => llm_messages[pos]
                    .content
                    .insert_str(at, &format!("[反思反馈]\n{advice}\n\n")),
                None => llm_messages.insert(pos + 1, LlmMessage::plain("assistant", advice)),
            }
        } else {
            // user:建议拼到该 user 消息内预设尾部标记之前(无标记则追加到末尾)
            let marker_at = preset_tail
                .filter(|t| !t.trim().is_empty())
                .and_then(|t| llm_messages[pos].content.find(t.trim()));
            match marker_at {
                Some(at) => llm_messages[pos]
                    .content
                    .insert_str(at, &format!("[反思反馈]\n{advice}\n\n")),
                None => llm_messages[pos]
                    .content
                    .push_str(&format!("\n\n[反思反馈]\n{advice}")),
            }
        }
        return;
    }
    // 3 无 user:追加一条 user 消息兜底(此时预设尾部已并入 system,建议仍在 system 之后)
    let target_role = if role == "assistant" {
        "assistant"
    } else {
        "user"
    };
    llm_messages.push(LlmMessage::plain(target_role, advice));
}

// ===================== @INJECT 精确消息插入(ST-Prompt-Template 兼容) =====================
// 语法(写在世界书条目 comment,条目须 enabled=false 才生效):
//   @INJECT pos=N,role=R                    绝对位置插入(pos=0 第一条非 system 消息,负值从尾部数)
//   @INJECT target=ROLE,index=N,at=before|after,role=R   目标消息插入(index 从 1 开始,-1=该角色最后一条)
//   @INJECT regex=PATTERN,at=before|after,role=R         正则匹配插入(大小写不敏感,匹配第一条)
// 参数大小写不敏感,逗号分隔,role 缺省 user。应用顺序:按插入位置从后往前(先插后面的位置,
// 前面索引不漂移),与 ST 原版「位置从后往前执行」一致。system 消息始终保持在数组开头,
// pos 索引只针对非 system 消息,避免破坏系统提示词首位。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::agents::engine) enum InjectAt {
    Before,
    After,
}

/// @INJECT 插入规格(解析自条目 comment;content 为渲染后的条目内容)
#[derive(Debug, Clone, PartialEq)]
pub(in crate::agents::engine) enum InjectInsertion {
    Pos {
        pos: i64,
        role: String,
    },
    Target {
        target_role: String,
        index: i64,
        at: InjectAt,
        role: String,
    },
    Regex {
        pattern: String,
        at: InjectAt,
        role: String,
    },
}

impl InjectInsertion {}

/// 解析 comment 中的 @INJECT 语法;无 @INJECT 或参数非法返回 None。
pub(in crate::agents::engine) fn parse_inject_insertion(comment: &str) -> Option<InjectInsertion> {
    let idx = comment.to_ascii_lowercase().find("@inject")?;
    let rest = comment[idx + "@inject".len()..].trim();
    if rest.is_empty() {
        return None;
    }
    let mut pos: Option<i64> = None;
    let mut target_role: Option<String> = None;
    let mut index: Option<i64> = None;
    let mut at: Option<InjectAt> = None;
    let mut regex: Option<String> = None;
    let mut role: Option<String> = None;
    for pair in rest.split(',') {
        let pair = pair.trim();
        let Some(eq) = pair.find('=') else { continue };
        let key = pair[..eq].trim().to_ascii_lowercase();
        let value = pair[eq + 1..].trim();
        if value.is_empty() {
            continue;
        }
        match key.as_str() {
            "pos" => pos = value.parse::<i64>().ok(),
            "target" => target_role = Some(value.to_string()),
            "index" => index = value.parse::<i64>().ok(),
            "at" => {
                at = match value.to_ascii_lowercase().as_str() {
                    "before" => Some(InjectAt::Before),
                    "after" => Some(InjectAt::After),
                    _ => None,
                };
            }
            "regex" => regex = Some(value.to_string()),
            "role" => role = Some(value.to_string()),
            _ => {}
        }
    }
    let role = role.unwrap_or_else(|| "user".to_string());
    if let Some(p) = pos {
        return Some(InjectInsertion::Pos { pos: p, role });
    }
    if let (Some(t), Some(i)) = (target_role, index) {
        return Some(InjectInsertion::Target {
            target_role: t,
            index: i,
            at: at.unwrap_or(InjectAt::After),
            role,
        });
    }
    if let Some(r) = regex {
        return Some(InjectInsertion::Regex {
            pattern: r,
            at: at.unwrap_or(InjectAt::After),
            role,
        });
    }
    None
}

/// 应用 @INJECT 插入到消息数组。injects 为 (插入规格, 渲染后内容) 列表。
/// 按插入位置从后往前应用;正则编译失败 / 目标缺失时静默跳过该条(不 panic、不整批失败)。
pub(in crate::agents::engine) fn apply_inject_insertions(
    messages: &mut Vec<LlmMessage>,
    injects: &[(InjectInsertion, String)],
) {
    if injects.is_empty() || messages.is_empty() {
        return;
    }
    // 非 system 消息的完整索引(插入定位用;system 保持在开头)
    let non_system_idx: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role != "system")
        .map(|(i, _)| i)
        .collect();
    if non_system_idx.is_empty() {
        return;
    }
    let n = non_system_idx.len();
    // 计算每个插入的目标完整索引(升序);之后从后往前应用
    let mut placements: Vec<(usize, &str, &str, &str)> = Vec::new(); // (full_idx, role, content, kind)
    for (spec, content) in injects {
        if content.trim().is_empty() {
            continue;
        }
        match spec {
            InjectInsertion::Pos { pos, role } => {
                // pos=0 → 第一条非 system 前;pos=-1 → 最后一条后;负值从尾部数
                let rel: i64 = if *pos < 0 { n as i64 + *pos + 1 } else { *pos };
                let rel = rel.clamp(0, n as i64) as usize;
                let full = if rel == 0 {
                    non_system_idx[0]
                } else if rel >= n {
                    non_system_idx[n - 1] + 1
                } else {
                    non_system_idx[rel]
                };
                placements.push((full, role, content, "pos"));
            }
            InjectInsertion::Target {
                target_role,
                index,
                at,
                role,
            } => {
                // 该角色消息 0-based 索引列表
                let rel_idx: Vec<usize> = messages
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| m.role == *target_role && m.role != "system")
                    .map(|(i, _)| i)
                    .collect();
                if rel_idx.is_empty() {
                    continue;
                }
                let k: i64 = if *index < 0 {
                    rel_idx.len() as i64 + *index
                } else {
                    *index - 1
                };
                if k < 0 || k as usize >= rel_idx.len() {
                    continue;
                }
                let target_full = rel_idx[k as usize];
                let full = match at {
                    InjectAt::Before => target_full,
                    InjectAt::After => target_full + 1,
                };
                placements.push((full, role, content, "target"));
            }
            InjectInsertion::Regex { pattern, at, role } => {
                let Ok(re) = regex::Regex::new(&format!("(?i){pattern}")) else {
                    continue;
                };
                let Some(found) = non_system_idx
                    .iter()
                    .find(|&&i| re.is_match(&messages[i].content))
                else {
                    continue;
                };
                let full = match at {
                    InjectAt::Before => *found,
                    InjectAt::After => *found + 1,
                };
                placements.push((full, role, content, "regex"));
            }
        }
    }
    // 从后往前插入:先插后面的位置,前面索引不漂移
    placements.sort_by_key(|(idx, _, _, _)| *idx);
    for (full, role, content, _kind) in placements.into_iter().rev() {
        let at = full.min(messages.len());
        messages.insert(at, LlmMessage::plain(role, content));
    }
}

// ===== 摘要槽(缓存感知管线·改造 A) =====
// 摘要不拼进首个 system(每次压缩改写 system = 前缀全 miss),改为 system 之后的
// 独立 system 消息槽(DeepSeek 等支持多条 system 消息),分层固定为
// 「system(静态)→ 摘要槽(半静态,增量追加)→ 尾部历史(只追加)」。

/// 摘要槽标记前缀:trim/测试据此识别摘要槽消息
pub(super) const SUMMARY_SLOT_MARKER: &str = "【早期对话摘要】";

/// 把历史压缩摘要插入为独立 system 消息槽(首条 system 之后、其余消息之前)。
/// 摘要为空时不动数组。返回是否插入。
pub(in crate::agents::engine) fn insert_summary_slot(
    messages: &mut Vec<LlmMessage>,
    summary: &str,
) -> bool {
    let summary = summary.trim();
    if summary.is_empty() {
        return false;
    }
    let slot = LlmMessage::plain("system", &format!("{SUMMARY_SLOT_MARKER}\n{summary}"));
    // 插入点:首条非 system 消息之前(即紧跟 system 头;正常布局为 1);
    // 无 system 头(异常布局)时置顶,保持「摘要位于数组前部」的分层
    let at = messages
        .iter()
        .position(|m| m.role != "system")
        .unwrap_or(messages.len())
        .min(1);
    messages.insert(at.min(messages.len()), slot);
    true
}

// ===== 记忆槽(跨会话记忆蒸馏·落地项 2) =====
// 布局扩展为「system(静态)→ 摘要槽(半静态)→ 记忆槽(半静态)→ 尾部历史(只追加)」。
// 记忆槽内容只由精选结果决定(select_for_injection 排序键确定性),同一记忆集合
// 两次构建逐字节一致;touch 回写发生在响应之后,不影响本轮已构建内容。

/// 记忆槽标记前缀:trim/测试据此识别记忆槽消息
pub(super) const MEMORY_SLOT_MARKER: &str = "【角色长期记忆】";

/// 把跨会话记忆插入为独立 system 消息槽:摘要槽之后(无摘要槽时紧跟首 system)、
/// 其余消息之前。contents 为已精选排序的记忆正文,每条一行「- content」;
/// 空列表或全空白不动数组(行为与无记忆现状一致)。返回是否插入。
pub(in crate::agents::engine) fn insert_memory_slot(
    messages: &mut Vec<LlmMessage>,
    contents: &[String],
) -> bool {
    let lines: Vec<&str> = contents
        .iter()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect();
    if lines.is_empty() {
        return false;
    }
    let body = lines
        .iter()
        .map(|l| format!("- {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    let slot = LlmMessage::plain("system", &format!("{MEMORY_SLOT_MARKER}\n{body}"));
    // 插入点:摘要槽(SUMMARY_SLOT_MARKER 开头的 system 消息)之后;
    // 无摘要槽时与 insert_summary_slot 同位(首条非 system 消息之前,正常布局为 1)
    let at = match messages
        .iter()
        .position(|m| m.role == "system" && m.content.starts_with(SUMMARY_SLOT_MARKER))
    {
        Some(i) => i + 1,
        None => messages
            .iter()
            .position(|m| m.role != "system")
            .unwrap_or(messages.len())
            .min(1),
    };
    messages.insert(at.min(messages.len()), slot);
    true
}

// ===== 召回槽(升级工作流 B2 通道 2) =====
// 通道 1(记忆槽)是「常驻精选」,位于数组前部;通道 2 按当前用户输入做相关性召回,
// 作为尾部追加的独立 system 消息——放在消息数组末尾,只影响尾部,不破坏
// system/摘要槽/记忆槽/历史的既有前缀(前缀缓存前提)。

/// 召回槽标记前缀:测试与调试据此识别通道 2 消息
pub(in crate::agents::engine) const RECALL_SLOT_MARKER: &str = "【相关记忆召回】";

/// 把按当前输入召回的记忆作为尾部独立 system 消息追加(数组末尾)。
/// contents 为已按相关性排序的记忆正文,每条一行「- content」;
/// 空列表或全空白不动数组(行为与无召回现状一致)。返回是否插入。
/// `notice` 非空时追加一行显式提示(预算截断等),不静默丢弃。
pub(in crate::agents::engine) fn insert_recall_slot(
    messages: &mut Vec<LlmMessage>,
    contents: &[String],
    notice: Option<&str>,
) -> bool {
    let lines: Vec<&str> = contents
        .iter()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect();
    if lines.is_empty() {
        return false;
    }
    let mut body = lines
        .iter()
        .map(|l| format!("- {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(n) = notice.map(str::trim).filter(|n| !n.is_empty()) {
        body.push('\n');
        body.push_str(n);
    }
    let slot = LlmMessage::plain("system", &format!("{RECALL_SLOT_MARKER}\n{body}"));
    // 尾部追加:通道 2 只在数组末尾生长,不打断已有公共前缀
    messages.push(slot);
    true
}

/// 预算截断提示兜底:在已插入的记忆槽/召回槽末尾追加一行显式提示
/// (从后往前找,优先召回槽;找不到槽返回 false)。用于「通道 2 无可注入条目、
/// 但通道 1 因预算被截断」的场景,保证截断不被静默丢弃。
pub(in crate::agents::engine) fn append_memory_notice(
    messages: &mut [LlmMessage],
    notice: &str,
) -> bool {
    let notice = notice.trim();
    if notice.is_empty() {
        return false;
    }
    if let Some(slot) = messages.iter_mut().rev().find(|m| {
        m.content.starts_with(MEMORY_SLOT_MARKER) || m.content.starts_with(RECALL_SLOT_MARKER)
    }) {
        slot.content.push('\n');
        slot.content.push_str(notice);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_msg(role: &str, content: &str) -> LlmMessage {
        LlmMessage::plain(role, content)
    }

    /// inject_reflect_advice:预设尾部独立消息时,建议插入该消息之前
    #[test]
    fn inject_advice_before_standalone_preset_tail_message() {
        let mut msgs = vec![
            plain_msg("system", "系统"),
            plain_msg("user", "最新输入"),
            plain_msg("assistant", "预设尾部内容"),
        ];
        inject_reflect_advice(&mut msgs, "建议X", "user", Some("预设尾部内容"));
        assert_eq!(msgs.len(), 4, "建议应为独立消息: {msgs:?}");
        assert_eq!(msgs[2].content, "建议X");
        assert_eq!(msgs[3].content, "预设尾部内容", "预设尾部保持最后");
    }

    /// inject_reflect_advice:预设尾部并入 user 消息内部时,建议插入 user 内标记之前
    #[test]
    fn inject_advice_before_embedded_preset_tail_in_user() {
        let mut msgs = vec![
            plain_msg("system", "系统"),
            plain_msg("user", "最新输入\n\n世界书激发\n\n预设尾部内容"),
        ];
        inject_reflect_advice(&mut msgs, "建议X", "user", Some("预设尾部内容"));
        assert_eq!(msgs.len(), 2, "不新增消息: {msgs:?}");
        let c = &msgs[1].content;
        let t = c.find("世界书激发").unwrap();
        let a = c.find("建议X").unwrap();
        let p = c.find("预设尾部内容").unwrap();
        assert!(
            t < a && a < p,
            "user 内顺序应为 激发 → 建议 → 预设尾部: {c}"
        );
    }

    /// inject_reflect_advice:无预设尾部时,建议追加到最新 user 消息尾部
    #[test]
    fn inject_advice_appends_to_last_user_without_preset_tail() {
        let mut msgs = vec![
            plain_msg("system", "系统"),
            plain_msg("user", "较早输入"),
            plain_msg("assistant", "回应"),
            plain_msg("user", "最新输入"),
        ];
        inject_reflect_advice(&mut msgs, "建议X", "user", None);
        assert_eq!(msgs.len(), 4, "不新增消息: {msgs:?}");
        assert!(
            msgs[3].content.starts_with("最新输入") && msgs[3].content.contains("建议X"),
            "建议应追加到最新 user 尾部: {}",
            msgs[3].content
        );
        assert!(!msgs[1].content.contains("建议X"), "不应改动较早 user 消息");
    }

    /// inject_reflect_advice:role=assistant 且 user 内无预设尾部标记时,紧跟 user 后插独立 assistant 消息
    #[test]
    fn inject_advice_assistant_after_last_user() {
        let mut msgs = vec![plain_msg("system", "系统"), plain_msg("user", "最新输入")];
        inject_reflect_advice(&mut msgs, "建议X", "assistant", Some("不存在的尾部"));
        assert_eq!(msgs.len(), 3, "插入独立 assistant 消息: {msgs:?}");
        assert_eq!(msgs[2].role, "assistant");
        assert_eq!(msgs[2].content, "建议X");
    }

    /// inject_reflect_advice:role=assistant 且预设尾部并入 user 时,建议插入 user 内标记之前
    #[test]
    fn inject_advice_assistant_before_embedded_tail_in_user() {
        let mut msgs = vec![
            plain_msg("system", "系统"),
            plain_msg("user", "最新输入\n\n预设尾部内容"),
        ];
        inject_reflect_advice(&mut msgs, "建议X", "assistant", Some("预设尾部内容"));
        assert_eq!(msgs.len(), 2, "不新增消息(嵌入 user): {msgs:?}");
        let c = &msgs[1].content;
        let a = c.find("建议X").unwrap();
        let p = c.find("预设尾部内容").unwrap();
        assert!(a < p, "assistant 建议应位于 user 内预设尾部之前: {c}");
    }

    /// inject_reflect_advice:无 user 消息时兜底追加一条消息(在 system 之后)
    #[test]
    fn inject_advice_no_user_appends_message() {
        let mut msgs = vec![
            plain_msg("system", "系统"),
            plain_msg("assistant", "开场白"),
        ];
        inject_reflect_advice(&mut msgs, "建议X", "user", None);
        assert_eq!(msgs.len(), 3, "追加一条: {msgs:?}");
        assert_eq!(msgs[2].role, "user");
        assert_eq!(msgs[2].content, "建议X");
    }

    /// inject_reflect_advice:空建议不注入
    #[test]
    fn inject_advice_empty_noop() {
        let mut msgs = vec![plain_msg("user", "输入")];
        inject_reflect_advice(&mut msgs, "   ", "user", None);
        assert_eq!(msgs.len(), 1, "空建议不应注入: {msgs:?}");
    }

    // ===== @INJECT 精确消息插入 =====

    fn inject_msgs() -> Vec<LlmMessage> {
        vec![
            plain_msg("system", "系统提示"),
            plain_msg("user", "第一条"),
            plain_msg("assistant", "回复一"),
            plain_msg("user", "第二条"),
        ]
    }

    #[test]
    fn parse_inject_pos() {
        let spec = parse_inject_insertion("背景注入 @INJECT pos=0,role=system").unwrap();
        assert_eq!(
            spec,
            InjectInsertion::Pos {
                pos: 0,
                role: "system".into()
            }
        );
        // 缺省 role = user;大小写不敏感;负 pos
        let spec2 = parse_inject_insertion("@inject POS=-1").unwrap();
        assert_eq!(
            spec2,
            InjectInsertion::Pos {
                pos: -1,
                role: "user".into()
            }
        );
    }

    #[test]
    fn parse_inject_target_and_regex() {
        let spec =
            parse_inject_insertion("@INJECT target=assistant,index=1,at=before,role=assistant")
                .unwrap();
        assert_eq!(
            spec,
            InjectInsertion::Target {
                target_role: "assistant".into(),
                index: 1,
                at: InjectAt::Before,
                role: "assistant".into()
            }
        );
        let spec2 = parse_inject_insertion("@INJECT regex=图书馆,at=after").unwrap();
        assert_eq!(
            spec2,
            InjectInsertion::Regex {
                pattern: "图书馆".into(),
                at: InjectAt::After,
                role: "user".into()
            }
        );
        // 无 @INJECT / 非法参数 → None
        assert!(parse_inject_insertion("普通注释").is_none());
        assert!(parse_inject_insertion("@INJECT pos=abc").is_none());
        assert!(parse_inject_insertion("@INJECT").is_none());
    }

    #[test]
    fn inject_pos_zero_inserts_first_non_system() {
        let mut msgs = inject_msgs();
        apply_inject_insertions(
            &mut msgs,
            &[(
                InjectInsertion::Pos {
                    pos: 0,
                    role: "user".into(),
                },
                "注入头部".into(),
            )],
        );
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[1].content, "注入头部");
        assert_eq!(msgs[2].content, "第一条");
    }

    #[test]
    fn inject_pos_negative_insets_at_tail() {
        let mut msgs = inject_msgs();
        apply_inject_insertions(
            &mut msgs,
            &[(
                InjectInsertion::Pos {
                    pos: -1,
                    role: "user".into(),
                },
                "注入尾部".into(),
            )],
        );
        assert_eq!(msgs.last().unwrap().content, "注入尾部");
    }

    #[test]
    fn inject_target_before_assistant() {
        let mut msgs = inject_msgs();
        apply_inject_insertions(
            &mut msgs,
            &[(
                InjectInsertion::Target {
                    target_role: "assistant".into(),
                    index: 1,
                    at: InjectAt::Before,
                    role: "assistant".into(),
                },
                "注入后回复".into(),
            )],
        );
        let roles: Vec<&str> = msgs.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(
            roles,
            vec!["system", "user", "assistant", "assistant", "user"]
        );
        // 目标索引 1 的 assistant(回复一)之前应插入注入内容
        assert_eq!(msgs[2].content, "注入后回复");
        assert_eq!(msgs[3].content, "回复一");
    }

    #[test]
    fn inject_regex_matches_first_message() {
        let mut msgs = inject_msgs();
        msgs[1].content = "我在图书馆看书".into();
        apply_inject_insertions(
            &mut msgs,
            &[(
                InjectInsertion::Regex {
                    pattern: "图书馆".into(),
                    at: InjectAt::After,
                    role: "user".into(),
                },
                "馆内设定".into(),
            )],
        );
        assert_eq!(msgs[2].content, "馆内设定");
    }

    #[test]
    fn inject_multiple_applied_back_to_front() {
        let mut msgs = inject_msgs();
        apply_inject_insertions(
            &mut msgs,
            &[
                (
                    InjectInsertion::Pos {
                        pos: 0,
                        role: "user".into(),
                    },
                    "头部注入".into(),
                ),
                (
                    InjectInsertion::Pos {
                        pos: 1,
                        role: "user".into(),
                    },
                    "次位注入".into(),
                ),
            ],
        );
        // pos 索引相对原数组:先插 pos=1(原第二条前),再插 pos=0(原第一条前) →
        // 头部注入在最前、次位注入紧跟原第一条之后
        let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(
            contents,
            vec![
                "系统提示",
                "头部注入",
                "第一条",
                "次位注入",
                "回复一",
                "第二条"
            ],
            "从后往前插入语义: {contents:?}"
        );
    }

    #[test]
    fn inject_skips_invalid_and_empty() {
        let mut msgs = inject_msgs();
        let before = msgs.len();
        apply_inject_insertions(
            &mut msgs,
            &[
                (
                    InjectInsertion::Pos {
                        pos: 0,
                        role: "user".into(),
                    },
                    "   ".into(),
                ),
                (
                    InjectInsertion::Regex {
                        pattern: "(".into(),
                        at: InjectAt::After,
                        role: "user".into(),
                    },
                    "无效正则".into(),
                ),
                (
                    InjectInsertion::Target {
                        target_role: "system".into(),
                        index: 9,
                        at: InjectAt::After,
                        role: "user".into(),
                    },
                    "越界目标".into(),
                ),
            ],
        );
        assert_eq!(msgs.len(), before, "无效插入应被跳过: {msgs:?}");
    }
}
