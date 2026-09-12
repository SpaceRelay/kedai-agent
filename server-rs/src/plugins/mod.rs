// 后端插件:自定义工具加载器
// 约定:data/plugins/tools/*.json 定义工具,格式:
// {
//   "name": "weather",                    // 工具名(唯一)
//   "description": "查询天气",
//   "parameters": { "type": "object", ... },   // JSON Schema(OpenAI 兼容)
//   "script": "if (args.city) { ... }",   // 白名单指令脚本
// }
// script 执行器为受控指令求值(算术/字符串/对象/数组/函数调用白名单),不使用 eval。
use crate::models::types::ToolDefinition;
use crate::tools::registry::ToolRegistry;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

pub struct ToolPluginLoader {
    dir: PathBuf,
}

impl ToolPluginLoader {
    pub fn new(dir: PathBuf) -> Self {
        ToolPluginLoader { dir }
    }

    /// 加载目录下全部工具插件并注册;返回 (加载数, 失败列表)
    pub fn load_all(&self, registry: &ToolRegistry) -> (usize, Vec<String>) {
        let mut count = 0;
        let mut errors = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return (0, errors);
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
            .collect();
        files.sort();
        for file in files {
            match self.load_file(&file, registry) {
                Ok(()) => count += 1,
                Err(e) => errors.push(format!(
                    "{}: {e}",
                    file.file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default()
                )),
            }
        }
        (count, errors)
    }

    /// 加载单个工具插件文件并注册(供导入 API 复用)
    pub fn load_file(&self, file: &Path, registry: &ToolRegistry) -> Result<(), String> {
        let raw = std::fs::read_to_string(file).map_err(|e| format!("读取失败: {e}"))?;
        let cfg: ToolPluginConfig =
            serde_json::from_str(&raw).map_err(|e| format!("JSON 解析失败: {e}"))?;
        if cfg.name.trim().is_empty() {
            return Err("name 不能为空".into());
        }
        if cfg.script.trim().is_empty() {
            return Err("script 不能为空".into());
        }
        let script = cfg.script.clone();
        let definition = ToolDefinition {
            name: cfg.name.clone(),
            description: cfg.description,
            parameters: cfg.parameters,
        };
        // 插件工具标记为外部来源:三档授权模式据此认定其参数不可信(可能含系统路径)
        registry.register_external(
            definition,
            Arc::new(move |args: Value, _ctx| -> futures::future::BoxFuture<'static, Result<String, String>> {
                let script = script.clone();
                Box::pin(async move { eval_tool_script(&script, args) })
            }),
            None,
            crate::tools::action_class::ToolOrigin::Plugin,
        );
        Ok(())
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
}
