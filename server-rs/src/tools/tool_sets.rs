// 工具集合常量与过滤器(单一出处)。
//
// 背景:同一份「只读安全集」原先在规划器侦察、子智能体、反思三处各硬编码一遍,
// 新增只读工具需多处同步,极易漂移。这里集中定义,调用点只引用常量。
// 另集中「正文元工具」排除逻辑:get_state/apply_patch 只服务多步变量驱动器与显式
// 白名单,不应出现在正文默认工具列表(否则诱导模型在正文轮误用变量补丁通道)。
use crate::models::types::ToolDefinition;

/// 正文元工具:多步变量驱动器专用,不进正文默认列表。
/// 聊天 agent 模式与任务全量模式共用此排除口径,避免同一模型在两处看到不同工具。
pub const META_TOOLS: &[&str] = &["get_state", "apply_patch"];

/// 规划器只读侦察白名单:规划阶段允许模型先收集信息再产出计划 JSON;
/// 严禁写操作(违背 plan/legacy「只规划不执行」零副作用纪律)与编排类(会把规划变成执行)。
pub const READONLY_SCOUT: &[&str] = &["read", "search", "memory_read", "calculator"];

/// 子智能体工具白名单:读/搜索类安全工具;写类与编排类一律剔除
/// (子 agent 不得再派子 agent;嵌套由白名单与深度守卫双重排除)。
pub const SUBAGENT: &[&str] = &[
    "read",
    "search",
    "todo",
    "sleep",
    "calculator",
    "memory_read",
];

/// 反思阶段工具白名单:仅禁词替换与定点修订;dirty 文本修正不引入检索类工具。
pub const REFLECT: &[&str] = &["censor_text", "revise_passage"];

/// 剔除正文元工具(get_state/apply_patch),保留其余工具与原顺序。
/// 顺序稳定性是前缀缓存的前提,故不做排序。
pub fn exclude_meta(defs: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    defs.into_iter()
        .filter(|d| !META_TOOLS.contains(&d.name.as_str()))
        .collect()
}

/// 按名称白名单过滤工具定义(保留入参顺序;未注册的名称自然被忽略)。
/// 语义为「交集」:调用方传入的白名单 ∩ 实际注册工具。
pub fn filter_by_names(defs: Vec<ToolDefinition>, names: &[String]) -> Vec<ToolDefinition> {
    defs.into_iter()
        .filter(|d| names.iter().any(|n| n == &d.name))
        .collect()
}

/// 按 &str 常量白名单过滤(defs 顺序保留)
pub fn filter_by_const(defs: Vec<ToolDefinition>, names: &[&str]) -> Vec<ToolDefinition> {
    defs.into_iter()
        .filter(|d| names.contains(&d.name.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn def(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.into(),
            description: name.into(),
            parameters: json!({ "type": "object" }),
        }
    }

    #[test]
    fn exclude_meta_removes_only_meta_tools() {
        let defs = vec![
            def("read"),
            def("get_state"),
            def("write"),
            def("apply_patch"),
        ];
        let out = exclude_meta(defs);
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["read", "write"]);
    }

    #[test]
    fn filter_by_names_is_intersection_and_keeps_order() {
        let defs = vec![def("write"), def("read"), def("search")];
        let out = filter_by_names(
            defs,
            &[
                "read".to_string(),
                "search".to_string(),
                "missing".to_string(),
            ],
        );
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["read", "search"]);
    }

    #[test]
    fn filter_by_const_works() {
        let defs = vec![def("read"), def("write"), def("search"), def("memory_read")];
        let out = filter_by_const(defs, READONLY_SCOUT);
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["read", "search", "memory_read"]);
    }

    /// 常量内容与改造前的字面量逐一比对,防止无意改动导致能力漂移
    #[test]
    fn constants_match_legacy_literals() {
        assert_eq!(META_TOOLS, &["get_state", "apply_patch"]);
        assert_eq!(
            READONLY_SCOUT,
            &["read", "search", "memory_read", "calculator"]
        );
        assert_eq!(
            SUBAGENT,
            &[
                "read",
                "search",
                "todo",
                "sleep",
                "calculator",
                "memory_read"
            ]
        );
        assert_eq!(REFLECT, &["censor_text", "revise_passage"]);
    }
}
