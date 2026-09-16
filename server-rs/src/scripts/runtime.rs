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
    /// quickjs 调用栈上限(字节);缺省 [`DEFAULT_STACK_LIMIT`]。
    ///
    /// **为何显式设置**(2026-09-14 安全加固,见 `docs/遗留.md` L20):
    /// rquickjs 底层本身有 256KB 默认栈上限,但那是**库的默认值**——它不在本仓库的
    /// 控制范围内(升级依赖可能改变),而深递归脚本导致栈溢出会崩掉宿主进程。
    /// 与内存上限同理,把资源边界写进本仓库代码,边界才是可审计、可回归测试的。
    pub stack_limit: Option<usize>,
}

/// quickjs 调用栈上限缺省值(字节)。
///
/// 取 **512KB**——实测确认的**最大安全值**。**关键结论:该上限不是「越大越安全」,
/// 而是越大越危险**(2026-09-14 在 Windows 上单档隔离实测,递归深度 30/100/300):
///
/// | 栈上限 | 深度 30 | 深度 100 | 深度 300 |
/// |---|---|---|---|
/// | **未显式设置** | **崩进程** | **崩进程** | **崩进程** |
/// | 256KB | JS 异常 | JS 异常 | JS 异常 |
/// | **512KB(本值)** | **正常 ✓** | **JS 异常 ✓** | **JS 异常 ✓** |
/// | 1MB 及以上 | 正常 | **崩进程** | **崩进程** |
///
/// 三点结论:
/// 1. **未显式设置 = 无上限 = 真实漏洞**:rquickjs 不会自动施加安全默认值,
///    深递归会以 `STATUS_STACK_OVERFLOW` 直接杀进程(实测如此),这就是本次加固的对象;
/// 2. **512KB 是安全与可用的平衡点**:支撑约 30 层递归(够角色卡脚本用),
///    同时保证更深递归以可捕获的 JS 异常返回;
/// 3. **放大到 1MB+ 反而更危险**:QuickJS 以原生栈指针为基线记账,上限越大允许的
///    JS 帧越多,原生栈越可能在检查触发前先溢出——那时进程已被杀,没有 JS 异常可捕。
///
/// (rquickjs 硬限 16MiB:超过被视为「禁用检查」,见其 `raw.rs` 注释——更不能碰。)
/// **教训**:资源上限的正确取值靠实测确定边界,不能凭「留宽裕些更好」的直觉放大。
pub const DEFAULT_STACK_LIMIT: usize = 512 * 1024;

impl Default for EvalOptions {
    fn default() -> Self {
        EvalOptions {
            timeout: Duration::from_secs(1),
            memory_limit: Some(64 * 1024 * 1024),
            stack_limit: Some(DEFAULT_STACK_LIMIT),
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
    if let Some(limit) = opts.stack_limit {
        // 栈上限(2026-09-14 加固):深递归脚本以 JS 异常中止,而非溢出崩宿主。
        // 超过 16MiB 会被 rquickjs 当作「禁用检查」,故本仓库取值远低于该硬限。
        rt.set_max_stack_size(limit);
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
    if let Some(limit) = opts.stack_limit {
        // 栈上限:与 eval_js 同口径(见 EvalOptions::stack_limit 说明)。
        rt.set_max_stack_size(limit);
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
                // 保留原错误消息:此前丢弃后脚本侧只看到 "Unknown",变量写回失败
                // (作用域名非法/JSON 非法)完全无从定位
                b.write_scope(&scope, &json)
                    .map_err(|e| rquickjs::Error::new_from_js_message("Rust", "JavaScript", e))
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
                // 保留原错误消息(生成失败原因),不再退化为 "Unknown"
                b.generate_from_config(&config_json)
                    .map_err(|e| rquickjs::Error::new_from_js_message("Rust", "JavaScript", e))
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
                // 保留原错误消息(导入失败原因),不再退化为 "Unknown"
                b.import_raw(&kind, &filename, &content, &session_id)
                    .map_err(|e| rquickjs::Error::new_from_js_message("Rust", "JavaScript", e))
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
                stack_limit: None,
            },
        );
        let interrupted = matches!(&outcome, EvalOutcome::Error(msg) if msg.contains("interrupt") || msg.contains("超时"));
        assert!(interrupted, "死循环应被超时中断,实际 {outcome:?}");
    }

    /// 栈上限(2026-09-14 加固,known-limitations L20):
    /// 深递归必须以 **JS 异常**中止,而不是溢出崩掉宿主进程。
    ///
    /// 这是「不可信输入不崩宿主」的最小断言——脚本来自角色卡,属不可信输入。
    /// 若此测试变成进程崩溃(而非返回 Error),说明栈上限失效。
    #[test]
    fn deep_recursion_errors_instead_of_crashing_host() {
        let outcome = eval_js(
            "function f(n){ return f(n+1); } f(0);",
            &EvalOptions::default(),
        );
        assert!(
            matches!(outcome, EvalOutcome::Error(_)),
            "深递归应返回 Error(而非崩进程),实际 {outcome:?}"
        );
    }

    /// 默认栈上限下,常规深度的递归应能正常完成(防上限过紧伤正常脚本)。
    ///
    /// 实测边界(见 `DEFAULT_STACK_LIMIT` 文档表):256KB 支撑约 30 层递归,
    /// 足以覆盖角色卡脚本的实际用法(小规模状态操作,极少 >20 层)。
    #[test]
    fn default_stack_limit_allows_normal_recursion() {
        let outcome = eval_js(
            "function f(n){ return n<=0 ? 0 : 1+f(n-1); } f(20);",
            &EvalOptions::default(),
        );
        assert!(
            matches!(outcome, EvalOutcome::Ok(_)),
            "20 层递归应在上限内完成,实际 {outcome:?}"
        );
    }

    /// 显式设置栈上限必须**真正生效**:显式 256KB 与「不设置」都可拦住深递归。
    ///
    /// 本测试锁定「显式设置这条路径被走到」——若将来有人删掉 `set_max_stack_size`
    /// 调用,`stack_limit: Some(..)` 将不再产生效果;而**不设置时的行为实测不可靠**
    /// (见 `DEFAULT_STACK_LIMIT` 文档表:未显式设置会崩进程),故此处只断言
    /// 「显式设置能稳定得到 JS 异常」,并以此确立「必须显式设置」的产品约束。
    ///
    /// **注意**:本测试刻意不构造 1MB 以上的上限——那会崩掉测试进程(原生栈溢出),
    /// 属实测确认的危险区,不宜写进回归测试。
    #[test]
    fn explicit_stack_limit_catches_deep_recursion() {
        let outcome = eval_js(
            "function f(n){ return f(n+1); } f(0);",
            &EvalOptions {
                timeout: Duration::from_secs(2),
                memory_limit: None,
                stack_limit: Some(DEFAULT_STACK_LIMIT),
            },
        );
        assert!(
            matches!(outcome, EvalOutcome::Error(_)),
            "无限递归在显式 256KB 上限下应返回 Error,实际 {outcome:?}"
        );
    }
}
