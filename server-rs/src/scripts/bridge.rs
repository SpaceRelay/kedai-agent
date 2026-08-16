// TavernHelper 兼容桥(阶段三 3b-2):把 rquickjs 运行时与 Kedai 7 作用域变量、
// 事件、slash 命令连通,注入酒馆助手风格的全局对象供后端脚本调用。
// 设计约束(对应 scripts/runtime.rs):
//   - 变量以 JSON 字面量快照注入(每次执行新建运行时,快照即最新);
//   - 写回/命令经 Rust 闭包(__kd_write_native / __kd_slash_native),参数仅字符串,
//     规避 quickjs 值跨闭包的生命周期问题;
//   - 事件 API 为最小实现:事件表 + emit,由引擎接线后驱动(3b-3)。
use crate::models::types::{GenerationParams, LlmMessage, TokenUsage};
use crate::parsing::scopes::{Scope, ScopeVars};
use crate::scripts::loader::LoadedScript;
use crate::slash::SlashRegistry;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// slash 命令处理器(引擎接线时挂接;缺省为内置最小实现)
pub type SlashHandler = dyn Fn(&str) -> String + Send + Sync;

/// 生成处理器(阶段六 6g-1):脚本 TavernHelper.generate(config) → 引擎非流式生成。
/// 捕获最小依赖(connector + 模型快照),不引用引擎自身;同步调用,引擎侧经
/// Handle::current().block_on 完成异步生成。
pub type GenerateHandler = dyn Fn(
        Vec<LlmMessage>,
        GenerationParams,
        watch::Receiver<bool>,
    ) -> Result<(String, TokenUsage), String>
    + Send
    + Sync;

/// 导入处理器(阶段六 6g-2):脚本 TavernHelper.importRaw*(type, filename, content, session_id)
/// → 引擎各 service(绕过 HTTP)。同步调用,缺省(未接线)返回「不支持」。
pub type ImportHandler = dyn Fn(String, String, String, String) -> Result<String, String> + Send + Sync;

#[derive(Clone)]
pub struct EvalBridge {
    /// 共享的 7 作用域变量容器(引擎在收集上下文时构建)
    pub scope_vars: Arc<Mutex<ScopeVars>>,
    /// 当前脚本(script 作用域数据快照 + id/name)
    pub script: LoadedScript,
    /// 角色作用域 id(引擎注入;无则跳过 character 层写回)
    pub character_id: Option<String>,
    /// 自定义 slash 命令入口(可选;优先于注册表与内置最小实现)
    pub slash: Option<Arc<SlashHandler>>,
    /// slash 命令注册表(阶段四 4a;引擎注入,命中注册命令后不再走内置实现)
    pub registry: Option<Arc<SlashRegistry>>,
    /// 生成处理器(6g-1;引擎注入;缺省时 TavernHelper.generate 返回「不支持」)
    pub generate: Option<Arc<GenerateHandler>>,
    /// 导入处理器(6g-2;引擎注入;缺省时 importRaw* 返回「不支持」)
    pub imports: Option<Arc<ImportHandler>>,
}

impl EvalBridge {
    pub fn new(scope_vars: Arc<Mutex<ScopeVars>>, script: LoadedScript) -> Self {
        EvalBridge {
            scope_vars,
            script,
            character_id: None,
            slash: None,
            registry: None,
            generate: None,
            imports: None,
        }
    }

    pub fn with_character(mut self, character_id: impl Into<String>) -> Self {
        self.character_id = Some(character_id.into());
        self
    }

    pub fn with_slash(mut self, handler: Arc<SlashHandler>) -> Self {
        self.slash = Some(handler);
        self
    }

    pub fn with_registry(mut self, registry: Arc<SlashRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn with_generate(mut self, handler: Arc<GenerateHandler>) -> Self {
        self.generate = Some(handler);
        self
    }

    pub fn with_imports(mut self, handler: Arc<ImportHandler>) -> Self {
        self.imports = Some(handler);
        self
    }

    /// 7 作用域变量快照(JSON 字面量注入;message/preset/extension 未加载时为空对象)
    pub fn vars_snapshot(&self) -> Value {
        let sv = self.scope_vars.lock().unwrap_or_else(|e| e.into_inner());
        json!({
            "global": sv.scope_data(Scope::Global, "").cloned().unwrap_or_else(|| json!({})),
            "character": sv.scope_data(Scope::Character, "").cloned().unwrap_or_else(|| json!({})),
            "chat": sv.scope_data(Scope::Chat, "").cloned().unwrap_or_else(|| json!({})),
            "script": self.script.data.clone(),
            "message": sv.scope_data(Scope::Message, "").cloned().unwrap_or_else(|| json!({})),
            "preset": sv.scope_data(Scope::Preset, "").cloned().unwrap_or_else(|| json!({})),
            "extension": sv.scope_data(Scope::Extension, "").cloned().unwrap_or_else(|| json!({})),
        })
    }

    /// 构建预置脚本:变量快照 + TavernHelper 全局对象(读写经 __kd_write_native /
    /// __kd_slash_native 两个 Rust 闭包;事件为内存表 + __kd_event 桥)。
    /// 仅做 API 兼容子集:getVariables/setVariables/insertOrAssignVariables/
    /// insertVariables/deleteVariable/getAllVariables/eventOn/eventEmit/triggerSlash。
    pub fn build_script(&self) -> String {
        let snapshot = self.vars_snapshot().to_string();
        let script_id = serde_json::to_string(&self.script.id).unwrap_or_else(|_| "\"\"".into());
        let script_name = serde_json::to_string(&self.script.name).unwrap_or_else(|_| "\"\"".into());
        format!(
            r#"
// —— 阶段三 TavernHelper 兼容桥(预置代码,由运行时注入)——
globalThis.__kd_vars = {snapshot};
globalThis.__kd_events = {{}};
globalThis.TavernHelper = {{
  _th_impl: {{ getScriptId: () => {script_id}, getScriptName: () => {script_name} }},
  getScriptId: () => {script_id},
  getScriptName: () => {script_name},
  getVariables: (opt) => {{
    const type = (opt && opt.type) || 'chat';
    return globalThis.__kd_vars[type] || {{}};
  }},
  getAllVariables: () => {{
    const v = globalThis.__kd_vars;
    return Object.assign({{}}, v.global, v.character, v.chat, v.script);
  }},
  setVariables: (vars, opt) => {{
    const type = (opt && opt.type) || 'chat';
    try {{ __kd_write_native(type, JSON.stringify(vars || {{}})); }} catch (e) {{ return false; }}
    return true;
  }},
  insertOrAssignVariables: (vars, opt) => {{
    const type = (opt && opt.type) || 'chat';
    const cur = globalThis.__kd_vars[type] || {{}};
    const merged = Object.assign({{}}, cur, vars || {{}});
    try {{ __kd_write_native(type, JSON.stringify(merged)); }} catch (e) {{ return merged; }}
    globalThis.__kd_vars[type] = merged;
    return merged;
  }},
  insertVariables: (vars, opt) => {{
    const type = (opt && opt.type) || 'chat';
    const cur = globalThis.__kd_vars[type] || {{}};
    const merged = Object.assign({{}}, cur);
    for (const k in (vars || {{}})) if (!(k in cur)) merged[k] = vars[k];
    try {{ __kd_write_native(type, JSON.stringify(merged)); }} catch (e) {{ return merged; }}
    globalThis.__kd_vars[type] = merged;
    return merged;
  }},
  deleteVariable: (path, opt) => {{
    const type = (opt && opt.type) || 'chat';
    const cur = Object.assign({{}}, globalThis.__kd_vars[type] || {{}});
    const segs = String(path || '').split('.');
    const key = segs[0];
    const existed = key in cur;
    if (segs.length === 1) {{ delete cur[key]; }}
    try {{ __kd_write_native(type, JSON.stringify(cur)); }} catch (e) {{ return {{ variables: cur, delete_occurred: existed }}; }}
    globalThis.__kd_vars[type] = cur;
    return {{ variables: cur, delete_occurred: existed }};
  }},
  eventOn: (name, fn) => {{
    if (!name || typeof fn !== 'function') return {{ stop: () => {{}} }};
    if (!globalThis.__kd_events[name]) globalThis.__kd_events[name] = [];
    globalThis.__kd_events[name].push(fn);
    return {{ stop: () => {{ const l = globalThis.__kd_events[name]; if (l) {{ const i = l.indexOf(fn); if (i >= 0) l.splice(i, 1); }} }} }};
  }},
  eventEmit: (name, data) => {{
    const l = globalThis.__kd_events[name];
    if (!l || !l.length) return;
    for (const fn of l.slice()) {{ try {{ fn(data); }} catch (e) {{}} }}
  }},
  triggerSlash: (cmd) => __kd_slash_native(String(cmd || '')),
  // —— 阶段六 6g-1 生成类:静默单发,不入聊天记录;子集参数,其余返回「不支持」——
  generate: (config) => __kd_generate_native(JSON.stringify(config || {{}})),
  stopGenerationById: () => {{ throw new Error('Kedai 不支持 stopGenerationById(单发同步生成,无注册表)'); }},
  // —— 阶段六 6g-2 导入类:映射现有 service,绕过 HTTP;regex 暂不支持——
  importRawCharacter: (filename, content) => __kd_import_native('character', String(filename || ''), String(content || ''), ''),
  importRawWorldbook: (filename, content) => __kd_import_native('worldbook', String(filename || ''), String(content || ''), ''),
  importRawPreset: (content) => __kd_import_native('preset', '', String(content || ''), ''),
  importRawChat: (content, sessionId) => __kd_import_native('chat', '', String(content || ''), String(sessionId || '')),
  importRawTavernRegex: (content) => __kd_import_native('regex', '', String(content || ''), ''),
  // —— 阶段六 6g-3 扩展管理:只读视图,本机单用户无扩展基础设施——
  isAdmin: () => true,
  getTavernHelperExtensionId: () => 'kedai',
  isInstalledExtension: () => false,
  getExtensionType: () => null,
  installExtension: () => {{ throw new Error('Kedai 不支持扩展安装'); }},
  uninstallExtension: () => {{ throw new Error('Kedai 不支持扩展安装'); }},
  reinstallExtension: () => {{ throw new Error('Kedai 不支持扩展安装'); }},
  updateExtension: () => {{ throw new Error('Kedai 不支持扩展安装'); }},
  getExtensionInstallationInfo: () => {{ throw new Error('Kedai 不支持扩展安装'); }},
}};
"#
        )
    }

    /// 阶段六 6g-1:脚本 TavernHelper.generate(config) → __kd_generate_native。
    /// 解析参数子集(user_input/max_tokens/temperature);custom_api/tools/json_schema
    /// 明确返回「不支持」;未接线时同样报错。生成为静默单发,不入聊天记录。
    pub fn generate_from_config(&self, config_json: &str) -> Result<String, String> {
        let cfg: Value = serde_json::from_str(config_json)
            .map_err(|e| format!("解析 generate 配置失败: {e}"))?;
        let user_input = cfg.get("user_input").and_then(|v| v.as_str()).unwrap_or("");
        if user_input.trim().is_empty() {
            return Err("generate 缺少 user_input".to_string());
        }
        for unsupported in ["custom_api", "tools", "json_schema"] {
            if cfg.get(unsupported).is_some() {
                return Err(format!("generate 暂不支持 {unsupported} 参数"));
            }
        }
        let handler = self
            .generate
            .as_ref()
            .ok_or_else(|| "generate 未接线(当前运行环境不支持)".to_string())?;
        let messages = vec![LlmMessage::plain("user", user_input)];
        let params = GenerationParams {
            temperature: cfg
                .get("temperature")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0),
            top_p: 1.0,
            max_tokens: cfg
                .get("max_tokens")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32)
                .unwrap_or(1024),
            stop: None,
            tools: Vec::new(),
            max_tool_rounds: None,
            tool_choice: crate::models::types::ToolChoice::Auto,
            parallel_tool_calls: None,
        };
        let (_, abort) = tokio::sync::watch::channel(false);
        let (text, _usage) = handler(messages, params, abort).map_err(|e| format!("生成失败: {e}"))?;
        if text.trim().is_empty() {
            Err("generate 返回空内容".to_string())
        } else {
            Ok(text)
        }
    }

    /// 阶段六 6g-2:脚本 TavernHelper.importRaw* → __kd_import_native。
    /// 透传 kind/filename/content/session_id 给引擎注入的导入处理器;未接线返回「不支持」。
    pub fn import_raw(
        &self,
        kind: &str,
        filename: &str,
        content: &str,
        session_id: &str,
    ) -> Result<String, String> {
        let handler = self
            .imports
            .as_ref()
            .ok_or_else(|| format!("importRaw{kind} 未接线(当前运行环境不支持)"))?;
        handler(
            kind.to_string(),
            filename.to_string(),
            content.to_string(),
            session_id.to_string(),
        )
    }

    /// 写回某作用域(整对象替换语义;chat 逐键写入树/扁平层)。
    /// 由 __kd_write_native 闭包调用,错误返回 Err(消息) → JS 抛异常。
    pub fn write_scope(&self, scope: &str, json_text: &str) -> Result<(), String> {
        let parsed: Value =
            serde_json::from_str(json_text).map_err(|e| format!("解析变量 JSON 失败: {e}"))?;
        let scope = Scope::from_str(scope).ok_or_else(|| format!("未知变量作用域: {scope}"))?;
        let mut sv = self.scope_vars.lock().unwrap_or_else(|e| e.into_inner());
        match scope {
            Scope::Chat => {
                if let Some(obj) = parsed.as_object() {
                    for (k, v) in obj {
                        sv.write(Scope::Chat, k, v.clone());
                    }
                }
            }
            Scope::Script => {
                let id = self.script.id.clone();
                sv.with_scope(Scope::Script, &id, parsed);
            }
            Scope::Character => {
                let id = self.character_id.clone().unwrap_or_default();
                sv.with_scope(Scope::Character, &id, parsed);
            }
            Scope::Global => {
                sv.with_scope(Scope::Global, "", parsed);
            }
            Scope::Message => {
                let id = sv.message_scope_id().unwrap_or("").to_string();
                sv.with_scope(Scope::Message, &id, parsed);
            }
            Scope::Preset => {
                let id = sv.preset_scope_id().unwrap_or("").to_string();
                sv.with_scope(Scope::Preset, &id, parsed);
            }
            Scope::Extension => {
                sv.with_scope(Scope::Extension, "", parsed);
            }
        }
        Ok(())
    }

    /// triggerSlash:优先自定义处理器(引擎挂接),其次命令注册表(阶段四 4a),
    /// 注册表未命中回退内置最小实现。
    pub fn trigger_slash(&self, command: &str) -> String {
        if let Some(handler) = &self.slash {
            return handler(command);
        }
        if let Some(registry) = &self.registry {
            // 注册表命中即返回;锁在块内释放,避免与内置实现再次加锁造成死锁
            // (std::sync::Mutex 非重入)。
            let out = {
                let mut sv = self.scope_vars.lock().unwrap_or_else(|e| e.into_inner());
                registry.execute(command, &mut sv)
            };
            if let Some(out) = out {
                return out;
            }
        }
        let cmd = command.trim();
        if cmd.is_empty() {
            return String::new();
        }
        let (name, arg) = match cmd.find(char::is_whitespace) {
            Some(i) => (&cmd[..i], cmd[i..].trim()),
            None => (cmd, ""),
        };
        match name {
            "/echo" => arg.to_string(),
            "/var" | "/setvar" => {
                let (k, v) = split_key_value(arg);
                if k.is_empty() {
                    return "/var 用法: /var <key> :: <value>(写入 global 作用域)".into();
                }
                let obj = json!({ k.clone(): v });
                match self.write_scope("global", &obj.to_string()) {
                    Ok(()) => format!("已设置 {k}"),
                    Err(e) => format!("设置失败: {e}"),
                }
            }
            "/getvar" => {
                let sv = self.scope_vars.lock().unwrap_or_else(|e| e.into_inner());
                sv.view(arg)
                    .map(|v| {
                        // 字符串值原样返回,其余 JSON 化
                        v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())
                    })
                    .unwrap_or_default()
            }
            _ => format!("未知 Slash 命令: {name}(最小实现支持 /echo /var /setvar /getvar)"),
        }
    }
}

/// 拆分 "key :: value" 或 "key = value";无分隔符时整个视为 key(空值)
fn split_key_value(s: &str) -> (String, String) {
    let s = s.trim();
    for sep in ["::", "="] {
        if let Some(i) = s.find(sep) {
            let k = s[..i].trim().to_string();
            let v = s[i + sep.len()..].trim().to_string();
            return (k, v);
        }
    }
    if s.is_empty() {
        (String::new(), String::new())
    } else {
        (s.to_string(), String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripts::runtime::{eval_with_bridge, EvalOptions};

    fn new_vars() -> Arc<Mutex<ScopeVars>> {
        Arc::new(Mutex::new(ScopeVars::new()))
    }

    fn script(id: &str, name: &str, data: Value) -> LoadedScript {
        LoadedScript {
            id: id.to_string(),
            name: name.to_string(),
            content: String::new(),
            data,
        }
    }

    fn run_with(source: &str, bridge: &EvalBridge) -> crate::scripts::runtime::EvalOutcome {
        eval_with_bridge(source, &EvalOptions::default(), bridge)
    }

    #[test]
    fn snapshot_covers_seven_scopes() {
        let sv = new_vars();
        {
            let mut s = sv.lock().unwrap();
            s.with_scope(Scope::Global, "", json!({ "g": 1 }));
            s.chat_tree.set("c", json!(2));
        }
        let bridge = EvalBridge::new(sv.clone(), script("s1", "测试", json!({ "d": 3 })));
        let snap = bridge.vars_snapshot();
        assert_eq!(snap["global"]["g"], json!(1));
        assert_eq!(snap["chat"]["c"], json!(2));
        assert_eq!(snap["script"]["d"], json!(3));
        assert_eq!(snap["message"], json!({}));
    }

    #[test]
    fn get_variables_reads_snapshot() {
        let sv = new_vars();
        sv.lock().unwrap().with_scope(Scope::Global, "", json!({ "hp": 100 }));
        let bridge = EvalBridge::new(sv.clone(), script("s1", "测试", json!({})));
        let out = run_with(
            "JSON.stringify(TavernHelper.getVariables({ type: 'global' }))",
            &bridge,
        );
        assert_eq!(out, crate::scripts::runtime::EvalOutcome::Ok("{\"hp\":100}".into()));
    }

    #[test]
    fn set_variables_writes_back() {
        let sv = new_vars();
        let bridge = EvalBridge::new(sv.clone(), script("s1", "测试", json!({})));
        let out = run_with(
            "TavernHelper.setVariables({ a: 1, b: 'x' }, { type: 'global' })",
            &bridge,
        );
        assert!(matches!(out, crate::scripts::runtime::EvalOutcome::Ok(_)));
        let s = sv.lock().unwrap();
        assert_eq!(s.scope_data(Scope::Global, "").and_then(|d| d.get("a")), Some(&json!(1)));
        assert_eq!(s.scope_data(Scope::Global, "").and_then(|d| d.get("b")), Some(&json!("x")));
    }

    #[test]
    fn insert_or_assign_merges_into_script_scope() {
        let sv = new_vars();
        let bridge = EvalBridge::new(sv.clone(), script("s1", "测试", json!({ "k": 1 })));
        let out = run_with(
            "JSON.stringify(TavernHelper.insertOrAssignVariables({ k: 2, n: 9 }, { type: 'script' }))",
            &bridge,
        );
        assert_eq!(out, crate::scripts::runtime::EvalOutcome::Ok("{\"k\":2,\"n\":9}".into()));
        let s = sv.lock().unwrap();
        assert_eq!(s.scope_data(Scope::Script, "s1").and_then(|d| d.get("n")), Some(&json!(9)));
    }

    #[test]
    fn delete_variable_reports_occurred() {
        let sv = new_vars();
        sv.lock().unwrap().with_scope(Scope::Global, "", json!({ "gone": 1, "keep": 2 }));
        let bridge = EvalBridge::new(sv.clone(), script("s1", "测试", json!({})));
        let out = run_with(
            "JSON.stringify(TavernHelper.deleteVariable('gone', { type: 'global' }))",
            &bridge,
        );
        assert_eq!(
            out,
            crate::scripts::runtime::EvalOutcome::Ok("{\"variables\":{\"keep\":2},\"delete_occurred\":true}".into())
        );
    }

    #[test]
    fn events_register_and_emit() {
        let sv = new_vars();
        let bridge = EvalBridge::new(sv.clone(), script("s1", "测试", json!({})));
        let out = run_with(
            "let got = null;
             TavernHelper.eventOn('my_event', (d) => { got = d; });
             TavernHelper.eventEmit('my_event', 42);
             JSON.stringify(got)",
            &bridge,
        );
        assert_eq!(out, crate::scripts::runtime::EvalOutcome::Ok("42".into()));
    }

    #[test]
    fn trigger_slash_echo_and_var() {
        let sv = new_vars();
        let bridge = EvalBridge::new(sv.clone(), script("s1", "测试", json!({})));
        assert_eq!(
            run_with("TavernHelper.triggerSlash('/echo 你好')", &bridge),
            crate::scripts::runtime::EvalOutcome::Ok("你好".into())
        );
        // /var 写 global 后回读
        assert_eq!(
            run_with("TavernHelper.triggerSlash('/var 金币 :: 100')", &bridge),
            crate::scripts::runtime::EvalOutcome::Ok("已设置 金币".into())
        );
        assert_eq!(
            run_with("TavernHelper.triggerSlash('/getvar 金币')", &bridge),
            crate::scripts::runtime::EvalOutcome::Ok("100".into())
        );
    }

    #[test]
    fn trigger_slash_unknown_command() {
        let sv = new_vars();
        let bridge = EvalBridge::new(sv.clone(), script("s1", "测试", json!({})));
        let out = run_with("TavernHelper.triggerSlash('/foo bar')", &bridge);
        assert!(matches!(out, crate::scripts::runtime::EvalOutcome::Ok(s) if s.contains("未知 Slash 命令")));
    }

    #[test]
    fn custom_slash_handler_wins() {
        let sv = new_vars();
        let mut bridge = EvalBridge::new(sv.clone(), script("s1", "测试", json!({})));
        bridge.slash = Some(Arc::new(|cmd| format!("custom:{cmd}")));
        let out = run_with("TavernHelper.triggerSlash('/anything')", &bridge);
        assert_eq!(out, crate::scripts::runtime::EvalOutcome::Ok("custom:/anything".into()));
    }

    #[test]
    fn write_rejects_bad_scope() {
        let sv = new_vars();
        let bridge = EvalBridge::new(sv.clone(), script("s1", "测试", json!({})));
        assert!(bridge.write_scope("nope", "{}").is_err());
        assert!(bridge.write_scope("global", "not json").is_err());
    }
}
