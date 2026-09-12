// 契约校验与依赖图计算(纯函数,可单测)。
//
// validate_contract:返回错误列表(路径冲突/依赖环/不变量引用/护栏非法)。
// compute_dependencies:静态分析 changeRule/derived.expr 的 ${path} 引用 + FieldDef.dependencies
//   显式声明的并集,拓扑排序拒绝环,再 BFS 取深度 ≤ maxDependencyDepth 的可达子图(§0.2)。
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::contracts::field::UpdateMode;
use crate::contracts::Contract;

/// 校验契约结构,返回错误列表(空 = 合法)。
pub fn validate_contract(c: &Contract) -> Vec<String> {
    let mut errors = Vec::new();

    // 1. updateRules 的 key 必须与 FieldDef.path 一致(唯一事实源,防错位)
    let mut seen_paths: BTreeSet<&str> = BTreeSet::new();
    for (key, field) in &c.update_rules {
        if key != &field.path {
            errors.push(format!(
                "updateRules 键 {key} 与字段 path {} 不一致",
                field.path
            ));
        }
        if !seen_paths.insert(field.path.as_str()) {
            errors.push(format!("字段 path 重复: {}", field.path));
        }
    }

    // 2. updateMode 约束:every_n_turns 必须给 everyN ≥ 1
    for field in c.update_rules.values() {
        if field.update_mode == UpdateMode::EveryNTurns && field.every_n.is_none_or(|n| n == 0) {
            errors.push(format!(
                "字段 {} 为 every_n_turns 但未声明有效的 everyN(≥1)",
                field.path
            ));
        }
    }

    // 3. 护栏数值合法性(保守下限)
    if c.guardrails.max_ops_per_turn == 0 {
        errors.push("guardrails.maxOpsPerTurn 必须 ≥ 1".into());
    }

    // 4. 不变量引用的路径必须在 updateRules 中声明
    for inv in &c.invariants {
        for path in &inv.paths {
            if !c.update_rules.contains_key(path) {
                errors.push(format!("不变量 {} 引用了未声明的字段 {path}", inv.id));
            }
        }
    }

    // 5. 依赖环检测(拓扑序拒绝,§0.2:环 = 逻辑矛盾,静默打破会导致推理不一致)
    let adj = build_adjacency(c);
    if let Some(cycle) = detect_cycle(&adj) {
        errors.push(format!("dependencies cycle: {}", cycle.join("→")));
    }

    // 6. 字段路径前缀互斥:一个 path 是另一个的点分前缀时两者在树中互斥
    // (标量层与对象层不可同存,default 填充会相互践踏,属结构矛盾)
    let paths: Vec<&str> = c.update_rules.keys().map(|k| k.as_str()).collect();
    let is_prefix = |short: &str, long: &str| long.starts_with(&format!("{short}."));
    for (i, a) in paths.iter().enumerate() {
        for b in paths.iter().skip(i + 1) {
            if is_prefix(a, b) || is_prefix(b, a) {
                errors.push(format!("字段路径前缀冲突: {a} 与 {b} 互为祖先/后代"));
            }
        }
    }

    errors
}

/// 计算 due 字段的传递依赖(深度 ≤ guardrails.maxDependencyDepth)。
///
/// 返回 Map<due 字段, 其依赖列表>(不含自身,含直接与传递依赖)。
/// 依赖方向:A → B 表示「A 依赖 B」;BFS 从 due 出发沿边遍历。
/// 检测到环返回 Err(环路径)。
pub fn compute_dependencies(
    c: &Contract,
    due: &[String],
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let adj = build_adjacency(c);
    if let Some(cycle) = detect_cycle(&adj) {
        return Err(format!("dependencies cycle: {}", cycle.join("→")));
    }

    let max_depth = c.guardrails.max_dependency_depth as usize;
    let mut result: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for root in due {
        if !adj.contains_key(root) {
            continue;
        }
        let mut deps: BTreeSet<String> = BTreeSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        queue.push_back((root.clone(), 0));
        while let Some((node, depth)) = queue.pop_front() {
            if depth > max_depth {
                continue;
            }
            // depth=0 是根自身,不算依赖;depth≥1 为直接/传递依赖
            if depth > 0 {
                deps.insert(node.clone());
            }
            if depth >= max_depth {
                continue;
            }
            for dep in adj.get(&node).map(|v| v.as_slice()).unwrap_or(&[]) {
                queue.push_back((dep.clone(), depth + 1));
            }
        }
        result.insert(root.clone(), deps.into_iter().collect());
    }

    Ok(result)
}

/// 构建邻接表:A → [B...](A 依赖 B)。
/// 边来源:changeRule 的 ${path} 引用 + FieldDef.dependencies 显式声明(两源取并集)。
fn build_adjacency(c: &Contract) -> BTreeMap<String, Vec<String>> {
    let field_paths: BTreeSet<&String> = c.update_rules.keys().collect();
    let mut adj: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (path, field) in &c.update_rules {
        let mut deps: BTreeSet<String> = BTreeSet::new();

        // 静态分析 changeRule 的 ${field.path} 引用
        if let Some(rule) = &field.change_rule {
            for r in extract_path_refs(rule) {
                if &r != path && field_paths.contains(&r) {
                    deps.insert(r);
                }
            }
        }
        // 显式声明兜底(§0.2:changeRule 未提及但逻辑上依赖的字段)
        if let Some(explicit) = &field.dependencies {
            for d in explicit {
                if field_paths.contains(&d) {
                    deps.insert(d.clone());
                }
            }
        }

        adj.insert(path.clone(), deps.into_iter().collect());
    }
    adj
}

/// 提取自然语言规则里的 ${path} 引用(§0.2 静态分析)。
fn extract_path_refs(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"\$\{([^}]+)\}").expect("引用正则应合法");
    re.captures_iter(text)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 检测依赖环(DFS 三色标记),返回环路径(如 ["A","B","A"])或 None。
fn detect_cycle(adj: &BTreeMap<String, Vec<String>>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        White,
        Gray,
        Black,
    }

    fn visit(
        node: &str,
        adj: &BTreeMap<String, Vec<String>>,
        marks: &mut BTreeMap<String, Mark>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        marks.insert(node.to_string(), Mark::Gray);
        stack.push(node.to_string());
        for dep in adj.get(node).map(|v| v.as_slice()).unwrap_or(&[]) {
            match marks.get(dep) {
                Some(Mark::White) => {
                    if let Some(cycle) = visit(dep, adj, marks, stack) {
                        return Some(cycle);
                    }
                }
                Some(Mark::Gray) => {
                    let start = stack.iter().position(|x| x == dep).unwrap_or(0);
                    let mut cycle: Vec<String> = stack[start..].to_vec();
                    cycle.push(dep.clone());
                    return Some(cycle);
                }
                _ => {}
            }
        }
        marks.insert(node.to_string(), Mark::Black);
        stack.pop();
        None
    }

    let mut marks: BTreeMap<String, Mark> = adj.keys().map(|k| (k.clone(), Mark::White)).collect();
    let mut stack: Vec<String> = Vec::new();
    for node in adj.keys().cloned().collect::<Vec<_>>() {
        if marks[&node] == Mark::White {
            if let Some(cycle) = visit(&node, adj, &mut marks, &mut stack) {
                return Some(cycle);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::field::{FieldDef, FieldType, UpdateMode};
    use serde_json::json;

    fn field(path: &str, deps: Option<Vec<String>>, change_rule: Option<&str>) -> FieldDef {
        FieldDef {
            path: path.to_string(),
            kind: FieldType::Number,
            default: None,
            update_mode: UpdateMode::EveryTurn,
            every_n: None,
            dynamic: false,
            change_rule: change_rule.map(|s| s.to_string()),
            cap: None,
            scope: None,
            ttl: None,
            persist: Default::default(),
            stability: Default::default(),
            display: true,
            dependencies: deps,
            ownership: None,
        }
    }

    fn contract_with(fields: Vec<FieldDef>) -> Contract {
        let mut c: Contract = serde_json::from_value(json!({
            "version": 1,
            "id": "t",
            "schema": { "properties": {} },
            "updateRules": {},
            "guardrails": {}
        }))
        .unwrap();
        c.update_rules = fields.into_iter().map(|f| (f.path.clone(), f)).collect();
        c
    }

    /// ${path} 引用 + 显式依赖取并集,构成 A→B 依赖边。
    #[test]
    fn compute_dependencies_merges_sources() {
        let c = contract_with(vec![
            field("A", None, Some("读取 ${B} 与 ${C} 后决定")),
            field("B", None, None),
            field("C", None, None),
        ]);
        let due = vec!["A".to_string()];
        let deps = compute_dependencies(&c, &due).unwrap();
        let mut got = deps["A"].clone();
        got.sort();
        assert_eq!(got, vec!["B", "C"], "changeRule 引用应纳入依赖");
    }

    /// 深度 ≤ maxDependencyDepth(=3) 的传递依赖进入,更深的不进。
    #[test]
    fn compute_dependencies_respects_depth() {
        let c = contract_with(vec![
            field("A", Some(vec!["B".into()]), None),
            field("B", Some(vec!["C".into()]), None),
            field("C", Some(vec!["D".into()]), None),
            field("D", Some(vec!["E".into()]), None),
            field("E", None, None),
        ]);
        // 深度: B=1, C=2, D=3, E=4(超限,不进)
        let deps = compute_dependencies(&c, &["A".to_string()]).unwrap();
        let mut got = deps["A"].clone();
        got.sort();
        assert_eq!(got, vec!["B", "C", "D"], "E 深度 4 超限应被截断");
    }

    /// 环检测:validate_contract 报错并指明环路径。
    #[test]
    fn validate_contract_reports_cycle_path() {
        let c = contract_with(vec![
            field("A", Some(vec!["B".into()]), None),
            field("B", Some(vec!["A".into()]), None),
        ]);
        let errors = validate_contract(&c);
        assert!(
            errors.iter().any(|e| e.contains("dependencies cycle")),
            "应报依赖环: {errors:?}"
        );
    }

    /// updateRules 键与 path 不一致 → 报错(唯一事实源防错位)。
    #[test]
    fn validate_contract_reports_key_path_mismatch() {
        let mut c = contract_with(vec![field("A", None, None)]);
        let f = c.update_rules.remove("A").unwrap();
        c.update_rules.insert("WRONG_KEY".into(), f);
        let errors = validate_contract(&c);
        assert!(
            errors.iter().any(|e| e.contains("不一致")),
            "应报键与 path 不一致: {errors:?}"
        );
    }

    /// every_n_turns 缺 everyN → 报错。
    #[test]
    fn validate_contract_requires_every_n() {
        let mut f = field("A", None, None);
        f.update_mode = UpdateMode::EveryNTurns;
        f.every_n = None;
        let c = contract_with(vec![f]);
        let errors = validate_contract(&c);
        assert!(
            errors.iter().any(|e| e.contains("everyN")),
            "应报缺 everyN: {errors:?}"
        );
    }

    /// 前缀互斥:a 与 a.b 同存属结构矛盾(default 填充会相互践踏);
    /// 兄弟路径与相同路径(重复已另有检查)不受影响。
    #[test]
    fn prefix_conflict_rejected() {
        let c = contract_with(vec![field("a", None, None), field("a.b", None, None)]);
        let errors = validate_contract(&c);
        assert!(
            errors.iter().any(|e| e.contains("前缀冲突")),
            "应报前缀冲突: {errors:?}"
        );

        let ok = contract_with(vec![field("a.b", None, None), field("a.c", None, None)]);
        assert!(
            validate_contract(&ok).is_empty(),
            "兄弟路径不冲突: {:?}",
            validate_contract(&ok)
        );
    }
}
