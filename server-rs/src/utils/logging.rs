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
//
// requestId 传递(批次 2):HTTP 中间件(api/request_id.rs)建 `http_request` span 时写
// `requestId` 字段,RequestIdLayer 把它存为 span extension,PinoFormat 自动合并进该请求
// 生命周期内所有事件行——调用点无需(也不应)重复声明 requestId 字段。
use chrono::Local;
use serde_json::{json, Map, Value};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Level, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// 日志文件前缀:文件名 kedai-YYYY-MM-DD.log(与旧 logger 一致,3 天清理逻辑无需变更)
const FILE_PREFIX: &str = "kedai-";

/// 单文件大小上限(2026-09-16,计划批次 6.1):超过即轮转到同日的下一个序号文件。
///
/// 为什么需要:此前只有「按天滚动 + 启动时删 3 天前文件」,单文件**无上限**。一次失控的
/// debug 级日志(或高频 warn 风暴)可以在一天内把磁盘写满——本仓已有「C 盘写满」的历史
/// 环境故障,量级数以 GB 计。加此上限后,单日日志占用被钳制在「上限 × 当日轮转份数」,
/// 而份数又受下面 MAX_ROTATED_PER_DAY 约束。
///
/// 取值 32 MiB:正常使用下一天的日志为几十 KB 到几 MB 量级,32 MiB 留了两个数量级余量;
/// 真到该量级说明确有异常在刷日志,此时轮转比继续追加更有价值(留证且不撑爆磁盘)。
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// 同日最多保留的轮转份数(含首份):超出的最旧份被删除。
/// 与 MAX_FILE_BYTES 相乘即「单日日志硬上限」——这是容量保护的关键一环:
/// 只有大小上限而没有份数上限时,疯狂刷日志仍能靠不断轮转写满磁盘。
/// 取 8:单日占用上限 256 MiB,足够留下完整现场,又不至于失控。
const MAX_ROTATED_PER_DAY: u32 = 8;

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

/// 请求关联 ID(span extension 载荷,批次 2 可观测性最小骨架)。
///
/// 中间件(`api/request_id.rs`)建 span 时写 `requestId` 字段,`RequestIdLayer` 在
/// on_new_span 时把它取成结构化值存进 span extension,`PinoFormat` 再读回并合并进
/// JSON 行——这样请求生命周期内的**所有**日志(含 service 层与 spawn 出去的引擎日志)
/// 自动携带 requestId,无需逐处传参。
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// span 字段名:中间件与 RequestIdLayer 的约定键,改名需两侧同步
const REQUEST_ID_SPAN_FIELD: &str = "requestId";

/// 从 span 属性里取出 requestId 的访问器
#[derive(Default)]
struct RequestIdVisitor {
    value: Option<String>,
}

impl Visit for RequestIdVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == REQUEST_ID_SPAN_FIELD {
            self.value = Some(value.to_string());
        }
    }

    /// `%value`(Display)形态的记录路径:兜底读取,避免调用点写法变化后静默失效
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == REQUEST_ID_SPAN_FIELD && self.value.is_none() {
            self.value = Some(format!("{value:?}"));
        }
    }
}

/// 把 span 的 `requestId` 字段结构化存为 span extension 的薄 Layer。
///
/// 为何必须走 extension:`FormattedFields` 给出的是**已格式化的字符串**,不是结构化值;
/// 要让格式化器拿到结构化 `requestId` 并合并进 JSON 行,只能在 on_new_span 时存 extension。
pub(crate) struct RequestIdLayer;

impl<S> Layer<S> for RequestIdLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut visitor = RequestIdVisitor::default();
        attrs.record(&mut visitor);
        let Some(value) = visitor.value else {
            return;
        };
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(RequestId(value));
        }
    }
}

/// pino 风格 JSON 行格式:{"level","time","msg",...自定义字段}
/// 不输出 target/span 前缀(旧 logger 无此概念);span 只经 extension 透出 requestId
pub struct PinoFormat;

impl<S, N> FormatEvent<S, N> for PinoFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut collected = PinoFields::default();
        event.record(&mut collected);

        let mut obj = Map::new();
        obj.insert("level".into(), json!(level_str(event.metadata().level())));
        obj.insert("time".into(), json!(iso_now()));
        obj.insert("msg".into(), json!(collected.msg.unwrap_or_default()));
        // span 穿透:事件作用域内任一 span 挂了 requestId extension 就合并进本行。
        // 先合并、后写入事件自身字段——同名时以调用点显式字段为准(既有语义不变)。
        if let Some(request_id) = ctx.event_scope().and_then(|scope| {
            scope
                .from_root()
                .find_map(|span| span.extensions().get::<RequestId>().cloned())
        }) {
            obj.insert("requestId".into(), json!(request_id.0));
        }
        for (k, v) in collected.fields {
            obj.insert(k, v);
        }
        writeln!(writer, "{}", Value::Object(obj))
    }
}

/// 按天滚动 + 超限轮转的文件写入器:保持 kedai-YYYY-MM-DD.log 命名(横杠,非
/// tracing-appender 默认点分隔);同一天超过 MAX_FILE_BYTES 时切到 `kedai-YYYY-MM-DD.<n>.log`。
/// 仅被 non-blocking 后台线程独占访问,内部无需锁;打开失败时静默丢弃(与旧 logger 行为一致)。
///
/// 轮转文件名的设计要点:`.log` 后缀与 FILE_PREFIX 前缀都保留,故 `cleanup_old_logs` 的
/// 既有过滤(`starts_with(FILE_PREFIX) && ends_with(".log")`)**无需改动**即覆盖轮转产物。
struct DailyFileWriter {
    dir: PathBuf,
    today: String,
    file: Option<File>,
    /// 当前文件的已写字节数。续写(重启/跨天落在既有文件)时取文件实际长度,故不是简单累加。
    written: u64,
    /// 0 = 首份 `kedai-<date>.log`;n>0 = 轮转产物 `kedai-<date>.<n>.log`
    seq: u32,
    /// 单文件上限(生产恒为 MAX_FILE_BYTES;测试用 with_limit 注入小值)
    max_bytes: u64,
}

impl DailyFileWriter {
    fn new(dir: &Path) -> Self {
        Self::with_limit(dir, MAX_FILE_BYTES)
    }

    /// 带显式上限的构造(测试注入小上限用;生产走 new)
    fn with_limit(dir: &Path, max_bytes: u64) -> Self {
        let today = Local::now().format("%Y-%m-%d").to_string();
        // 重启续写:找到当日最后一个文件;若它已满则另起一份,避免往满文件继续追加。
        let seq = Self::resume_seq(dir, &today, max_bytes);
        let (file, written) = Self::open(dir, &today, seq);
        DailyFileWriter {
            dir: dir.to_path_buf(),
            today,
            file,
            written,
            seq,
            max_bytes,
        }
    }

    /// 序号 → 文件路径:0 为 `kedai-<date>.log`,n 为 `kedai-<date>.<n>.log`
    fn file_name(date: &str, seq: u32) -> String {
        if seq == 0 {
            format!("{FILE_PREFIX}{date}.log")
        } else {
            format!("{FILE_PREFIX}{date}.{seq}.log")
        }
    }

    /// 打开(或创建)指定序号的当日文件,返回句柄与其当前长度。
    /// 打开失败返回 (None, 0) —— 调用方据此进入「静默丢弃」路径,与旧行为一致。
    fn open(dir: &Path, date: &str, seq: u32) -> (Option<File>, u64) {
        let path = dir.join(Self::file_name(date, seq));
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(f) => {
                let size = f.metadata().map(|m| m.len()).unwrap_or(0);
                (Some(f), size)
            }
            Err(_) => (None, 0),
        }
    }

    /// 解析目录中当日各份日志的序号(供 max_seq 与淘汰使用)。
    /// 只认 `<date>.log` 与 `<date>.<n>.log` 两种形态,其它日期/命名的文件一概忽略。
    fn seqs_of(dir: &Path, date: &str) -> Vec<u32> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let dotted = format!("{date}.");
        let mut out = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(rest) = name.strip_prefix(FILE_PREFIX) else {
                continue;
            };
            let Some(stem) = rest.strip_suffix(".log") else {
                continue;
            };
            if stem == date {
                out.push(0);
            } else if let Some(n) = stem.strip_prefix(&dotted) {
                if let Ok(v) = n.parse::<u32>() {
                    out.push(v);
                }
            }
        }
        out.sort_unstable();
        out
    }

    /// 重启续写起点:当日无文件则 0;最后一个文件未满则续写它;已满则另起下一序号。
    /// 「已满」判据用 `>= max_bytes`:轮转本就是在写到「将要超限」前触发,
    /// 故正常轮转产物不会正好等于上限,此判据只兜「上次退出时刚好写满」这一情形。
    fn resume_seq(dir: &Path, date: &str, max_bytes: u64) -> u32 {
        let Some(&last) = Self::seqs_of(dir, date).last() else {
            return 0;
        };
        let size = fs::metadata(dir.join(Self::file_name(date, last)))
            .map(|m| m.len())
            .unwrap_or(0);
        if size >= max_bytes {
            last + 1
        } else {
            last
        }
    }

    /// 轮转到下一序号,并按 MAX_ROTATED_PER_DAY 淘汰当日最旧份。
    fn rotate(&mut self) {
        // 进程重启后 seq 从既有最大值继续,不会撞名覆盖已轮转文件
        self.seq = Self::seqs_of(&self.dir, &self.today)
            .last()
            .map_or(0, |&m| m.max(self.seq))
            + 1;
        let (file, written) = Self::open(&self.dir, &self.today, self.seq);
        self.file = file;
        self.written = written;
        self.prune_rotated();
    }

    /// 淘汰当日超出 MAX_ROTATED_PER_DAY 的最旧轮转份(序号小者旧)。
    /// 只删轮转产物,不动首份 `kedai-<date>.log` 与新写的那份——首份是「当日开场」,
    /// 保留它有利于「当天日志从哪里开始」的可读性,且它正是跨天清理的自然对象。
    fn prune_rotated(&self) {
        let seqs = Self::seqs_of(&self.dir, &self.today);
        // 首份(0)+ 轮转份;超限时从序号最小的轮转份删起
        if seqs.len() <= MAX_ROTATED_PER_DAY as usize {
            return;
        }
        let mut to_remove = seqs.len() - MAX_ROTATED_PER_DAY as usize;
        for seq in seqs {
            if to_remove == 0 {
                break;
            }
            if seq == 0 {
                continue; // 跳过首份,它不计入淘汰
            }
            let path = self.dir.join(Self::file_name(&self.today, seq));
            if fs::remove_file(path).is_ok() {
                to_remove -= 1;
            }
        }
    }
}

impl Write for DailyFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // 跨天时切换文件(序号回到首份)
        let today = Local::now().format("%Y-%m-%d").to_string();
        if today != self.today {
            self.today = today;
            let seq = Self::resume_seq(&self.dir, &self.today, self.max_bytes);
            let (file, written) = Self::open(&self.dir, &self.today, seq);
            self.file = file;
            self.written = written;
            self.seq = seq;
        }
        // 超限轮转。`written > 0` 前置条件很重要:单条日志本身若大于上限(极长 payload),
        // 轮转到空文件也解决不了问题,只会每次写入都开一个新文件。此时直接写进当前文件,
        // 让它成为「唯一一个超限文件」,下一轮写入再轮转走。
        if self.written > 0 && self.written + buf.len() as u64 > self.max_bytes {
            self.rotate();
        }
        match self.file.as_mut() {
            Some(f) => {
                let n = f.write(buf)?;
                self.written += n as u64;
                Ok(n)
            }
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
    // RequestIdLayer 必须注册在这里:span 的 requestId extension 由它写入,
    // 两个 PinoFormat 层才能在事件行里合并出 requestId。
    let _ = tracing_subscriber::registry()
        .with(RequestIdLayer)
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
    use tracing_subscriber::filter::LevelFilter;
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

    /// 带 RequestIdLayer 的捕获:在 span extension 生效的 subscriber 下运行 f,
    /// 返回解析后的全部日志行(批次 2 span 穿透测试用)
    fn capture_lines_with_request_id_layer(f: impl FnOnce()) -> Vec<Value> {
        let writer = CaptureWriter::default();
        let buf = writer.0.clone();
        let subscriber = tracing_subscriber::registry().with(RequestIdLayer).with(
            tracing_subscriber::fmt::layer()
                .event_format(PinoFormat)
                .with_writer(writer)
                .with_filter(LevelFilter::TRACE),
        );
        tracing::subscriber::with_default(subscriber, f);
        let bytes = buf.lock().unwrap().clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("应为合法 JSON 行"))
            .collect()
    }

    /// 单行版本(取首行)
    fn capture_line_with_request_id_layer(f: impl FnOnce()) -> Value {
        capture_lines_with_request_id_layer(f)
            .into_iter()
            .next()
            .expect("应有一行日志")
    }

    #[test]
    fn span扩展的request_id合并进事件行() {
        // 模拟中间件:span 上写 requestId 字段,内层事件不重复声明 requestId,
        // 靠 RequestIdLayer 存的 extension 穿透到 JSON 行
        let obj = capture_line_with_request_id_layer(|| {
            let span = tracing::info_span!("http_request", requestId = "req-abc-123");
            span.in_scope(|| {
                tracing::info!(status = 200u16, duration_ms = 3u64, method = "GET", "http");
            });
        });
        assert_eq!(obj["level"], json!("info"));
        assert_eq!(obj["requestId"], json!("req-abc-123"));
        assert_eq!(obj["status"], json!(200));
        assert_eq!(obj["duration_ms"], json!(3));
    }

    #[test]
    fn 事件显式字段优先于span扩展的request_id() {
        let obj = capture_line_with_request_id_layer(|| {
            let span = tracing::info_span!("http_request", requestId = "from-span");
            span.in_scope(|| {
                tracing::info!(requestId = "from-event", "显式覆盖");
            });
        });
        assert_eq!(obj["requestId"], json!("from-event"));
    }

    #[test]
    fn 无request_id的span不凭空产生该字段() {
        let obj = capture_line_with_request_id_layer(|| {
            tracing::info!(note = "外出", "无 span");
        });
        assert!(obj.get("requestId").is_none());
    }

    // ===== 容量上限与轮转(计划批次 6.1)=====
    // 这些用例直接对 DailyFileWriter 断言(不经 tracing 层):轮转是纯文件层行为,
    // 走 subscriber 反而引入格式噪声,且无法精确控制每次写入的字节数。

    use crate::utils::test_support::TempDataDir;
    use std::time::{Duration, SystemTime};

    /// 目录里当日所有日志文件名(排序后)
    fn log_names(dir: &Path, date: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(FILE_PREFIX) && n.ends_with(".log"))
            .collect();
        names.sort();
        // 断言 date 参数被用到,避免「静默只匹配前缀」的假通过
        assert!(
            names.iter().all(|n| n.contains(date)),
            "应只匹配当日文件,实际 {names:?}"
        );
        names
    }

    fn today() -> String {
        Local::now().format("%Y-%m-%d").to_string()
    }

    #[test]
    fn 未超上限不轮转() {
        let tmp = TempDataDir::new("log-rotate-under");
        let mut w = DailyFileWriter::with_limit(tmp.path(), 1024);
        w.write_all(b"short line\n").unwrap();
        w.flush().unwrap();
        assert_eq!(
            log_names(tmp.path(), &today()),
            vec![format!("{FILE_PREFIX}{}.log", today())]
        );
    }

    #[test]
    fn 超上限轮转出新文件且命名仍匹配清理过滤() {
        let tmp = TempDataDir::new("log-rotate-over");
        let date = today();
        let mut w = DailyFileWriter::with_limit(tmp.path(), 64);
        // 每次 40 字节:第一次写入后 written=40;第二次 40+40>64 触发轮转
        w.write_all(&[b'a'; 40]).unwrap();
        w.write_all(&[b'b'; 40]).unwrap();
        w.write_all(&[b'c'; 40]).unwrap();
        w.flush().unwrap();

        let names = log_names(tmp.path(), &date);
        assert_eq!(
            names,
            vec![
                format!("{FILE_PREFIX}{date}.1.log"),
                format!("{FILE_PREFIX}{date}.2.log"),
                format!("{FILE_PREFIX}{date}.log"),
            ],
            "应产生首份 + 两份轮转产物"
        );
        // 关键不变量:轮转产物的前缀与后缀都保留,故 cleanup_old_logs 的既有过滤
        // (starts_with(FILE_PREFIX) && ends_with(".log"))无需改动即可覆盖它们
        for n in &names {
            assert!(
                n.starts_with(FILE_PREFIX) && n.ends_with(".log"),
                "{n} 未被清理过滤覆盖"
            );
        }
        // 各份内容互不混淆
        let read = |n: &str| fs::read(tmp.path().join(n)).unwrap();
        assert_eq!(read(&format!("{FILE_PREFIX}{date}.log")), vec![b'a'; 40]);
        assert_eq!(read(&format!("{FILE_PREFIX}{date}.1.log")), vec![b'b'; 40]);
        assert_eq!(read(&format!("{FILE_PREFIX}{date}.2.log")), vec![b'c'; 40]);
    }

    #[test]
    fn 轮转后按新文件重新计数() {
        let tmp = TempDataDir::new("log-rotate-count");
        let mut w = DailyFileWriter::with_limit(tmp.path(), 100);
        w.write_all(&[b'x'; 80]).unwrap();
        assert_eq!(w.written, 80);
        // 80 + 80 > 100 → 轮转,written 重置为新文件长度(0)
        w.write_all(&[b'y'; 80]).unwrap();
        assert_eq!(w.seq, 1, "应已轮转到第 1 份");
        assert_eq!(w.written, 80, "应在新文件上重新计数,而非在 80 上累加");
        // 再写 30:80+30>100 → 再轮转
        w.write_all(&[b'z'; 30]).unwrap();
        assert_eq!(w.seq, 2);
        assert_eq!(w.written, 30);
        w.flush().unwrap();
    }

    #[test]
    fn 单条超上限的写入不产生空转轮转() {
        let tmp = TempDataDir::new("log-rotate-oversized");
        let mut w = DailyFileWriter::with_limit(tmp.path(), 16);
        // 首份为空(written==0)时写入一条超大 payload:轮转无意义,应直接写入
        w.write_all(&[b'q'; 100]).unwrap();
        w.flush().unwrap();
        assert_eq!(w.seq, 0, "空文件上的超长写入不应轮转");
        assert_eq!(w.written, 100);
        let names = log_names(tmp.path(), &today());
        assert_eq!(names.len(), 1, "不应产生多余文件");
    }

    #[test]
    fn 重启后续写未满文件而不是另起一份() {
        let tmp = TempDataDir::new("log-rotate-resume");
        {
            let mut w = DailyFileWriter::with_limit(tmp.path(), 1000);
            w.write_all(b"first\n").unwrap();
            w.flush().unwrap();
        } // 模拟进程退出:writer 析构,文件保留
        let mut w = DailyFileWriter::with_limit(tmp.path(), 1000);
        assert_eq!(w.seq, 0, "未满应续写首份");
        assert_eq!(w.written, 6, "written 应取既有文件长度,而非从 0 开始");
        w.write_all(b"second\n").unwrap();
        w.flush().unwrap();
        let content = fs::read(tmp.path().join(format!("{FILE_PREFIX}{}.log", today()))).unwrap();
        assert_eq!(content, b"first\nsecond\n");
    }

    #[test]
    fn 重启后续写已满文件时另起下一序号() {
        let tmp = TempDataDir::new("log-rotate-resume-full");
        let date = today();
        {
            let mut w = DailyFileWriter::with_limit(tmp.path(), 8);
            w.write_all(b"12345678").unwrap(); // 正好写满
            w.flush().unwrap();
        }
        let mut w = DailyFileWriter::with_limit(tmp.path(), 8);
        assert_eq!(w.seq, 1, "首份已满,应另起第 1 份而非继续追加");
        assert_eq!(w.written, 0);
        w.write_all(b"next").unwrap();
        w.flush().unwrap();
        // 首份内容未被破坏
        assert_eq!(
            fs::read(tmp.path().join(format!("{FILE_PREFIX}{date}.log"))).unwrap(),
            b"12345678"
        );
    }

    #[test]
    fn 当日轮转份数超限时淘汰最旧份() {
        let tmp = TempDataDir::new("log-rotate-prune");
        // 上限远小于写入量,触发多次轮转,总数超过 MAX_ROTATED_PER_DAY
        let mut w = DailyFileWriter::with_limit(tmp.path(), 4);
        for i in 0..(MAX_ROTATED_PER_DAY + 4) {
            w.write_all(&[b'0' + (i % 10) as u8; 4]).unwrap();
        }
        w.flush().unwrap();
        let names = log_names(tmp.path(), &today());
        assert!(
            names.len() <= MAX_ROTATED_PER_DAY as usize,
            "当日份数应被钳制在 {MAX_ROTATED_PER_DAY},实际 {} 份:{names:?}",
            names.len()
        );
        // 首份始终保留(它是当日开场,且是跨天清理的自然对象)
        assert!(
            names.contains(&format!("{FILE_PREFIX}{}.log", today())),
            "首份不应被淘汰:{names:?}"
        );
        // 最旧的轮转份(序号小者)应已被删除
        assert!(
            !names.contains(&format!("{FILE_PREFIX}{}.1.log", today())),
            "序号最小的轮转份应被淘汰:{names:?}"
        );
    }

    #[test]
    fn cleanup_old_logs覆盖轮转产物() {
        let tmp = TempDataDir::new("log-rotate-cleanup");
        let old_first = format!("{FILE_PREFIX}2020-01-01.log");
        let old_rotated = format!("{FILE_PREFIX}2020-01-01.3.log");
        let fresh = format!("{FILE_PREFIX}{}.2.log", today());
        for n in [&old_first, &old_rotated, &fresh] {
            fs::write(tmp.path().join(n), b"x\n").unwrap();
        }
        // 注意:cleanup_old_logs 按**文件 mtime** 判定新旧,与文件名里的日期无关。
        // 故必须真的把「旧」文件的修改时间回拨,否则刚写的文件 mtime 是现在、永远不删。
        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 24 * 3600);
        for n in [&old_first, &old_rotated] {
            OpenOptions::new()
                .write(true)
                .open(tmp.path().join(n))
                .unwrap()
                .set_modified(ten_days_ago)
                .unwrap();
        }

        cleanup_old_logs(tmp.path());

        let left: Vec<String> = fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(left, vec![fresh], "陈旧的轮转产物应随日志一并清理");
    }
}
