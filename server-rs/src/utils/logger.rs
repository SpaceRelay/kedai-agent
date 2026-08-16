// 结构化 JSON 日志(pino 风格):控制台 + logs/kedai-YYYY-MM-DD.log 双写,按天归档,3 天清理
use chrono::Local;
use serde_json::json;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

struct Inner {
    stdout: std::io::Stdout,
    file: Option<File>,
    today: String,
}

pub struct Logger {
    level: String,
    log_dir: PathBuf,
    inner: Mutex<Inner>,
}

static LOGGER: Mutex<Option<Logger>> = Mutex::new(None);

fn rank(level: &str) -> i32 {
    match level {
        "debug" => 0,
        "info" => 1,
        "warn" => 2,
        "error" => 3,
        _ => 1,
    }
}

fn iso_now() -> String {
    Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%z").to_string()
}

fn log_file_name(today: &str) -> String {
    format!("kedai-{}.log", today)
}

impl Logger {
    fn new(level: &str, log_dir: &PathBuf) -> Self {
        let _ = fs::create_dir_all(log_dir);
        let today = Local::now().format("%Y-%m-%d").to_string();
        let path = log_dir.join(log_file_name(&today));
        let file = OpenOptions::new().create(true).append(true).open(path).ok();
        Logger {
            level: level.to_string(),
            log_dir: log_dir.clone(),
            inner: Mutex::new(Inner {
                stdout: std::io::stdout(),
                file,
                today,
            }),
        }
    }

    fn write(&self, lvl: &str, msg: &str, fields: &[(&str, serde_json::Value)]) {
        if rank(&self.level) > rank(lvl) {
            return;
        }
        let mut obj = json!({ "level": lvl, "time": iso_now(), "msg": msg });
        for (k, v) in fields {
            obj[k] = v.clone();
        }
        let line = obj.to_string() + "\n";
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let _ = g.stdout.write_all(line.as_bytes());
        // 跨天时切换文件
        let today = Local::now().format("%Y-%m-%d").to_string();
        if g.today != today {
            g.today = today.clone();
            let path = self.log_dir.join(log_file_name(&today));
            g.file = OpenOptions::new().create(true).append(true).open(path).ok();
        }
        if let Some(f) = g.file.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// 初始化日志(进程启动时调用一次)
pub fn init(level: &str, log_dir: &PathBuf) {
    let mut g = LOGGER.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(Logger::new(level, log_dir));
    cleanup_old_logs(log_dir);
}

/// 清理 3 天前的 kedai-*.log
pub fn cleanup_old_logs(log_dir: &PathBuf) {
    let Ok(entries) = fs::read_dir(log_dir) else {
        return;
    };
    let cutoff = Local::now() - chrono::Duration::days(3);
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with("kedai-") && name.ends_with(".log") {
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

pub fn info(msg: &str, fields: &[(&str, serde_json::Value)]) {
    if let Some(l) = LOGGER.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        l.write("info", msg, fields);
    }
}

pub fn warn(msg: &str, fields: &[(&str, serde_json::Value)]) {
    if let Some(l) = LOGGER.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        l.write("warn", msg, fields);
    }
}

pub fn error(msg: &str, fields: &[(&str, serde_json::Value)]) {
    if let Some(l) = LOGGER.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        l.write("error", msg, fields);
    }
}

pub fn debug(msg: &str, fields: &[(&str, serde_json::Value)]) {
    if let Some(l) = LOGGER.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        l.write("debug", msg, fields);
    }
}

/// Agent 步骤结构化日志: { sessionId, step, detail }
pub fn agent_step(session_id: &str, step: &str, detail: Option<&str>) {
    let mut fields = vec![
        ("sessionId", json!(session_id)),
        ("step", json!(step)),
        ("msg", json!(format!("[agent] {}", step))),
    ];
    if let Some(d) = detail {
        fields.push(("detail", json!(d)));
    }
    info("[agent]", &fields);
}
