// 表达式求值:eval_expr(含短路/赋值/自增自减/字面量/函数调用分发)与
// binary_eval/strict_eq/read_target/write_target。函数调用经 exec.rs 的 call 分发。
use super::ast::*;
use super::env::*;
use super::exec::call;
use super::value::*;

pub(super) fn eval_expr(env: &mut Env, expr: &Expr) -> Result<JsValue, String> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Ident(name) => Ok(env.lookup(name).unwrap_or(JsValue::Undefined)),
        Expr::Member { obj, prop } => {
            let recv = eval_expr(env, obj)?;
            Ok(get_prop(&recv, prop))
        }
        Expr::Index { obj, idx } => {
            let recv = eval_expr(env, obj)?;
            let key = eval_expr(env, idx)?;
            match (recv, key) {
                (JsValue::Arr(items), JsValue::Num(n)) => {
                    let i = n as usize;
                    Ok(items.get(i).cloned().unwrap_or(JsValue::Undefined))
                }
                (JsValue::Arr(items), JsValue::Str(s)) => {
                    let i = s.parse::<usize>().unwrap_or(usize::MAX);
                    Ok(items.get(i).cloned().unwrap_or(JsValue::Undefined))
                }
                (JsValue::Obj(fields), k) => {
                    let key = js_to_string(&k);
                    Ok(fields
                        .iter()
                        .find(|(kk, _)| kk == &key)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(JsValue::Undefined))
                }
                _ => Ok(JsValue::Undefined),
            }
        }
        Expr::Call { callee, args } => {
            let mut args_val = Vec::with_capacity(args.len());
            for a in args {
                args_val.push(eval_expr(env, a)?);
            }
            // 方法调用:保留接收者
            match callee.as_ref() {
                Expr::Member { obj, prop } => {
                    let recv = eval_expr(env, obj)?;
                    let f = get_prop(&recv, prop);
                    call(env, &f, Some(&recv), &args_val)
                }
                Expr::Index { obj, idx } => {
                    let recv = eval_expr(env, obj)?;
                    let key = eval_expr(env, idx)?;
                    let f = get_prop(&recv, &js_to_string(&key));
                    call(env, &f, Some(&recv), &args_val)
                }
                _ => {
                    let f = eval_expr(env, callee)?;
                    call(env, &f, None, &args_val)
                }
            }
        }
        Expr::Unary { op, expr } => {
            let v = eval_expr(env, expr)?;
            match op {
                UnOp::Not => Ok(JsValue::Bool(!truthy(&v))),
                UnOp::Neg => Ok(JsValue::Num(-js_to_num(&v))),
                UnOp::Pos => Ok(JsValue::Num(js_to_num(&v))),
            }
        }
        Expr::Binary { op, left, right } => {
            let l = eval_expr(env, left)?;
            // 短路
            match op {
                BinOp::And => {
                    if !truthy(&l) {
                        return Ok(l);
                    }
                    return eval_expr(env, right);
                }
                BinOp::Or => {
                    if truthy(&l) {
                        return Ok(l);
                    }
                    return eval_expr(env, right);
                }
                _ => {}
            }
            let r = eval_expr(env, right)?;
            Ok(binary_eval(*op, l, r))
        }
        Expr::Ternary { cond, then, els } => {
            if truthy(&eval_expr(env, cond)?) {
                eval_expr(env, then)
            } else {
                eval_expr(env, els)
            }
        }
        Expr::Assign { target, op, value } => {
            let v = eval_expr(env, value)?;
            let v = match op {
                AssignOp::Set => v,
                AssignOp::Add => {
                    let cur = read_target(env, target)?;
                    binary_eval(BinOp::Add, cur, v.clone())
                }
                AssignOp::Sub => {
                    let cur = read_target(env, target)?;
                    binary_eval(BinOp::Sub, cur, v.clone())
                }
                AssignOp::Mul => {
                    let cur = read_target(env, target)?;
                    binary_eval(BinOp::Mul, cur, v.clone())
                }
                AssignOp::Div => {
                    let cur = read_target(env, target)?;
                    binary_eval(BinOp::Div, cur, v.clone())
                }
            };
            write_target(env, target, v.clone())?;
            Ok(v)
        }
        Expr::Update { target, op, prefix } => {
            let old = read_target(env, target)?;
            let old_n = js_to_num(&old);
            let new_n = if matches!(op, UpdateOp::Inc) {
                old_n + 1.0
            } else {
                old_n - 1.0
            };
            write_target(env, target, JsValue::Num(new_n))?;
            if *prefix {
                Ok(JsValue::Num(new_n))
            } else {
                Ok(JsValue::Num(old_n))
            }
        }
        Expr::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for i in items {
                out.push(eval_expr(env, i)?);
            }
            Ok(JsValue::Arr(out))
        }
        Expr::Object(fields) => {
            let mut out = Vec::with_capacity(fields.len());
            for (k, e) in fields {
                out.push((k.clone(), eval_expr(env, e)?));
            }
            Ok(JsValue::Obj(out))
        }
        Expr::Func { params, body } => Ok(JsValue::Fn(JsFn {
            params: params.clone(),
            body: body.clone(),
            captured: env.capture(),
        })),
        Expr::Template(items) => {
            let mut out = String::new();
            for i in items {
                let v = eval_expr(env, i)?;
                out.push_str(&js_to_string(&v));
            }
            Ok(JsValue::Str(out))
        }
    }
}

fn binary_eval(op: BinOp, l: JsValue, r: JsValue) -> JsValue {
    match op {
        BinOp::Eq => JsValue::Bool(loose_eq(&l, &r)),
        BinOp::NotEq => JsValue::Bool(!loose_eq(&l, &r)),
        BinOp::StrictEq => JsValue::Bool(strict_eq(&l, &r)),
        BinOp::StrictNotEq => JsValue::Bool(!strict_eq(&l, &r)),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => JsValue::Bool(compare(op, &l, &r)),
        BinOp::Add => {
            // JS:+ 任一字符串 → 拼接
            if matches!(l, JsValue::Str(_)) || matches!(r, JsValue::Str(_)) {
                JsValue::Str(format!("{}{}", js_to_string(&l), js_to_string(&r)))
            } else {
                JsValue::Num(js_to_num(&l) + js_to_num(&r))
            }
        }
        BinOp::Sub => JsValue::Num(js_to_num(&l) - js_to_num(&r)),
        BinOp::Mul => JsValue::Num(js_to_num(&l) * js_to_num(&r)),
        BinOp::Div => JsValue::Num(js_to_num(&l) / js_to_num(&r)),
        BinOp::Mod => JsValue::Num(js_to_num(&l) % js_to_num(&r)),
        BinOp::And | BinOp::Or => unreachable!("短路已在上层处理"),
    }
}

fn strict_eq(a: &JsValue, b: &JsValue) -> bool {
    match (a, b) {
        (JsValue::Num(x), JsValue::Num(y)) => x == y,
        (JsValue::Str(x), JsValue::Str(y)) => x == y,
        (JsValue::Bool(x), JsValue::Bool(y)) => x == y,
        (JsValue::Null, JsValue::Null) => true,
        (JsValue::Undefined, JsValue::Undefined) => true,
        (JsValue::Arr(_) | JsValue::Obj(_), JsValue::Arr(_) | JsValue::Obj(_)) => {
            js_to_json(a) == js_to_json(b)
        }
        _ => false,
    }
}

fn read_target(env: &mut Env, target: &AssignTarget) -> Result<JsValue, String> {
    match target {
        AssignTarget::Ident(name) => Ok(env.lookup(name).unwrap_or(JsValue::Undefined)),
        AssignTarget::Member(obj, prop) => {
            let recv = eval_expr(env, obj)?;
            Ok(get_prop(&recv, prop))
        }
        AssignTarget::Index(obj, idx) => {
            let recv = eval_expr(env, obj)?;
            let key = eval_expr(env, idx)?;
            match recv {
                JsValue::Arr(items) => {
                    let i = js_to_num(&key) as usize;
                    Ok(items.get(i).cloned().unwrap_or(JsValue::Undefined))
                }
                JsValue::Obj(fields) => {
                    let k = js_to_string(&key);
                    Ok(fields
                        .iter()
                        .find(|(kk, _)| kk == &k)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(JsValue::Undefined))
                }
                _ => Ok(JsValue::Undefined),
            }
        }
    }
}

fn write_target(env: &mut Env, target: &AssignTarget, value: JsValue) -> Result<(), String> {
    match target {
        AssignTarget::Ident(name) => {
            env.assign(name, value);
            Ok(())
        }
        AssignTarget::Member(obj, prop) => {
            let mut recv = eval_expr(env, obj)?;
            set_prop(&mut recv, prop, value)
        }
        AssignTarget::Index(obj, idx) => {
            let mut recv = eval_expr(env, obj)?;
            let key = eval_expr(env, idx)?;
            let k = js_to_string(&key);
            match &mut recv {
                JsValue::Arr(items) => {
                    if let Ok(i) = k.parse::<usize>() {
                        if items.len() <= i {
                            items.resize(i + 1, JsValue::Undefined);
                        }
                        items[i] = value;
                    }
                    Ok(())
                }
                JsValue::Obj(fields) => {
                    if let Some((_, v)) = fields.iter_mut().find(|(kk, _)| kk == &k) {
                        *v = value;
                    } else {
                        fields.push((k, value));
                    }
                    Ok(())
                }
                _ => Ok(()),
            }
        }
    }
}
