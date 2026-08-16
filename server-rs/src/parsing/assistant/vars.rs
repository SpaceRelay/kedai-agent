// 酒馆助手变量树(stat_data)维护(L1 老层 · parsing/assistant):
// AssistantVars 结构体 + 点路径工具(split_path/path_get/path_set/set_at/remove_path/value_to_f64)
// 与 YAML 风格格式化(format_value/format_scalar)。
// 与前端 web/src/mvu/ 的约定保持一致:变量树根即 stat_data,点路径访问。
use serde_json::{json, Map, Value};

use super::ejs;
use super::patch::PatchOp;

// ===================== 变量树 =====================

/// 会话级酒馆助手变量树(根即 stat_data)
#[derive(Debug, Clone)]
pub struct AssistantVars {
    /// 变量树根;pub(super) 供同模块子文件(initvar 构造)访问,crate 外仅经 tree() 方法读取
    pub(super) tree: Value,
}

impl Default for AssistantVars {
    fn default() -> Self {
        Self::new()
    }
}

impl AssistantVars {
    pub fn new() -> Self {
        AssistantVars { tree: json!({}) }
    }

    pub fn from_value(v: Value) -> Self {
        AssistantVars { tree: v }
    }

    /// 从持久化 JSON 加载;解析失败返回空树
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s)
            .map(Self::from_value)
            .unwrap_or_else(|_| Self::new())
    }

    pub fn tree(&self) -> &Value {
        &self.tree
    }

    pub fn is_empty(&self) -> bool {
        self.tree.as_object().map(|m| m.is_empty()).unwrap_or(true)
    }

    pub fn to_json(&self) -> String {
        self.tree.to_string()
    }

    /// 按点路径取树中值(不存在返回 None)
    pub fn get_value(&self, path: &str) -> Option<&Value> {
        let segs = split_path(path);
        if segs.is_empty() {
            return Some(&self.tree);
        }
        path_get(&self.tree, &segs)
    }

    /// 按点路径写值(自动创建中间对象/数组)
    pub fn set(&mut self, path: &str, v: Value) {
        let segs = split_path(path);
        path_set(&mut self.tree, &segs, v);
    }

    /// 数值加 delta;目标非数字返回 Err;路径不存在按 0 起步创建
    pub fn add(&mut self, path: &str, delta: f64) -> Result<(), String> {
        let segs = split_path(path);
        let n = match path_get(&self.tree, &segs) {
            Some(cur) => value_to_f64(cur).ok_or_else(|| format!("变量不是数字: {path}"))?,
            None => 0.0,
        };
        path_set(&mut self.tree, &segs, json!(n + delta));
        Ok(())
    }

    /// EJS 解释器用:取值为 JsValue(undefined 表示不存在)
    pub(crate) fn get_js(&self, path: &str) -> ejs::JsValue {
        match self.get_value(path) {
            Some(v) => ejs::value_to_js(v),
            None => ejs::JsValue::Undefined,
        }
    }

    /// EJS 解释器用:写值
    pub(crate) fn set_js(&mut self, path: &str, v: ejs::JsValue) {
        self.set(path, ejs::js_to_json(&v));
    }

    /// EJS 解释器用:数值加(失败静默,保持原值)
    pub(crate) fn add_js(&mut self, path: &str, delta: f64) {
        let _ = self.add(path, delta);
    }

    /// 格式化输出({{format_message_variable}} / <StatusPlaceHolderImpl/>):
    /// YAML 风格缩进,字符串带双引号,数字/布尔裸输出。
    pub fn format(&self, path: Option<&str>) -> String {
        let v = match path {
            Some(p) => self.get_value(p).unwrap_or(&Value::Null),
            None => &self.tree,
        };
        let mut out = String::new();
        format_value(v, 0, &mut out);
        out
    }

    /// 应用 JSON Patch 操作序列(全部成功或首个失败即返回 Err,不做部分回滚)。
    /// 支持 op 集合:replace(含 insert,二者语义相同均为 set)/ delta / remove / move;
    /// Insert 已并入 Replace,此处分支不复存在。
    pub fn apply_patches(&mut self, ops: &[PatchOp]) -> Result<(), String> {
        for op in ops {
            match op {
                PatchOp::Replace { path, value, .. } => self.set(path, value.clone()),
                PatchOp::Delta { path, value, .. } => {
                    self.add(path, *value)
                        .map_err(|e| format!("delta 失败({path}): {e}"))?;
                }
                PatchOp::Remove { path, .. } => {
                    let segs = split_path(path);
                    remove_path(&mut self.tree, &segs);
                }
                PatchOp::Move { from, to, .. } => {
                    let segs = split_path(from);
                    // 与前端一致:源缺失时整条跳过(容错优先,不整批失败)
                    let Some(v) = path_get(&self.tree, &segs).cloned() else {
                        continue;
                    };
                    self.set(to, v);
                    remove_path(&mut self.tree, &segs);
                }
            }
        }
        Ok(())
    }
}

// ===================== 路径工具 =====================

/// 拆分变量路径:剥 stat_data 前缀;支持点分隔与斜杠分隔(/心之所向/好感度)。
/// 危险段(__proto__/constructor/prototype)被过滤,与前端 pathGet/pathSet 语义一致
/// (LLM 输出不可信,防御原型污染;serde_json::Value 无原型但保持路径行为一致)。
pub(crate) fn split_path(path: &str) -> Vec<String> {
    let mut p = path.trim();
    if p == "stat_data" {
        return Vec::new();
    }
    if let Some(rest) = p.strip_prefix("stat_data.") {
        p = rest;
    }
    let p = p.trim_start_matches('/').trim_start_matches('.');
    if p.is_empty() {
        return Vec::new();
    }
    let safe = |s: &str| {
        let t = s.trim();
        !t.is_empty() && !matches!(t, "__proto__" | "constructor" | "prototype")
    };
    if p.contains('/') {
        p.split('/')
            .map(|s| s.trim().to_string())
            .filter(|s| safe(s))
            .collect()
    } else {
        p.split('.')
            .map(|s| s.trim().to_string())
            .filter(|s| safe(s))
            .collect()
    }
}

pub(crate) fn path_get<'a>(mut cur: &'a Value, segs: &[String]) -> Option<&'a Value> {
    for s in segs {
        match cur {
            Value::Object(m) => cur = m.get(s)?,
            Value::Array(a) => {
                let idx: usize = s.parse().ok()?;
                cur = a.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(cur)
}

/// 按路径写值;中间标量自动替换为对象/数组(与前端 pathSet 语义一致)
pub(crate) fn path_set(root: &mut Value, segs: &[String], v: Value) {
    if segs.is_empty() {
        *root = v;
        return;
    }
    let mut cur = root;
    let mut idx = 0usize;
    while idx < segs.len() {
        let s = &segs[idx];
        let last = idx == segs.len() - 1;
        if last {
            set_at(cur, s, v);
            return;
        }
        let next_is_idx = segs[idx + 1].parse::<usize>().is_ok() || segs[idx + 1] == "-";
        if cur.is_object() {
            let m = cur.as_object_mut().unwrap();
            if !m.contains_key(s) {
                m.insert(s.clone(), if next_is_idx { json!([]) } else { json!({}) });
            }
            cur = m.get_mut(s).unwrap();
            idx += 1;
        } else if cur.is_array() {
            let idx_num: usize = s.parse().unwrap_or(0);
            let a = cur.as_array_mut().unwrap();
            while a.len() <= idx_num {
                a.push(Value::Null);
            }
            cur = &mut a[idx_num];
            idx += 1;
        } else {
            // 标量被中间路径穿越:替换为容器后重试本段
            *cur = if next_is_idx { json!([]) } else { json!({}) };
        }
    }
}

fn set_at(cur: &mut Value, key: &str, v: Value) {
    if key == "-" {
        // 数组追加
        if let Value::Array(a) = cur {
            a.push(v);
        } else {
            *cur = json!([v]);
        }
        return;
    }
    if let Value::Array(a) = cur {
        let idx: usize = key.parse().unwrap_or(0);
        while a.len() <= idx {
            a.push(Value::Null);
        }
        a[idx] = v;
        return;
    }
    if let Value::Object(m) = cur {
        m.insert(key.to_string(), v);
        return;
    }
    let mut m = Map::new();
    m.insert(key.to_string(), v);
    *cur = Value::Object(m);
}

fn remove_path(root: &mut Value, segs: &[String]) {
    if segs.is_empty() {
        return;
    }
    let mut cur = root;
    for s in &segs[..segs.len() - 1] {
        match cur {
            Value::Object(m) => match m.get_mut(s) {
                Some(v) => cur = v,
                None => return,
            },
            Value::Array(a) => {
                let idx: usize = s.parse().unwrap_or(0);
                match a.get_mut(idx) {
                    Some(v) => cur = v,
                    None => return,
                }
            }
            _ => return,
        }
    }
    let last = &segs[segs.len() - 1];
    match cur {
        Value::Object(m) => {
            m.remove(last);
        }
        Value::Array(a) => {
            if let Ok(idx) = last.parse::<usize>() {
                if idx < a.len() {
                    a.remove(idx);
                }
            }
        }
        _ => {}
    }
}

fn value_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Null => Some(0.0),
        _ => None,
    }
}

// ===================== 格式化 =====================

fn format_value(v: &Value, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                out.push_str(&pad);
                out.push_str(k);
                out.push_str(": ");
                match val {
                    Value::Object(_) | Value::Array(_) => {
                        out.push('\n');
                        format_value(val, indent + 1, out);
                    }
                    _ => {
                        out.push_str(&format_scalar(val));
                        out.push('\n');
                    }
                }
            }
        }
        Value::Array(a) => {
            for item in a {
                out.push_str(&pad);
                out.push_str("- ");
                match item {
                    Value::Object(_) | Value::Array(_) => {
                        out.push('\n');
                        format_value(item, indent + 1, out);
                    }
                    _ => {
                        out.push_str(&format_scalar(item));
                        out.push('\n');
                    }
                }
            }
        }
        _ => {
            out.push_str(&format_scalar(v));
            out.push('\n');
        }
    }
}

pub(super) fn format_scalar(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{s}\""),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
            Some(f) => format!("{f}"),
            None => n.to_string(),
        },
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".into(),
        _ => String::new(),
    }
}
