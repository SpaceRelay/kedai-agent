// slash 命令注册表(阶段四 4a):命令注册/查询/执行 + 参数解析。
// 服务两条链路:
//   - 脚本侧:TavernHelper.triggerSlash → EvalBridge.trigger_slash → registry.execute
//     (bridge 的 SlashHandler 自定义入口优先,注册表次之,内置最小实现兜底);
//   - API 侧:GET /api/slash/commands 返回清单(前端输入框联想)。
// 设计约束:
//   - 不 eval;命令 handler 为纯 Rust 闭包,签名 (args, &mut ScopeVars) -> String;
//   - /help 由 execute 特判(避免闭包自引用注册表),list() 以虚拟命令补入清单;
//   - 未知命令返回 None,由调用方(bridge)回退内置实现或错误提示;
//   - 会话副作用 slash(/clear 等)与生成类 slash(/continue 等,计划六)不实现,留扩展位。
use crate::parsing::scopes::{Scope, ScopeVars};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// 命令清单元信息(API 与前端联想用;name 不含前导 /)
#[derive(Debug, Clone, Serialize)]
pub struct SlashCommandMeta {
    pub name: String,
    pub description: String,
    pub params: String,
}

/// 命令定义:handler 接收「命令名之后的原始参数」与当前作用域容器。
/// 参数原文保留(引号/`::` 的还原由各 handler 经 parse_args 按需处理)。
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub params: String,
    pub handler: Box<dyn Fn(&str, &mut ScopeVars) -> String + Send + Sync>,
}

/// 全局 slash 命令注册表(engine 与 AppState 共享同一实例)。
pub struct SlashRegistry {
    commands: RwLock<HashMap<String, Arc<SlashCommand>>>,
}

impl SlashRegistry {
    /// 构建注册表并注册内置命令(/echo /var /setvar /getvar /addvar;
    /// /help 为虚拟命令,execute 特判、list 补入)。
    pub fn new() -> Arc<Self> {
        let registry = Arc::new(SlashRegistry {
            commands: RwLock::new(HashMap::new()),
        });
        registry.register(SlashCommand {
            name: "echo".into(),
            description: "原样回显参数".into(),
            params: "<text>".into(),
            handler: Box::new(cmd_echo),
        });
        registry.register(SlashCommand {
            name: "var".into(),
            description: "写入 global 作用域变量".into(),
            params: "<key> :: <value>".into(),
            handler: Box::new(cmd_set_var),
        });
        registry.register(SlashCommand {
            name: "setvar".into(),
            description: "/var 的别名".into(),
            params: "<key> :: <value>".into(),
            handler: Box::new(cmd_set_var),
        });
        registry.register(SlashCommand {
            name: "getvar".into(),
            description: "读取变量(合并视图)".into(),
            params: "<key>".into(),
            handler: Box::new(cmd_get_var),
        });
        registry.register(SlashCommand {
            name: "addvar".into(),
            description: "对 global 数值变量累加".into(),
            params: "<key> :: <数值>".into(),
            handler: Box::new(cmd_add_var),
        });
        registry
    }

    /// 注册自定义命令(计划六生成类命令走同一接口)。
    pub fn register(&self, command: SlashCommand) {
        self.commands
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(command.name.clone(), Arc::new(command));
    }

    /// 命令清单(含虚拟 /help),按名称排序。
    pub fn list(&self) -> Vec<SlashCommandMeta> {
        let mut out: Vec<SlashCommandMeta> = {
            let guard = self.commands.read().unwrap_or_else(|e| e.into_inner());
            guard
                .values()
                .map(|c| SlashCommandMeta {
                    name: c.name.clone(),
                    description: c.description.clone(),
                    params: c.params.clone(),
                })
                .collect()
        };
        out.push(SlashCommandMeta {
            name: "help".into(),
            description: "列出全部 Slash 命令与用法".into(),
            params: String::new(),
        });
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// 执行命令:命中返回 Some(结果字符串),未知命令返回 None(调用方兜底)。
    /// 命令名去前导 /;参数为命令名后的原始文本。
    pub fn execute(&self, raw: &str, vars: &mut ScopeVars) -> Option<String> {
        let cmd = raw.trim();
        if cmd.is_empty() {
            return Some(String::new());
        }
        let (name, args) = match cmd.find(char::is_whitespace) {
            Some(i) => (&cmd[..i], cmd[i..].trim()),
            None => (cmd, ""),
        };
        let name = name.trim_start_matches('/');
        if name == "help" {
            return Some(self.help_text());
        }
        let command = self
            .commands
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned();
        command.map(|c| (c.handler)(args, vars))
    }

    /// /help 正文:逐行列出命令名 + 参数 + 说明。
    fn help_text(&self) -> String {
        let mut lines = vec!["可用 Slash 命令:".to_string()];
        for m in self.list() {
            let params = if m.params.is_empty() {
                String::new()
            } else {
                format!(" {}", m.params)
            };
            lines.push(format!("/{}{} — {}", m.name, params, m.description));
        }
        lines.join("\n")
    }
}

// ===================== 内置命令 handler =====================

/// /echo:原样回显参数。
fn cmd_echo(args: &str, _vars: &mut ScopeVars) -> String {
    args.to_string()
}

/// /var|/setvar:写单个键进 global 作用域(合并写入,不覆盖其他键)。
fn cmd_set_var(args: &str, vars: &mut ScopeVars) -> String {
    let (key, value) = split_key_value(&parse_args(args));
    if key.is_empty() {
        return "/var 用法: /var <key> :: <value>(写入 global 作用域)".into();
    }
    write_global_key(vars, &key, json!(value));
    format!("已设置 {key}")
}

/// /getvar:合并视图读取(优先级 message→chat→character→preset→global),
/// 与 bridge 内置实现一致:字符串值原样返回,其余 JSON 化,查无返回空串。
fn cmd_get_var(args: &str, vars: &mut ScopeVars) -> String {
    let key = args.trim();
    if key.is_empty() {
        return "/getvar 用法: /getvar <key>".into();
    }
    vars.view(key)
        .map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string()))
        .unwrap_or_default()
}

/// /addvar:对 global 数字键累加(键不存在时初始化为加数)。
fn cmd_add_var(args: &str, vars: &mut ScopeVars) -> String {
    let (key, delta_s) = split_key_value(&parse_args(args));
    if key.is_empty() {
        return "/addvar 用法: /addvar <key> :: <数值>(global 数值累加)".into();
    }
    let delta: f64 = match delta_s.trim().parse() {
        Ok(n) => n,
        Err(_) => return format!("/addvar: 「{delta_s}」不是有效数值"),
    };
    let current = match vars.view(&key) {
        // /var 写入的是字符串,先尝试数值化再累加
        Some(v) => v
            .as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
            .unwrap_or(0.0),
        None => 0.0,
    };
    write_global_key(vars, &key, json!(current + delta));
    format!("{key} = {}", current + delta)
}

/// 把键值写入 global 作用域(合并语义:读现有对象、插入键、整体写回)。
fn write_global_key(vars: &mut ScopeVars, key: &str, value: Value) {
    let mut obj = vars
        .scope_data(Scope::Global, "")
        .cloned()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}));
    if let Some(o) = obj.as_object_mut() {
        o.insert(key.to_string(), value);
    }
    vars.with_scope(Scope::Global, "", obj);
}

// ===================== 参数解析(纯函数,handler 与测试复用) =====================

/// 拆分参数串:按空白分词;单/双引号包裹段视为整体;`::` 独立成 token。
/// 例:"key :: hello world" → ["key", "::", "hello", "world"];
///     "a b" :: "c d"     → ["a b", "::", "c d"]。
pub fn parse_args(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                cur.push(c);
            }
            i += 1;
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                i += 1;
            }
            ':' if i + 1 < chars.len() && chars[i + 1] == ':' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push("::".to_string());
                i += 2;
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                i += 1;
            }
            _ => {
                cur.push(c);
                i += 1;
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 从解析后的 tokens 提取 "key :: value"(或 "key = value");无分隔符时整个视为 key。
/// 分隔符两侧相邻 token 拼接回原值(引号包裹段已还原)。
pub fn split_key_value(tokens: &[String]) -> (String, String) {
    let pos = tokens.iter().position(|t| t == "::" || t == "=");
    match pos {
        Some(i) => {
            let key = tokens[..i].join(" ").trim().to_string();
            let value = tokens[i + 1..].join(" ").trim().to_string();
            (key, value)
        }
        None => (tokens.join(" ").trim().to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> Arc<SlashRegistry> {
        SlashRegistry::new()
    }

    /// 便捷执行:注册表 + 新变量容器。
    fn run(registry: &SlashRegistry, raw: &str) -> Option<String> {
        registry.execute(raw, &mut ScopeVars::new())
    }

    // ---------- parse_args ----------

    #[test]
    fn parse_args_splits_whitespace_and_double_colon() {
        assert_eq!(
            parse_args("key :: hello world"),
            vec!["key".to_string(), "::".to_string(), "hello".to_string(), "world".to_string()]
        );
    }

    #[test]
    fn parse_args_keeps_quoted_segments_together() {
        assert_eq!(
            parse_args("\"a b\" :: 'c d'"),
            vec!["a b".to_string(), "::".to_string(), "c d".to_string()]
        );
    }

    #[test]
    fn parse_args_handles_empty_and_equals() {
        assert!(parse_args("").is_empty());
        assert_eq!(
            parse_args("key = value"),
            vec!["key".to_string(), "=".to_string(), "value".to_string()]
        );
    }

    #[test]
    fn split_key_value_handles_missing_separator() {
        assert_eq!(split_key_value(&parse_args("justkey")), ("justkey".into(), String::new()));
        assert_eq!(split_key_value(&parse_args("")), (String::new(), String::new()));
    }

    // ---------- 内置命令 ----------

    #[test]
    fn echo_returns_args_verbatim() {
        assert_eq!(run(&reg(), "/echo hello world").unwrap(), "hello world");
        assert_eq!(run(&reg(), "/echo").unwrap(), "");
    }

    #[test]
    fn var_writes_global_and_getvar_reads_back() {
        let r = reg();
        let mut vars = ScopeVars::new();
        assert!(r.execute("/var level :: 42", &mut vars).unwrap().contains("已设置 level"));
        assert_eq!(r.execute("/getvar level", &mut vars).unwrap(), "42");
    }

    #[test]
    fn setvar_is_alias_of_var() {
        let r = reg();
        let mut vars = ScopeVars::new();
        r.execute("/setvar name :: kedai", &mut vars);
        assert_eq!(r.execute("/getvar name", &mut vars).unwrap(), "kedai");
    }

    #[test]
    fn var_merges_without_clearing_other_keys() {
        let r = reg();
        let mut vars = ScopeVars::new();
        r.execute("/var a :: 1", &mut vars);
        r.execute("/var b :: 2", &mut vars);
        assert_eq!(r.execute("/getvar a", &mut vars).unwrap(), "1");
        assert_eq!(r.execute("/getvar b", &mut vars).unwrap(), "2");
    }

    #[test]
    fn addvar_accumulates_numeric() {
        let r = reg();
        let mut vars = ScopeVars::new();
        r.execute("/var score :: 10", &mut vars);
        let msg = r.execute("/addvar score :: 5", &mut vars).unwrap();
        assert!(msg.contains("score = 15"));
        assert_eq!(r.execute("/getvar score", &mut vars).unwrap(), "15.0");
    }

    #[test]
    fn addvar_initializes_missing_key() {
        let r = reg();
        let mut vars = ScopeVars::new();
        r.execute("/addvar count :: 3", &mut vars);
        assert_eq!(r.execute("/getvar count", &mut vars).unwrap(), "3.0");
    }

    #[test]
    fn addvar_rejects_non_numeric() {
        let r = reg();
        let mut vars = ScopeVars::new();
        let msg = r.execute("/addvar hp :: abc", &mut vars).unwrap();
        assert!(msg.contains("不是有效数值"));
    }

    #[test]
    fn getvar_unknown_returns_empty() {
        let r = reg();
        let mut vars = ScopeVars::new();
        assert_eq!(r.execute("/getvar missing", &mut vars).unwrap(), "");
    }

    #[test]
    fn help_lists_all_commands() {
        let r = reg();
        let mut vars = ScopeVars::new();
        let msg = r.execute("/help", &mut vars).unwrap();
        for name in ["/echo", "/var", "/setvar", "/getvar", "/addvar", "/help"] {
            assert!(msg.contains(name), "help 应包含 {name},实际: {msg}");
        }
    }

    #[test]
    fn list_includes_six_commands() {
        let names: Vec<String> = reg().list().into_iter().map(|m| m.name).collect();
        assert_eq!(names.len(), 6);
        assert!(names.contains(&"help".to_string()));
    }

    #[test]
    fn unknown_command_returns_none() {
        let r = reg();
        let mut vars = ScopeVars::new();
        assert!(r.execute("/no-such-cmd x", &mut vars).is_none());
    }

    #[test]
    fn empty_command_returns_empty_string() {
        let r = reg();
        let mut vars = ScopeVars::new();
        assert_eq!(r.execute("", &mut vars).unwrap(), "");
        assert_eq!(r.execute("   ", &mut vars).unwrap(), "");
    }
}
