// 配置加载:环境变量 + .env,与 Node 版语义一致
use rusqlite::{Connection, OpenFlags};
use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    /// 数据目录(绝对路径):SQLite + characters + avatars
    pub data_dir: PathBuf,
    /// 日志目录(绝对路径)
    pub log_dir: PathBuf,
    /// web 前端产物目录;仅 `KEDAI_WEB_DIST` 显式配置时启用磁盘资源
    pub web_dist: Option<PathBuf>,
    /// "openai-compatible" | "mock"
    pub connector: String,
    /// 去掉尾部斜杠
    pub openai_base_url: String,
    pub openai_api_key: String,
    pub openai_model: String,
    pub default_temperature: f64,
    pub default_top_p: f64,
    pub default_max_tokens: u32,
    /// 上下文窗口上限(token):发送给 LLM 的历史超出后按时间裁剪
    pub default_max_context_tokens: u32,
    pub log_level: String,
    /// 本地 API bearer token,仅驻留内存;可由 KEDAI_API_TOKEN 显式覆盖。
    pub api_token: String,
    /// 是否强制 API 鉴权(仅测试构造器可关闭)。
    pub auth_required: bool,
    /// token 是否由环境或启动器显式注入。
    pub api_token_injected: bool,
    /// 非 loopback 监听须通过 KEDAI_ALLOW_REMOTE=1 显式放行。
    pub allow_remote: bool,
    /// 仅 loopback 监听可通过同源 bootstrap 向页面交付 token。
    pub bootstrap_enabled: bool,
    /// 写请求强化校验(默认关):开启后写请求必须携带 X-Kedai-Client 头
    /// 且带 Origin(堵跨站表单/图片 GET 之外的无 Origin 写旁路)。由 KEDAI_STRICT_CLIENT_HEADER=1 开启。
    pub strict_client_header: bool,
}

fn env_str(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

/// 项目根目录定位:从 exe 所在目录向上逐级查找,找到同时含 `web` 与 `server-rs`(或 `data`)的目录。
/// 兼容开发(server-rs/target/debug)与发布(server-rs/target/release)两种布局。
/// 找不到时回退到当前工作目录。
fn project_root() -> PathBuf {
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let mut cur = Some(dir.to_path_buf());
            for _ in 0..6 {
                if let Some(p) = cur {
                    if p.join("web").is_dir()
                        && (p.join("server-rs").is_dir() || p.join("data").is_dir())
                    {
                        return p;
                    }
                    cur = p.parent().map(|x| x.to_path_buf());
                }
            }
        }
    }
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// 用户级统一数据目录(2026-09 修复双库分叉):
/// Windows 上为 %APPDATA%\com.kedai.app\data(与 Tauri 桌面版同一目录,见 src-tauri/lib.rs)。
/// 仅当该目录已含用户数据时才返回 Some——空壳目录(曾启动过一次但没真正用过)不抢占。
/// 非 Windows 或无用户数据时返回 None,回退项目根 data/。
fn canonical_user_data_dir() -> Option<PathBuf> {
    let appdata = env::var_os("APPDATA").filter(|v| !v.is_empty())?;
    let dir = PathBuf::from(appdata).join("com.kedai.app").join("data");
    if dir_has_user_data(&dir) {
        Some(dir)
    } else {
        None
    }
}

/// 「已有用户数据」判据:settings.json 存在(存过设置即生成),或 kedai.db 里有
/// 非内置角色 / 任何会话(排除桌面版首启自动建库留下的空壳)。
fn dir_has_user_data(dir: &Path) -> bool {
    if dir.join("settings.json").is_file() {
        return true;
    }
    let db = dir.join("kedai.db");
    if !db.is_file() {
        return false;
    }
    let Ok(conn) = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    let has = |sql: &str| -> bool {
        conn.query_row(sql, [], |row| row.get::<_, bool>(0))
            .unwrap_or(false)
    };
    // 表可能尚不存在(全新库):查询失败按 false 处理
    has("SELECT EXISTS(SELECT 1 FROM characters WHERE id <> 'builtin-system')")
        || has("SELECT EXISTS(SELECT 1 FROM sessions)")
}

/// 最小 AppConfig(仅供测试:不读环境变量,避免受本机 .env 与用户数据目录影响)。
/// 设置加载/保存类测试共用同一份字面量,避免多处复制漂移。
#[cfg(test)]
pub(crate) fn test_config() -> AppConfig {
    AppConfig {
        host: "127.0.0.1".into(),
        port: 0,
        data_dir: std::env::temp_dir(),
        log_dir: std::env::temp_dir(),
        web_dist: None,
        connector: "mock".into(),
        openai_base_url: "https://example.com/v1".into(),
        openai_api_key: String::new(),
        openai_model: "test-model".into(),
        default_temperature: 0.8,
        default_top_p: 0.9,
        default_max_tokens: 1024,
        default_max_context_tokens: 65_536,
        log_level: "info".into(),
        api_token: "test-token".into(),
        auth_required: false,
        api_token_injected: true,
        allow_remote: false,
        bootstrap_enabled: true,
        strict_client_header: false,
    }
}

impl AppConfig {
    pub fn from_env() -> Self {
        let _ = dotenvy::dotenv(); // 向上搜索 .env(默认从 cwd 开始)

        let root = project_root();
        // 数据目录解析顺序(2026-09 统一):DATA_DIR 环境变量 > 用户级统一目录
        // (%APPDATA%\com.kedai.app\data,已有用户数据时) > 项目根 data/。
        // 背景:桌面版(Tauri)强制 %APPDATA%,而直接跑 kedai-server.exe / start.ps1 曾用
        // 项目目录,两套库并行分叉——一边写的聊天记录另一边不可见(「重进后记录丢失」主因)。
        let data_dir = match env_str("DATA_DIR") {
            Some(d) => absolutize(Path::new(&d), &root),
            None => canonical_user_data_dir().unwrap_or_else(|| root.join("data")),
        };
        // LOG_DIR 支持环境变量(Tauri 桌面场景注入,避免写入不可写的安装目录);缺省与 data 同级
        let log_dir = match env_str("LOG_DIR") {
            Some(d) => absolutize(Path::new(&d), &root),
            None => root.join("logs"),
        };
        let web_dist = env_str("KEDAI_WEB_DIST").map(|d| absolutize(Path::new(&d), &root));

        let base_url =
            env_str("OPENAI_BASE_URL").unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let base_url = base_url.trim_end_matches('/').to_string();

        let connector = env_str("CONNECTOR").unwrap_or_else(|| "openai-compatible".into());
        let connector = if connector == "openai-compatible" || connector == "mock" {
            connector
        } else {
            "mock".into() // 未知回退 mock
        };

        let temperature = env_str("DEFAULT_TEMPERATURE")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.8);
        let top_p = env_str("DEFAULT_TOP_P")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.9);
        let max_tokens = env_str("DEFAULT_MAX_TOKENS")
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| (1..=131_072).contains(v))
            .unwrap_or(1024);
        let max_context = env_str("DEFAULT_MAX_CONTEXT_TOKENS")
            .and_then(|v| v.parse::<u32>().ok())
            // 上下文窗口范围:最低 64K,上限 1M
            .filter(|v| (65_536..=1_048_576).contains(v))
            .unwrap_or(65_536);
        let port = env_str("PORT")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(3001);

        let host = env_str("HOST").unwrap_or_else(|| "127.0.0.1".into());
        let allow_remote = matches!(
            env_str("KEDAI_ALLOW_REMOTE").as_deref(),
            Some("1") | Some("true")
        );
        let strict_client_header = matches!(
            env_str("KEDAI_STRICT_CLIENT_HEADER").as_deref(),
            Some("1") | Some("true")
        );
        let loopback = matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1");
        let injected_token = env_str("KEDAI_API_TOKEN");
        let api_token = injected_token.clone().unwrap_or_else(|| {
            format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )
        });

        AppConfig {
            host,
            port,
            data_dir,
            log_dir,
            web_dist,
            connector,
            openai_base_url: base_url,
            openai_api_key: env_str("OPENAI_API_KEY").unwrap_or_default(),
            openai_model: env_str("OPENAI_MODEL").unwrap_or_else(|| "gpt-4o-mini".into()),
            default_temperature: temperature,
            default_top_p: top_p,
            default_max_tokens: max_tokens,
            default_max_context_tokens: max_context,
            log_level: env_str("LOG_LEVEL").unwrap_or_else(|| "info".into()),
            api_token,
            auth_required: true,
            api_token_injected: injected_token.is_some(),
            allow_remote,
            bootstrap_enabled: loopback,
            strict_client_header,
        }
    }
}

fn absolutize(p: &Path, root: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}
