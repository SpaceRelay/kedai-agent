// 结构化 JSON 日志(tracing 生态,D-4):控制台 + logs/kedai-YYYY-MM-DD.log 双写。
// 与旧 logger(utils/logger.rs,已删除)的差异:
// - 热路径不再持全局锁:stdout/文件均走 tracing-appender non-blocking channel,由后台线程落盘;
// - level 支持 RUST_LOG 覆盖,默认取 config.log_level;第三方库噪音(tower_http/hyper 等)白名单压制;
// - 行格式保持 pino 风格 {"level","time","msg",...自定义字段},与旧输出逐字段等价
//   (键序不同:旧实现 BTreeMap 字母序,此处 level/time/msg 在前、自定义字段按调用点顺序,故不做逐字节对比)。
//
// 调用点字段约定(自定义 PinoFormat 的解析规则,新增日志时务必遵守):
// - 字符串:`key = s.as_str()` 或 `key = s`(String/&str)→ record_str → JSON 字符串,绝不回解析;
// - 数字/布尔:直接传原生类型(u64/i64/f64/bool 等)→ JSON 原生类型;
// - serde_json::Value 复合值(数组/对象/需保留类型的值):`key = %JsonField(v)` →
//   record_debug 文本按 JSON 回解析后原样嵌入。`%`/`?` 在本仓日志调用点仅限 JsonField 使用,
//   普通字符串禁止用 %/?(内容若恰为合法 JSON 会被回解析,改变字段类型)。
use chrono::Local;
use serde_json::{json, Map, Value};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// 日志文件前缀:文件名 kedai-YYYY-MM-DD.log(与旧 logger 一致,3 天清理逻辑无需变更)
const FILE_PREFIX: &str = "kedai-";

fn iso_now() -> String {
    Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%z").to_string()
}

fn level_str(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

/// serde_json::Value 透传字段:Display 输出紧凑 JSON,PinoFormat 回解析后原样嵌入,结构不变
pub struct JsonField(pub Value);

impl fmt::Display for JsonField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 事件字段收集器:message 归 msg,其余按类型进自定义字段表。
/// 特例:自定义字段恰名为 message 时(retry.rs),字符串经 record_str 入自定义字段表,
/// 格式串消息经 record_debug 归 msg,两路互不覆盖。
#[derive(Default)]
struct PinoFields {
    msg: Option<String>,
    fields: Map<String, Value>,
}

impl PinoFields {
    fn insert(&mut self, field: &Field, value: Value) {
        // 剥离原始标识符前缀:调用点关键字字段写 r#type,输出键名须保持 "type"
        let name = field.name().strip_prefix("r#").unwrap_or(field.name());
        self.fields.insert(name.to_string(), value);
    }
}

impl Visit for PinoFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, Value::String(value.to_string()));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, json!(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, json!(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, json!(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, json!(value));
    }

    /// %/? 记录的值:格式串消息(field 名 message)原文归 msg,不回解析;
    /// 其余(JsonField 约定)文本按 JSON 回解析,失败退化为字符串
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let text = format!("{value:?}");
        if field.name() == "message" {
            self.msg = Some(text);
            return;
        }
        let parsed = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
        self.insert(field, parsed);
    }
}

/// pino 风格 JSON 行格式:{"level","time","msg",...自定义字段}
/// 不输出 target/span(旧 logger 无此概念;span 在本仓未使用)
pub struct PinoFormat;

impl<S, N> FormatEvent<S, N> for PinoFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut collected = PinoFields::default();
        event.record(&mut collected);

        let mut obj = Map::new();
        obj.insert("level".into(), json!(level_str(event.metadata().level())));
        obj.insert("time".into(), json!(iso_now()));
        obj.insert("msg".into(), json!(collected.msg.unwrap_or_default()));
        for (k, v) in collected.fields {
            obj.insert(k, v);
        }
        writeln!(writer, "{}", Value::Object(obj))
    }
}

/// 按天滚动文件写入器:保持 kedai-YYYY-MM-DD.log 命名(横杠,非 tracing-appender 默认点分隔)。
/// 仅被 non-blocking 后台线程独占访问,内部无需锁;打开失败时静默丢弃(与旧 logger 行为一致)。
struct DailyFileWriter {
    dir: PathBuf,
    today: String,
    file: Option<File>,
}

impl DailyFileWriter {
    fn new(dir: &Path) -> Self {
        let today = Local::now().format("%Y-%m-%d").to_string();
        let file = Self::open(dir, &today);
        DailyFileWriter {
            dir: dir.to_path_buf(),
            today,
            file,
        }
    }

    fn open(dir: &Path, today: &str) -> Option<File> {
        let path = dir.join(format!("{FILE_PREFIX}{today}.log"));
        OpenOptions::new().create(true).append(true).open(path).ok()
    }
}

impl Write for DailyFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // 跨天时切换文件
        let today = Local::now().format("%Y-%m-%d").to_string();
        if today != self.today {
            self.today = today.clone();
            self.file = Self::open(&self.dir, &today);
        }
        match self.file.as_mut() {
            Some(f) => f.write(buf),
            None => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

/// 组装 EnvFilter 指令:RUST_LOG 优先(用户全权控制);否则取 config 级别,
/// 并白名单压制第三方库噪音(即使业务开 debug,tower_http/hyper 等也不放 debug)
fn filter_directives(level: &str) -> String {
    if let Ok(v) = std::env::var("RUST_LOG") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    let base = match level {
        "trace" | "debug" | "info" | "warn" | "error" => level,
        _ => "info",
    };
    format!(
        "{base},tower_http=info,hyper=info,h2=info,rustls=info,reqwest=info,tokio=info,mio=info,rquickjs=info"
    )
}

/// 初始化日志(进程启动时调用一次;重复调用仅首次生效)。
/// 返回的 WorkerGuard 必须由调用方持有到进程退出(run_server 作用域绑定),
/// 提前 drop 会关闭 non-blocking 后台线程导致日志丢失。
pub fn init(level: &str, log_dir: &Path) -> Vec<WorkerGuard> {
    let _ = fs::create_dir_all(log_dir);
    let directives = filter_directives(level);

    // stdout 非阻塞写
    let (stdout_nb, stdout_guard) = tracing_appender::non_blocking(io::stdout());
    // 文件非阻塞写(按天滚动,kedai-YYYY-MM-DD.log)
    let (file_nb, file_guard) = tracing_appender::non_blocking(DailyFileWriter::new(log_dir));

    let stdout_layer = tracing_subscriber::fmt::layer()
        .event_format(PinoFormat)
        .with_writer(stdout_nb)
        .with_filter(EnvFilter::new(directives.clone()));
    let file_layer = tracing_subscriber::fmt::layer()
        .event_format(PinoFormat)
        .with_writer(file_nb)
        .with_filter(EnvFilter::new(directives));

    // try_init:Tauri 壳与命令行入口重复初始化时忽略第二次,不 panic
    let _ = tracing_subscriber::registry()
        .with(stdout_layer)
        .with(file_layer)
        .try_init();

    cleanup_old_logs(log_dir);
    vec![stdout_guard, file_guard]
}

/// 清理 3 天前的 kedai-*.log(沿用旧 logger 逻辑)
pub fn cleanup_old_logs(log_dir: &Path) {
    let Ok(entries) = fs::read_dir(log_dir) else {
        return;
    };
    let cutoff = Local::now() - chrono::Duration::days(3);
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(FILE_PREFIX) && name.ends_with(".log") {
            if let Ok(meta) = e.metadata() {
                if let Ok(modified) = meta.modified() {
                    let dt: chrono::DateTime<chrono::Local> = modified.into();
                    if dt < cutoff {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        }
    }
}

/// Agent 步骤结构化日志:{ sessionId, step, detail },字段名与旧 logger 保持一致
pub fn agent_step(session_id: &str, step: &str, detail: Option<&str>) {
    match detail {
        Some(d) => tracing::info!(
            sessionId = session_id,
            step = step,
            detail = d,
            "[agent] {step}"
        ),
        None => tracing::info!(sessionId = session_id, step = step, "[agent] {step}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    /// 捕获写入内容的测试 writer
    #[derive(Clone, Default)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn capture_line(f: impl FnOnce()) -> Value {
        let writer = CaptureWriter::default();
        let buf = writer.0.clone();
        let subscriber = tracing_subscriber::fmt()
            .event_format(PinoFormat)
            .with_writer(writer)
            .with_max_level(Level::TRACE)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let bytes = buf.lock().unwrap().clone();
        let text = String::from_utf8(bytes).unwrap();
        let line = text.lines().next().expect("应有一行日志");
        serde_json::from_str(line).expect("应为合法 JSON 行")
    }

    #[test]
    fn 字符串数字布尔字段保持原生类型() {
        let obj = capture_line(|| {
            tracing::info!(host = "127.0.0.1", port = 8765u16, ok = true, "启动");
        });
        assert_eq!(obj["level"], json!("info"));
        assert_eq!(obj["msg"], json!("启动"));
        assert_eq!(obj["host"], json!("127.0.0.1"));
        assert_eq!(obj["port"], json!(8765));
        assert_eq!(obj["ok"], json!(true));
        assert!(obj["time"].as_str().unwrap().contains('T'));
    }

    #[test]
    fn jsonfield复合值原样嵌入() {
        let obj = capture_line(|| {
            let v = json!({"a": [1, 2], "b": null});
            tracing::warn!(payload = %JsonField(v.clone()), n = %JsonField(json!(3.5)), "复合");
        });
        assert_eq!(obj["level"], json!("warn"));
        assert_eq!(obj["payload"], json!({"a": [1, 2], "b": null}));
        assert_eq!(obj["n"], json!(3.5));
    }

    #[test]
    fn agent_step字段名与msg格式不变() {
        let obj = capture_line(|| {
            agent_step("sess-1", "plan", Some("摘要"));
        });
        assert_eq!(obj["sessionId"], json!("sess-1"));
        assert_eq!(obj["step"], json!("plan"));
        assert_eq!(obj["detail"], json!("摘要"));
        assert_eq!(obj["msg"], json!("[agent] plan"));

        let obj2 = capture_line(|| {
            agent_step("sess-2", "reflect", None);
        });
        assert_eq!(obj2["sessionId"], json!("sess-2"));
        assert!(obj2.get("detail").is_none());
        assert_eq!(obj2["msg"], json!("[agent] reflect"));
    }

    #[test]
    fn error级别与debug级别映射为小写() {
        let obj = capture_line(|| {
            tracing::error!(error = "boom", "失败");
        });
        assert_eq!(obj["level"], json!("error"));
        assert_eq!(obj["error"], json!("boom"));
    }

    #[test]
    fn 自定义message字段与格式串msg互不覆盖() {
        // retry.rs 场景:业务字段恰名 message(字符串),与格式串消息共存
        let obj = capture_line(|| {
            tracing::info!(
                attempt = 1u32,
                message = "连接失败".to_string(),
                "模型请求重试"
            );
        });
        assert_eq!(obj["msg"], json!("模型请求重试"));
        assert_eq!(obj["message"], json!("连接失败"));
        assert_eq!(obj["attempt"], json!(1));
    }

    #[test]
    fn 关键字字段r前缀剥离() {
        // models/types.rs 场景:字段名 type 是 Rust 关键字,调用点写 r#type,输出须为 "type"
        let obj = capture_line(|| {
            tracing::warn!(
                r#type = "task_status",
                value = "bogus",
                "任务状态未知值,回退 pending"
            );
        });
        assert_eq!(obj["type"], json!("task_status"));
        assert_eq!(obj["value"], json!("bogus"));
        assert!(obj.get("r#type").is_none());
    }
}
