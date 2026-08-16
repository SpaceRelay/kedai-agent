// [InitVar] 初始变量收集(L1 老层 · parsing/assistant):
// 世界书条目 comment 含 [InitVar]/[InitialVariables] 的初始变量收集与深度合并;
// 内容支持 _.set(...) 语句 / 整体 JSON / YAML 风格缩进(兼容 ST-Prompt-Template)。
use serde_json::{json, Map, Value};

use crate::parsing::world_book::WorldEntry;

use super::patch::{parse_set_statements, PatchOp};
use super::vars::{path_set, split_path, AssistantVars};

// ===================== [InitVar] 初始变量 =====================

/// 从世界书条目(角色卡内嵌 + 独立世界书)收集 [InitVar] 初始变量。
/// 与前端 initvar.ts 约定一致:只看 comment 含 [InitVar] 的条目(不要求 enabled),
/// 条目内容支持 _.set(...) 语句 / 整体 JSON / YAML 风格缩进;按条目顺序深度合并。
/// 兼容 ST-Prompt-Template 的 [InitialVariables] 标签(与 [InitVar] 语义一致)。
pub fn collect_init_vars(entries: &[WorldEntry]) -> AssistantVars {
    let mut tree = json!({});
    for e in entries {
        let comment_upper = e.comment.to_uppercase();
        let is_init_tag = comment_upper.contains("[INITVAR]")
            || comment_upper.contains("[INITIALVARIABLES]");
        if !is_init_tag || e.content.trim().is_empty() {
            continue;
        }
        if let Some(p) = parse_init_var_content(&e.content) {
            merge_deep(&mut tree, &p);
        }
    }
    AssistantVars { tree }
}

fn parse_init_var_content(content: &str) -> Option<Value> {
    // 1. _.set(...) 语句序列
    let ops = parse_set_statements(content);
    if !ops.is_empty() {
        let mut tree = json!({});
        for op in ops {
            if let PatchOp::Replace { path, value, .. } = op {
                path_set(&mut tree, &split_path(&path), value);
            }
        }
        return Some(tree);
    }
    // 2. 整体 JSON 对象
    if let Ok(v) = serde_json::from_str::<Value>(content) {
        if v.is_object() {
            return Some(v);
        }
    }
    // 3. YAML 风格缩进键值对(社区 [InitVar] 常见格式)
    let y = parse_indented_yaml(content);
    if y.as_object().map(|m| !m.is_empty()).unwrap_or(false) {
        Some(y)
    } else {
        None
    }
}

/// 解析 YAML 风格缩进键值对(字符串/数字/布尔/null;忽略注释与列表)
fn parse_indented_yaml(content: &str) -> Value {
    let lines: Vec<(usize, String)> = content
        .split('\n')
        .map(|l| l.replace('\t', "  "))
        .map(|l| {
            let indent = l.chars().take_while(|c| *c == ' ').count();
            (indent, l.trim().to_string())
        })
        .filter(|(_, raw)| !raw.is_empty() && !raw.starts_with('#'))
        .collect();
    if lines.is_empty() {
        return json!({});
    }
    let mut idx = 0usize;
    let root_indent = lines[0].0;
    Value::Object(parse_yaml_block(&lines, &mut idx, root_indent))
}

/// 递归解析同一缩进层的键值对;更深缩进作为子对象递归
fn parse_yaml_block(
    lines: &[(usize, String)],
    idx: &mut usize,
    indent: usize,
) -> Map<String, Value> {
    let mut obj = Map::new();
    while *idx < lines.len() {
        let (ind, raw) = &lines[*idx];
        if *ind < indent {
            break; // 缩进变浅,返回上层
        }
        if *ind > indent {
            // 防御:深层行应由递归消费,跳过避免死循环
            *idx += 1;
            continue;
        }
        let Some(colon) = raw.find(':') else {
            *idx += 1;
            continue;
        };
        if colon == 0 {
            *idx += 1;
            continue;
        }
        let key = raw[..colon]
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        let value_raw = raw[colon + 1..].trim().to_string();
        *idx += 1;
        if value_raw.is_empty() {
            // 子节点:下一行缩进更深则递归
            if *idx < lines.len() && lines[*idx].0 > indent {
                let child_indent = lines[*idx].0;
                let child = parse_yaml_block(lines, idx, child_indent);
                obj.insert(key, Value::Object(child));
            } else {
                obj.insert(key, Value::Object(Map::new()));
            }
        } else {
            obj.insert(key, parse_yaml_scalar(&value_raw));
        }
    }
    obj
}

fn parse_yaml_scalar(raw: &str) -> Value {
    let t = raw.trim();
    if (t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')) {
        return Value::String(t[1..t.len() - 1].to_string());
    }
    if t == "true" {
        return json!(true);
    }
    if t == "false" {
        return json!(false);
    }
    if t == "null" || t == "~" {
        return Value::Null;
    }
    if let Ok(n) = t.parse::<i64>() {
        return json!(n);
    }
    if let Ok(f) = t.parse::<f64>() {
        return json!(f);
    }
    Value::String(t.to_string())
}

/// 深度合并(对象递归,标量/数组覆盖)
fn merge_deep(target: &mut Value, patch: &Value) {
    if let (Value::Object(t), Value::Object(p)) = (target, patch) {
        for (k, v) in p {
            match t.get_mut(k) {
                Some(tv) if tv.is_object() && v.is_object() => merge_deep(tv, v),
                _ => {
                    t.insert(k.clone(), v.clone());
                }
            }
        }
    }
}
