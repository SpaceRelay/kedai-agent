// 内容渲染(注入链路用, L1 老层 · parsing/assistant):
// EJS 模板渲染 + {{format_message_variable}} / {{get_message_variable}} 宏展开 +
// <StatusPlaceHolderImpl/> 全树格式化替换 + <status_current_variable> 段标签剥离;
// 宏层值转换(infer_value/value_to_display)供 parsing/macros.rs 使用。
use serde_json::Value;

use super::ejs;
use super::vars::{format_scalar, AssistantVars};
use super::RenderCtx;

// ===================== 内容渲染(注入链路用) =====================

/// 渲染世界书条目/提示词中的酒馆助手内容(兼容入口:空渲染上下文)。
/// 见 render_assistant_content_with 的完整说明。
pub fn render_assistant_content(text: &str, vars: &mut AssistantVars) -> String {
    let mut ctx = RenderCtx::new(vars);
    render_assistant_content_with(text, &mut ctx)
}

/// 渲染世界书条目/提示词中的酒馆助手内容(带渲染上下文):
///   1. EJS 模板渲染(内建 getvar/setvar/addvar 读写变量树;
///      getwi/getchar/getpreset/getqr/getChatMessage 等经 ctx 真实读取;
///      injectPrompt 登记到 ctx.data.injected,渲染后由 engine 消费)
///   2. {{format_message_variable::path}} / {{get_message_variable::path}} 宏 → 变量树格式化文本
///   3. <StatusPlaceHolderImpl/> → 全树格式化文本
///   4. <status_current_variable> 段标签剥离(原版标记变量状态段位置,内容保留)
///
/// 注:ejs 内建的 injectPrompt(key, prompt, order?, sticky?, uid?) 为 ST-Prompt-Template
/// 兼容实现——登记到渲染上下文的注入清单(render 后由 engine 按 order 排序并入提示词流);
/// getPromptsInjected/hasPromptsInjected 读取该清单。
pub fn render_assistant_content_with(text: &str, ctx: &mut RenderCtx<'_>) -> String {
    let (mut out, errors) = ejs::render_template_with_ctx(text, ctx, &[]);
    for e in &errors {
        tracing::warn!(error = e.clone(), "酒馆助手 EJS 渲染跳过部分内容");
    }
    // 容错降级:渲染出错且输出为空时,回退保留原文。
    // 酒馆助手角色卡(如 WuWa MVU 版)常使用词法/语法超出本解释器子集的 EJS 片段
    // (裸反斜杠、getMessageVar、默认参数等),失败会把整条条目内容清空——
    // 即使条目大部分是纯文本设定也会一并丢失。此处保证出错时内容不丢;
    // 正常渲染(含条件分支未命中输出为空、无错误)不受影响,语义保持不变。
    if !errors.is_empty() && out.is_empty() && !text.trim().is_empty() {
        tracing::warn!(
            text_len = text.len(),
            "酒馆助手 EJS 渲染失败,已回退保留原文(内容不丢失)"
        );
        out = text.to_string();
    }
    let out = expand_format_message_variable(&out, &*ctx.vars);
    let out = strip_status_current_variable(&out);
    if out.contains("<StatusPlaceHolderImpl/>") {
        let formatted = ctx.vars.format(None);
        out.replace("<StatusPlaceHolderImpl/>", &formatted)
    } else {
        out
    }
}

/// {{format_message_variable::path}} / {{get_message_variable::path}} 宏展开(大小写不敏感;path 可省略)
fn expand_format_message_variable(text: &str, vars: &AssistantVars) -> String {
    // 正则为写死字面量,编译必然成功
    let re = regex::Regex::new(
        r"(?i)\{\{\s*(?:format_message_variable|get_message_variable)\s*::\s*([^}]*?)\s*\}\}",
    )
    .expect("format_message_variable 宏正则为常量,编译必然成功");
    re.replace_all(text, |caps: &regex::Captures| {
        let p = caps
            .get(1)
            .map(|m| m.as_str().trim())
            .filter(|s| !s.is_empty());
        vars.format(p)
    })
    .to_string()
}

/// 剥离 <status_current_variable> 起止标签(大小写不敏感;内部内容保留)
fn strip_status_current_variable(text: &str) -> String {
    // 正则为写死字面量,编译必然成功
    let re = regex::Regex::new(r"(?i)</?\s*status_current_variable\s*>")
        .expect("status_current_variable 剥离正则为常量,编译必然成功");
    re.replace_all(text, "").to_string()
}

/// 宏层 {{setvar::path::value}} 用:值类型推断(数字/布尔/null/JSON 对象数组/字符串)
pub fn infer_value(s: &str) -> Value {
    let t = s.trim();
    // JSON 可解析的标量/对象/数组按 JSON;其余(裸词等)按字符串
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return v;
    }
    Value::String(s.to_string())
}

/// 树中值 → 显示字符串(标量裸输出,对象/数组 JSON)
pub fn value_to_display(v: &Value) -> String {
    match v {
        Value::Object(_) | Value::Array(_) => serde_json::to_string(v).unwrap_or_default(),
        _ => format_scalar(v),
    }
}
