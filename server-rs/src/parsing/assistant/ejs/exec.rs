// 语句执行:exec 逐语句解释(含 for/while 作用域与控制流传播),以及函数调用
// call/call_builtin(用户函数闭包展开 + 全部内建函数实现)。
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value;

use super::super::{InjectedPrompt, RenderCtx, RenderCtxData};
use super::ast::*;
use super::env::*;
use super::eval::*;
use super::value::*;
use crate::parsing::scopes::Scope;

// evalTemplate 嵌套渲染深度计数(线程局部):
// render_template 每次新建 Env(depth 重置 0),JS 调用深度无法约束嵌套渲染,
// 故用独立计数防止 `<%= evalTemplate(...) %>` 无限递归导致栈溢出。
// 上限取 16:正常模板嵌套渲染仅 1~3 层;debug 构建下每层 render_template 的
// Rust 栈帧开销较大,128 层会栈溢出,16 层在 debug/release 下均安全。
std::thread_local! {
    static EVAL_TEMPLATE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// evalTemplate 嵌套渲染深度上限
const MAX_EVAL_TEMPLATE_DEPTH: usize = 16;

// ===== 渲染资源预算(线程局部)=====
// 角色卡属外部不可信输入:模板可含 `while(true){}` / `for(;;){}` 等死循环,
// 而渲染在 async fn 内**同步**执行(worldbook.rs / inject_tag.rs 等),会挂死 tokio worker;
// 深嵌套表达式还会让递归下降解析器爆栈。故对「单次顶层渲染」设迭代步数与墙钟双上限。
//
// 预算计数为 0 表示**不限制**(未进入渲染入口时的默认值):这样既保证旧调用路径行为不变,
// 又避免任何入口遗漏导致误报。仅最外层渲染入口设置预算,嵌套 evalTemplate 共享同一预算
// (见 enter_render/leave_render)。
std::thread_local! {
    static RENDER_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static RENDER_DEADLINE: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
    static RENDER_NEST_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 单次顶层渲染的循环迭代总步数上限(正常角色卡用量在数百级,20 万留足余量)
pub(super) const MAX_RENDER_STEPS: u64 = 200_000;
/// 单次顶层渲染的墙钟上限(毫秒);同步执行无法被 tokio 超时打断,故循环内自查
pub(super) const MAX_RENDER_MILLIS: u128 = 2_000;

/// 进入一次渲染:仅最外层设置预算与墙钟基准;返回 true 表示调用方需在退出时恢复。
/// 嵌套渲染(evalTemplate → render_template_with_ctx)复用外层预算,不重置——
/// 否则内层可无限次「重新充满」预算绕过限制。
fn enter_render() -> bool {
    RENDER_NEST_DEPTH.with(|d| {
        let outermost = d.get() == 0;
        d.set(d.get() + 1);
        if outermost {
            RENDER_STEPS.with(|b| b.set(MAX_RENDER_STEPS));
            RENDER_DEADLINE.with(|dl| dl.set(Some(std::time::Instant::now())));
        }
        outermost
    })
}

/// 退出一次渲染(参数即为 enter_render 的返回值);最外层退出时清空预算。
fn leave_render(outermost: bool) {
    RENDER_NEST_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    if outermost {
        RENDER_STEPS.with(|b| b.set(0));
        RENDER_DEADLINE.with(|dl| dl.set(None));
    }
}

/// 渲染预算 RAII 守卫:渲染入口持有一个,函数退出(含 panic 展开)时自动恢复,
/// 避免预算计数泄漏到同线程的下一次渲染。
pub(super) struct RenderGuard {
    outermost: bool,
}

impl RenderGuard {
    pub(super) fn new() -> Self {
        RenderGuard {
            outermost: enter_render(),
        }
    }
}

impl Drop for RenderGuard {
    fn drop(&mut self) {
        leave_render(self.outermost);
    }
}

/// 循环每轮「收费」:先扣迭代预算,再查墙钟是否超出。返回 Err 时由 exec 逐层冒泡,
/// 最终被 render_template_with_ctx 的逐语句错误收集吞掉(该块回退原文,不影响其余块)。
pub(super) fn charge_step() -> Result<(), String> {
    RENDER_STEPS.with(|b| -> Result<(), String> {
        let left = b.get();
        // 0 = 未设预算(不限制);扣到 1 即最后一轮,视为耗尽
        if left == 0 {
            return Ok(());
        }
        if left == 1 {
            return Err("EJS 渲染超出步数预算(疑似死循环,已中止)".to_string());
        }
        b.set(left - 1);
        Ok(())
    })?;
    RENDER_DEADLINE.with(|dl| -> Result<(), String> {
        if let Some(t0) = dl.get() {
            if t0.elapsed().as_millis() >= MAX_RENDER_MILLIS {
                return Err("EJS 渲染超出时间预算(2 秒,已中止)".to_string());
            }
        }
        Ok(())
    })
}

/// 嵌套渲染(evalTemplate / getwi / getqr / getpreset 的内容渲染共用):
/// 共享同一渲染上下文(重新拷贝引用),嵌套登记的注入清单合并回父级;
/// 深度超限或渲染出错返回原文,保证模板不炸、不无限递归。
fn render_nested(env: &mut Env<'_, '_>, content: &str, locals: &[(&str, JsValue)]) -> String {
    EVAL_TEMPLATE_DEPTH.with(|d| {
        if d.get() >= MAX_EVAL_TEMPLATE_DEPTH {
            return content.to_string();
        }
        d.set(d.get() + 1);
        let mut nctx = RenderCtx {
            vars: &mut *env.vars,
            data: RenderCtxData {
                character: env.ctx.character,
                world_entries: env.ctx.world_entries,
                history: env.ctx.history,
                quick_replies: env.ctx.quick_replies,
                preset_prompts: env.ctx.preset_prompts,
                scopes: env.ctx.scopes.as_deref_mut(),
                injected: Vec::new(),
            },
        };
        let (out, _errs) = super::render_template_with_ctx(content, &mut nctx, locals);
        env.ctx.injected.extend(nctx.data.injected);
        d.set(d.get() - 1);
        out
    })
}

pub(super) fn exec(env: &mut Env, stmt: &Stmt) -> Result<Flow, String> {
    match stmt {
        Stmt::Empty => Ok(Flow::Normal),
        Stmt::Expr(e) => {
            eval_expr(env, e)?;
            Ok(Flow::Normal)
        }
        Stmt::Var(decls) => {
            for (name, value) in decls {
                let v = match value {
                    Some(e) => eval_expr(env, e)?,
                    None => JsValue::Undefined,
                };
                env.declare(name, v);
            }
            Ok(Flow::Normal)
        }
        Stmt::If { cond, then, els } => {
            if truthy(&eval_expr(env, cond)?) {
                exec(env, then)
            } else if let Some(e) = els {
                exec(env, e)
            } else {
                Ok(Flow::Normal)
            }
        }
        Stmt::Block(stmts) => {
            env.push_scope();
            let mut flow = Flow::Normal;
            for s in stmts {
                flow = exec(env, s)?;
                if !matches!(flow, Flow::Normal) {
                    break;
                }
            }
            env.pop_scope();
            Ok(flow)
        }
        Stmt::For {
            init,
            cond,
            update,
            body,
        } => {
            if let Some(i) = init {
                let f = exec(env, i)?;
                if !matches!(f, Flow::Normal) {
                    return Ok(f);
                }
            }
            loop {
                if let Some(c) = cond {
                    if !truthy(&eval_expr(env, c)?) {
                        break;
                    }
                }
                // 资源预算:防 `for(;;)` 死循环挂死 worker
                charge_step()?;
                env.push_scope();
                let flow = exec(env, body)?;
                if matches!(flow, Flow::Break | Flow::Return(_)) {
                    env.pop_scope();
                    return Ok(flow);
                }
                env.pop_scope();
                if let Some(u) = update {
                    eval_expr(env, u)?;
                }
            }
            Ok(Flow::Normal)
        }
        Stmt::ForOf { name, iter, body } => {
            let it = eval_expr(env, iter)?;
            let items: Vec<JsValue> = match &it {
                JsValue::Arr(a) => a.clone(),
                JsValue::Str(s) => s.chars().map(|c| JsValue::Str(c.to_string())).collect(),
                _ => Vec::new(),
            };
            for item in items {
                // 资源预算:数组可被脚本放大,同样计入循环步数
                charge_step()?;
                env.push_scope();
                env.declare(name, item);
                let flow = exec(env, body)?;
                if matches!(flow, Flow::Break | Flow::Return(_)) {
                    env.pop_scope();
                    return Ok(flow);
                }
                env.pop_scope();
            }
            Ok(Flow::Normal)
        }
        Stmt::While { cond, body } => {
            loop {
                if !truthy(&eval_expr(env, cond)?) {
                    break;
                }
                // 资源预算:防 `while(true)` 死循环挂死 worker
                charge_step()?;
                env.push_scope();
                let flow = exec(env, body)?;
                if matches!(flow, Flow::Break | Flow::Return(_)) {
                    env.pop_scope();
                    return Ok(flow);
                }
                env.pop_scope();
            }
            Ok(Flow::Normal)
        }
        Stmt::Function { name, params, body } => {
            let f = JsValue::Fn(JsFn {
                params: params.clone(),
                body: FnBody::Block(body.clone()),
                captured: env.capture(),
            });
            env.declare(name, f);
            Ok(Flow::Normal)
        }
        Stmt::Return(v) => {
            let val = match v {
                Some(e) => eval_expr(env, e)?,
                None => JsValue::Undefined,
            };
            Ok(Flow::Return(val))
        }
        Stmt::Break => Ok(Flow::Break),
        Stmt::Continue => Ok(Flow::Continue),
    }
}

pub(super) fn call(
    env: &mut Env,
    f: &JsValue,
    recv: Option<&JsValue>,
    args: &[JsValue],
) -> Result<JsValue, String> {
    match f {
        JsValue::Fn(fun) => {
            if env.depth >= MAX_CALL_DEPTH {
                return Err("函数调用深度超限".into());
            }
            let mut scope = HashMap::new();
            // 1) 先绑定显式传入的参数
            for (i, p) in fun.params.iter().enumerate() {
                if let Some(v) = args.get(i) {
                    scope.insert(p.name.clone(), v.clone());
                }
            }
            // 2) 缺参参数求默认值(默认值在函数作用域内求值,可引用前面已绑定的参数,
            //    如 (x, y = x * 2) => y;与 JS 默认参数语义一致)
            for (i, p) in fun.params.iter().enumerate() {
                if args.get(i).is_some() {
                    continue;
                }
                match &p.default {
                    Some(e) => {
                        let mut sub_scopes = fun.captured.clone();
                        sub_scopes.push(Rc::new(RefCell::new(scope.clone())));
                        let vars = &mut *env.vars;
                        let out = &mut *env.out;
                        let mut tmp = Env {
                            scopes: sub_scopes,
                            vars,
                            depth: env.depth + 1,
                            out,
                            ctx: &mut *env.ctx,
                        };
                        let val = eval_expr(&mut tmp, e)?;
                        scope.insert(p.name.clone(), val);
                    }
                    None => {
                        scope.insert(p.name.clone(), JsValue::Undefined);
                    }
                }
            }
            // 作用域链 = 定义处捕获的链 + 本次调用参数
            let mut sub_scopes = fun.captured.clone();
            sub_scopes.push(Rc::new(RefCell::new(scope)));
            let vars = &mut *env.vars;
            let out = &mut *env.out;
            let mut sub = Env {
                scopes: sub_scopes,
                vars,
                depth: env.depth + 1,
                out,
                ctx: &mut *env.ctx,
            };
            match &fun.body {
                FnBody::Block(stmts) => {
                    let mut flow = Flow::Normal;
                    for s in stmts {
                        flow = exec(&mut sub, s)?;
                        if matches!(flow, Flow::Return(_)) {
                            break;
                        }
                    }
                    match flow {
                        Flow::Return(v) => Ok(v),
                        _ => Ok(JsValue::Undefined),
                    }
                }
                FnBody::Expr(e) => eval_expr(&mut sub, e),
            }
        }
        JsValue::Builtin(b) => call_builtin(env, *b, recv, args),
        JsValue::Method(b, bound_recv) => call_builtin(env, *b, Some(bound_recv.as_ref()), args),
        _ => Err(format!("值不是函数: {f:?}")),
    }
}

fn call_builtin(
    env: &mut Env,
    b: Builtin,
    recv: Option<&JsValue>,
    args: &[JsValue],
) -> Result<JsValue, String> {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(JsValue::Undefined);
    let arg_str = |i: usize| js_to_string(&arg(i));
    let recv_str = || recv.map(js_to_string).unwrap_or_default();
    let recv_arr = || match recv {
        Some(JsValue::Arr(a)) => a.clone(),
        _ => Vec::new(),
    };
    match b {
        Builtin::Out => {
            let s = js_to_string(&arg(0));
            env.out.push_str(&s);
            Ok(JsValue::Undefined)
        }
        Builtin::GetVar => {
            let path = arg_str(0);
            // 计划二:scopes 上下文存在时读合并视图(message→chat树→chat扁平→…);
            // chat 树以 env.vars(实时权威)优先——scopes.chat_tree 为回合边界镜像,
            // 渲染回合内 @@var/EJS setvar(stat_data 路径)写入必须实时可读。
            // 无 scopes 保持既有行为(直接读变量树)
            match env.ctx.scopes.as_deref() {
                Some(sv) => {
                    if let Some(v) = env.vars.get_value(&path) {
                        Ok(value_to_js(v))
                    } else {
                        Ok(sv
                            .view(&path)
                            .map(|v| value_to_js(&v))
                            .unwrap_or(JsValue::Undefined))
                    }
                }
                None => Ok(env.vars.get_js(&path)),
            }
        }
        Builtin::SetVar => {
            let path = arg_str(0);
            let value = arg(1);
            // 计划二:scopes 上下文存在时,默认写 message 作用域(ST 语义);
            // 但 stat_data.* 或已存在于 chat 树中的路径仍写 chat 树(兼容规则 §2.2),
            // 与引擎既有行为一致。无 scopes 保持旧行为(写变量树)。
            match env.ctx.scopes.as_deref_mut() {
                Some(sv) => {
                    let p = path.trim();
                    let is_tree_path = p == "stat_data"
                        || p.starts_with("stat_data.")
                        || env.vars.get_value(&path).is_some();
                    if is_tree_path {
                        env.vars.set_js(&path, value);
                    } else {
                        sv.write(Scope::Message, &path, js_to_json(&value));
                    }
                    Ok(JsValue::Undefined)
                }
                None => {
                    env.vars.set_js(&path, value);
                    Ok(JsValue::Undefined)
                }
            }
        }
        Builtin::AddVar => {
            let path = arg_str(0);
            let delta = js_to_num(&arg(1));
            env.vars.add_js(&path, delta);
            Ok(JsValue::Undefined)
        }
        Builtin::ParseInt => {
            let s = arg_str(0);
            let digits: String = s
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            let n = if digits.is_empty() {
                f64::NAN
            } else {
                digits.parse::<f64>().unwrap_or(f64::NAN)
            };
            Ok(JsValue::Num(n))
        }
        Builtin::ParseFloat => {
            let s = arg_str(0).trim_start().to_string();
            let mut end = 0;
            let bytes = s.as_bytes();
            let mut seen_dot = false;
            while end < bytes.len() {
                // 循环条件保证 end 在界内,必有下一字符
                let c = s[end..].chars().next().expect("end 在界内,必有下一字符");
                if c.is_ascii_digit() {
                    end += 1;
                } else if c == '.' && !seen_dot {
                    seen_dot = true;
                    end += 1;
                } else {
                    break;
                }
            }
            let n = if end == 0 {
                f64::NAN
            } else {
                s[..end].parse::<f64>().unwrap_or(f64::NAN)
            };
            Ok(JsValue::Num(n))
        }
        Builtin::Number => Ok(JsValue::Num(js_to_num(&arg(0)))),
        Builtin::Str => Ok(JsValue::Str(js_to_string(&arg(0)))),
        Builtin::Bool => Ok(JsValue::Bool(truthy(&arg(0)))),
        Builtin::IsNaN => Ok(JsValue::Bool(js_to_num(&arg(0)).is_nan())),
        Builtin::MathAbs => Ok(JsValue::Num(js_to_num(&arg(0)).abs())),
        Builtin::MathCeil => Ok(JsValue::Num(js_to_num(&arg(0)).ceil())),
        Builtin::MathFloor => Ok(JsValue::Num(js_to_num(&arg(0)).floor())),
        Builtin::MathRound => Ok(JsValue::Num(js_to_num(&arg(0)).round())),
        Builtin::MathTrunc => Ok(JsValue::Num(js_to_num(&arg(0)).trunc())),
        Builtin::MathMax => Ok(JsValue::Num(
            args.iter().map(js_to_num).fold(f64::NEG_INFINITY, f64::max),
        )),
        Builtin::MathMin => Ok(JsValue::Num(
            args.iter().map(js_to_num).fold(f64::INFINITY, f64::min),
        )),
        Builtin::MathPow => Ok(JsValue::Num(js_to_num(&arg(0)).powf(js_to_num(&arg(1))))),
        Builtin::MathRandom => {
            use std::time::{SystemTime, UNIX_EPOCH};
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let mut seed = (nanos as u64) ^ 0x9E3779B97F4A7C15;
            seed = seed
                .wrapping_mul(0x2545F4914F6CDD1D)
                .wrapping_add(0x9E3779B9);
            seed ^= seed >> 30;
            let x = (seed >> 11) as f64 / (1u64 << 53) as f64;
            Ok(JsValue::Num(x))
        }
        Builtin::MathSqrt => Ok(JsValue::Num(js_to_num(&arg(0)).sqrt())),
        Builtin::StrToUpperCase => Ok(JsValue::Str(recv_str().to_uppercase())),
        Builtin::StrToLowerCase => Ok(JsValue::Str(recv_str().to_lowercase())),
        Builtin::StrIncludes => Ok(JsValue::Bool(recv_str().contains(&arg_str(0)))),
        Builtin::StrStartsWith => Ok(JsValue::Bool(recv_str().starts_with(&arg_str(0)))),
        Builtin::StrEndsWith => Ok(JsValue::Bool(recv_str().ends_with(&arg_str(0)))),
        Builtin::StrSplit => {
            let sep = arg_str(0);
            let parts: Vec<JsValue> = if sep.is_empty() {
                recv_str()
                    .chars()
                    .map(|c| JsValue::Str(c.to_string()))
                    .collect()
            } else {
                recv_str()
                    .split(&sep)
                    .map(|s| JsValue::Str(s.to_string()))
                    .collect()
            };
            Ok(JsValue::Arr(parts))
        }
        Builtin::StrTrim => Ok(JsValue::Str(recv_str().trim().to_string())),
        Builtin::StrTrimEnd => Ok(JsValue::Str(recv_str().trim_end().to_string())),
        Builtin::StrRepeat => {
            let n = js_to_num(&arg(0)) as usize;
            Ok(JsValue::Str(recv_str().repeat(n)))
        }
        Builtin::StrReplace => {
            let from = arg_str(0);
            let to = arg_str(1);
            Ok(JsValue::Str(recv_str().replacen(&from, &to, 1)))
        }
        Builtin::StrCharAt => {
            let idx = js_to_num(&arg(0)) as usize;
            Ok(JsValue::Str(
                recv_str()
                    .chars()
                    .nth(idx)
                    .map(|c| c.to_string())
                    .unwrap_or_default(),
            ))
        }
        Builtin::StrIndexOf => {
            let s = recv_str();
            let needle = arg_str(0);
            Ok(JsValue::Num(
                s.find(&needle)
                    .map(|b| s[..b].chars().count() as f64)
                    .unwrap_or(-1.0),
            ))
        }
        Builtin::StrSlice => {
            let s = recv_str();
            let chars: Vec<char> = s.chars().collect();
            let start = js_to_num(&arg(0)) as isize;
            let end = if args.len() > 1 {
                js_to_num(&arg(1)) as isize
            } else {
                chars.len() as isize
            };
            let norm = |i: isize| -> usize {
                let n = if i < 0 { chars.len() as isize + i } else { i };
                n.clamp(0, chars.len() as isize) as usize
            };
            let (a, b) = (norm(start), norm(end));
            if b <= a {
                Ok(JsValue::Str(String::new()))
            } else {
                Ok(JsValue::Str(chars[a..b].iter().collect()))
            }
        }
        Builtin::StrSubstring => {
            let s = recv_str();
            let chars: Vec<char> = s.chars().collect();
            let mut start = js_to_num(&arg(0)) as isize;
            let mut end = if args.len() > 1 {
                js_to_num(&arg(1)) as isize
            } else {
                chars.len() as isize
            };
            if start > end {
                std::mem::swap(&mut start, &mut end);
            }
            let norm = |i: isize| -> usize { i.clamp(0, chars.len() as isize) as usize };
            let (a, b) = (norm(start), norm(end));
            Ok(JsValue::Str(chars[a..b].iter().collect()))
        }
        Builtin::ArrPush => {
            // 值语义:返回追加后的新数组
            let mut items = recv_arr();
            items.push(arg(0));
            Ok(JsValue::Arr(items))
        }
        Builtin::ArrPop => {
            let mut items = recv_arr();
            let last = items.pop().unwrap_or(JsValue::Undefined);
            Ok(last)
        }
        Builtin::ArrJoin => Ok(JsValue::Str(
            recv_arr()
                .iter()
                .map(js_to_string)
                .collect::<Vec<_>>()
                .join(&arg_str(0)),
        )),
        Builtin::ArrIncludes => {
            let items = recv_arr();
            let target = arg(0);
            Ok(JsValue::Bool(items.iter().any(|x| loose_eq(x, &target))))
        }
        Builtin::ArrIndexOf => {
            let items = recv_arr();
            let target = arg(0);
            let idx = items.iter().position(|x| loose_eq(x, &target));
            Ok(JsValue::Num(idx.map(|i| i as f64).unwrap_or(-1.0)))
        }
        Builtin::ArrForEach => {
            let items = recv_arr();
            let cb = arg(0);
            for item in &items {
                call(env, &cb, None, &[item.clone(), JsValue::Num(0.0)])?;
            }
            Ok(JsValue::Undefined)
        }
        Builtin::ArrMap => {
            let items = recv_arr();
            let cb = arg(0);
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                let r = call(env, &cb, None, &[item.clone(), JsValue::Num(i as f64)])?;
                out.push(r);
            }
            Ok(JsValue::Arr(out))
        }
        Builtin::ArrFilter => {
            let items = recv_arr();
            let cb = arg(0);
            let mut out = Vec::new();
            for item in &items {
                let r = call(env, &cb, None, &[item.clone(), JsValue::Num(0.0)])?;
                if truthy(&r) {
                    out.push(item.clone());
                }
            }
            Ok(JsValue::Arr(out))
        }
        Builtin::ArrFind => {
            let items = recv_arr();
            let cb = arg(0);
            for (i, item) in items.iter().enumerate() {
                let r = call(env, &cb, None, &[item.clone(), JsValue::Num(i as f64)])?;
                if truthy(&r) {
                    return Ok(item.clone());
                }
            }
            Ok(JsValue::Undefined)
        }
        Builtin::MatchChatMessages => {
            // matchChatMessages(关键词[, role]):检查聊天历史是否含任一关键词(大小写不敏感,
            // 子串匹配);role 缺省/undefined = 不限角色;无历史上下文返回 false
            let history = env.ctx.history.unwrap_or(&[]);
            if history.is_empty() {
                return Ok(JsValue::Bool(false));
            }
            let keywords: Vec<String> = match &arg(0) {
                JsValue::Arr(items) => items.iter().map(js_to_string).collect(),
                v => vec![js_to_string(v)],
            };
            let role = match args.get(1) {
                Some(v) if !matches!(v, JsValue::Undefined) => js_to_string(v),
                _ => String::new(),
            };
            let hit = history.iter().any(|m| {
                if !role.is_empty() && !m.role.eq_ignore_ascii_case(&role) {
                    return false;
                }
                let lower = m.content.to_lowercase();
                keywords
                    .iter()
                    .any(|k| !k.is_empty() && lower.contains(&k.to_lowercase()))
            });
            Ok(JsValue::Bool(hit))
        }
        // ===== ST-Prompt-Template 兼容:读取类函数(经渲染上下文真实读取) =====
        // getwi/getWorldInfo(world?, uidOrName?, data?):按 id 或 comment(大小写不敏感)
        // 在世界书条目中查找,命中后对其 content 嵌套渲染(共享上下文;world_info 注入局部变量)
        Builtin::GetWorldInfo => {
            let entries = env.ctx.world_entries.unwrap_or(&[]);
            // 参数语义:getwi(uidOrName) 或 getwi(world, uidOrName[, data]);
            // 扁平条目集下取最后一个字符串/数字参数作为查找键,world 名忽略
            let needle: Option<String> = args
                .iter()
                .filter_map(|a| match a {
                    JsValue::Str(s) if !s.trim().is_empty() => Some(s.clone()),
                    JsValue::Num(n) => Some(fmt_num(*n)),
                    _ => None,
                })
                .next_back();
            let Some(needle) = needle else {
                return Ok(JsValue::Str(String::new()));
            };
            let found = entries.iter().find(|e| {
                e.id.to_string() == needle || e.comment.eq_ignore_ascii_case(needle.trim())
            });
            match found {
                Some(e) => {
                    let wi_json = serde_json::to_value(e).unwrap_or(Value::Null);
                    let locals = [("world_info", value_to_js(&wi_json))];
                    Ok(JsValue::Str(render_nested(env, &e.content, &locals)))
                }
                None => Ok(JsValue::Str(String::new())),
            }
        }
        // getchar/getChara(name?):按字段名返回角色信息(name/description/personality/scenario/
        // avatar/mes_example/first_mes);无角色上下文或无匹配字段 → ""
        Builtin::GetChara => {
            let Some(c) = env.ctx.character else {
                return Ok(JsValue::Str(String::new()));
            };
            let name = arg_str(0).to_lowercase();
            let key = name.trim();
            let value = match key {
                "" | "name" | "chara_name" => JsValue::Str(c.name.clone()),
                "description" | "desc" => JsValue::Str(c.description.clone()),
                "personality" => JsValue::Str(c.personality.clone()),
                "scenario" => JsValue::Str(c.scenario.clone()),
                "avatar" | "avatar_url" => JsValue::Str(c.avatar_url.clone().unwrap_or_default()),
                "mes_example" | "example" => JsValue::Str(c.mes_example.clone()),
                "first_mes" | "first_message" => JsValue::Str(c.first_mes.clone()),
                // 未知字段:返回空字符串(保持「查无 → 空值」容错语义,不输出 undefined)
                _ => JsValue::Str(String::new()),
            };
            Ok(value)
        }
        // getpreset/getPresetPrompt(name?):按名称(大小写不敏感)查提示词预设楼层,嵌套渲染
        Builtin::GetPreset => {
            let name = arg_str(0);
            let presets = env.ctx.preset_prompts.unwrap_or(&[]);
            match presets.iter().find(|p| p.name.eq_ignore_ascii_case(&name)) {
                Some(p) => Ok(JsValue::Str(render_nested(env, &p.content, &[]))),
                None => Ok(JsValue::Str(String::new())),
            }
        }
        // getqr/getQuickReply(name[, label]):按名称(+标签,均大小写不敏感)查快速回复,嵌套渲染
        Builtin::GetQuickReply => {
            let name = arg_str(0);
            let label = arg_str(1);
            let qrs = env.ctx.quick_replies.unwrap_or(&[]);
            let found = qrs.iter().find(|q| {
                q.name.eq_ignore_ascii_case(&name)
                    && (label.is_empty() || q.label.eq_ignore_ascii_case(&label))
            });
            match found {
                Some(q) => Ok(JsValue::Str(render_nested(env, &q.content, &[]))),
                None => Ok(JsValue::Str(String::new())),
            }
        }
        // getChatMessage(idx[, role]):按索引取消息内容(负数从尾数,-1 = 最新);
        // 可选 role(user/assistant/system)过滤;无匹配 → ""
        Builtin::GetChatMessage => {
            let history = env.ctx.history.unwrap_or(&[]);
            if history.is_empty() {
                return Ok(JsValue::Str(String::new()));
            }
            let idx = match &arg(0) {
                JsValue::Num(n) => *n as isize,
                _ => -1,
            };
            let role = match args.get(1) {
                Some(v) if !matches!(v, JsValue::Undefined) => js_to_string(v),
                _ => String::new(),
            };
            let index = if idx < 0 {
                history.len() as isize + idx
            } else {
                idx
            };
            let m = if index >= 0 {
                history
                    .get(index as usize)
                    .filter(|m| role.is_empty() || m.role.eq_ignore_ascii_case(&role))
            } else {
                None
            };
            Ok(JsValue::Str(
                m.map(|m| m.content.clone()).unwrap_or_default(),
            ))
        }
        // getChatMessages(...):返回历史消息对象数组({role, content})
        Builtin::GetChatMessages => {
            let history = env.ctx.history.unwrap_or(&[]);
            let msgs: Vec<JsValue> = history
                .iter()
                .map(|m| {
                    JsValue::Obj(vec![
                        ("role".into(), JsValue::Str(m.role.clone())),
                        ("content".into(), JsValue::Str(m.content.clone())),
                    ])
                })
                .collect();
            Ok(JsValue::Arr(msgs))
        }
        // injectPrompt(key, prompt, order?, sticky?, uid?):
        // 登记到渲染上下文注入清单(渲染后由 engine 并入提示词流),返回空字符串
        Builtin::InjectPrompt => {
            // 缺参/Undefined 参数视为空串(避免登记 "undefined" 假条目)
            let opt_str = |i: usize| match args.get(i) {
                Some(v) if !matches!(v, JsValue::Undefined) => js_to_string(v),
                _ => String::new(),
            };
            let key = opt_str(0);
            let prompt = opt_str(1);
            if key.trim().is_empty() && prompt.trim().is_empty() {
                return Ok(JsValue::Str(String::new()));
            }
            let order = match args.get(2) {
                Some(JsValue::Num(n)) => *n as i64,
                _ => 100,
            };
            let sticky = match args.get(3) {
                Some(JsValue::Bool(b)) => *b,
                _ => false,
            };
            let uid = opt_str(4);
            env.ctx.injected.push(InjectedPrompt {
                key,
                prompt,
                order,
                position: 0,
                sticky,
                uid,
            });
            Ok(JsValue::Str(String::new()))
        }
        // getPromptsInjected(key?):返回已登记注入的 JSON 数组字符串(key/prompt/order);
        // 无登记返回空字符串
        Builtin::GetPromptsInjected => {
            let key = match args.first() {
                Some(v) if !matches!(v, JsValue::Undefined) => js_to_string(v),
                _ => String::new(),
            };
            let list: Vec<Value> = env
                .ctx
                .injected
                .iter()
                .filter(|p| key.is_empty() || p.key == key)
                .map(|p| {
                    serde_json::json!({
                        "key": p.key,
                        "prompt": p.prompt,
                        "order": p.order,
                    })
                })
                .collect();
            if list.is_empty() {
                Ok(JsValue::Str(String::new()))
            } else {
                Ok(JsValue::Str(
                    serde_json::to_string(&list).unwrap_or_default(),
                ))
            }
        }
        // hasPromptsInjected(key?):是否有已登记的注入(key 过滤可选)
        Builtin::HasPromptsInjected => {
            let key = arg_str(0);
            Ok(JsValue::Bool(
                env.ctx
                    .injected
                    .iter()
                    .any(|p| key.is_empty() || p.key == key),
            ))
        }
        // parseJSON(text):宽松解析,成功返回解析值,失败原样返回字符串
        Builtin::ParseJSON => {
            let s = arg_str(0);
            match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(v) => Ok(value_to_js(&v)),
                // 宽松语义:解析失败不报错,返回原字符串
                Err(_) => Ok(JsValue::Str(s)),
            }
        }
        // jsonPatch(dest, change):容错返回原对象(浅克隆),change 参数忽略
        // (kedai 变量树更新走 getvar/setvar,无独立 patch 语义)
        Builtin::JsonPatch => Ok(arg(0)),
        // evalTemplate(content):嵌套渲染(与 getwi/getqr/getpreset 同一深度受控通道,
        // 共享渲染上下文;嵌套登记的注入清单合并回父级)。深度超限或出错返回原文。
        Builtin::EvalTemplate => Ok(JsValue::Str(render_nested(env, &arg_str(0), &[]))),
        // print(...args):全部参数追加到输出缓冲(与 __kedai_out__ 语义一致)
        Builtin::Print => {
            for a in args {
                env.out.push_str(&js_to_string(a));
            }
            Ok(JsValue::Undefined)
        }
        // incvar(path)/decvar(path):变量自增/自减(路径不存在按 0 起步)
        Builtin::IncVar => {
            env.vars.add_js(&arg_str(0), 1.0);
            Ok(JsValue::Undefined)
        }
        Builtin::DecVar => {
            env.vars.add_js(&arg_str(0), -1.0);
            Ok(JsValue::Undefined)
        }
        // ===== 计划二 · 7 作用域变量 builtin =====
        // 读:scopes 上下文存在时按作用域取值(缺失回退链);None 回退旧行为(树根即 stat_data)
        Builtin::GetGlobalVar => scoped_get(env, Scope::Global, &arg_str(0)),
        Builtin::GetMessageVar => scoped_get(env, Scope::Message, &arg_str(0)),
        Builtin::GetCharacterVar => scoped_get(env, Scope::Character, &arg_str(0)),
        Builtin::GetPresetVar => scoped_get(env, Scope::Preset, &arg_str(0)),
        // 写:setGlobalVar → global;setLocalVar → chat 树;setMessageVar → message 作用域
        Builtin::SetGlobalVar => scoped_set(env, Scope::Global, &arg_str(0), arg(1)),
        Builtin::SetLocalVar => scoped_set(env, Scope::Chat, &arg_str(0), arg(1)),
        Builtin::SetMessageVar => scoped_set(env, Scope::Message, &arg_str(0), arg(1)),
        Builtin::ObjKeys => {
            let v = arg(0);
            match v {
                JsValue::Obj(fields) => Ok(JsValue::Arr(
                    fields
                        .iter()
                        .map(|(k, _)| JsValue::Str(k.clone()))
                        .collect(),
                )),
                _ => Ok(JsValue::Arr(Vec::new())),
            }
        }
        Builtin::ObjValues => {
            let v = arg(0);
            match v {
                JsValue::Obj(fields) => Ok(JsValue::Arr(
                    fields.iter().map(|(_, val)| val.clone()).collect(),
                )),
                _ => Ok(JsValue::Arr(Vec::new())),
            }
        }
        Builtin::ObjEntries => {
            let v = arg(0);
            match v {
                JsValue::Obj(fields) => Ok(JsValue::Arr(
                    fields
                        .iter()
                        .map(|(k, val)| JsValue::Arr(vec![JsValue::Str(k.clone()), val.clone()]))
                        .collect(),
                )),
                _ => Ok(JsValue::Arr(Vec::new())),
            }
        }
        Builtin::JsonStringify => {
            let json = js_to_json(&arg(0));
            Ok(JsValue::Str(
                serde_json::to_string(&json).unwrap_or_default(),
            ))
        }
        Builtin::JsonParse => {
            let s = arg_str(0);
            match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(v) => Ok(value_to_js(&v)),
                Err(_) => Ok(JsValue::Undefined),
            }
        }
    }
}

/// 作用域读取 builtin 公共实现(计划二):scopes 上下文存在时按作用域取值,
/// 缺失回退链(ScopeVars::read_scope);均未命中再以 env.vars(实时 chat 树)兜底——
/// 保证渲染回合内 @@var/EJS 写入的树值实时可读。None 回退旧行为(直接读变量树)。
fn scoped_get(env: &mut Env<'_, '_>, scope: Scope, path: &str) -> Result<JsValue, String> {
    match env.ctx.scopes.as_deref() {
        Some(sv) => match sv.read_scope(scope, path) {
            Some(v) => Ok(value_to_js(&v)),
            None => Ok(env.vars.get_js(path)),
        },
        None => Ok(env.vars.get_js(path)),
    }
}

/// 作用域写入 builtin 公共实现(计划二):scopes 上下文存在时写目标作用域;
/// None 回退旧行为(直接写变量树)。
fn scoped_set(
    env: &mut Env<'_, '_>,
    scope: Scope,
    path: &str,
    value: JsValue,
) -> Result<JsValue, String> {
    match env.ctx.scopes.as_mut() {
        Some(sv) => {
            sv.write(scope, path, js_to_json(&value));
            Ok(JsValue::Undefined)
        }
        None => {
            env.vars.set_js(path, value);
            Ok(JsValue::Undefined)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::types::MessageRecord;
    use crate::parsing::assistant::{AssistantVars, CharacterCtx, QuickReplyCtx, RenderCtxData};
    use crate::parsing::world_book::WorldEntry;
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
            scan_depth: None,
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

    fn message(role: &str, content: &str) -> MessageRecord {
        MessageRecord {
            id: 0,
            session_id: "s".into(),
            role: role.into(),
            content: content.into(),
            extra: Value::Null,
            created_at: "".into(),
        }
    }

    /// 带上下文的渲染辅助:构建 RenderCtx(指定数据源与初始变量树),渲染并返回 (输出, 登记注入清单)
    #[allow(clippy::too_many_arguments)]
    fn render_with(
        tpl: &str,
        character: Option<CharacterCtx>,
        entries: Vec<WorldEntry>,
        history: Vec<MessageRecord>,
        qrs: Vec<QuickReplyCtx>,
        presets: Vec<crate::parsing::assistant::PresetPromptCtx>,
        init_vars: serde_json::Value,
    ) -> (String, Vec<InjectedPrompt>) {
        let mut vars = AssistantVars::from_value(init_vars);
        let mut ctx = crate::parsing::assistant::RenderCtx {
            vars: &mut vars,
            data: RenderCtxData {
                character: character.as_ref(),
                world_entries: if entries.is_empty() {
                    None
                } else {
                    Some(leak_slice(entries))
                },
                history: if history.is_empty() {
                    None
                } else {
                    Some(leak_slice(history))
                },
                quick_replies: if qrs.is_empty() {
                    None
                } else {
                    Some(leak_slice(qrs))
                },
                preset_prompts: if presets.is_empty() {
                    None
                } else {
                    Some(leak_slice(presets))
                },
                scopes: None,
                injected: Vec::new(),
            },
        };
        let out = crate::parsing::assistant::render_assistant_content_with(tpl, &mut ctx);
        let injected = std::mem::take(&mut ctx.data.injected);
        (out, injected)
    }

    /// 泄漏 Vec 为切片(测试专用:一次性数据,生命周期足够)
    fn leak_slice<T>(v: Vec<T>) -> &'static [T] {
        Box::leak(v.into_boxed_slice())
    }

    /// getwi 在世界书中按 comment 查找并嵌套渲染(内容中的 EJS 生效)
    #[test]
    fn getwi_finds_entry_and_nested_renders() {
        let mut entry_a = entry("地点", "图书馆很安静");
        entry_a.id = 1;
        let entry_b = entry("联动", "<%= getwi('地点') %> 与 <%= getvar('好感度') %>");
        let mut vars = AssistantVars::new();
        vars.set("好感度", json!(150));
        let (out, _ctx) = render_with(
            "<%= getwi('联动') %>",
            None,
            vec![entry_a, entry_b],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            json!({ "好感度": 150 }),
        );
        let _ = vars;
        assert_eq!(out, "图书馆很安静 与 150");
        // 未找到 → 空字符串
        let (out2, _) = render_with(
            "<%= getwi('不存在') %>",
            None,
            vec![entry("x", "y")],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            json!({}),
        );
        assert_eq!(out2, "");
    }

    /// getchar 按字段名返回角色信息;未知字段/无角色 → ""
    #[test]
    fn getchar_returns_character_fields() {
        let c = CharacterCtx {
            name: "芽衣".into(),
            description: "兔族少女".into(),
            personality: "温柔".into(),
            scenario: "雪山".into(),
            avatar_url: Some("/av/1.png".into()),
            mes_example: "你好".into(),
            first_mes: "初次见面".into(),
        };
        let (out, _) = render_with(
            "<%= getchar('name') %>/<%= getchar('description') %>/<%= getchar('personality') %>/<%= getchar('scenario') %>/<%= getchar('avatar') %>/<%= getchar('first_mes') %>/<%= getchar('不存在') %>",
            Some(c),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            json!({}),
        );
        assert_eq!(out, "芽衣/兔族少女/温柔/雪山//av/1.png/初次见面/");
        // 无角色上下文 → 空
        let (out2, _) = render_with(
            "<%= getchar('name') %>",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            json!({}),
        );
        assert_eq!(out2, "");
    }

    /// getqr 按名称(+标签)查快速回复并嵌套渲染
    #[test]
    fn getqr_finds_quick_reply() {
        let qrs = vec![
            QuickReplyCtx {
                name: "战斗".into(),
                label: "开始".into(),
                content: "拔剑战斗,士气 <%= getvar('士气') %>".into(),
            },
            QuickReplyCtx {
                name: "战斗".into(),
                label: "撤退".into(),
                content: "果断撤退".into(),
            },
        ];
        let mut vars = AssistantVars::new();
        vars.set("士气", json!(88));
        let (out, _ctx) = render_with(
            "<%= getqr('战斗', '开始') %>|<%= getqr('战斗', '撤退') %>",
            None,
            Vec::new(),
            Vec::new(),
            qrs,
            Vec::new(),
            json!({ "士气": 88 }),
        );
        let _ = vars;
        assert_eq!(out, "拔剑战斗,士气 88|果断撤退");
        // 未找到 → 空
        let (out2, _) = render_with(
            "<%= getqr('不存在') %>",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            json!({}),
        );
        assert_eq!(out2, "");
    }

    /// getpreset 按名称查预设楼层并嵌套渲染
    #[test]
    fn getpreset_finds_preset_prompt() {
        let presets = vec![crate::parsing::assistant::PresetPromptCtx {
            name: "战斗风格".into(),
            content: "注重动作描写(<%= getvar('模式') %>)".into(),
        }];
        let mut vars = AssistantVars::new();
        vars.set("模式", json!("写实"));
        let (out, _ctx) = render_with(
            "<%= getpreset('战斗风格') %>",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            presets,
            json!({ "模式": "写实" }),
        );
        let _ = vars;
        assert_eq!(out, "注重动作描写(写实)");
    }

    /// getChatMessage 按索引/角色取消息(负数从尾数);getChatMessages 返回消息数组
    #[test]
    fn get_chat_message_and_messages() {
        let history = vec![
            message("user", "第一句"),
            message("assistant", "回复"),
            message("user", "最新"),
        ];
        let (out, _) = render_with(
            "<%= getChatMessage(0, 'user') %>|<%= getChatMessage(-1) %>|<%= getChatMessage(-2, 'assistant') %>",
            None,
            Vec::new(),
            history,
            Vec::new(),
            Vec::new(),
            json!({}),
        );
        assert_eq!(out, "第一句|最新|回复");
        // getChatMessages:取 role/content
        let (out2, _) = render_with(
            "<%= getChatMessages().length %>:<%= getChatMessages(0)[0].role %>:<%= getChatMessages()[2].content %>",
            None,
            Vec::new(),
            vec![
                message("user", "a"),
                message("assistant", "b"),
                message("user", "c"),
            ],
            Vec::new(),
            Vec::new(),
            json!({}),
        );
        assert_eq!(out2, "3:user:c");
        // 无历史 → 空 / 0
        let (out3, _) = render_with(
            "<%= getChatMessage(0) %>|<%= getChatMessages().length %>",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            json!({}),
        );
        assert_eq!(out3, "|0");
    }

    /// matchChatMessages 按关键词(可选 role)检查历史;无历史 false
    #[test]
    fn match_chat_messages_checks_history() {
        let history = vec![message("user", "我在图书馆看书")];
        let (out, _) = render_with(
            "<% if (matchChatMessages(['图书馆'])) { %>命中<% } else { %>未命中<% } %>|<% if (matchChatMessages(['车站'])) { %>命中<% } else { %>未命中<% } %>",
            None,
            Vec::new(),
            history,
            Vec::new(),
            Vec::new(),
            json!({}),
        );
        assert_eq!(out, "命中|未命中");
        // role 过滤:仅匹配 user 消息
        let (out2, _) = render_with(
            "<% if (matchChatMessages(['图书馆'], 'assistant')) { %>命中<% } else { %>未命中<% } %>",
            None,
            Vec::new(),
            vec![message("user", "在图书馆")],
            Vec::new(),
            Vec::new(),
            json!({}),
        );
        assert_eq!(out2, "未命中");
    }

    /// injectPrompt 登记到上下文清单;getPromptsInjected/hasPromptsInjected 读取
    #[test]
    fn inject_prompt_registers_to_context() {
        let mut vars = AssistantVars::new();
        let mut ctx = crate::parsing::assistant::RenderCtx {
            vars: &mut vars,
            data: RenderCtxData::default(),
        };
        let out = crate::parsing::assistant::render_assistant_content_with(
            "<% injectPrompt('风格', '注重氛围描写', 10, true, 'u1'); %><%= hasPromptsInjected('风格') %>",
            &mut ctx,
        );
        assert_eq!(out, "true");
        assert_eq!(ctx.data.injected.len(), 1);
        assert_eq!(ctx.data.injected[0].key, "风格");
        assert_eq!(ctx.data.injected[0].order, 10);
        assert!(ctx.data.injected[0].sticky);
        assert_eq!(ctx.data.injected[0].uid, "u1");
        // getPromptsInjected 返回 JSON 数组
        let out2 = crate::parsing::assistant::render_assistant_content_with(
            "<%= getPromptsInjected() %>",
            &mut ctx,
        );
        assert!(
            out2.contains("风格") && out2.contains("注重氛围描写"),
            "out2: {out2}"
        );
    }
}
