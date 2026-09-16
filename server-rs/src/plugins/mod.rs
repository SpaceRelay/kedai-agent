// 后端插件:自定义工具加载器
// 约定:data/plugins/tools/*.json 定义工具,格式:
// {
//   "name": "weather",                    // 工具名(唯一)
//   "description": "查询天气",
//   "parameters": { "type": "object", ... },   // JSON Schema(OpenAI 兼容)
//   "script": "if (args.city) { ... }",   // 白名单指令脚本
// }
// script 执行器为受控指令求值(算术/字符串/对象/数组/函数调用白名单),不使用 eval。
//
// ## 代际边界(L3 青层·活;2026-09-14 依赖倒置)
//
// 本模块是**纯解析器 + 白名单求值器**:它把 JSON 文件解析成工具定义、把脚本求值成字符串,
// **不持有工具注册表**。注册动作由宿主(组合根 `api/app_state.rs` / `api/plugins.rs`)完成。
//
// 为什么这样切分(三结合「隔离」判据):L3 不得依赖 L2,而工具注册表是 L2 骨干设施。
// 若本模块直接 `registry.register_external(...)`,就构成 `L3→L2` 越代依赖。
// 倒置后本模块只依赖 L1(`models::types::ToolDefinition`),注册由组合根装配
// ——这正是「青层能力经显式接缝注入」的标准形态(参照 `task_core::TaskBackend` 先例)。
use crate::models::types::ToolDefinition;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 工具插件定义(从 JSON 文件加载)
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ToolPluginConfig {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub script: String,
}

/// 已解析待注册的工具插件。
///
/// 宿主拿到它之后自行调用注册表登记(执行器由 [`plugin_executor`] 构造);
/// 本模块不参与注册,故不依赖任何 L2 设施。
#[derive(Debug, Clone)]
pub struct LoadedToolPlugin {
    /// 可直接交给工具注册表的定义(name/description/parameters)
    pub definition: ToolDefinition,
    /// 原始脚本正文(交给 [`plugin_executor`] 构造执行器)
    pub script: String,
}

pub struct ToolPluginLoader {
    dir: PathBuf,
}

impl ToolPluginLoader {
    pub fn new(dir: PathBuf) -> Self {
        ToolPluginLoader { dir }
    }

    /// 解析目录下全部工具插件(**不注册**);返回 (已解析列表, 失败列表)
    ///
    /// 宿主负责把返回的定义逐条注册进工具注册表——注册是 L2 装配动作,不属 L3 加载器职责。
    pub fn parse_all(&self) -> (Vec<LoadedToolPlugin>, Vec<String>) {
        let mut loaded = Vec::new();
        let mut errors = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return (loaded, errors);
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
            .collect();
        files.sort();
        for file in files {
            match self.parse_file(&file) {
                Ok(plugin) => loaded.push(plugin),
                Err(e) => errors.push(format!(
                    "{}: {e}",
                    file.file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default()
                )),
            }
        }
        (loaded, errors)
    }

    /// 解析单个工具插件文件(**不注册**;供导入 API 复用)
    pub fn parse_file(&self, file: &Path) -> Result<LoadedToolPlugin, String> {
        let raw = std::fs::read_to_string(file).map_err(|e| format!("读取失败: {e}"))?;
        let cfg: ToolPluginConfig =
            serde_json::from_str(&raw).map_err(|e| format!("JSON 解析失败: {e}"))?;
        if cfg.name.trim().is_empty() {
            return Err("name 不能为空".into());
        }
        if cfg.script.trim().is_empty() {
            return Err("script 不能为空".into());
        }
        // 名称格式校验（2026-09-14 安全加固，见 known-limitations L19）：
        // ① 只允许小写字母/数字/下划线，且首字符为字母——防注入怪异字符与不可见字符；
        // ② **不得占用保留前缀** `mcp_`（MCP 工具命名空间）与 `agent` 系内置域，
        //    否则插件可在授权裁决的「未知外部工具」语义上伪装成已知工具族。
        //    跨命名空间的名称伪造比单纯重名更隐蔽（重名由 register_plugins 拦），
        //    故在解析期就拒绝。
        validate_plugin_name(&cfg.name)?;
        Ok(LoadedToolPlugin {
            definition: ToolDefinition {
                name: cfg.name,
                description: cfg.description,
                parameters: cfg.parameters,
            },
            script: cfg.script,
        })
    }

    /// 列出已加载工具插件文件(供 API 展示)
    pub fn list_files(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".json"))
            .collect();
        names.sort();
        names
    }
}

/// 把已解析的插件构造成可直接注册的执行器（**纯构造，无 L2 依赖**）。
///
/// 宿主拿到返回值后自行调 `registrar.register_external(def, exec, None, ToolOrigin::Plugin)`。
/// 之所以把「构造」与「注册」分开，是为了让本模块（L3）不依赖工具注册表（L2）——
/// 这正是依赖倒置的落点。
pub fn plugin_executor(script: &str) -> crate::models::types::ToolExecutor {
    let script = script.to_string();
    std::sync::Arc::new(
        move |args: Value, _ctx| -> futures::future::BoxFuture<'static, Result<String, String>> {
            let script = script.clone();
            Box::pin(async move { eval_tool_script(&script, args) })
        },
    )
}

/// 插件工具名格式校验(2026-09-14;安全加固,见 `docs/遗留.md` L19)。
///
/// 规则:
/// - 非空、`^[a-z][a-z0-9_]*$`、长度 ≤ 48;
/// - 不得以保留前缀开头:`mcp_`(MCP 命名空间)、`agent`(内置 agent 工具域)。
///
/// 拒绝的理由不是「不好看」,而是**命名空间伪造**:授权裁决按工具名判定风险与来源,
/// 一个叫 `mcp_fs_read` 的插件会被误认为 MCP 工具。跨命名空间的伪造比重名更隐蔽,
/// 故在解析期即拒(重名另有 `register_plugins` 的内置名校验兜底)。
fn validate_plugin_name(name: &str) -> Result<(), String> {
    const MAX_LEN: usize = 48;
    const RESERVED_PREFIXES: &[&str] = &["mcp_", "agent"];
    if name.len() > MAX_LEN {
        return Err(format!("name 过长({} > {MAX_LEN})", name.len()));
    }
    let mut chars = name.chars();
    let first = chars.next().ok_or("name 不能为空")?;
    if !first.is_ascii_lowercase() {
        return Err("name 首字符须为小写字母".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err("name 只允许小写字母/数字/下划线".into());
    }
    for p in RESERVED_PREFIXES {
        if name.starts_with(p) {
            return Err(format!("name 不得使用保留前缀 `{p}`(防命名空间伪造)"));
        }
    }
    Ok(())
}

/// 白名单指令求值:支持字面量、数组/对象字面量、变量、对象属性访问、算术、
/// 三元、字符串方法(toUpperCase/toLowerCase/length/includes)、JSON 函数。
/// 入口约定:args 为参数对象;返回值作为工具输出字符串。
fn eval_tool_script(script: &str, args: Value) -> Result<String, String> {
    let mut env: HashMap<String, Value> = HashMap::new();
    env.insert("args".to_string(), args);
    // 预绑定常用全局
    let code = script.trim();
    if code.starts_with("return ") || code.starts_with("return(") {
        let expr = code.trim_start_matches("return").trim();
        let value = eval_expr(expr, &env)?;
        return value_to_string(&value);
    }
    // 支持 `result = ...` 形式
    if let Some(eq) = code.find('=') {
        let lhs = code[..eq].trim();
        let rhs = code[eq + 1..].trim();
        if lhs == "result" {
            let value = eval_expr(rhs, &env)?;
            return value_to_string(&value);
        }
    }
    Err("脚本须以 `return <表达式>` 或 `result = <表达式>` 开头".into())
}

fn value_to_string(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Ok(String::new()),
        other => serde_json::to_string(other).map_err(|e| format!("序列化失败: {e}")),
    }
}

/// 表达式求值(递归下降,白名单)
fn eval_expr(input: &str, env: &HashMap<String, Value>) -> Result<Value, String> {
    let t = input.trim();
    if t.is_empty() {
        return Err("空表达式".into());
    }
    // 1) 三元
    if let Some(q) = top_level_find(t, '?') {
        let colon = top_level_find(&t[q + 1..], ':');
        if let Some(c) = colon {
            let cond = eval_truthy(t[..q].trim(), env)?;
            let when_true = &t[q + 1..q + 1 + c].trim();
            let when_false = &t[q + 1 + c + 1..].trim();
            return if cond {
                eval_expr(when_true, env)
            } else {
                eval_expr(when_false, env)
            };
        }
    }
    // 2) 字符串字面量
    if (t.starts_with('"') && t.ends_with('"') && t.len() >= 2)
        || (t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2)
    {
        return Ok(Value::String(t[1..t.len() - 1].to_string()));
    }
    // 3) 数字 / 布尔 / null
    if let Ok(n) = t.parse::<i64>() {
        return Ok(Value::from(n));
    }
    if let Ok(f) = t.parse::<f64>() {
        return Ok(Value::from(f));
    }
    if t == "true" {
        return Ok(Value::Bool(true));
    }
    if t == "false" {
        return Ok(Value::Bool(false));
    }
    if t == "null" || t == "undefined" {
        return Ok(Value::Null);
    }
    // 4) JSON 对象字面量
    if t.starts_with('{') && t.ends_with('}') {
        let inner = &t[1..t.len() - 1];
        if inner.trim().is_empty() {
            return Ok(Value::Object(Default::default()));
        }
        return parse_object(inner, env);
    }
    // 5) JSON 数组字面量
    if t.starts_with('[') && t.ends_with(']') {
        let inner = &t[1..t.len() - 1];
        if inner.trim().is_empty() {
            return Ok(Value::Array(Vec::new()));
        }
        return parse_array(inner, env);
    }
    // 6) 函数调用(白名单)
    if let Some(open) = top_level_find(t, '(') {
        let fn_name = t[..open].trim();
        let args_str = &t[open + 1..t.len() - 1];
        return eval_call(fn_name, args_str, env);
    }
    // 7) 成员访问 / 变量(含函数调用形式已在 6 处理)
    if let Some(dot) = t.find('.') {
        let base = t[..dot].trim();
        let member = t[dot + 1..].trim();
        // 属性 + 可能的调用
        let base_val = eval_expr(base, env)?;
        return eval_member(base_val, member, env);
    }
    // 8) 裸变量(args / args.x 已在 7 覆盖)
    if let Some(v) = env.get(t) {
        return Ok(v.clone());
    }
    // 9) 简单算术(a + b / a - b ...)
    if let Some(v) = eval_binary_arith(t, env)? {
        return Ok(v);
    }
    Err(format!("无法求值表达式: {t}"))
}

/// 成员访问:支持 .length / .toUpperCase() / .toLowerCase() / .includes(x) / .x 属性
fn eval_member(base: Value, member: &str, env: &HashMap<String, Value>) -> Result<Value, String> {
    let m = member.trim();
    // 方法调用
    if let Some(open) = m.find('(') {
        let name = m[..open].trim();
        let args_str = m[open + 1..].trim_end_matches(')').trim();
        let args = parse_args(args_str, env)?;
        return apply_method(base, name, &args);
    }
    // 属性
    match (base, m) {
        (Value::String(s), "length") => Ok(Value::from(s.len())),
        (Value::Array(a), "length") => Ok(Value::from(a.len())),
        (Value::Object(o), key) => Ok(o.get(key).cloned().unwrap_or(Value::Null)),
        (Value::Array(a), idx) => {
            if let Ok(i) = idx.parse::<usize>() {
                Ok(a.get(i).cloned().unwrap_or(Value::Null))
            } else {
                Err(format!("非法数组索引: {idx}"))
            }
        }
        (other, key) => Err(format!("{other:?} 上无属性 {key}")),
    }
}

fn apply_method(base: Value, name: &str, args: &[Value]) -> Result<Value, String> {
    match (base, name) {
        (Value::String(s), "toUpperCase") => Ok(Value::String(s.to_uppercase())),
        (Value::String(s), "toLowerCase") => Ok(Value::String(s.to_lowercase())),
        (Value::String(s), "includes") => {
            let needle = args.first().and_then(|a| a.as_str()).unwrap_or("");
            Ok(Value::Bool(s.contains(needle)))
        }
        (Value::String(s), "trim") => Ok(Value::String(s.trim().to_string())),
        (Value::String(_), "replace") => {
            // 简单字符串替换:replace(旧, 新)
            if args.len() >= 2 {
                let old = args[0].as_str().unwrap_or("");
                let new = args[1].as_str().unwrap_or("");
                let src = match args.get(2) {
                    Some(Value::String(s0)) => s0.clone(),
                    _ => return Err("replace 需传 (source, old, new)".into()),
                };
                return Ok(Value::String(src.replace(old, new)));
            }
            Err("replace 参数不足".into())
        }
        (Value::Array(a), "length") => Ok(Value::from(a.len())),
        (other, n) => Err(format!("对象 {other:?} 不支持方法 {n}")),
    }
}

fn eval_call(name: &str, args_str: &str, env: &HashMap<String, Value>) -> Result<Value, String> {
    let args = parse_args(args_str, env)?;
    match name {
        "JSON.stringify" => {
            let v = args.first().ok_or("JSON.stringify 缺参数")?;
            Ok(Value::String(
                serde_json::to_string(v).map_err(|e| format!("序列化失败: {e}"))?,
            ))
        }
        "JSON.parse" => {
            let s = args
                .first()
                .and_then(|a| a.as_str())
                .ok_or("JSON.parse 缺参数")?;
            serde_json::from_str(s).map_err(|e| format!("JSON 解析失败: {e}"))
        }
        "String" => {
            let v = args.first().ok_or("String() 缺参数")?;
            Ok(Value::String(value_to_string(v)?))
        }
        "Number" => {
            let v = args.first().ok_or("Number() 缺参数")?;
            match v {
                Value::Number(n) => Ok(Value::from(n.as_f64().unwrap_or(0.0))),
                // 有意丢弃 ParseFloatError:唯一信息是「不是数字」,原值 s 已在消息中
                Value::String(s) => s
                    .parse::<f64>()
                    .map(Value::from)
                    .map_err(|_| format!("无法转为数字: {s}")),
                _ => Ok(Value::Null),
            }
        }
        "Math.max" | "Math.min" => {
            if args.is_empty() {
                return Err("Math.max/min 缺参数".into());
            }
            let nums: Vec<f64> = args
                .iter()
                .map(|a| a.as_f64().unwrap_or(f64::NAN))
                .collect();
            let v = if name == "Math.max" {
                nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
            } else {
                nums.iter().cloned().fold(f64::INFINITY, f64::min)
            };
            Ok(Value::from(v))
        }
        "Math.floor" | "Math.ceil" | "Math.round" => {
            let v = args.first().and_then(|a| a.as_f64()).ok_or("缺数字参数")?;
            let r = match name {
                "Math.floor" => v.floor(),
                "Math.ceil" => v.ceil(),
                _ => v.round(),
            };
            Ok(Value::from(r))
        }
        _ => Err(format!("未支持函数: {name}")),
    }
}

fn parse_args(args_str: &str, env: &HashMap<String, Value>) -> Result<Vec<Value>, String> {
    let parts = split_top_level(args_str);
    let mut out = Vec::new();
    for p in parts {
        if p.trim().is_empty() {
            continue;
        }
        out.push(eval_expr(&p, env)?);
    }
    Ok(out)
}

fn parse_object(inner: &str, env: &HashMap<String, Value>) -> Result<Value, String> {
    let parts = split_top_level(inner);
    let mut map = serde_json::Map::new();
    for p in parts {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        let colon = p.find(':').ok_or(format!("非法对象键值对: {p}"))?;
        let key = p[..colon].trim().trim_matches('"').trim_matches('\'');
        let val = eval_expr(&p[colon + 1..], env)?;
        map.insert(key.to_string(), val);
    }
    Ok(Value::Object(map))
}

fn parse_array(inner: &str, env: &HashMap<String, Value>) -> Result<Value, String> {
    let parts = split_top_level(inner);
    let mut arr = Vec::new();
    for p in parts {
        if p.trim().is_empty() {
            continue;
        }
        arr.push(eval_expr(&p, env)?);
    }
    Ok(Value::Array(arr))
}

fn eval_truthy(expr: &str, env: &HashMap<String, Value>) -> Result<bool, String> {
    // 比较运算:== != > < >= <=
    for op in [">=", "<=", "==", "!=", ">", "<"] {
        if let Some(pos) = top_level_op_find(expr, op) {
            let l = eval_expr(expr[..pos].trim(), env)?;
            let r = eval_expr(expr[pos + op.len()..].trim(), env)?;
            return Ok(compare_values(&l, &r, op));
        }
    }
    // 逻辑 && / ||
    if let Some(pos) = top_level_op_find(expr, "||") {
        let l = eval_truthy(expr[..pos].trim(), env)?;
        if l {
            return Ok(true);
        }
        return eval_truthy(expr[pos + 2..].trim(), env);
    }
    if let Some(pos) = top_level_op_find(expr, "&&") {
        let l = eval_truthy(expr[..pos].trim(), env)?;
        if !l {
            return Ok(false);
        }
        return eval_truthy(expr[pos + 2..].trim(), env);
    }
    let v = eval_expr(expr, env)?;
    Ok(match v {
        Value::Bool(b) => b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    })
}

fn compare_values(l: &Value, r: &Value, op: &str) -> bool {
    let lf = l.as_f64().unwrap_or(f64::NAN);
    let rf = r.as_f64().unwrap_or(f64::NAN);
    match op {
        "==" => !lf.is_nan() && !rf.is_nan() && (lf == rf) || (l == r),
        "!=" => !(l == r),
        ">" => !lf.is_nan() && !rf.is_nan() && lf > rf,
        "<" => !lf.is_nan() && !rf.is_nan() && lf < rf,
        ">=" => !lf.is_nan() && !rf.is_nan() && lf >= rf,
        "<=" => !lf.is_nan() && !rf.is_nan() && lf <= rf,
        _ => false,
    }
}

/// 简单二元算术
fn eval_binary_arith(t: &str, env: &HashMap<String, Value>) -> Result<Option<Value>, String> {
    for op in ["+", "-", "*", "/", "%"] {
        if let Some(pos) = top_level_op_find(t, op) {
            let l = eval_expr(t[..pos].trim(), env)?;
            let r = eval_expr(t[pos + op.len()..].trim(), env)?;
            // 字符串 + 字符串 → 拼接
            if op == "+" {
                if let (Some(a), Some(b)) = (l.as_str(), r.as_str()) {
                    return Ok(Some(Value::String(format!("{a}{b}"))));
                }
            }
            let lf = l.as_f64().ok_or("左操作数非数字")?;
            let rf = r.as_f64().ok_or("右操作数非数字")?;
            let v = match op {
                "+" => lf + rf,
                "-" => lf - rf,
                "*" => lf * rf,
                "/" => {
                    if rf == 0.0 {
                        return Err("除零".into());
                    }
                    lf / rf
                }
                "%" => lf % rf,
                _ => unreachable!(),
            };
            return Ok(Some(Value::from(v)));
        }
    }
    Ok(None)
}

/// 顶层找字符(忽略引号与括号嵌套)
/// 注意:必须先判断目标字符再更新括号深度——否则遇到 '(' 时先 depth+=1,
/// `c == ch && depth == 0` 永远不成立,函数调用分支(JSON.stringify/Math.* 等)会不可达。
fn top_level_find(s: &str, ch: char) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        if c == ch && depth == 0 {
            return Some(i);
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// 顶层找运算符(避免 `>` 与 `>=` 混淆:匹配最长优先由调用方排序)
fn top_level_op_find(s: &str, op: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        if depth == 0 && s[i..].starts_with(op) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 顶层逗号切分
fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(s[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(s[start..].to_string());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn eval(script: &str, args: Value) -> Result<String, String> {
        eval_tool_script(script, args)
    }

    /// 回归:top_level_find 必须先判断目标字符再更新括号深度,
    /// 否则 '(' 永远匹配不到,函数调用分支(JSON.stringify/Math.*/String/Number)不可达。
    #[test]
    fn tool_plugin_function_call_works() {
        // Math.max/min 返回 f64,JSON 序列化带 .0
        assert_eq!(
            eval(
                "result = Math.max(args.min, Math.min(args.max, args.value))",
                json!({"value": 150, "min": 0, "max": 100})
            )
            .unwrap(),
            "100.0"
        );
        assert_eq!(
            eval(
                "result = Math.max(args.min, Math.min(args.max, args.value))",
                json!({"value": -5, "min": 0, "max": 100})
            )
            .unwrap(),
            "0.0"
        );
        assert_eq!(
            eval("return JSON.stringify(args.text)", json!({"text": "你好"})).unwrap(),
            "\"你好\""
        );
        assert_eq!(
            eval("return String(Math.floor(args.n))", json!({"n": 3.7})).unwrap(),
            "3.0"
        );
    }

    /// 三元 + 对象字面量返回(score_eval 示范插件的脚本)
    #[test]
    fn tool_plugin_ternary_object_works() {
        let script = r#"result = args.score >= args.pass ? {"passed": true, "grade": "合格"} : {"passed": false, "grade": "待改进"}"#;
        assert!(eval(script, json!({"score": 85, "pass": 60}))
            .unwrap()
            .contains("合格"));
        assert!(eval(script, json!({"score": 40, "pass": 60}))
            .unwrap()
            .contains("待改进"));
    }

    /// 名称格式校验(安全加固,known-limitations L19):
    /// 合法名通过;**保留前缀/非法字符/大写/超长**一律拒绝。
    ///
    /// 这条约束的意义是防**命名空间伪造**:授权裁决按工具名判风险与来源,
    /// 若插件能叫 `mcp_fs_read`,它就会被误当作 MCP 工具。
    #[test]
    fn plugin_name_validation_rejects_reserved_and_invalid_names() {
        // 合法:小写字母开头 + 小写/数字/下划线
        for ok in ["weather", "score_eval", "my_tool_2", "a"] {
            assert!(
                validate_plugin_name(ok).is_ok(),
                "应接受合法名: {ok} → {:?}",
                validate_plugin_name(ok)
            );
        }
        // 保留前缀:防命名空间伪造
        assert!(
            validate_plugin_name("mcp_fs_read").is_err(),
            "不得冒用 mcp_ 前缀"
        );
        assert!(validate_plugin_name("mcp_x").is_err());
        assert!(
            validate_plugin_name("agentgo").is_err(),
            "不得冒用 agent 域前缀"
        );
        // 非法字符 / 大写 / 首字符非字母
        assert!(validate_plugin_name("").is_err());
        assert!(validate_plugin_name("Weather").is_err(), "大写应拒绝");
        assert!(validate_plugin_name("1tool").is_err(), "数字开头应拒绝");
        assert!(validate_plugin_name("my-tool").is_err(), "连字符应拒绝");
        assert!(validate_plugin_name("my tool").is_err(), "空格应拒绝");
        assert!(validate_plugin_name("工具").is_err(), "非 ASCII 应拒绝");
        assert!(
            validate_plugin_name("../evil").is_err(),
            "路径穿越样式应拒绝"
        );
        // 超长
        assert!(validate_plugin_name(&"a".repeat(49)).is_err(), "超长应拒绝");
        assert!(validate_plugin_name(&"a".repeat(48)).is_ok(), "48 恰好合法");
    }
}
