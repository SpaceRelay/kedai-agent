// 运行时值:JsValue/JsFn/Builtin 定义与类型转换(fmt_num/js_to_string/js_to_num/truthy/
// js_to_json/value_to_js)、宽松比较(loose_eq/compare)。eval.rs/exec.rs 与
// assistant 模块(经 mod.rs re-export 的 JsValue/js_to_json/value_to_js)共同使用。
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::ast::{BinOp, FnBody};

#[derive(Debug, Clone)]
pub(crate) enum JsValue {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<JsValue>),
    Obj(Vec<(String, JsValue)>),
    /// 用户函数(普通函数/箭头函数),含创建时的闭包捕获
    Fn(JsFn),
    Builtin(Builtin),
    /// 绑定接收者的内建方法(如 "abc".toUpperCase)
    Method(Builtin, Box<JsValue>),
}

/// 用户函数(普通函数/箭头函数),持有定义处作用域链引用(闭包可见外部变量,
/// 赋值会写入外部作用域——与 JS 语义一致)
#[derive(Debug, Clone)]
pub(crate) struct JsFn {
    pub(super) params: Vec<crate::parsing::assistant::ejs::ast::FuncParam>,
    pub(super) body: FnBody,
    pub(super) captured: Vec<Rc<RefCell<HashMap<String, JsValue>>>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Builtin {
    GetVar,
    SetVar,
    AddVar,
    Out,
    ParseInt,
    ParseFloat,
    Number,
    Str,
    Bool,
    IsNaN,
    MathAbs,
    MathCeil,
    MathFloor,
    MathRound,
    MathTrunc,
    MathMax,
    MathMin,
    MathPow,
    MathRandom,
    MathSqrt,
    StrToUpperCase,
    StrToLowerCase,
    StrIncludes,
    StrStartsWith,
    StrEndsWith,
    StrSplit,
    StrTrim,
    StrTrimEnd,
    StrRepeat,
    StrReplace,
    StrCharAt,
    StrIndexOf,
    StrSlice,
    StrSubstring,
    ArrPush,
    ArrPop,
    ArrJoin,
    ArrIncludes,
    ArrIndexOf,
    ArrForEach,
    ArrMap,
    ArrFilter,
    ArrFind,
    ObjKeys,
    ObjValues,
    ObjEntries,
    JsonStringify,
    JsonParse,
    MatchChatMessages,
    // ===== ST-Prompt-Template 兼容:读取类容错函数(无上下文 → 默认值) =====
    // 渲染上下文无聊天历史/无角色信息时返回空字符串,保证模板调用不报错
    GetWorldInfo,    // getwi / getWorldInfo(name, title?) → ""
    GetChara,        // getchar / getChara(name) → ""
    GetPreset,       // getpreset / getPresetPrompt(name) → ""
    GetQuickReply,   // getqr / getQuickReply(name) → ""
    GetChatMessage,  // getChatMessage(idx, role) → ""
    GetChatMessages, // getChatMessages(...) → [] (空数组)
    // ===== ST-Prompt-Template 兼容:注入/JSON/嵌套渲染 =====
    InjectPrompt, // injectPrompt(key, prompt, order?, sticky?, uid?) → "" (静默成功)
    GetPromptsInjected, // getPromptsInjected(key) → "" (无注入清单)
    HasPromptsInjected, // hasPromptsInjected(key) → false
    ParseJSON,    // parseJSON(text) → 宽松解析,失败返回原字符串
    JsonPatch,    // jsonPatch(dest, change) → 原对象(浅克隆,容错)
    EvalTemplate, // evalTemplate(content) → 嵌套渲染结果,失败返回原文
    Print,        // print(...args) → 追加输出(__kedai_out__ 语义)
    // ===== ST-Prompt-Template 兼容:变量别名/自增自减 =====
    IncVar, // incvar(path) → 变量 +1
    DecVar, // decvar(path) → 变量 -1
    // ===== 计划二 · 7 作用域变量 builtin =====
    // 读:getGlobalVar/getMessageVar/getCharacterVar/getPresetVar(按作用域取值,缺失回退链);
    // 写:setGlobalVar(global)/setLocalVar(chat 树)/setMessageVar(message)。
    // 均在 scopes 上下文存在时生效,None 时回退旧 GetVar/SetVar 行为。
    GetGlobalVar,
    GetMessageVar,
    GetCharacterVar,
    GetPresetVar,
    SetGlobalVar,
    SetLocalVar,
    SetMessageVar,
}

pub(super) fn fmt_num(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

pub(super) fn js_to_string(v: &JsValue) -> String {
    match v {
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::Bool(b) => b.to_string(),
        JsValue::Num(n) => fmt_num(*n),
        JsValue::Str(s) => s.clone(),
        JsValue::Arr(_) | JsValue::Obj(_) => {
            serde_json::to_string(&js_to_json(v)).unwrap_or_else(|_| String::new())
        }
        JsValue::Fn(_) => "[function]".into(),
        JsValue::Builtin(_) | JsValue::Method(..) => "[function]".into(),
    }
}

pub(super) fn js_to_num(v: &JsValue) -> f64 {
    match v {
        JsValue::Num(n) => *n,
        JsValue::Str(s) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
        JsValue::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        JsValue::Null => 0.0,
        JsValue::Undefined => f64::NAN,
        _ => f64::NAN,
    }
}

pub(super) fn truthy(v: &JsValue) -> bool {
    match v {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Num(n) => *n != 0.0 && !n.is_nan(),
        JsValue::Str(s) => !s.is_empty(),
        _ => true, // 数组/对象/函数恒真(JS 语义)
    }
}

pub(crate) fn js_to_json(v: &JsValue) -> serde_json::Value {
    match v {
        JsValue::Undefined | JsValue::Null => serde_json::Value::Null,
        JsValue::Bool(b) => serde_json::Value::Bool(*b),
        JsValue::Num(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        JsValue::Str(s) => serde_json::Value::String(s.clone()),
        JsValue::Arr(items) => serde_json::Value::Array(items.iter().map(js_to_json).collect()),
        JsValue::Obj(fields) => {
            let mut m = serde_json::Map::new();
            for (k, val) in fields {
                m.insert(k.clone(), js_to_json(val));
            }
            serde_json::Value::Object(m)
        }
        JsValue::Fn(_) | JsValue::Builtin(_) | JsValue::Method(..) => serde_json::Value::Null,
    }
}

pub(crate) fn value_to_js(v: &serde_json::Value) -> JsValue {
    match v {
        serde_json::Value::Null => JsValue::Null,
        serde_json::Value::Bool(b) => JsValue::Bool(*b),
        serde_json::Value::Number(n) => JsValue::Num(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => JsValue::Str(s.clone()),
        serde_json::Value::Array(a) => JsValue::Arr(a.iter().map(value_to_js).collect()),
        serde_json::Value::Object(m) => {
            JsValue::Obj(m.iter().map(|(k, v)| (k.clone(), value_to_js(v))).collect())
        }
    }
}

pub(super) fn loose_eq(a: &JsValue, b: &JsValue) -> bool {
    match (a, b) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Undefined, JsValue::Null) | (JsValue::Null, JsValue::Undefined) => true,
        (JsValue::Num(x), JsValue::Num(y)) => x == y,
        (JsValue::Str(x), JsValue::Str(y)) => x == y,
        (JsValue::Bool(x), JsValue::Bool(y)) => x == y,
        // 数字与数字字符串
        (JsValue::Num(x), JsValue::Str(s)) | (JsValue::Str(s), JsValue::Num(x)) => {
            s.trim().parse::<f64>().map(|y| x == &y).unwrap_or(false)
        }
        (JsValue::Bool(b), JsValue::Num(n)) | (JsValue::Num(n), JsValue::Bool(b)) => {
            let bn = if *b { 1.0 } else { 0.0 };
            bn == *n
        }
        (JsValue::Bool(b), JsValue::Str(s)) | (JsValue::Str(s), JsValue::Bool(b)) => {
            let bn = if *b {
                "true".to_string()
            } else {
                "false".to_string()
            };
            *s == bn
        }
        (JsValue::Null, JsValue::Num(n)) | (JsValue::Num(n), JsValue::Null) => *n == 0.0,
        (JsValue::Null, JsValue::Str(s)) | (JsValue::Str(s), JsValue::Null) => s.is_empty(),
        (JsValue::Undefined, _) | (_, JsValue::Undefined) => false,
        // 对象/数组:JSON 结构相等(简化)
        (JsValue::Arr(_) | JsValue::Obj(_), JsValue::Arr(_) | JsValue::Obj(_)) => {
            js_to_json(a) == js_to_json(b)
        }
        _ => false,
    }
}

/// 比较运算符(JS 语义简化):双方可转数字按数字比,否则字符串比
pub(super) fn compare(op: BinOp, a: &JsValue, b: &JsValue) -> bool {
    let na = js_to_num(a);
    let nb = js_to_num(b);
    let use_num = match (a, b) {
        (JsValue::Num(_), _) | (_, JsValue::Num(_)) => true,
        (JsValue::Str(x), JsValue::Str(y)) => {
            x.trim().parse::<f64>().is_ok() && y.trim().parse::<f64>().is_ok()
        }
        _ => false,
    };
    let (ord, valid) = if use_num {
        if na.is_nan() || nb.is_nan() {
            (std::cmp::Ordering::Equal, false)
        } else {
            (
                na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal),
                true,
            )
        }
    } else {
        (js_to_string(a).cmp(&js_to_string(b)), true)
    };
    if !valid {
        return false;
    }
    match op {
        BinOp::Lt => ord == std::cmp::Ordering::Less,
        BinOp::Le => ord != std::cmp::Ordering::Greater,
        BinOp::Gt => ord == std::cmp::Ordering::Greater,
        BinOp::Ge => ord != std::cmp::Ordering::Less,
        _ => false,
    }
}
