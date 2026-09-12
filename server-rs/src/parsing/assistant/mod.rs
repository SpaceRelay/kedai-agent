// 酒馆助手(SillyTavern-Assistant)插件兼容层:
// 变量树(stat_data)维护、{{format_message_variable}} 格式化、<UpdateVariable>/<JSONPatch>
// 输出协议解析与应用、[InitVar] 初始变量收集、世界书条目内容渲染(含 EJS 模板)。
//
// 与前端 web/src/mvu/ 的约定保持一致:变量树根即 stat_data,点路径访问,
// 兼容 JSON Patch(RFC 6902 子集 + delta 扩展)与 MagVarUpdate 的 _.set(...) 两种协议。
// MagVarUpdate 原作者:MagicalAstrogy(github.com/MagicalAstrogy/MagVarUpdate,MIT);
// Kedai 为独立兼容实现,不包含原版代码。
//
// 模块拆分(纯文件拆分,行为不变,L1 老层 · 见 docs/ARCHITECTURE-3H.md):
//   vars.rs      变量树(stat_data)维护 + 路径工具 + 格式化
//   patch.rs     输出协议(<UpdateVariable>/<JSONPatch>/MagVarUpdate)解析
//   initvar.rs   [InitVar] 初始变量收集(YAML/JSON/_.set)
//   inject_tag.rs 注入标签解析与带标签条目收集(ST-Prompt-Template)
//   render.rs    内容渲染(EJS + format_message_variable 宏 + 段标签剥离)
//   plugins.rs   角色卡内嵌插件检测
// 本文件为聚合入口:子模块声明 + pub use 重导出,外部 crate::parsing::assistant::xxx 路径不变;
// 既有测试就近保留在本文件(经 pub use 与显式 use 访问全部被测项)。
pub mod ejs;

mod ctx;
mod initvar;
mod inject_tag;
mod patch;
mod plugins;
mod render;
mod vars;

pub use ctx::*;
pub use initvar::*;
pub use inject_tag::*;
pub use patch::*;
pub use plugins::*;
pub use render::*;
pub use vars::*;

// ===================== 测试 =====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsing::world_book::WorldEntry;
    use serde_json::{json, Value};

    fn entry(comment: &str, content: &str) -> WorldEntry {
        WorldEntry {
            id: 0,
            comment: comment.to_string(),
            keys: vec![],
            keys_secondary: vec![],
            regex: None,
            use_regex: false,
            content: content.to_string(),
            constant: false,
            enabled: false,
            position: 0,
            depth: 4,
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

    #[test]
    fn path_get_set_nested() {
        let mut v = AssistantVars::new();
        v.set("世界.年分", json!(2024));
        v.set("芽衣.心之所向.好感度", json!(0));
        assert_eq!(v.get_value("世界.年分"), Some(&json!(2024)));
        assert_eq!(
            v.get_value("stat_data.芽衣.心之所向.好感度"),
            Some(&json!(0))
        );
        assert_eq!(v.get_value("不存在"), None);
        // 数字段 → 数组
        v.set("列表.0", json!("a"));
        assert_eq!(v.get_value("列表.0"), Some(&json!("a")));
    }

    #[test]
    fn add_numeric() {
        let mut v = AssistantVars::new();
        v.set("好感度", json!(10));
        v.add("好感度", 5.0).unwrap();
        assert_eq!(v.get_value("好感度"), Some(&json!(15.0)));
        // 非数值目标报错
        v.set("非数字", json!("abc"));
        assert!(v.add("非数字", 1.0).is_err());
        // 字符串数字也可加
        v.set("str_num", json!("20"));
        v.add("str_num", 2.0).unwrap();
        assert_eq!(v.get_value("str_num"), Some(&json!(22.0)));
        // 不存在的路径按 0 起步
        v.add("新计数", 3.0).unwrap();
        assert_eq!(v.get_value("新计数"), Some(&json!(3.0)));
    }

    #[test]
    fn format_message_variable_output() {
        let mut v = AssistantVars::new();
        v.set("世界.年分", json!(2024));
        v.set("世界.时间", json!("14:30"));
        v.set("芽衣.情绪", json!("平静"));
        v.set("心之所向.好感度", json!(150));
        v.set("开关", json!(true));
        let out = v.format(None);
        assert!(out.contains("年分: 2024"), "out: {out}");
        assert!(out.contains("时间: \"14:30\""), "out: {out}");
        assert!(out.contains("情绪: \"平静\""), "out: {out}");
        assert!(out.contains("好感度: 150"), "out: {out}");
        assert!(out.contains("开关: true"), "out: {out}");
        // 子路径
        let sub = v.format(Some("芽衣"));
        assert!(sub.contains("情绪: \"平静\""));
        assert!(!sub.contains("年分"));
    }

    #[test]
    fn apply_patches_all_ops() {
        let mut v = AssistantVars::new();
        v.set("世界.年分", json!(2024));
        v.set("心之所向.好感度", json!(0));
        v.set("列表", json!(["a", "b"]));
        let ops = vec![
            PatchOp::Replace {
                path: "/世界/年分".into(),
                value: json!(2025),
                reason: None,
            },
            PatchOp::Delta {
                path: "/心之所向/好感度".into(),
                value: 5.0,
                reason: None,
            },
            PatchOp::Replace {
                path: "/芽衣/当前行动".into(),
                value: json!("看书"),
                reason: None,
            },
            PatchOp::Remove {
                path: "/列表/0".into(),
                reason: None,
            },
            PatchOp::Move {
                from: "/芽衣/当前行动".into(),
                to: "/芽衣/行动记录".into(),
                reason: None,
            },
        ];
        v.apply_patches(&ops).unwrap();
        assert_eq!(v.get_value("世界.年分"), Some(&json!(2025)));
        assert_eq!(v.get_value("心之所向.好感度"), Some(&json!(5.0)));
        assert_eq!(v.get_value("芽衣.行动记录"), Some(&json!("看书")));
        assert_eq!(v.get_value("芽衣.当前行动"), None);
        assert_eq!(v.get_value("列表.0"), Some(&json!("b")));
    }

    #[test]
    fn parse_update_variable_jsonpatch() {
        let text = "这是正文\n<UpdateVariable>\n<Analysis>好感度上升</Analysis>\n<JSONPatch>\n[\n  { \"op\": \"replace\", \"path\": \"/心之所向/好感度\", \"value\": 5 },\n  { \"op\": \"delta\", \"path\": \"/世界/年分\", \"value\": 1 }\n]\n</JSONPatch>\n</UpdateVariable>";
        let (clean, ops) = parse_update_variable(text);
        assert_eq!(clean.trim(), "这是正文");
        assert_eq!(ops.len(), 2);
        assert_eq!(
            ops[0],
            PatchOp::Replace {
                path: "/心之所向/好感度".into(),
                value: json!(5),
                reason: None
            }
        );
        assert_eq!(
            ops[1],
            PatchOp::Delta {
                path: "/世界/年分".into(),
                value: 1.0,
                reason: None
            }
        );
    }

    #[test]
    fn parse_update_variable_magvar_update() {
        let text =
            "正文\n<UpdateVariable>_.set('心之所向.好感度', 0, 10);//提升好感度</UpdateVariable>";
        let (clean, ops) = parse_update_variable(text);
        assert_eq!(clean.trim(), "正文");
        assert_eq!(ops.len(), 1);
        assert_eq!(
            ops[0],
            PatchOp::Replace {
                path: "心之所向.好感度".into(),
                value: json!(10),
                reason: Some("提升好感度".into())
            }
        );
    }

    #[test]
    fn parse_update_variable_multiple_blocks() {
        let text = "a<UpdateVariable><JSONPatch>[{\"op\":\"remove\",\"path\":\"/x\"}]</JSONPatch></UpdateVariable>b<UpdateVariable><JSONPatch>[{\"op\":\"replace\",\"path\":\"/y\",\"value\":\"z\"}]</JSONPatch></UpdateVariable>c";
        let (clean, ops) = parse_update_variable(text);
        assert_eq!(clean, "abc");
        assert_eq!(ops.len(), 2);
    }

    /// parse_patch_array:JSON 数组 → PatchOp(工具 update_variables 的 patches 参数复用)
    #[test]
    fn parse_patch_array_all_ops() {
        let arr = json!([
            { "op": "replace", "path": "/心之所向/好感度", "value": 150 },
            { "op": "delta", "path": "/心之所向/好感度", "value": 2 },
            { "op": "insert", "path": "/芽衣/行动", "value": "看书" },
            { "op": "remove", "path": "/列表/0" },
            { "op": "move", "from": "/芽衣/行动", "path": "/芽衣/记录" },
            { "op": "unknown", "path": "/忽略" }
        ]);
        let ops = parse_patch_array(&arr).expect("应解析");
        assert_eq!(ops.len(), 5, "未知 op 应跳过;insert 并入 replace");
        assert_eq!(
            ops[0],
            PatchOp::Replace {
                path: "/心之所向/好感度".into(),
                value: json!(150),
                reason: None
            }
        );
        assert_eq!(
            ops[1],
            PatchOp::Delta {
                path: "/心之所向/好感度".into(),
                value: 2.0,
                reason: None
            }
        );
        // insert op 与 replace 语义相同,并入 Replace(reason 恒 None)
        assert_eq!(
            ops[2],
            PatchOp::Replace {
                path: "/芽衣/行动".into(),
                value: json!("看书"),
                reason: None
            }
        );
        assert_eq!(
            ops[3],
            PatchOp::Remove {
                path: "/列表/0".into(),
                reason: None
            }
        );
        assert_eq!(
            ops[4],
            PatchOp::Move {
                from: "/芽衣/行动".into(),
                to: "/芽衣/记录".into(),
                reason: None
            }
        );
        // 空数组 / 非数组 → None
        assert!(parse_patch_array(&json!([])).is_none());
        assert!(parse_patch_array(&json!({})).is_none());
        assert!(parse_patch_array(&json!("x")).is_none());
    }

    #[test]
    fn no_block_returns_unchanged() {
        let text = "普通消息没有变量块";
        let (clean, ops) = parse_update_variable(text);
        assert_eq!(clean, text);
        assert!(ops.is_empty());
    }

    #[test]
    fn collect_init_vars_yaml() {
        let content = "世界:\n  年分: 2024\n  月份: 2\n  时间: \"14:30\"\n芽衣:\n  当前打扮: \"白外套\"\n心之所向:\n  好感度: 0";
        let v = collect_init_vars(&[entry("[InitVar]", content)]);
        assert_eq!(v.get_value("世界.年分"), Some(&json!(2024)));
        assert_eq!(v.get_value("世界.月份"), Some(&json!(2)));
        assert_eq!(v.get_value("世界.时间"), Some(&json!("14:30")));
        assert_eq!(v.get_value("芽衣.当前打扮"), Some(&json!("白外套")));
        assert_eq!(v.get_value("心之所向.好感度"), Some(&json!(0)));
    }

    #[test]
    fn collect_init_vars_json_and_set() {
        let v = collect_init_vars(&[entry("[InitVar]", r#"{"a": 1, "b": {"c": true}}"#)]);
        assert_eq!(v.get_value("a"), Some(&json!(1)));
        assert_eq!(v.get_value("b.c"), Some(&json!(true)));

        let v2 = collect_init_vars(&[entry("[InitVar]", "_.set('x', 0, 42);//初始")]);
        assert_eq!(v2.get_value("x"), Some(&json!(42)));
    }

    #[test]
    fn collect_init_vars_merges_entries_in_order() {
        let v = collect_init_vars(&[
            entry("[InitVar]", "a: 1\nb: 2"),
            entry("[InitVar]", "b: 3\nc: 4"),
        ]);
        assert_eq!(v.get_value("a"), Some(&json!(1)));
        assert_eq!(v.get_value("b"), Some(&json!(3))); // 后条目覆盖
        assert_eq!(v.get_value("c"), Some(&json!(4)));
    }

    #[test]
    fn non_init_var_entries_ignored() {
        let v = collect_init_vars(&[entry("背景设定", "a: 1")]);
        assert!(v.is_empty());
    }

    #[test]
    fn render_assistant_content_full() {
        let mut v = AssistantVars::new();
        v.set("心之所向.好感度", json!(150));
        let tpl = "<%_ if (getvar('stat_data.心之所向.好感度') > 100 && getvar('stat_data.心之所向.好感度') <= 200) { _%>悄然萌芽<%_ } else { _%>其他阶段<%_ } _%>\n{{format_message_variable::stat_data}}\n<StatusPlaceHolderImpl/>";
        let out = render_assistant_content(tpl, &mut v);
        assert!(out.starts_with("悄然萌芽"));
        assert!(out.contains("好感度: 150"));
        // StatusPlaceHolderImpl 被替换,不再出现标签本身
        assert!(!out.contains("<StatusPlaceHolderImpl/>"));
    }

    /// {{get_message_variable::path}} 与原版 MagVarUpdate 等价展开;段标签剥离
    #[test]
    fn get_message_variable_and_status_current_variable() {
        let mut v = AssistantVars::new();
        v.set("心之所向.好感度", json!(88));
        // 原版 <status_current_variable> 段写法:标签包裹 {{get_message_variable}}
        let tpl = "<status_current_variable>\n{{get_message_variable::stat_data.心之所向.好感度}}\n</status_current_variable>";
        let out = render_assistant_content(tpl, &mut v);
        assert_eq!(out.trim(), "88", "宏应展开且段标签剥离,实际: {out:?}");
        // 大小写不敏感(format 输出尾部带换行,trim 比较)
        let out2 = render_assistant_content("<STATUS_CURRENT_VARIABLE>{{GET_MESSAGE_VARIABLE::stat_data.心之所向.好感度}}</STATUS_CURRENT_VARIABLE>", &mut v);
        assert_eq!(out2.trim(), "88");
        // 无变量树时宏展开为空(不报错)
        let mut empty = AssistantVars::new();
        let out3 = render_assistant_content("{{get_message_variable::stat_data}}", &mut empty);
        assert_eq!(out3.trim(), "");
    }

    /// 渲染出错导致整条为空时,回退保留原文(内容不丢失)
    #[test]
    fn render_failure_falls_back_to_original() {
        let mut v = AssistantVars::new();
        // 词法错误(裸反斜杠在表达式内)会令整条渲染为空 → 应回退原文
        let tpl = "<CHARACTER_Rover>\n这是人设正文。\n<% const x = 1 \\ 2; %>\n更多正文";
        let out = render_assistant_content(tpl, &mut v);
        assert_eq!(out, tpl, "渲染失败应保留原文,实际: {out:?}");
        // 正常渲染不受影响(条件未命中输出为空、无错误 → 空)
        let tpl2 = "<% if (false) { %>隐藏内容<% } %>";
        let out2 = render_assistant_content(tpl2, &mut v);
        assert_eq!(out2, "", "无错误的条件未命中仍为空,不触发回退");
        // 纯文本无 EJS → 原样
        let tpl3 = "纯文本设定";
        assert_eq!(render_assistant_content(tpl3, &mut v), tpl3);
    }

    #[test]
    fn infer_value_types() {
        assert_eq!(infer_value("42"), json!(42));
        assert_eq!(infer_value("3.5"), json!(3.5));
        assert_eq!(infer_value("true"), json!(true));
        assert_eq!(infer_value("null"), Value::Null);
        assert_eq!(infer_value("{\"a\":1}"), json!({"a": 1}));
        assert_eq!(infer_value("普通文本"), json!("普通文本"));
    }

    #[test]
    fn value_to_display_format() {
        assert_eq!(value_to_display(&json!(150)), "150");
        assert_eq!(value_to_display(&json!("安静")), "\"安静\"");
    }

    #[test]
    fn detect_card_plugins_finds_assistant() {
        // 酒馆助手卡特征:[InitVar] + EJS 模板 + 状态注入 + 输出协议
        let raw = json!({
            "spec": "chara_card_v2",
            "data": {
                "name": "雪白芽衣",
                "character_book": {
                    "entries": [
                        { "id": 0, "comment": "[InitVar]", "content": "心之所向:\n  好感度: 0", "enabled": false },
                        { "id": 1, "comment": "分阶段人设", "content": "<%_ if (getvar('stat_data.心之所向.好感度') <= 0) { _%>惊弓之鸟<%_ } _%>", "constant": true },
                        { "id": 2, "comment": "变量更新规则", "content": "{{format_message_variable::stat_data}}\n<UpdateVariable><JSONPatch>[]</JSONPatch></UpdateVariable>", "constant": true }
                    ]
                }
            }
        });
        let plugins = detect_card_plugins(&raw);
        assert_eq!(plugins.len(), 1);
        let p = &plugins[0];
        assert_eq!(p.id, "sillytavern-assistant");
        assert!(p.enabled);
        assert_eq!(p.source, "character_card");
        let detected = |id: &str| {
            p.features
                .iter()
                .find(|f| f.id == id)
                .map(|f| f.detected)
                .unwrap_or(false)
        };
        assert!(detected("initvar"), "应检测到 [InitVar]");
        assert!(detected("ejs"), "应检测到 EJS 模板");
        assert!(detected("variable"), "应检测到 getvar");
        assert!(detected("status"), "应检测到状态注入");
        assert!(detected("protocol"), "应检测到输出协议");
    }

    #[test]
    fn detect_card_plugins_empty_for_plain_card() {
        // 普通卡(有世界书但无插件特征)→ 无插件
        let raw = json!({
            "data": {
                "name": "普通角色",
                "character_book": {
                    "entries": [
                        { "id": 0, "comment": "地点", "content": "图书馆安静。", "constant": true }
                    ]
                }
            }
        });
        assert!(detect_card_plugins(&raw).is_empty());
        // 无 character_book → 无插件
        assert!(detect_card_plugins(&json!({ "name": "x" })).is_empty());
    }

    // ============ 注入标签解析(parse_inject_tag) ============

    /// 全部注入标签变体解析
    #[test]
    fn parse_inject_tag_all_variants() {
        assert_eq!(
            parse_inject_tag("[GENERATE:BEFORE]"),
            InjectTag::GenerateBefore
        );
        assert_eq!(
            parse_inject_tag("[GENERATE:AFTER]"),
            InjectTag::GenerateAfter
        );
        assert_eq!(
            parse_inject_tag("[GENERATE:2:BEFORE]"),
            InjectTag::GenerateIndex {
                idx: 2,
                before: true
            }
        );
        assert_eq!(
            parse_inject_tag("[GENERATE:0:AFTER]"),
            InjectTag::GenerateIndex {
                idx: 0,
                before: false
            }
        );
        assert_eq!(
            parse_inject_tag("[GENERATE:REGEX:\\d+岁]"),
            InjectTag::GenerateRegex {
                pattern: "\\d+岁".into(),
                before: true
            }
        );
        assert_eq!(parse_inject_tag("[RENDER:BEFORE]"), InjectTag::RenderBefore);
        assert_eq!(parse_inject_tag("[RENDER:AFTER]"), InjectTag::RenderAfter);
        assert_eq!(
            parse_inject_tag("[InitialVariables]"),
            InjectTag::InitialVariables
        );
        // 兼容原 [InitVar]
        assert_eq!(parse_inject_tag("[InitVar]"), InjectTag::InitialVariables);
        // 无匹配
        assert_eq!(parse_inject_tag("普通标题"), InjectTag::None);
        assert_eq!(parse_inject_tag(""), InjectTag::None);
    }

    /// 大小写不敏感 + 注释前后空白 + 标签嵌入标题文字中
    #[test]
    fn parse_inject_tag_case_and_whitespace_insensitive() {
        assert_eq!(
            parse_inject_tag("  [generate:before]  "),
            InjectTag::GenerateBefore
        );
        assert_eq!(
            parse_inject_tag("[GENERATE:3:after]"),
            InjectTag::GenerateIndex {
                idx: 3,
                before: false
            }
        );
        assert_eq!(
            parse_inject_tag("分阶段人设 [Render:After] 注入"),
            InjectTag::RenderAfter
        );
        assert_eq!(
            parse_inject_tag("[[initialvariables]]"),
            InjectTag::InitialVariables
        );
    }

    /// 失败路径:未知/畸形标签 → None(不 panic)
    #[test]
    fn parse_inject_tag_unknown_and_malformed() {
        assert_eq!(parse_inject_tag("[GENERATE:MEOW]"), InjectTag::None);
        assert_eq!(parse_inject_tag("[GENERATE:abc:BEFORE]"), InjectTag::None);
        assert_eq!(parse_inject_tag("[RENDER:ANY]"), InjectTag::None);
        assert_eq!(parse_inject_tag("[GENERATE"), InjectTag::None);
        assert_eq!(parse_inject_tag("GENERATE:BEFORE]"), InjectTag::None);
    }

    /// collect_init_vars 兼容 [InitialVariables] 标签
    #[test]
    fn collect_init_vars_accepts_initial_variables_tag() {
        let v = collect_init_vars(&[entry("[InitialVariables]", "a: 1\nb: 2")]);
        assert_eq!(v.get_value("a"), Some(&json!(1)));
        assert_eq!(v.get_value("b"), Some(&json!(2)));
        // 混合两种标签按顺序合并
        let v2 = collect_init_vars(&[
            entry("[InitVar]", "a: 1"),
            entry("[InitialVariables]", "b: 2"),
        ]);
        assert_eq!(v2.get_value("a"), Some(&json!(1)));
        assert_eq!(v2.get_value("b"), Some(&json!(2)));
    }

    // ============ 注入标签条目收集(collect_generate_entries) ============

    /// 带标签条目被收集并渲染;[InitVar] 与普通条目走原链路
    #[test]
    fn collect_generate_entries_splits_and_renders() {
        let mut v = AssistantVars::new();
        v.set("好感度", json!(150));
        let mut e1 = entry("[GENERATE:BEFORE]", "常规生成前提示");
        e1.enabled = true;
        let e2 = entry("[InitVar]", "a: 1");
        let e3 = entry("普通条目", "走原链路内容");
        let mut e4 = entry("[GENERATE:REGEX:.*affection.*]", "<%= getvar('好感度') %>");
        e4.enabled = true;
        let mut e5 = entry("[RENDER:AFTER]", "渲染后提示");
        e5.enabled = true;
        let mut e6 = entry("[GENERATE:0:AFTER]", "零号索引");
        e6.enabled = true;
        let mut e7 = entry("[GENERATE:2:BEFORE]", "二号索引");
        e7.enabled = true;
        let entries = vec![e1, e2, e3, e4, e5, e6, e7];
        let (gen, rest) = collect_generate_entries(&entries, &mut v);
        // GENERATE x4 + RENDER x1 进收集;[InitVar] 与普通条目留原链路
        assert_eq!(
            gen.len(),
            5,
            "gen: {:?}",
            gen.iter()
                .map(|g| g.entry.comment.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0].comment, "[InitVar]");
        assert_eq!(rest[1].comment, "普通条目");
        // 内容已渲染
        let re = gen
            .iter()
            .find(|g| g.entry.comment.contains("REGEX"))
            .unwrap();
        assert_eq!(re.rendered, "150");
        // GenerateIndex 按 idx 升序(0 在 2 前)
        let idx_order: Vec<usize> = gen
            .iter()
            .filter_map(|g| match g.tag {
                InjectTag::GenerateIndex { idx, .. } => Some(idx),
                _ => None,
            })
            .collect();
        assert_eq!(idx_order, vec![0, 2]);
    }

    /// 失败路径:禁用条目与无标签条目不进收集
    #[test]
    fn collect_generate_entries_ignores_disabled_and_plain() {
        let mut v = AssistantVars::new();
        let mut e1 = entry("[GENERATE:BEFORE]", "禁用条目");
        e1.enabled = false;
        let e2 = entry("[InitialVariables]", "a: 1");
        let e3 = entry("普通", "内容");
        let (gen, rest) = collect_generate_entries(&[e1, e2, e3], &mut v);
        assert!(gen.is_empty());
        assert_eq!(rest.len(), 3);
    }
}
