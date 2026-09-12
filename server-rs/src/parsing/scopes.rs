// 7 作用域变量(计划二 · 酒馆助手 TavernHelper 兼容,L2 中层 · parsing):
// global/chat/character/preset/message/script/extension 作用域模型。
// 融合现有两套会话级存储:session_assistant_vars(chat 树,规范存储)与
// session_vars(chat 扁平层),其余作用域存 scope_variables 表(内存中懒加载)。
// 读优先级(合并视图,低→高):global < preset < character < chat扁平 < chat树 < message,
// script/extension 不进读链(仅存储层 + API,插件生态中二者本无宏)。
use std::collections::HashMap;

use serde_json::{json, Value};

use super::assistant::{path_get, split_path, AssistantVars};

/// 作用域枚举:与 scope_variables 表 scope 列取值一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Global,
    Chat,
    Character,
    Preset,
    Message,
    Script,
    Extension,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Chat => "chat",
            Scope::Character => "character",
            Scope::Preset => "preset",
            Scope::Message => "message",
            Scope::Script => "script",
            Scope::Extension => "extension",
        }
    }

    #[allow(clippy::should_implement_trait)] // 自定义作用域解析,刻意不实现 FromStr(无对称 Display/规范语义)
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "global" => Some(Scope::Global),
            "chat" => Some(Scope::Chat),
            "character" => Some(Scope::Character),
            "preset" => Some(Scope::Preset),
            "message" => Some(Scope::Message),
            "script" => Some(Scope::Script),
            "extension" => Some(Scope::Extension),
            _ => None,
        }
    }
}

/// 7 作用域变量容器(纯内存,读写经此):
/// - chat_tree / chat_flat 与既有存储同构,由引擎加载/持久化;
/// - others 为 (scope, scope_id) → 变量对象(global/character/preset/message/script/extension),
///   懒加载:引擎在渲染上下文收集时经 `with_scope` 注入,写入时懒创建。
#[derive(Debug)]
pub struct ScopeVars {
    /// chat 作用域树(与 session_assistant_vars 同步;根即 stat_data)
    pub chat_tree: AssistantVars,
    /// chat 作用域扁平层(与 session_vars 同步;key → value 字符串)
    pub chat_flat: HashMap<String, String>,
    /// 其余作用域数据(scope, scope_id) → 变量对象(点路径访问)
    others: HashMap<(Scope, String), Value>,
    /// 当前消息作用域 id(渲染/更新所针对的消息 rowid;无则跳过 message 层)
    message_scope_id: Option<String>,
    /// character 作用域 id(角色卡 id;无则跳过 character 层)
    character_scope_id: Option<String>,
    /// preset 作用域 id(楼层配置 id;无则跳过 preset 层)
    preset_scope_id: Option<String>,
}

impl Default for ScopeVars {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeVars {
    pub fn new() -> Self {
        ScopeVars {
            chat_tree: AssistantVars::new(),
            chat_flat: HashMap::new(),
            others: HashMap::new(),
            message_scope_id: None,
            character_scope_id: None,
            preset_scope_id: None,
        }
    }

    pub fn set_message_scope_id(&mut self, id: Option<String>) {
        self.message_scope_id = id;
    }

    pub fn set_character_scope_id(&mut self, id: Option<String>) {
        self.character_scope_id = id;
    }

    /// 当前消息作用域 id(供脚本桥读取;None 表示未指定)
    pub fn message_scope_id(&self) -> Option<&str> {
        self.message_scope_id.as_deref()
    }

    /// 当前角色作用域 id(供脚本桥读取;None 表示未指定)
    pub fn character_scope_id(&self) -> Option<&str> {
        self.character_scope_id.as_deref()
    }

    /// 当前预设作用域 id(供脚本桥读取;None 表示未指定)
    pub fn preset_scope_id(&self) -> Option<&str> {
        self.preset_scope_id.as_deref()
    }

    /// 同步 chat 树镜像(渲染回合边界调用:引擎把 assistant_vars 当前状态同步进
    /// 读链,保证 message→chat 回退读到最新树)。渲染回合内的 chat 树写仍以
    /// env.vars(assistant_vars)为权威,回合间同步。
    pub fn sync_chat_tree(&mut self, tree: &AssistantVars) {
        self.chat_tree = tree.clone();
    }

    /// 同步 chat 扁平层镜像(同上;宏 {{setvar}} 写 session_vars 后回合间同步)。
    pub fn sync_chat_flat(&mut self, flat: &HashMap<String, String>) {
        self.chat_flat = flat.clone();
    }

    /// 取出非 chat 作用域全部数据(scope, scope_id, data_raw JSON),供引擎收尾落库。
    pub fn take_others(&mut self) -> Vec<(Scope, String, String)> {
        let mut out = Vec::new();
        for ((scope, id), data) in self.others.drain() {
            out.push((scope, id, data.to_string()));
        }
        out
    }

    /// 注入某作用域已加载数据(引擎上下文收集时调用;懒加载语义,覆写式)。
    pub fn with_scope(&mut self, scope: Scope, scope_id: &str, data: Value) {
        self.others.insert((scope, scope_id.to_string()), data);
    }

    /// 读取指定作用域原始数据(API 整表读取用;无记录返回 None)。
    pub fn scope_data(&self, scope: Scope, scope_id: &str) -> Option<&Value> {
        match scope {
            Scope::Chat => Some(self.chat_tree.tree()),
            Scope::Message
            | Scope::Character
            | Scope::Preset
            | Scope::Global
            | Scope::Script
            | Scope::Extension => {
                let id = match scope {
                    Scope::Message => self.message_scope_id.as_deref().unwrap_or(scope_id),
                    Scope::Character => self.character_scope_id.as_deref().unwrap_or(scope_id),
                    Scope::Preset => self.preset_scope_id.as_deref().unwrap_or(scope_id),
                    _ => scope_id,
                };
                self.others.get(&(scope, id.to_string()))
            }
        }
    }

    // ===================== 合并视图(读) =====================

    /// 合并视图:按优先级 message → chat树 → chat扁平 → character → preset → global,
    /// 返回第一个命中值的克隆。路径经 split_path 剥 stat_data 前缀。
    pub fn view(&self, path: &str) -> Option<Value> {
        let segs = split_path(path);
        // 1) message 作用域(指定了消息 id 时)
        if let Some(id) = &self.message_scope_id {
            if let Some(v) = self
                .others
                .get(&(Scope::Message, id.clone()))
                .and_then(|d| path_get(d, &segs))
            {
                return Some(v.clone());
            }
        }
        // 2) chat 树(规范存储;含 stat_data 前缀等价路径)
        if let Some(v) = self.chat_tree.get_value(path) {
            return Some(v.clone());
        }
        // 3) chat 扁平层(单层键;路径无点/斜杠时精确匹配)
        if segs.len() == 1 {
            if let Some(v) = self.chat_flat.get(&segs[0]) {
                return Some(json!(v));
            }
        }
        // 4) character → 5) preset → 6) global
        for scope in [Scope::Character, Scope::Preset, Scope::Global] {
            if let Some(v) = self.scope_data(scope, "").and_then(|d| path_get(d, &segs)) {
                return Some(v.clone());
            }
        }
        None
    }

    /// getvar 兼容读取(计划二):message/chat树/character/preset/global 命中返回
    /// JSON 显示值(value_to_display 带引号);chat 扁平层不在此查(宏层以实时 vars 兜底,
    /// 保证展开中途 setvar→getvar 时序正确,见 macros.rs getvar 分支)。
    pub fn getvar_display(&self, path: &str) -> Option<String> {
        let segs = split_path(path);
        // message 层(指定了消息 id 时)
        if let Some(id) = &self.message_scope_id {
            if let Some(v) = self
                .others
                .get(&(Scope::Message, id.clone()))
                .and_then(|d| path_get(d, &segs))
            {
                return Some(crate::parsing::assistant::value_to_display(v));
            }
        }
        // chat 树(规范存储;含 stat_data 前缀等价路径)
        if let Some(v) = self.chat_tree.get_value(path) {
            return Some(crate::parsing::assistant::value_to_display(v));
        }
        // character → preset → global(JSON 显示)
        for scope in [Scope::Character, Scope::Preset, Scope::Global] {
            if let Some(v) = self.scope_data(scope, "").and_then(|d| path_get(d, &segs)) {
                return Some(crate::parsing::assistant::value_to_display(v));
            }
        }
        // 扁平层(chat 作用域)不在此查:宏展开的 setvar 实时写调用方 vars 表,
        // 镜像仅在回合边界同步,展开中途会读到陈旧值;由宏层以实时 vars 兜底。
        None
    }

    /// 精确作用域读取;message 作用域按 §2.1 强制回退 chat 树 → chat 扁平。
    pub fn read_scope(&self, scope: Scope, path: &str) -> Option<Value> {
        match scope {
            Scope::Chat => {
                if let Some(v) = self.chat_tree.get_value(path) {
                    return Some(v.clone());
                }
                let segs = split_path(path);
                if segs.len() == 1 {
                    if let Some(v) = self.chat_flat.get(&segs[0]) {
                        return Some(json!(v));
                    }
                }
                None
            }
            Scope::Message => {
                // message 未命中 → 回退 chat 树 → chat 扁平(兼容既有卡片 stat_data 读取)
                if let Some(id) = &self.message_scope_id {
                    if let Some(v) = self
                        .others
                        .get(&(Scope::Message, id.clone()))
                        .and_then(|d| path_get(d, &split_path(path)))
                    {
                        return Some(v.clone());
                    }
                }
                self.read_scope(Scope::Chat, path)
            }
            _ => self
                .scope_data(scope, "")
                .and_then(|d| path_get(d, &split_path(path)))
                .cloned(),
        }
    }

    /// YAML 风格格式化({{format_*_variable}} 宏);path 为空格式化整树。
    pub fn format_scope(&self, scope: Scope, path: Option<&str>) -> String {
        let data = match scope {
            Scope::Chat => Some(self.chat_tree.tree().clone()),
            Scope::Message
            | Scope::Character
            | Scope::Preset
            | Scope::Global
            | Scope::Script
            | Scope::Extension => self.read_scope(scope, ""),
        };
        let Some(data) = data else {
            return String::new();
        };
        let tmp = AssistantVars::from_value(data);
        tmp.format(path)
    }

    // ===================== 写 =====================

    /// 写目标作用域:chat 走树/扁平层(路径含点或已在树 → 树,否则扁平键);
    /// 其余作用域懒创建对象后写点路径;作用域之间互不传播(§2.3)。
    pub fn write(&mut self, scope: Scope, path: &str, value: Value) {
        let segs = split_path(path);
        match scope {
            Scope::Chat => {
                let p = path.trim();
                // stat_data.* 前缀或已在树中的路径 → 树(兼容规则,§2.2);
                // 否则单层扁平键 → 扁平层(与宏 {{setvar::k::v}} 语义一致)
                let is_tree_path = p == "stat_data"
                    || p.starts_with("stat_data.")
                    || self.chat_tree.get_value(path).is_some();
                if !is_tree_path && segs.len() <= 1 {
                    self.chat_flat.insert(
                        segs.first()
                            .cloned()
                            .unwrap_or_else(|| path.trim().to_string()),
                        value
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| value.to_string()),
                    );
                } else {
                    self.chat_tree.set(path, value);
                }
            }
            Scope::Message
            | Scope::Character
            | Scope::Preset
            | Scope::Global
            | Scope::Script
            | Scope::Extension => {
                let id = match scope {
                    Scope::Message => self.message_scope_id.clone().unwrap_or_default(),
                    Scope::Character => self.character_scope_id.clone().unwrap_or_default(),
                    Scope::Preset => self.preset_scope_id.clone().unwrap_or_default(),
                    _ => String::new(),
                };
                let entry = self.others.entry((scope, id)).or_insert_with(|| json!({}));
                crate::parsing::assistant::path_set(entry, &segs, value);
            }
        }
    }
}

// ===================== 测试 =====================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope_vars() -> ScopeVars {
        ScopeVars::new()
    }

    /// 合并视图优先级:低层(global)被高层(message)遮蔽
    #[test]
    fn view_priority_overrides() {
        let mut sv = scope_vars();
        sv.with_scope(Scope::Global, "", json!({ "x": "global", "only_g": 1 }));
        sv.with_scope(Scope::Character, "c1", json!({ "x": "character" }));
        sv.chat_tree.set("x", json!("tree"));
        sv.chat_tree.set("tree_only", json!(true));
        sv.chat_flat.insert("x".into(), "flat".into());
        sv.set_message_scope_id(Some("42".into()));
        sv.with_scope(Scope::Message, "42", json!({ "x": "message" }));

        assert_eq!(sv.view("x"), Some(json!("message")));
        // message 层缺路径 → 回退 chat 树
        assert_eq!(sv.view("tree_only"), Some(json!(true)));
        // 树缺 → 扁平层
        assert_eq!(sv.view("chat_only_flat"), None);
        // 全部缺 → character → global 兜底
        assert_eq!(sv.view("only_g"), Some(json!(1)));
    }

    /// message 未命中强制回退 chat 树(兼容既有卡片 stat_data 读取)
    #[test]
    fn message_falls_back_to_chat_tree() {
        let mut sv = scope_vars();
        sv.chat_tree.set("好感度", json!(88));
        sv.set_message_scope_id(Some("1".into()));
        sv.with_scope(Scope::Message, "1", json!({ "好感度": 77, "仅消息": "m" }));

        assert_eq!(sv.read_scope(Scope::Message, "好感度"), Some(json!(77)));
        assert_eq!(sv.read_scope(Scope::Message, "仅消息"), Some(json!("m")));
        // 消息表缺 → 回退 chat 树(stat_data. 前缀等价路径)
        assert_eq!(
            sv.read_scope(Scope::Message, "stat_data.好感度"),
            Some(json!(77))
        );
        sv.with_scope(Scope::Message, "1", json!({}));
        assert_eq!(sv.read_scope(Scope::Message, "好感度"), Some(json!(88)));
    }

    /// 写目标判定:默认写 message;stat_data.* 与「已在 chat 树」写 chat;作用域隔离
    #[test]
    fn write_target_and_isolation() {
        let mut sv = scope_vars();
        sv.chat_tree.set("已有", json!("tree"));
        sv.set_message_scope_id(Some("1".into()));

        // 默认 message(新键):写消息作用域,不污染 chat 树
        sv.write(Scope::Message, "新键", json!(1));
        assert_eq!(sv.read_scope(Scope::Message, "新键"), Some(json!(1)));
        assert_eq!(sv.view("新键"), Some(json!(1)));
        assert_eq!(sv.chat_tree.get_value("新键"), None);

        // stat_data.* 路径 → chat 树(兼容规则,§2.2)
        sv.write(Scope::Chat, "stat_data.好感度", json!(90));
        assert_eq!(sv.chat_tree.get_value("好感度"), Some(&json!(90)));

        // 单层扁平键写 chat 扁平层
        sv.write(Scope::Chat, "flat_key", json!("v"));
        assert_eq!(sv.chat_flat.get("flat_key").map(|s| s.as_str()), Some("v"));

        // global 写不污染其他作用域
        sv.write(Scope::Global, "g", json!(9));
        assert_eq!(sv.read_scope(Scope::Global, "g"), Some(json!(9)));
        assert_eq!(sv.view("g"), Some(json!(9)));
        assert_eq!(sv.read_scope(Scope::Message, "g"), None);
    }

    /// format_scope:YAML 风格(与 AssistantVars::format 一致)
    #[test]
    fn format_scope_yaml() {
        let mut sv = scope_vars();
        sv.with_scope(Scope::Global, "", json!({ "name": "林晚", "lv": 10 }));
        let out = sv.format_scope(Scope::Global, Some("name"));
        assert_eq!(out.trim(), "\"林晚\"");
        let whole = sv.format_scope(Scope::Global, None);
        assert!(whole.contains("lv: 10"), "out: {whole}");
    }

    /// scope_data 整表读取(API 用)
    #[test]
    fn scope_data_reads_whole() {
        let mut sv = scope_vars();
        sv.chat_tree.set("a", json!(1));
        sv.with_scope(Scope::Global, "", json!({ "b": 2 }));
        assert_eq!(
            sv.scope_data(Scope::Chat, "").and_then(|d| d.get("a")),
            Some(&json!(1))
        );
        assert_eq!(
            sv.scope_data(Scope::Global, "").and_then(|d| d.get("b")),
            Some(&json!(2))
        );
        assert_eq!(sv.scope_data(Scope::Script, ""), None);
    }
}
