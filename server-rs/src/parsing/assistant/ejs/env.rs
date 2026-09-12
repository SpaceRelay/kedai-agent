// 执行环境:Env 作用域链(闭包捕获用 Rc)与 Flow 控制流信号(正常/break/continue/return),
// 以及内建全局对象(builtin_global)与属性读写(get_prop/set_prop)。
// Env 持有 AssistantVars 引用与模板输出缓冲,供 eval.rs/exec.rs 使用。
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::super::AssistantVars;
use super::super::RenderCtxData;
use super::value::*;

#[derive(Debug, Clone)]
pub(super) enum Flow {
    Normal,
    Break,
    Continue,
    Return(JsValue),
}

pub(super) struct Env<'a, 'd> {
    /// 作用域链(从内到外;Rc 使闭包捕获持有引用而非值快照)
    pub(super) scopes: Vec<Rc<RefCell<HashMap<String, JsValue>>>>,
    pub(super) vars: &'a mut AssistantVars,
    pub(super) depth: usize,
    /// 模板输出缓冲(__kedai_out__ 追加)
    pub(super) out: &'a mut String,
    /// 渲染上下文(世界书/角色/历史/快速回复/预设 + injectPrompt 注入清单);
    /// 内建读取类函数(getwi/getchar/getqr/getChatMessage 等)经此读写
    pub(super) ctx: &'a mut RenderCtxData<'d>,
}

pub(super) const MAX_CALL_DEPTH: usize = 128;

impl<'a, 'd> Env<'a, 'd> {
    pub(super) fn new(
        vars: &'a mut AssistantVars,
        locals: &[(&str, JsValue)],
        out: &'a mut String,
        ctx: &'a mut RenderCtxData<'d>,
    ) -> Self {
        let mut top = HashMap::new();
        for (k, v) in locals {
            top.insert(k.to_string(), v.clone());
        }
        // ST-Prompt-Template 兼容常量:variables = 变量树根(即 getvar(null) 语义)。
        // 仅当模板未自定义同名局部变量时注入,避免覆盖用户定义。
        // 计划二:scopes 上下文存在时改为合并视图(含 global/character 等作用域键),
        // 否则保持变量树根(既有行为)。
        if !top.contains_key("variables") {
            let root = if let Some(sv) = ctx.scopes.as_deref() {
                sv.view("").unwrap_or_else(|| vars.tree().clone())
            } else {
                vars.tree().clone()
            };
            top.insert("variables".into(), value_to_js(&root));
        }
        Env {
            scopes: vec![Rc::new(RefCell::new(top))],
            vars,
            depth: 0,
            out,
            ctx,
        }
    }

    pub(super) fn push_scope(&mut self) {
        self.scopes.push(Rc::new(RefCell::new(HashMap::new())));
    }

    pub(super) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub(super) fn declare(&mut self, name: &str, value: JsValue) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.borrow_mut().insert(name.to_string(), value);
        }
    }

    pub(super) fn lookup(&self, name: &str) -> Option<JsValue> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.borrow().get(name) {
                return Some(v.clone());
            }
        }
        builtin_global(name)
    }

    pub(super) fn assign(&mut self, name: &str, value: JsValue) {
        // 最近作用域已存在则更新(闭包可修改外部变量),否则在最内层创建
        for scope in self.scopes.iter().rev() {
            if scope.borrow().contains_key(name) {
                scope.borrow_mut().insert(name.to_string(), value);
                return;
            }
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.borrow_mut().insert(name.to_string(), value);
        }
    }

    /// 捕获当前作用域链引用(闭包)
    pub(super) fn capture(&self) -> Vec<Rc<RefCell<HashMap<String, JsValue>>>> {
        self.scopes.clone()
    }
}

fn builtin_global(name: &str) -> Option<JsValue> {
    match name {
        "getvar" => Some(JsValue::Builtin(Builtin::GetVar)),
        // 计划二 · 7 作用域变量:别名映射到真实作用域 builtin(scopes 上下文存在时生效,
        // 缺失回退旧 GetVar/SetVar 行为)
        "getMessageVar" | "get_message_variable" | "get_message_var" => {
            Some(JsValue::Builtin(Builtin::GetMessageVar))
        }
        "getGlobalVar" | "get_global_variable" => Some(JsValue::Builtin(Builtin::GetGlobalVar)),
        "getCharacterVar" | "get_character_variable" => {
            Some(JsValue::Builtin(Builtin::GetCharacterVar))
        }
        "getPresetVar" | "get_preset_variable" => Some(JsValue::Builtin(Builtin::GetPresetVar)),
        // getLocalVar(酒馆助手局部变量读取)同样映射到变量树(chat 作用域)
        "getLocalVar" | "get_local_variable" => Some(JsValue::Builtin(Builtin::GetVar)),
        // matchChatMessages(酒馆助手:检查最近消息是否含关键词):渲染上下文无历史,
        // 降级为不匹配(false),避免未定义函数报错丢弃整条内容
        "matchChatMessages" => Some(JsValue::Builtin(Builtin::MatchChatMessages)),
        // ===== ST-Prompt-Template 兼容:无上下文容错读取函数(均返回空字符串/空数组) =====
        // getwi/getWorldInfo(世界书条目读取):kedai 无世界书读取上下文 → ""
        "getwi" | "getWorldInfo" => Some(JsValue::Builtin(Builtin::GetWorldInfo)),
        // getchar/getChara(角色卡信息读取)→ ""
        "getchar" | "getChara" => Some(JsValue::Builtin(Builtin::GetChara)),
        // getpreset/getPresetPrompt(预设提示词读取)→ ""
        "getpreset" | "getPresetPrompt" => Some(JsValue::Builtin(Builtin::GetPreset)),
        // getqr/getQuickReply(快捷回复读取)→ ""
        "getqr" | "getQuickReply" => Some(JsValue::Builtin(Builtin::GetQuickReply)),
        // getChatMessage(单条聊天消息读取,任意 idx/role)→ ""
        "getChatMessage" => Some(JsValue::Builtin(Builtin::GetChatMessage)),
        // getChatMessages(聊天消息列表读取,任意参数数量)→ 空数组
        "getChatMessages" => Some(JsValue::Builtin(Builtin::GetChatMessages)),
        // injectPrompt(注入提示词登记):engine 层注入清单未接入,静默成功返回 ""
        "injectPrompt" => Some(JsValue::Builtin(Builtin::InjectPrompt)),
        // getPromptsInjected(读取注入清单)→ "" ;hasPromptsInjected(是否有注入)→ false
        "getPromptsInjected" => Some(JsValue::Builtin(Builtin::GetPromptsInjected)),
        "hasPromptsInjected" => Some(JsValue::Builtin(Builtin::HasPromptsInjected)),
        // parseJSON(宽松 JSON 解析,失败返回原字符串)/ jsonPatch(容错,返回原对象)
        "parseJSON" => Some(JsValue::Builtin(Builtin::ParseJSON)),
        "jsonPatch" => Some(JsValue::Builtin(Builtin::JsonPatch)),
        // evalTemplate(嵌套渲染,调用 render_template 自身;失败返回原文)
        "evalTemplate" => Some(JsValue::Builtin(Builtin::EvalTemplate)),
        // print(...args):追加到输出,与 __kedai_out__ 语义一致
        "print" => Some(JsValue::Builtin(Builtin::Print)),
        // 变量写入别名:setGlobalVar → global 作用域;setLocalVar → chat 树;setMessageVar → message 作用域
        "setGlobalVar" => Some(JsValue::Builtin(Builtin::SetGlobalVar)),
        "setLocalVar" => Some(JsValue::Builtin(Builtin::SetLocalVar)),
        "setMessageVar" => Some(JsValue::Builtin(Builtin::SetMessageVar)),
        // incvar/decvar:变量自增/自减(与 ST-Prompt-Template 语义一致)
        "incvar" => Some(JsValue::Builtin(Builtin::IncVar)),
        "decvar" => Some(JsValue::Builtin(Builtin::DecVar)),
        "setvar" => Some(JsValue::Builtin(Builtin::SetVar)),
        "addvar" => Some(JsValue::Builtin(Builtin::AddVar)),
        "__kedai_out__" => Some(JsValue::Builtin(Builtin::Out)),
        "parseInt" => Some(JsValue::Builtin(Builtin::ParseInt)),
        "parseFloat" => Some(JsValue::Builtin(Builtin::ParseFloat)),
        "Number" => Some(JsValue::Builtin(Builtin::Number)),
        "String" => Some(JsValue::Builtin(Builtin::Str)),
        "Boolean" => Some(JsValue::Builtin(Builtin::Bool)),
        "isNaN" => Some(JsValue::Builtin(Builtin::IsNaN)),
        "JSON" => Some(JsValue::Obj(vec![
            ("stringify".into(), JsValue::Builtin(Builtin::JsonStringify)),
            ("parse".into(), JsValue::Builtin(Builtin::JsonParse)),
        ])),
        "Math" => Some(JsValue::Obj(vec![
            ("PI".into(), JsValue::Num(std::f64::consts::PI)),
            ("E".into(), JsValue::Num(std::f64::consts::E)),
            ("abs".into(), JsValue::Builtin(Builtin::MathAbs)),
            ("ceil".into(), JsValue::Builtin(Builtin::MathCeil)),
            ("floor".into(), JsValue::Builtin(Builtin::MathFloor)),
            ("round".into(), JsValue::Builtin(Builtin::MathRound)),
            ("trunc".into(), JsValue::Builtin(Builtin::MathTrunc)),
            ("max".into(), JsValue::Builtin(Builtin::MathMax)),
            ("min".into(), JsValue::Builtin(Builtin::MathMin)),
            ("pow".into(), JsValue::Builtin(Builtin::MathPow)),
            ("random".into(), JsValue::Builtin(Builtin::MathRandom)),
            ("sqrt".into(), JsValue::Builtin(Builtin::MathSqrt)),
        ])),
        "Object" => Some(JsValue::Obj(vec![
            ("keys".into(), JsValue::Builtin(Builtin::ObjKeys)),
            ("values".into(), JsValue::Builtin(Builtin::ObjValues)),
            ("entries".into(), JsValue::Builtin(Builtin::ObjEntries)),
        ])),
        _ => None,
    }
}

pub(super) fn get_prop(recv: &JsValue, prop: &str) -> JsValue {
    match recv {
        JsValue::Str(s) => match prop {
            "length" => JsValue::Num(s.chars().count() as f64),
            "toUpperCase" => JsValue::Method(Builtin::StrToUpperCase, Box::new(recv.clone())),
            "toLowerCase" => JsValue::Method(Builtin::StrToLowerCase, Box::new(recv.clone())),
            "includes" => JsValue::Method(Builtin::StrIncludes, Box::new(recv.clone())),
            "startsWith" => JsValue::Method(Builtin::StrStartsWith, Box::new(recv.clone())),
            "endsWith" => JsValue::Method(Builtin::StrEndsWith, Box::new(recv.clone())),
            "split" => JsValue::Method(Builtin::StrSplit, Box::new(recv.clone())),
            "trim" => JsValue::Method(Builtin::StrTrim, Box::new(recv.clone())),
            "trimEnd" | "trimRight" => JsValue::Method(Builtin::StrTrimEnd, Box::new(recv.clone())),
            "repeat" => JsValue::Method(Builtin::StrRepeat, Box::new(recv.clone())),
            "replace" => JsValue::Method(Builtin::StrReplace, Box::new(recv.clone())),
            "charAt" => JsValue::Method(Builtin::StrCharAt, Box::new(recv.clone())),
            "indexOf" => JsValue::Method(Builtin::StrIndexOf, Box::new(recv.clone())),
            "slice" => JsValue::Method(Builtin::StrSlice, Box::new(recv.clone())),
            "substring" => JsValue::Method(Builtin::StrSubstring, Box::new(recv.clone())),
            _ => JsValue::Undefined,
        },
        JsValue::Arr(_) => match prop {
            "length" => {
                if let JsValue::Arr(items) = recv {
                    JsValue::Num(items.len() as f64)
                } else {
                    JsValue::Undefined
                }
            }
            "push" => JsValue::Method(Builtin::ArrPush, Box::new(recv.clone())),
            "pop" => JsValue::Method(Builtin::ArrPop, Box::new(recv.clone())),
            "join" => JsValue::Method(Builtin::ArrJoin, Box::new(recv.clone())),
            "includes" => JsValue::Method(Builtin::ArrIncludes, Box::new(recv.clone())),
            "indexOf" => JsValue::Method(Builtin::ArrIndexOf, Box::new(recv.clone())),
            "forEach" => JsValue::Method(Builtin::ArrForEach, Box::new(recv.clone())),
            "map" => JsValue::Method(Builtin::ArrMap, Box::new(recv.clone())),
            "filter" => JsValue::Method(Builtin::ArrFilter, Box::new(recv.clone())),
            "find" => JsValue::Method(Builtin::ArrFind, Box::new(recv.clone())),
            _ => JsValue::Undefined,
        },
        JsValue::Obj(fields) => fields
            .iter()
            .find(|(k, _)| k == prop)
            .map(|(_, v)| v.clone())
            .unwrap_or(JsValue::Undefined),
        _ => JsValue::Undefined,
    }
}

pub(super) fn set_prop(recv: &mut JsValue, prop: &str, value: JsValue) -> Result<(), String> {
    match recv {
        JsValue::Obj(fields) => {
            if let Some((_, v)) = fields.iter_mut().find(|(k, _)| k == prop) {
                *v = value;
            } else {
                fields.push((prop.to_string(), value));
            }
            Ok(())
        }
        JsValue::Arr(_) => {
            // 数组属性赋值:忽略(简化)
            Ok(())
        }
        _ => Err(format!("不能给 {recv:?} 设置属性 {prop}")),
    }
}
