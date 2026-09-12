// 后端 JS 运行时(阶段三 3b-1):rquickjs 封装。
// 设计:
//   - 每次执行新建独立 Runtime/Context(quickjs 值非线程安全,不做跨线程共享),
//     天然隔离不同脚本的状态,避免脚本间变量/全局污染。
//   - 超时采用 quickjs interrupt handler(每 N 条指令回调一次,可中断死循环);
//     调用方(引擎)若需整体强约束,应经 spawn_blocking + 外部超时兜底。
//   - 返回值经 JSON 序列化返回;执行出错收集错误信息,不 panic。
use rquickjs::{Context, Function, Runtime, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 单次执行结果
#[derive(Debug, Clone, PartialEq)]
pub enum EvalOutcome {
    /// 正常返回,值为 JSON 序列化文本(undefined → "undefined")
    Ok(String),
    /// JS 抛异常(含中断)
    Error(String),
}

#[derive(Clone)]
pub struct EvalOptions {
    /// 执行超时(quickjs interrupt 合作式中断;缺省 1 秒)
    pub timeout: Duration,
    /// quickjs 内存上限(字节);缺省 64MB
    pub memory_limit: Option<usize>,
}

impl Default for EvalOptions {
    fn default() -> Self {
        EvalOptions {
            timeout: Duration::from_secs(1),
            memory_limit: Some(64 * 1024 * 1024),
        }
    }
}

struct InterruptState {
    deadline: Mutex<Option<Instant>>,
    interrupted: AtomicBool,
}

/// 执行一段 JS,返回 JSON 序列化结果或错误信息。
pub fn eval_js(source: &str, opts: &EvalOptions) -> EvalOutcome {
    let state = Arc::new(InterruptState {
        deadline: Mutex::new(None),
        interrupted: AtomicBool::new(false),
    });
    let Ok(rt) = Runtime::new() else {
        return EvalOutcome::Error("初始化 JS 运行时失败".into());
    };
    if let Some(limit) = opts.memory_limit {
        // 内存上限:超限执行抛 JS 异常
        rt.set_memory_limit(limit);
    }
    let st = state.clone();
    rt.set_interrupt_handler(Some(Box::new(move || {
        if st.interrupted.load(Ordering::SeqCst) {
            return true;
        }
        if let Some(dl) = *st.deadline.lock().unwrap_or_else(|e| e.into_inner()) {
            if Instant::now() > dl {
                st.interrupted.store(true, Ordering::SeqCst);
                return true;
            }
        }
        false
    })));
    let Ok(ctx) = Context::full(&rt) else {
        return EvalOutcome::Error("初始化 JS 上下文失败".into());
    };
    *state.deadline.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + opts.timeout);
    let outcome = ctx.with(|ctx| eval_in_ctx(&ctx, source));
    *state.deadline.lock().unwrap_or_else(|e| e.into_inner()) = None;
    outcome
}

/// 带 TavernHelper 兼容桥执行:注册 __kd_write_native / __kd_slash_native 闭包,
/// 注入预置脚本(变量快照 + TavernHelper),再执行用户脚本。
pub fn eval_with_bridge(
    source: &str,
    opts: &EvalOptions,
    bridge: &crate::scripts::bridge::EvalBridge,
) -> EvalOutcome {
    let state = Arc::new(InterruptState {
        deadline: Mutex::new(None),
        interrupted: AtomicBool::new(false),
    });
    let Ok(rt) = Runtime::new() else {
        return EvalOutcome::Error("初始化 JS 运行时失败".into());
    };
    if let Some(limit) = opts.memory_limit {
        rt.set_memory_limit(limit);
    }
    let st = state.clone();
    rt.set_interrupt_handler(Some(Box::new(move || {
        if st.interrupted.load(Ordering::SeqCst) {
            return true;
        }
        if let Some(dl) = *st.deadline.lock().unwrap_or_else(|e| e.into_inner()) {
            if Instant::now() > dl {
                st.interrupted.store(true, Ordering::SeqCst);
                return true;
            }
        }
        false
    })));
    let Ok(ctx) = Context::full(&rt) else {
        return EvalOutcome::Error("初始化 JS 上下文失败".into());
    };
    *state.deadline.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + opts.timeout);
    let outcome = ctx.with(|ctx| {
        let bridge = Arc::new(bridge.clone());
        // 写回闭包:脚本调 TavernHelper.setVariables 等 → Rust 写作用域
        let b = bridge.clone();
        let write_fn = Function::new(
            ctx.clone(),
            move |scope: String, json: String| -> Result<(), rquickjs::Error> {
                b.write_scope(&scope, &json)
                    .map_err(|_| rquickjs::Error::Unknown)
            },
        );
        // slash 闭包:脚本调 TavernHelper.triggerSlash → Rust 处理
        let b = bridge.clone();
        let slash_fn = Function::new(
            ctx.clone(),
            move |cmd: String| -> Result<String, rquickjs::Error> { Ok(b.trigger_slash(&cmd)) },
        );
        // 生成闭包(阶段六 6g-1):脚本调 TavernHelper.generate → Rust 非流式生成。
        // 闭包捕获 bridge 的 Arc clone(引擎注入的 GenerateHandler 已捕获 connector)。
        let b = bridge.clone();
        let generate_fn = Function::new(
            ctx.clone(),
            move |config_json: String| -> Result<String, rquickjs::Error> {
                b.generate_from_config(&config_json)
                    .map_err(|_| rquickjs::Error::Unknown)
            },
        );
        // 导入闭包(阶段六 6g-2):脚本调 TavernHelper.importRaw* → Rust 各 service。
        let b = bridge.clone();
        let import_fn = Function::new(
            ctx.clone(),
            move |kind: String,
                  filename: String,
                  content: String,
                  session_id: String|
                  -> Result<String, rquickjs::Error> {
                b.import_raw(&kind, &filename, &content, &session_id)
                    .map_err(|_| rquickjs::Error::Unknown)
            },
        );
        let globals = ctx.globals();
        if let (Ok(w), Ok(s)) = (write_fn, slash_fn) {
            let _ = globals.set("__kd_write_native", w);
            let _ = globals.set("__kd_slash_native", s);
        }
        if let Ok(g) = generate_fn {
            let _ = globals.set("__kd_generate_native", g);
        }
        if let Ok(i) = import_fn {
            let _ = globals.set("__kd_import_native", i);
        }
        // 注入预置脚本(变量快照 + TavernHelper 定义);失败视为内部错误
        if let Err(e) = ctx.eval::<Value, _>(bridge.build_script()) {
            return EvalOutcome::Error(format!("注入 TavernHelper 桥失败: {e:?}"));
        }
        eval_in_ctx(&ctx, source)
    });
    *state.deadline.lock().unwrap_or_else(|e| e.into_inner()) = None;
    outcome
}

/// 在已配置的上下文中执行一段脚本并序列化结果(供 eval_js / eval_with_bridge 共用)
fn eval_in_ctx(ctx: &rquickjs::Ctx<'_>, source: &str) -> EvalOutcome {
    match ctx.eval::<Value, _>(source) {
        Ok(v) => {
            // 字符串值(如脚本自行 JSON.stringify 的结果)直接返回,避免二次序列化;
            // 其余值经 JSON 序列化返回,undefined → "undefined"
            if let Some(s) = v.as_string() {
                match s.to_string() {
                    Ok(text) => EvalOutcome::Ok(text),
                    Err(_) => EvalOutcome::Error("读取执行结果失败".into()),
                }
            } else {
                match ctx.json_stringify(v) {
                    Ok(Some(s)) => match s.to_string() {
                        Ok(text) => EvalOutcome::Ok(text),
                        Err(_) => EvalOutcome::Error("序列化执行结果失败".into()),
                    },
                    Ok(None) => EvalOutcome::Ok("undefined".into()),
                    Err(_) => EvalOutcome::Error("序列化执行结果失败".into()),
                }
            }
        }
        Err(e) => EvalOutcome::Error(describe_error(ctx, e)),
    }
}

/// 提取 JS 异常消息(优先异常对象的 message,其次 name,最后 debug 文本)
fn describe_error(ctx: &rquickjs::Ctx<'_>, e: rquickjs::Error) -> String {
    // 取挂起的异常对象,读 message/name;失败则退回 debug 描述
    let exc = ctx.catch();
    if let Some(obj) = exc.as_object() {
        if let Ok(msg) = obj.get::<_, String>("message") {
            return msg;
        }
        if let Ok(name) = obj.get::<_, String>("name") {
            return name;
        }
    }
    format!("{e:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> EvalOutcome {
        eval_js(src, &EvalOptions::default())
    }

    #[test]
    fn evaluates_primitives() {
        assert_eq!(run("1 + 2"), EvalOutcome::Ok("3".into()));
        // 字符串值原样返回(不再二次 JSON 序列化)
        assert_eq!(run("'a' + 'b'"), EvalOutcome::Ok("ab".into()));
        assert_eq!(run("true && false"), EvalOutcome::Ok("false".into()));
    }

    #[test]
    fn evaluates_object_as_json() {
        assert_eq!(
            run("JSON.stringify({ a: 1, b: [2, 3] })"),
            EvalOutcome::Ok("{\"a\":1,\"b\":[2,3]}".into())
        );
    }

    #[test]
    fn returns_undefined_as_text() {
        // 无 return 的脚本返回 undefined
        assert_eq!(run("var x = 1;"), EvalOutcome::Ok("undefined".into()));
    }

    #[test]
    fn syntax_error_reported() {
        assert!(matches!(run("function ("), EvalOutcome::Error(msg) if !msg.is_empty()));
    }

    #[test]
    fn runtime_error_message_extracted() {
        assert!(
            matches!(run("throw new Error('炸了')"), EvalOutcome::Error(msg) if msg.contains("炸了"))
        );
    }

    #[test]
    fn infinite_loop_times_out() {
        let outcome = eval_js(
            "while (true) {}",
            &EvalOptions {
                timeout: Duration::from_millis(200),
                memory_limit: None,
            },
        );
        let interrupted = matches!(&outcome, EvalOutcome::Error(msg) if msg.contains("interrupt") || msg.contains("超时"));
        assert!(interrupted, "死循环应被超时中断,实际 {outcome:?}");
    }
}
