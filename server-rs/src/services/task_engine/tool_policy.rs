// 任务模式工具策略:把 settings 里的策略档位编译为「下发工具定义 + 放行名单」。
//
// 背景:任务模式没有 UI 授权上下文,不能走「等待授权」(会空等超时)。因此任务侧
// 一律使用 ToolGate { no_ui_authorization: true } —— 名单内工具放行,名单外立即拒绝。
// 策略档位:
//   all            → 全量(剔除正文元工具),等价改造前的全放行行为
//   deny_dangerous → 全量 − 元工具 − 危险级(默认;无人值守任务不默认放行写类操作)
//   allowlist      → 仅配置白名单 ∩ 已注册(最严格)
use crate::models::types::ToolDefinition;
use crate::tools::permissions::ToolRisk;
use crate::tools::registry::ToolRegistry;
use crate::tools::tool_sets;

/// 编译结果。`allowed` 为放行名单(供 ToolGate 使用),需与 `defs` 同源:
/// 名单内 = 可被模型看到并执行;名单外工具即使被模型臆造调用也会被闸门拒绝。
pub(crate) struct TaskToolPolicy {
    pub defs: Vec<ToolDefinition>,
    pub allowed: Vec<String>,
}

impl TaskToolPolicy {
    /// 本策略对应的授权闸门:任务模式恒不等待授权(无 UI 上下文)。
    /// 注意:调用方若已取走 `defs`,应改用 `ToolGate::listed(&allowed)` 以免部分移动冲突。
    #[cfg(test)]
    pub(crate) fn gate(&self) -> crate::agents::engine::executor::ToolGate<'_> {
        crate::agents::engine::executor::ToolGate {
            whitelist: Some(&self.allowed),
            no_ui_authorization: true,
        }
    }
}

/// 按策略档位编译工具集。
/// `policy`:all / deny_dangerous / allowlist(非法值按 deny_dangerous 处理,与加载侧钳制一致)。
/// `allowlist`:仅 allowlist 档位使用。
pub(crate) fn compile(policy: &str, allowlist: &[String], registry: &ToolRegistry) -> TaskToolPolicy {
    let all = tool_sets::exclude_meta(registry.list_definitions());
    let selected: Vec<ToolDefinition> = match policy {
        "all" => all,
        "allowlist" => tool_sets::filter_by_names(all, allowlist),
        // 默认(含非法值回退):拒绝危险级
        _ => {
            let risk = registry.permissions();
            all.into_iter()
                .filter(|d| risk.risk_for(&d.name) != ToolRisk::Dangerous)
                .collect()
        }
    };
    let allowed = selected.iter().map(|d| d.name.clone()).collect();
    TaskToolPolicy {
        defs: selected,
        allowed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::ToolRegistry;
    use serde_json::json;

    /// 注册若干代表性工具(含元工具与各风险级),用于策略断言
    fn registry_with_tools() -> ToolRegistry {
        let reg = ToolRegistry::new();
        let noop: crate::tools::registry::ToolExecutor =
            std::sync::Arc::new(|_a, _c| Box::pin(async { Ok("{}".to_string()) }));
        for name in [
            "read",
            "search",
            "todo",
            "agentgo",
            "agentend",
            "write",
            "replace",
            "create",
            "memory_write",
            "update_variables",
            "get_state",
            "apply_patch",
        ] {
            reg.register(
                ToolDefinition {
                    name: name.into(),
                    description: name.into(),
                    parameters: json!({ "type": "object" }),
                },
                noop.clone(),
            );
        }
        reg
    }

    #[test]
    fn deny_dangerous_excludes_dangerous_and_meta() {
        let reg = registry_with_tools();
        let p = compile("deny_dangerous", &[], &reg);
        let names: Vec<&str> = p.defs.iter().map(|d| d.name.as_str()).collect();
        for banned in ["write", "replace", "create", "memory_write", "update_variables"] {
            assert!(!names.contains(&banned), "危险工具 {banned} 应被拒绝");
        }
        for meta in ["get_state", "apply_patch"] {
            assert!(!names.contains(&meta), "元工具 {meta} 不应下发");
        }
        assert!(names.contains(&"read"));
        assert!(names.contains(&"agentend"), "agentend 属敏感级,应保留");
    }

    #[test]
    fn allowlist_only_keeps_configured_and_registered() {
        let reg = registry_with_tools();
        let p = compile(
            "allowlist",
            &["read".to_string(), "not_registered".to_string()],
            &reg,
        );
        let names: Vec<&str> = p.defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["read"]);
    }

    #[test]
    fn all_keeps_everything_except_meta() {
        let reg = registry_with_tools();
        let p = compile("all", &[], &reg);
        let names: Vec<&str> = p.defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"write"));
        assert!(!names.contains(&"get_state"));
    }

    #[test]
    fn allowed_names_match_defs() {
        let reg = registry_with_tools();
        let p = compile("deny_dangerous", &[], &reg);
        let defs: Vec<&str> = p.defs.iter().map(|d| d.name.as_str()).collect();
        let allowed: Vec<&str> = p.allowed.iter().map(|s| s.as_str()).collect();
        assert_eq!(defs, allowed);
    }

    #[test]
    fn gate_never_waits_for_authorization() {
        let reg = registry_with_tools();
        let p = compile("deny_dangerous", &[], &reg);
        assert!(p.gate().no_ui_authorization, "任务模式闸门不得等待授权");
    }
}
