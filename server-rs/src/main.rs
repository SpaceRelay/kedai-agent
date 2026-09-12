// Kedai 后端入口:Rust + axum + SQLite + SSE(命令行模式)
// 桌面场景请使用 src-tauri(Tauri 壳复用 kedai_server::run_server)
use kedai_server::config;

#[tokio::main]
async fn main() {
    let config = config::AppConfig::from_env();

    // 启动前自检:健康检查通过说明已有实例在跑,直接复用退出;
    // 否则启动服务(bind 失败会由 run_server 明确报错,不存在 TOCTOU 竞态)。
    // 注:此处在 run_server 之前,tracing 尚未初始化,事件与旧 logger 一样静默丢弃,
    // 用户可见输出由下方 println 承担(与迁移前行为一致)。
    if let Some(url) = probe_existing(&config).await {
        tracing::info!(url = url.as_str(), "检测到已有 Kedai 服务在运行,直接退出");
        println!("[OK] Kedai 服务已在运行 → {url}");
        return;
    }

    if let Err(e) = kedai_server::run_server(config).await {
        eprintln!("[错误] {e}");
        std::process::exit(1);
    }
}

/// 探测本机是否已有 Kedai 服务在跑(最多 3 次,间隔 500ms,响应体须含 ok:true)。
async fn probe_existing(config: &config::AppConfig) -> Option<String> {
    let url = format!("http://{}:{}/api/health", config.host, config.port);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(1200))
        .build()
        .ok()?;
    for _ in 0..3 {
        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                        return Some(url);
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    None
}
