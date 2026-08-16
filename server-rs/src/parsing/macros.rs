// 酒馆宏系统(SillyTavern 兼容):提示词注入内容中的 {{...}} 宏展开。
// 兼容 ST 常用宏:{{char}} {{user}} {{time}} {{datetime}} {{random}} {{roll}}
// {{newline}} {{personality}} {{scenario}} {{lastMessage}} {{lastUserMessage}}
// {{lastCharMessage}} {{firstMessage}} {{trim}} {{//注释}} 与会话变量宏
// {{setvar::key::value}} {{addvar::key::value}} {{getvar::key}}。
// 未知宏保留原样(不报错,便于用户从 ST 预设直接搬运)。
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::parsing::scopes::Scope;

/// 宏展开上下文(一次请求构建消息时组装;变量 map 可写,展开后由引擎持久化)
pub struct MacroCtx<'a> {
    pub character_name: &'a str,
    pub character_description: &'a str,
    pub user_name: &'a str,
    pub personality: &'a str,
    pub scenario: &'a str,
    /// 当前用户输入({{input}} 宏;显示层/无输入场景为空串)
    pub user_input: &'a str,
    /// 历史消息 [(role, content)](角色名消息 = assistant,用户消息 = user)
    pub history: &'a [(String, String)],
    /// 会话变量(key → value);setvar/addvar 写入,getvar 读取
    pub vars: &'a mut HashMap<String, String>,
    /// 酒馆助手变量树(assistant 插件 stat_data);Some 时 getvar/setvar/addvar 优先树路径
    pub assistant_vars: Option<&'a mut crate::parsing::assistant::AssistantVars>,
    /// 7 作用域变量(计划二);Some 时作用域宏族与 getvar 读合并视图,
    /// None 走既有逻辑(零行为变化)
    pub scopes: Option<&'a mut crate::parsing::scopes::ScopeVars>,
}

/// 展开文本中的全部宏(从左到右单遍扫描;trim 修饰紧随其后的宏输出)
pub fn expand_macros(text: &str, ctx: &mut MacroCtx<'_>) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0usize;
    // trim 修饰标志:{{trim}} 后面紧跟的宏输出需要 trim
    let mut trim_next = false;
    while i < bytes.len() {
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(end) = find_macro_end(text, i + 2) {
                let inner = &text[i + 2..end];
                // {{trim}} 修饰宏:不输出,标记下一个宏输出需 trim(仿 ST 用法 {{trim}}{{getvar::..}})
                if inner.trim().eq_ignore_ascii_case("trim") {
                    trim_next = true;
                    i = end + 2;
                    continue;
                }
                let expanded = expand_one(inner, ctx);
                // {{//...}} 注释宏:输出空并吸收到行尾(删除整行残留)
                if inner.trim_start().starts_with("//") {
                    // 吸收到行尾(含换行),避免注释留下空行
                    let mut j = end + 2;
                    while j < bytes.len() && bytes[j] != b'\n' {
                        j += 1;
                    }
                    if j < bytes.len() {
                        j += 1; // 吃掉换行
                    }
                    i = j;
                    trim_next = false;
                    continue;
                }
                if trim_next {
                    out.push_str(expanded.trim());
                    trim_next = false;
                } else {
                    out.push_str(&expanded);
                }
                i = end + 2;
                continue;
            }
        }
        // 普通字符:按 UTF-8 字符整体输出,避免在字符中间切片(中文等多字节文本)
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 查找宏结束位置(遇到 }} 为止;支持嵌套大括号内的内容)
fn find_macro_end(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i + 1 < bytes.len() {
        if bytes[i] == b'}' && bytes[i + 1] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 展开单个宏内文(不含 {{ }});返回展开结果
fn expand_one(inner: &str, ctx: &mut MacroCtx<'_>) -> String {
    let name = inner.trim();
    let lower = name.to_lowercase();
    match lower.as_str() {
        "char" | "character_name" => ctx.character_name.to_string(),
        "user" | "user_name" => ctx.user_name.to_string(),
        "newline" | "lf" => "\n".to_string(),
        "time" => now_str("%H:%M:%S"),
        "datetime" | "date" => now_str("%Y-%m-%d %H:%M:%S"),
        "personality" => ctx.personality.to_string(),
        "scenario" => ctx.scenario.to_string(),
        "character_description" => ctx.character_description.to_string(),
        "lastmessage" | "mes" => last_by_role(ctx.history, None),
        "lastusermessage" => last_by_role(ctx.history, Some("user")),
        "lastcharmessage" => last_by_role(ctx.history, Some("assistant")),
        "firstmessage" => ctx
            .history
            .first()
            .map(|(_, c)| c.clone())
            .unwrap_or_default(),
        // ST-Prompt-Template 兼容宏:
        // {{input}} 当前用户输入;{{idle}} 空白;{{pipe}} 无管道语义 → 空;
        // {{first}}/{{last}} = 历史首条/末条消息(与 firstMessage/lastMessage 同义)
        "input" => ctx.user_input.to_string(),
        "idle" => String::new(),
        "pipe" => String::new(),
        "first" => ctx
            .history
            .first()
            .map(|(_, c)| c.clone())
            .unwrap_or_default(),
        "last" => last_by_role(ctx.history, None),
        "random" => rand_range(0, 1000).to_string(),
        "roll" => roll("1d20").to_string(),
        "trim" => String::new(), // 修饰宏:由外层处理(输出空,后续宏 trim)
        _ => {
            // 带参形式
            if let Some(rest) = name.strip_prefix("random:") {
                return parse_range(rest).to_string();
            }
            if let Some(rest) = name.strip_prefix("roll:") {
                return roll(rest).to_string();
            }
            if let Some(rest) = name.strip_prefix("getvar::") {
                let key = rest.trim();
                // 7 作用域存在时:先查作用域镜像(message/chat树/character/preset/global,
                // 命中返回 JSON 显示),扁平层以实时 vars 兜底——宏展开的 setvar 实时写
                // 调用方 vars 表,镜像仅在回合边界同步,展开中途须读实时值(旧 getvar 语义)
                if let Some(sv) = ctx.scopes.as_deref() {
                    if let Some(v) = sv.getvar_display(key) {
                        return v;
                    }
                }
                return ctx.vars.get(key).cloned().unwrap_or_default();
            }
            if let Some(rest) = name.strip_prefix("setvar::") {
                // setvar::key::value — 值部分不再递归展开(避免循环),原样存入
                let (k, v) = split_key_value(rest);
                if !k.is_empty() {
                    if let Some(av) = ctx.assistant_vars.as_deref_mut() {
                        let in_tree = k == "stat_data"
                            || k.starts_with("stat_data.")
                            || av.get_value(k).is_some();
                        if in_tree {
                            av.set(k, crate::parsing::assistant::infer_value(v));
                            return String::new();
                        }
                    }
                    ctx.vars.insert(k.to_string(), v.to_string());
                }
                return String::new();
            }
            if let Some(rest) = name.strip_prefix("addvar::") {
                let (k, v) = split_key_value(rest);
                if !k.is_empty() {
                    if let Some(av) = ctx.assistant_vars.as_deref_mut() {
                        let in_tree = k == "stat_data"
                            || k.starts_with("stat_data.")
                            || av.get_value(k).is_some();
                        if in_tree {
                            // 数值加;失败(非数字)回退字符串追加
                            if let Ok(d) = v.trim().parse::<f64>() {
                                if av.add(k, d).is_ok() {
                                    return String::new();
                                }
                            }
                            if let Some(cur) = av.get_value(k) {
                                if let Some(s) = cur.as_str() {
                                    av.set(k, serde_json::json!(format!("{s}{v}")));
                                    return String::new();
                                }
                            }
                            av.set(k, crate::parsing::assistant::infer_value(v));
                            return String::new();
                        }
                    }
                    let entry = ctx.vars.entry(k.to_string()).or_default();
                    entry.push_str(v);
                }
                return String::new();
            }
            // ===== 7 作用域宏族(计划二 · 酒馆助手兼容) =====
            // get_(chat|character|preset|global)_variable / format_(chat|character|preset|global)_variable:
            // 按作用域取值,缺失回退链(§2.1);scopes 为 None 时返回空(等价于该作用域无数据)。
            if let Some((scope, rest, is_format)) = scope_macro_match(name) {
                if let Some(sv) = ctx.scopes.as_deref() {
                    let p = rest.trim();
                    return if is_format {
                        sv.format_scope(scope, if p.is_empty() { None } else { Some(p) })
                    } else {
                        sv.read_scope(scope, p)
                            .map(|v| crate::parsing::assistant::value_to_display(&v))
                            .unwrap_or_default()
                    };
                }
                return String::new();
            }
            if let Some(rest) = name
                .strip_prefix("format_message_variable::")
                .or_else(|| name.strip_prefix("get_message_variable::"))
            {
                // 计划二:message 作用域优先、缺失强制回退 chat(兼容既有卡片 stat_data 读取);
                // 无 scopes 时保持既有行为(直接读变量树)。
                if let Some(sv) = ctx.scopes.as_deref() {
                    let is_format = name.starts_with("format_");
                    let p = rest.trim();
                    return if is_format {
                        sv.format_scope(Scope::Message, if p.is_empty() { None } else { Some(p) })
                    } else {
                        sv.read_scope(Scope::Message, p)
                            .map(|v| crate::parsing::assistant::value_to_display(&v))
                            .unwrap_or_default()
                    };
                }
                if let Some(av) = ctx.assistant_vars.as_deref() {
                    let p = rest.trim();
                    return av.format(if p.is_empty() { None } else { Some(p) });
                }
            }
            if let Some(rest) = name.strip_prefix("var::") {
                return ctx.vars.get(rest.trim()).cloned().unwrap_or_default();
            }
            // 未知宏:保留原样(兼容 ST 预设搬运)
            format!("{{{{{}}}}}", name)
        }
    }
}

/// 作用域宏族匹配(计划二):返回 (作用域, 参数, 是否 format 变体);未命中返回 None。
/// script/extension 无对应宏(与插件生态一致),未命中时走下方未知宏保留原样。
fn scope_macro_match(name: &str) -> Option<(crate::parsing::scopes::Scope, &str, bool)> {
    use crate::parsing::scopes::Scope;
    for (prefix, scope, is_format) in [
        ("get_chat_variable::", Scope::Chat, false),
        ("get_character_variable::", Scope::Character, false),
        ("get_preset_variable::", Scope::Preset, false),
        ("get_global_variable::", Scope::Global, false),
        ("format_chat_variable::", Scope::Chat, true),
        ("format_character_variable::", Scope::Character, true),
        ("format_preset_variable::", Scope::Preset, true),
        ("format_global_variable::", Scope::Global, true),
    ] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return Some((scope, rest, is_format));
        }
    }
    None
}

/// 历史中最后一条指定角色(或任意)的消息内容
fn last_by_role(history: &[(String, String)], role: Option<&str>) -> String {
    for (r, c) in history.iter().rev() {
        match role {
            Some(target) if r == target => return c.clone(),
            None => return c.clone(),
            _ => {}
        }
    }
    String::new()
}

/// 拆分 setvar/addvar 的 key::value(仅按第一个 :: 分割)
fn split_key_value(rest: &str) -> (&str, &str) {
    match rest.find("::") {
        Some(i) => (rest[..i].trim(), &rest[i + 2..]),
        None => (rest.trim(), ""),
    }
}

fn parse_range(rest: &str) -> i64 {
    let parts: Vec<&str> = rest.split(',').map(|s| s.trim()).collect();
    let min: i64 = parts.first().and_then(|p| p.parse().ok()).unwrap_or(0);
    let max: i64 = parts
        .get(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(min + 999);
    rand_range(min, max)
}

/// 简化骰子:支持 NdM(+/-x)(如 1d20、2d6+3、1d100-10);不支持 N=0
fn roll(spec: &str) -> i64 {
    let s = spec.trim().to_lowercase();
    let (dice_part, mod_part) = match s.find('+') {
        Some(i) => (&s[..i], s[i..].parse::<i64>().unwrap_or(0)),
        None => match s.find('-') {
            Some(i) => (&s[..i], -s[i..].parse::<i64>().unwrap_or(0)),
            None => (s.as_str(), 0),
        },
    };
    let (n_str, m_str) = match dice_part.find('d') {
        Some(i) => (&dice_part[..i], &dice_part[i + 1..]),
        None => ("1", dice_part),
    };
    let n: i64 = n_str.trim().parse().unwrap_or(1).max(1);
    let m: i64 = m_str.trim().parse().unwrap_or(20).max(1);
    let mut total = 0i64;
    for _ in 0..n {
        total += rand_range(1, m + 1);
    }
    total + mod_part
}

fn rand_range(min: i64, max: i64) -> i64 {
    if max <= min {
        return min;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // 简单伪随机:纳秒时间 + 混合
    let mut seed = (now as u64) ^ 0x9E3779B97F4A7C15;
    seed = seed
        .wrapping_mul(0x2545F4914F6CDD1D)
        .wrapping_add(0x9E3779B9);
    seed ^= seed >> 30;
    let range = (max - min) as u64;
    min + (seed % range) as i64
}

/// 本地时间格式化
fn now_str(fmt: &str) -> String {
    // chrono 已在依赖中(代码库使用 chrono::Utc)
    chrono::Local::now().format(fmt).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        history: &'a [(String, String)],
        vars: &'a mut HashMap<String, String>,
    ) -> MacroCtx<'a> {
        MacroCtx {
            character_name: "林晚",
            character_description: "温柔的女剑客",
            user_name: "旅人",
            user_input: "",
            personality: "冷静沉着",
            scenario: "雪山相遇",
            history,
            vars,
            assistant_vars: None,
            scopes: None,
        }
    }

    #[test]
    fn expands_basic_macros() {
        let history = vec![
            ("user".to_string(), "你好".to_string()),
            ("assistant".to_string(), "你好,旅人".to_string()),
        ];
        let mut vars = HashMap::new();
        let mut c = ctx(&history, &mut vars);
        let out = expand_macros(
            "{{char}}对{{user}}说:{{newline}}性格{{personality}},场景{{scenario}}",
            &mut c,
        );
        assert!(out.contains("林晚对旅人说"));
        assert!(out.contains('\n'));
        assert!(out.contains("性格冷静沉着"));
        assert!(out.contains("场景雪山相遇"));
    }

    #[test]
    fn expands_history_macros() {
        let history = vec![
            ("user".to_string(), "第一条用户消息".to_string()),
            ("assistant".to_string(), "角色回复".to_string()),
            ("user".to_string(), "最新的用户消息".to_string()),
        ];
        let mut vars = HashMap::new();
        let mut c = ctx(&history, &mut vars);
        let out = expand_macros(
            "last={{lastMessage}};lu={{lastUserMessage}};lc={{lastCharMessage}};first={{firstMessage}}",
            &mut c,
        );
        assert!(out.contains("last=最新的用户消息"));
        assert!(out.contains("lu=最新的用户消息"));
        assert!(out.contains("lc=角色回复"));
        assert!(out.contains("first=第一条用户消息"));
    }

    #[test]
    fn random_and_roll_in_range() {
        let history: Vec<(String, String)> = Vec::new();
        let mut vars = HashMap::new();
        let mut c = ctx(&history, &mut vars);
        for _ in 0..20 {
            let out = expand_macros("{{random:5,5}}", &mut c);
            assert_eq!(out, "5");
        }
        for _ in 0..20 {
            let out = expand_macros("{{roll:1d6}}", &mut c);
            let v: i64 = out.parse().unwrap();
            assert!((1..=6).contains(&v), "roll out: {out}");
        }
        let out = expand_macros("{{roll:2d6+10}}", &mut c);
        let v: i64 = out.parse().unwrap();
        assert!((12..=22).contains(&v), "roll out: {out}");
    }

    #[test]
    fn setvar_getvar_addvar_flow() {
        let history: Vec<(String, String)> = Vec::new();
        let mut vars = HashMap::new();
        // setvar 输出空,getvar 读到值
        let out = expand_macros(
            "A{{setvar::count::3}}B{{getvar::count}}",
            &mut ctx(&history, &mut vars),
        );
        assert_eq!(out, "AB3");
        assert_eq!(vars.get("count").map(|s| s.as_str()), Some("3"));
        // addvar 追加
        let out = expand_macros(
            "{{addvar::count::+1}}x={{getvar::count}}",
            &mut ctx(&history, &mut vars),
        );
        assert_eq!(out, "x=3+1");
        assert_eq!(vars.get("count").map(|s| s.as_str()), Some("3+1"));
        // 未知变量 getvar 空
        let out = expand_macros("[{{getvar::nope}}]", &mut ctx(&history, &mut vars));
        assert_eq!(out, "[]");
    }

    #[test]
    fn unknown_macro_preserved() {
        let history: Vec<(String, String)> = Vec::new();
        let mut vars = HashMap::new();
        let mut c = ctx(&history, &mut vars);
        let out = expand_macros("{{someUnknownMacro}}", &mut c);
        assert_eq!(out, "{{someUnknownMacro}}");
    }

    #[test]
    fn comment_macro_removes_line() {
        let history: Vec<(String, String)> = Vec::new();
        let mut vars = HashMap::new();
        let mut c = ctx(&history, &mut vars);
        let out = expand_macros("第一行\n{{//这是一条注释}}\n第三行", &mut c);
        assert_eq!(out, "第一行\n第三行");
        // 注释不在行首:仍吸收到行尾(删掉注释及其后同行的残留)
        let out = expand_macros("前{{//注释}}后", &mut c);
        assert_eq!(out, "前");
    }

    #[test]
    fn trim_modifier_trims_next_macro() {
        let history: Vec<(String, String)> = Vec::new();
        let mut vars = HashMap::new();
        vars.insert("key".to_string(), "  带空白  ".to_string());
        let mut c = ctx(&history, &mut vars);
        let out = expand_macros("[{{trim}}{{getvar::key}}]", &mut c);
        assert_eq!(out, "[带空白]");
    }

    /// ST-Prompt-Template 兼容宏:{{input}}/{{idle}}/{{pipe}}/{{first}}/{{last}}
    #[test]
    fn st_compat_macros() {
        let history = vec![
            ("user".to_string(), "第一条".to_string()),
            ("assistant".to_string(), "角色回复".to_string()),
            ("user".to_string(), "最新输入".to_string()),
        ];
        let mut vars = HashMap::new();
        let mut c = ctx(&history, &mut vars);
        c.user_input = "当前输入";
        let out = expand_macros(
            "in={{input}}|idle=[{{idle}}]|pipe=[{{pipe}}]|first={{first}}|last={{last}}",
            &mut c,
        );
        assert!(out.contains("in=当前输入"), "out: {out}");
        assert!(out.contains("idle=[]"), "out: {out}");
        assert!(out.contains("pipe=[]"), "out: {out}");
        assert!(out.contains("first=第一条"), "out: {out}");
        assert!(out.contains("last=最新输入"), "out: {out}");
        // 无输入场景:{{input}} 展开为空
        let mut c2 = ctx(&history, &mut vars);
        let out2 = expand_macros("[{{input}}]", &mut c2);
        assert_eq!(out2, "[]");
    }

    /// 7 作用域宏族(计划二):按作用域取值、缺失回退、script/extension 宏保留原样
    #[test]
    fn scoped_variable_macros() {
        use crate::parsing::assistant::AssistantVars;
        use crate::parsing::scopes::{Scope, ScopeVars};
        use serde_json::json;

        let history: Vec<(String, String)> = Vec::new();
        let mut vars = HashMap::new();
        let mut sv = ScopeVars::new();
        sv.with_scope(Scope::Global, "", json!({ "g": "全局" }));
        sv.with_scope(Scope::Character, "", json!({ "c": "角色" }));
        sv.with_scope(Scope::Preset, "", json!({ "p": "预设" }));
        sv.chat_tree = AssistantVars::from_value(json!({ "ct": "树" }));
        sv.chat_flat.insert("cf".into(), "扁平".into());
        sv.set_message_scope_id(Some("9".into()));
        sv.with_scope(Scope::Message, "9", json!({ "m": "消息" }));

        let mut c = ctx(&history, &mut vars);
        c.scopes = Some(&mut sv);
        let out = expand_macros(
            "g={{get_global_variable::g}}|c={{get_character_variable::c}}|p={{get_preset_variable::p}}|m={{get_message_variable::m}}|ct={{get_chat_variable::ct}}|cf={{get_chat_variable::cf}}",
            &mut c,
        );
        assert_eq!(out, "g=\"全局\"|c=\"角色\"|p=\"预设\"|m=\"消息\"|ct=\"树\"|cf=\"扁平\"");
        // message 缺失 → 强制回退 chat 树(兼容既有卡片 stat_data 读取)
        let mut c2 = ctx(&history, &mut vars);
        c2.scopes = Some(&mut sv);
        let out2 = expand_macros("[{{get_message_variable::ct}}]", &mut c2);
        assert_eq!(out2, "[\"树\"]");
        // 未知路径 → 空;script/extension 无宏 → 保留原样
        let mut c3 = ctx(&history, &mut vars);
        c3.scopes = Some(&mut sv);
        let out3 = expand_macros(
            "[{{get_global_variable::nope}}]{{get_script_variable::x}}{{get_extension_variable::y}}",
            &mut c3,
        );
        assert_eq!(out3, "[]{{get_script_variable::x}}{{get_extension_variable::y}}");
        // format 变体:YAML 风格
        let mut c4 = ctx(&history, &mut vars);
        c4.scopes = Some(&mut sv);
        let out4 = expand_macros("[{{format_global_variable::g}}]", &mut c4);
        assert!(out4.contains("\"全局\""), "format 应含 YAML 风格值: {out4}");
        // scopes=None:作用域宏返回空(无数据语义),既有宏不受影响
        let mut c5 = ctx(&history, &mut vars);
        let out5 = expand_macros("[{{get_global_variable::g}}]", &mut c5);
        assert_eq!(out5, "[]");
    }
}
