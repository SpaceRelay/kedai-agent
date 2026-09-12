// 契约 DSL 字段定义(FieldDef)及其子类型。
//
// 字段名与序列化形式对齐万花筒交接稿 §7 的 camelCase DSL(updateMode/changeRule/everyN 等),
// 因为契约是「唯一事实源」:它会被角色卡 extensions.nlkaleido / 世界书 [nlkaleido_contract]
// 内嵌、被前端面板编辑、被导出/导入。序列化字段名必须与文档 DSL 逐字一致。
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 字段值类型(§7-FieldDef.type)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Number,
    Boolean,
    List,
    Kv,
    Object,
}

/// 字段更新策略(§7-FieldDef.updateMode):决定 dueFields 是否把该字段排进本轮候选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    EveryTurn,
    Fixed,
    EveryNTurns,
    Trigger,
}

/// 持久化作用域(§7-FieldDef.persist,默认 chat)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PersistScope {
    #[default]
    Chat,
    Run,
    Global,
}

/// 字段稳定性三档(§7-StabilityClass):决定「注入方式」(值/引用/不注入),
/// 与 classifyField 的「调度池分类」是正交维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum StabilityClass {
    /// 动态池(L3 尾部真实值,每轮可变)
    #[default]
    Volatile,
    /// 静态池(STABLE_BATCH 引用 token / L2 沿用值)
    Stable,
    /// 完全不注入,由作者自定义规则维护
    Frozen,
}

/// 更新频率/写入上限(§7-FieldDef.cap)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cap {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_turn: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_n_turns: Option<u32>,
}

/// 状态所有权模型(§4.7/§7-FieldDef.ownership):多写者场景的第一等概念。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ownership {
    /// 属主子系统 id:agent/manual/plot/dice/memory/自定义 id。
    #[serde(default = "default_owner")]
    pub owner: String,
    /// 可写者白名单(默认 [owner];"*" 表示任意)。未列入的写者提交 op → 拒绝(not_owner)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writers: Option<Vec<String>>,
    /// 写入优先级:越大越优先(同轮冲突取高者)。
    #[serde(default)]
    pub priority: i32,
    /// 冲突合并规则。
    #[serde(default = "default_merge")]
    pub merge: MergeRule,
    /// 是否记录变更审计(默认 true;false 用于高频临时字段)。
    #[serde(default = "default_audit")]
    pub audit: bool,
}

impl Default for Ownership {
    fn default() -> Self {
        Ownership {
            owner: default_owner(),
            writers: None,
            priority: 0,
            merge: default_merge(),
            audit: true,
        }
    }
}

fn default_owner() -> String {
    "agent".to_string()
}

fn default_merge() -> MergeRule {
    MergeRule::LastWrite
}

fn default_audit() -> bool {
    true
}

/// 冲突合并规则(§4.7):last_write/sum/max/min/custom_fn_id。
/// custom_fn_id 为作者经注册表注册的合并函数 id。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeRule {
    LastWrite,
    Sum,
    Max,
    Min,
    CustomFnId,
}

/// 字段定义(§7-FieldDef)。所有字段均可选或带默认值,
/// 保证「新增字段向后兼容」(缺失用默认填充)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldDef {
    /// 点分路径(命名规则见 §4.1:禁 `.` 与 `[`/`]`)
    pub path: String,
    #[serde(rename = "type")]
    pub kind: FieldType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 初始值:会话初始化时仅填充缺失/null 的字段(InitVar 已写的值不覆盖);
    /// 显式 null 视为未初始化(与 changelog 的 old 语义一致)。
    /// 契约后续变更不回填——留给版本调和机制(见 engine 接入点注释)。
    pub default: Option<Value>,
    pub update_mode: UpdateMode,
    /// every_n_turns 的 N(仅当 update_mode=every_n_turns 时有效)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_n: Option<u32>,
    /// 是否参与每轮刷新
    #[serde(default)]
    pub dynamic: bool,
    /// 自然语言更新规则(进 L1 前缀)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<Cap>,
    /// 场景/章节/楼层区间
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Vec<String>>,
    /// 到期轮数
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<u32>,
    #[serde(default)]
    pub persist: PersistScope,
    #[serde(default)]
    pub stability: StabilityClass,
    pub display: bool,
    /// 显式依赖声明(静态分析 change_rule 之外的兜底,§0.2)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<Ownership>,
}

impl FieldDef {
    /// 归属的可写者白名单(未声明 ownership 时回退默认值)。
    pub fn writers(&self) -> Vec<String> {
        match &self.ownership {
            Some(o) => o.writers.clone().unwrap_or_else(|| vec![o.owner.clone()]),
            None => vec!["agent".to_string(), "manual".to_string()],
        }
    }

    /// 属主 id(未声明时默认 agent)。
    pub fn owner(&self) -> String {
        self.ownership
            .as_ref()
            .map(|o| o.owner.clone())
            .unwrap_or_else(default_owner)
    }

    /// 冲突合并规则(未声明时默认 last_write)。
    pub fn merge(&self) -> MergeRule {
        self.ownership
            .as_ref()
            .map(|o| o.merge.clone())
            .unwrap_or_else(default_merge)
    }

    /// 写入优先级(未声明时默认 0)。
    pub fn priority(&self) -> i32 {
        self.ownership.as_ref().map(|o| o.priority).unwrap_or(0)
    }

    /// 是否审计(未声明时默认 true)。
    pub fn audit(&self) -> bool {
        self.ownership.as_ref().map(|o| o.audit).unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 序列化字段名必须与文档 DSL 的 camelCase 逐字一致(对外唯一事实源)。
    #[test]
    fn field_def_serializes_camel_case() {
        let f = FieldDef {
            path: "角色.络络.好感度".into(),
            kind: FieldType::Number,
            default: Some(json!(0)),
            update_mode: UpdateMode::EveryNTurns,
            every_n: Some(3),
            dynamic: false,
            change_rule: Some("按剧情推进调整".into()),
            cap: None,
            scope: None,
            ttl: None,
            persist: PersistScope::Chat,
            stability: StabilityClass::Stable,
            display: true,
            dependencies: None,
            ownership: None,
        };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["updateMode"], "every_n_turns");
        assert_eq!(v["everyN"], 3);
        assert_eq!(v["changeRule"], "按剧情推进调整");
        assert_eq!(v["stability"], "stable");
        assert!(v.get("dependencies").is_none(), "空可选字段应省略");
    }

    /// 默认 ownership 回退:owner=agent,writers=[agent,manual],merge=last_write,audit=true。
    #[test]
    fn field_def_default_ownership() {
        let f = FieldDef {
            path: "x".into(),
            kind: FieldType::Number,
            default: None,
            update_mode: UpdateMode::EveryTurn,
            every_n: None,
            dynamic: false,
            change_rule: None,
            cap: None,
            scope: None,
            ttl: None,
            persist: PersistScope::Chat,
            stability: StabilityClass::Volatile,
            display: true,
            dependencies: None,
            ownership: None,
        };
        assert_eq!(f.owner(), "agent");
        assert_eq!(f.writers(), vec!["agent", "manual"]);
        assert_eq!(f.merge(), MergeRule::LastWrite);
        assert_eq!(f.priority(), 0);
        assert!(f.audit());
    }
}
