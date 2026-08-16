// 角色卡内嵌插件检测(L1 老层 · parsing/assistant):
// 扫描角色卡 data_raw(character_book 条目)检测内嵌插件(如酒馆助手 SillyTavern-Assistant)。
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::parsing::world_book::character_book_entries;

// ===================== 角色卡内嵌插件检测 =====================

/// 插件能力点(角色卡内嵌插件检测用;全部列出,detected 区分是否命中)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginFeature {
    pub id: String,
    pub label: String,
    pub detected: bool,
}

/// 检测到的角色卡内嵌插件(如酒馆助手 SillyTavern-Assistant)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CardPluginInfo {
    /// 稳定标识(如 sillytavern-assistant)
    pub id: String,
    pub name: String,
    pub name_en: String,
    /// 检测到即视为启用(kedai 内置实现,无需安装)
    pub enabled: bool,
    /// 来源:character_card(角色卡内嵌)
    pub source: String,
    pub description: String,
    pub features: Vec<PluginFeature>,
}

/// 扫描角色卡 data_raw(character_book 条目)检测内嵌插件。
/// 当前实现:酒馆助手插件(SillyTavern-Assistant)特征检测——
///   [InitVar] 初始变量 / EJS 模板(<%) / getvar 变量系统 /
///   {{format_message_variable}} 状态注入 / <UpdateVariable> 输出协议。
pub fn detect_card_plugins(data_raw: &Value) -> Vec<CardPluginInfo> {
    let entries = character_book_entries(data_raw);
    if entries.is_empty() {
        return Vec::new();
    }
    // 拼接全部条目文本(注释 + 内容)作为检测语料
    let hay: String = entries
        .iter()
        .map(|e| format!("{}\n{}", e.comment, e.content))
        .collect::<Vec<_>>()
        .join("\n");
    let has = |needle: &str| hay.contains(needle);
    let features = vec![
        PluginFeature {
            id: "initvar".into(),
            label: "初始变量 [InitVar]".into(),
            detected: has("[InitVar]"),
        },
        PluginFeature {
            id: "ejs".into(),
            label: "EJS 模板渲染(分阶段人设等)".into(),
            detected: hay.contains("<%"),
        },
        PluginFeature {
            id: "variable".into(),
            label: "变量系统 getvar / stat_data".into(),
            detected: has("getvar(") || has("{{getvar::"),
        },
        PluginFeature {
            id: "status".into(),
            label: "状态注入 {{format_message_variable}}".into(),
            detected: has("{{format_message_variable") || has("<StatusPlaceHolderImpl/>"),
        },
        PluginFeature {
            id: "protocol".into(),
            label: "输出协议 <UpdateVariable>".into(),
            detected: has("<UpdateVariable") || has("JSONPatch"),
        },
    ];
    if features.iter().any(|f| f.detected) {
        vec![CardPluginInfo {
            id: "sillytavern-assistant".into(),
            name: "酒馆助手插件".into(),
            name_en: "SillyTavern-Assistant".into(),
            enabled: true,
            source: "character_card".into(),
            description: "角色卡内嵌的酒馆助手插件:分阶段人设等条目按 stat_data 变量实时渲染(EJS 模板),模型回复中的 <UpdateVariable>/<JSONPatch> 自动剥离并应用,会话状态随对话推进。kedai 已内置完整兼容实现,无需额外安装。".into(),
            features,
        }]
    } else {
        Vec::new()
    }
}
