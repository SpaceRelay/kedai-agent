// 渲染上下文(ST-Prompt-Template / 酒馆助手生态兼容,L1 老层 · parsing/assistant):
// 一次 EJS 渲染中内建读取类函数(getwi/getchar/getpreset/getqr/getChatMessage 等)
// 与 injectPrompt 的共享数据源。engine 在上下文收集阶段构建 RenderCtx,
// 渲染完成后把 ctx.injected 登记的注入提示词并入提示词流。
use crate::models::types::MessageRecord;
use crate::parsing::world_book::{parse_entry_decorators, WorldEntry};
use serde_json::Value;

use super::ejs;
use super::AssistantVars;

/// getchar/getChara 返回的角色信息视图(从角色卡提取;缺省字段为空串)
#[derive(Debug, Clone, Default)]
pub struct CharacterCtx {
    pub name: String,
    pub description: String,
    pub personality: String,
    pub scenario: String,
    /// 头像(角色卡 avatar 路径/URL;缺省为 None)
    pub avatar_url: Option<String>,
    /// 示例对话(mes_example)
    pub mes_example: String,
    /// 开场白(first_mes)
    pub first_mes: String,
}

/// 快速回复条目(getqr/getQuickReply 读取;来自 quick_replies 表)
#[derive(Debug, Clone)]
pub struct QuickReplyCtx {
    pub name: String,
    pub label: String,
    pub content: String,
}

/// 提示词预设楼层(getpreset/getPresetPrompt 读取;来自提示词注入楼层配置)
#[derive(Debug, Clone)]
pub struct PresetPromptCtx {
    pub name: String,
    pub content: String,
}

/// injectPrompt(key, prompt, order?, sticky?, uid?) 登记的注入提示词
/// (渲染完成后由 engine 按 order 排序并入提示词流)
#[derive(Debug, Clone)]
pub struct InjectedPrompt {
    pub key: String,
    pub prompt: String,
    /// 注入顺序(数值小者在前;缺省 100)
    pub order: i64,
    /// 注入位置权重(0-4,对齐酒馆 injection position;当前统一并入 system 尾部)
    pub position: i64,
    /// 是否粘性(本轮之后持续注入;当前仅登记,由 engine 决定消费策略)
    pub sticky: bool,
    /// 唯一标识(缺省空串)
    pub uid: String,
}

/// 渲染上下文中「借用数据 + 可变注入清单」部分:
/// EJS 解释器 Env 持有其可变引用,内建函数据此读写(避免整棵树复制)。
#[derive(Debug, Default)]
pub struct RenderCtxData<'a> {
    /// getchar/getChara 的角色信息;None = 无角色上下文(返回 "")
    pub character: Option<&'a CharacterCtx>,
    /// getwi/getWorldInfo 的世界书条目全集;None = 无世界书上下文(返回 "")
    pub world_entries: Option<&'a [WorldEntry]>,
    /// getChatMessage/getChatMessages 的聊天历史;None = 无历史上下文
    pub history: Option<&'a [MessageRecord]>,
    /// getqr/getQuickReply 的快速回复库;None = 无快速回复上下文
    pub quick_replies: Option<&'a [QuickReplyCtx]>,
    /// getpreset/getPresetPrompt 的提示词预设(楼层);None = 无预设上下文
    pub preset_prompts: Option<&'a [PresetPromptCtx]>,
    /// 7 作用域变量(计划二);None = 无作用域上下文(作用域 builtin 回退旧行为)
    pub scopes: Option<&'a mut crate::parsing::scopes::ScopeVars>,
    /// injectPrompt 登记清单(渲染后由 engine 消费)
    pub injected: Vec<InjectedPrompt>,
}

/// 渲染上下文:一次渲染中 EJS 内建读取类函数与 injectPrompt 的完整数据源。
/// vars 为可变引用(渲染副作用写入变量树),其余数据为共享引用(不复制)。
pub struct RenderCtx<'a> {
    pub vars: &'a mut AssistantVars,
    pub data: RenderCtxData<'a>,
}

impl<'a> RenderCtx<'a> {
    /// 空上下文(无世界书/角色/历史/快速回复/预设):保持既有「无上下文容错」语义
    pub fn new(vars: &'a mut AssistantVars) -> Self {
        RenderCtx {
            vars,
            data: RenderCtxData::default(),
        }
    }
}

// ===================== @@ 装饰器执行(ST-Prompt-Template) =====================

/// 执行世界书条目的 `@@` 装饰器(ST-Prompt-Template 兼容),在渲染前决定是否注入:
///   @@if <expr>           表达式求值为真才注入
///   @@unless <expr>       表达式求值为假才注入
///   @@var <path> = <expr> / @@set <path> = <expr>:渲染前写入变量树(影响本条后续模板)
/// 表达式经 EJS 白名单解释器求值(非 eval);求值失败按「未命中装饰器」容错处理
/// (条目照常注入,保证内容不丢)。
/// 返回 false 表示条目应跳过注入。
pub fn apply_entry_decorators(e: &WorldEntry, ctx: &mut RenderCtx<'_>) -> bool {
    let deco = parse_entry_decorators(e);
    for (i, line) in deco.all.iter().enumerate() {
        let name = line.split(' ').next().unwrap_or("");
        let arg = deco.args.get(i).map(|s| s.as_str()).unwrap_or("");
        match name {
            "@@if" => {
                if !eval_decorator_bool(arg, ctx) {
                    return false;
                }
            }
            "@@unless" => {
                if eval_decorator_bool(arg, ctx) {
                    return false;
                }
            }
            "@@var" | "@@set" => assign_decorator_var(arg, ctx),
            _ => {} // 未识别装饰器:忽略(兼容层不丢信息,行已剥离保存)
        }
    }
    true
}

/// 装饰器条件求值:表达式经 EJS 渲染为 'true'/'false';求值失败返回 true(容错放行)
fn eval_decorator_bool(expr: &str, ctx: &mut RenderCtx<'_>) -> bool {
    if expr.trim().is_empty() {
        return true;
    }
    let (out, errors) =
        ejs::render_template_with_ctx(&format!("<%= ({expr}) ? 'true' : 'false' %>"), ctx, &[]);
    if !errors.is_empty() {
        return true;
    }
    matches!(out.trim(), "true")
}

/// 装饰器变量写入:`@@var path = expr`(按首个 '=' 分割;值经 JSON 序列化求值,
/// 对象/数组/数字/布尔保真,字符串经 JSON 解析还原)
fn assign_decorator_var(arg: &str, ctx: &mut RenderCtx<'_>) {
    let Some(eq) = arg.find('=') else {
        return;
    };
    let path = arg[..eq].trim();
    let expr = arg[eq + 1..].trim();
    if path.is_empty() || expr.is_empty() {
        return;
    }
    let (out, errors) =
        ejs::render_template_with_ctx(&format!("<%= JSON.stringify(({expr})) %>"), ctx, &[]);
    if errors.is_empty() {
        let t = out.trim();
        if !t.is_empty() && t != "undefined" {
            if let Ok(v) = serde_json::from_str::<Value>(t) {
                // JS 数字在解释器内为 f64:整数值序列化为 "88.0"。还原为整数形态,
                // 保持 JSON 数值语义(与 {{setvar}} 的 infer_value 行为一致)。
                let v = match v {
                    Value::Number(n) if n.is_f64() => {
                        let f = n.as_f64().unwrap_or(f64::NAN);
                        if f.fract() == 0.0 && f.is_finite() && f.abs() < 9.007_199_254_740_992e15 {
                            if let Some(i) = f64_to_i64(f) {
                                serde_json::json!(i)
                            } else {
                                Value::Number(n)
                            }
                        } else {
                            Value::Number(n)
                        }
                    }
                    v => v,
                };
                ctx.vars.set(path, v);
            }
        }
    }
}

/// f64 整数 → i64(超出 i64 范围返回 None,保持原浮点)
fn f64_to_i64(f: f64) -> Option<i64> {
    if f >= i64::MIN as f64 && f <= i64::MAX as f64 {
        Some(f as i64)
    } else {
        None
    }
}

// ===================== 测试 =====================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(comment: &str, content: &str) -> WorldEntry {
        WorldEntry {
            id: 0,
            comment: comment.to_string(),
            keys: vec![],
            keys_secondary: vec![],
            regex: None,
            use_regex: false,
            content: content.to_string(),
            constant: false,
            enabled: true,
            position: 0,
            depth: 4,
            order: 100,
            case_sensitive: false,
            sticky: 0,
            cooldown: 0,
            probability: 100,
            use_probability: false,
            role: None,
            decorators: Vec::new(),
        }
    }

    /// @@if 真 → 注入;假 → 跳过
    #[test]
    fn if_decorator_gates_injection() {
        let mut vars = AssistantVars::new();
        vars.set("好感度", json!(150));
        let mut e = entry("条件条目", "内容");
        e.decorators = vec!["@@if getvar('好感度') > 100".into()];
        let mut ctx = RenderCtx::new(&mut vars);
        assert!(apply_entry_decorators(&e, &mut ctx));
        vars.set("好感度", json!(50));
        let mut ctx = RenderCtx::new(&mut vars);
        assert!(!apply_entry_decorators(&e, &mut ctx));
    }

    /// @@unless 与 @@if 语义相反;求值失败容错放行
    #[test]
    fn unless_decorator_and_error_tolerance() {
        let mut vars = AssistantVars::new();
        vars.set("开关", json!(true));
        let mut e = entry("除非条目", "内容");
        e.decorators = vec!["@@unless getvar('开关')".into()];
        let mut ctx = RenderCtx::new(&mut vars);
        assert!(!apply_entry_decorators(&e, &mut ctx));
        // 语法错误 → 放行(内容不丢)
        let mut e2 = entry("坏条件", "内容");
        e2.decorators = vec!["@@if (".into()];
        let mut ctx = RenderCtx::new(&mut vars);
        assert!(apply_entry_decorators(&e2, &mut ctx));
    }

    /// @@var 在渲染前写入变量树(数字/对象保真)
    #[test]
    fn var_decorator_writes_tree() {
        let mut vars = AssistantVars::new();
        let mut e = entry("变量条目", "内容");
        e.decorators = vec![
            "@@var 好感度 = 88".into(),
            "@@set 状态.心情 = '平静'".into(),
        ];
        let mut ctx = RenderCtx::new(&mut vars);
        assert!(apply_entry_decorators(&e, &mut ctx));
        assert_eq!(vars.get_value("好感度"), Some(&json!(88)));
        assert_eq!(vars.get_value("状态.心情"), Some(&json!("平静")));
    }

    /// 未知装饰器与空参数不影响注入
    #[test]
    fn unknown_decorator_ignored() {
        let mut vars = AssistantVars::new();
        let mut e = entry("未知", "内容");
        e.decorators = vec!["@@unknown x".into()];
        let mut ctx = RenderCtx::new(&mut vars);
        assert!(apply_entry_decorators(&e, &mut ctx));
    }
}
