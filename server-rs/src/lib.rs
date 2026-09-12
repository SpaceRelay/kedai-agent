// Kedai 后端库:供 main 与集成测试使用
// 优化项 B-4:生产代码 unwrap 告警(锁中毒/None 解包等应显式处理);
// not(test) 豁免 #[cfg(test)] 单测与 tests/ 集成测试(测试编译期 cfg(test) 生效,lint 关闭)。
#![cfg_attr(not(test), warn(clippy::unwrap_used))]
pub mod agents;
pub mod api;
pub mod config;
pub mod connectors;
pub mod contracts;
pub mod mcp;
pub mod migration;
pub mod models;
pub mod parsing;
pub mod plugins;
pub mod scripts;
pub mod services;
pub mod slash;
pub mod tools;
pub mod utils;

use std::sync::Once;

/// 构建测试应用:mock 连接器 + 临时数据目录
pub fn build_test_app() -> Result<axum::Router, String> {
    static INIT: Once = Once::new();
    let mut data_dir = std::env::temp_dir();
    data_dir.push(format!("kedai-test-{}", std::process::id()));
    // 每次构建前清空:消除 Windows PID 复用或上次运行残留的旧 DB/文件
    let _ = std::fs::remove_dir_all(&data_dir);
    let _ = std::fs::create_dir_all(&data_dir);

    INIT.call_once(|| {
        std::env::set_var("CONNECTOR", "mock");
        std::env::set_var("DATA_DIR", &data_dir);
        std::env::set_var("LOG_LEVEL", "error");
    });

    let mut config = config::AppConfig::from_env();
    // 既有集成测试聚焦业务接缝;安全中间件另用显式配置测试。
    config.auth_required = false;
    let state = api::app_state::AppState::new(config)?;
    Ok(api::build_router(state))
}

/// 构建启用鉴权的隔离测试应用。
pub fn build_secure_test_app(token: &str) -> Result<axum::Router, String> {
    secure_test_app(token, false)
}

/// 构建启用鉴权且开启 `strict_client_header` 写请求强化校验的隔离测试应用。
pub fn build_strict_test_app(token: &str) -> Result<axum::Router, String> {
    secure_test_app(token, true)
}

fn secure_test_app(token: &str, strict_client_header: bool) -> Result<axum::Router, String> {
    let mut data_dir = std::env::temp_dir();
    data_dir.push(format!("kedai-secure-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let mut config = config::AppConfig::from_env();
    config.data_dir = data_dir;
    config.connector = "mock".into();
    config.api_token = token.to_string();
    config.api_token_injected = true;
    config.bootstrap_enabled = true;
    config.auth_required = true;
    config.strict_client_header = strict_client_header;
    let state = api::app_state::AppState::new(config)?;
    Ok(api::build_router(state))
}

/// 启动 HTTP 服务并运行,直至进程退出或收到 Ctrl+C(优雅关闭)。
/// 供命令行入口 `kedai-server.exe` 与 Tauri 桌面壳复用:
/// - 命令行入口:直接 await 本函数,退出码由调用方决定;
/// - Tauri 壳:`tauri::async_runtime::spawn` 本函数,进程退出即服务停止
///   (Windows GUI 进程收不到 Ctrl+C,serve 将持续运行,无副作用)。
pub async fn run_server(config: config::AppConfig) -> Result<(), String> {
    let loopback = matches!(config.host.as_str(), "127.0.0.1" | "localhost" | "::1");
    if !loopback && !config.allow_remote {
        return Err("非 loopback 监听必须显式设置 KEDAI_ALLOW_REMOTE=1".into());
    }
    if !loopback && (!config.api_token_injected || config.api_token.len() < 32) {
        return Err("非 loopback 监听要求由环境或启动器注入至少 32 字符的 KEDAI_API_TOKEN".into());
    }

    // 初始化日志(tracing,non-blocking 双写:控制台 + logs/kedai-YYYY-MM-DD.log);
    // guard 持有到 run_server 返回,保证优雅关闭时 non-blocking 缓冲落盘
    let _log_guards = utils::logging::init(&config.log_level, &config.log_dir);

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        host = config.host.as_str(),
        port = config.port,
        data_dir = config.data_dir.to_string_lossy().as_ref(),
        connector = config.connector.as_str(),
        model = config.openai_model.as_str(),
        "Kedai server starting"
    );

    let state = api::app_state::AppState::new(config).map_err(|e| {
        tracing::error!(error = e.as_str(), "初始化失败");
        format!("初始化失败: {e}")
    })?;

    // MCP stdio 服务器装配(批次 6.2,L3 隔离):mcp_enabled=false 时完全跳过;
    // 单台握手最坏 30s 超时(有界),失败仅禁用该台,不阻断启动。
    state.start_mcp().await;

    let addr = format!("{}:{}", state.config.host, state.config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
        tracing::error!(
            addr = addr.as_str(),
            error = e.to_string().as_str(),
            "端口绑定失败"
        );
        format!("无法监听 {addr}: {e}")
    })?;

    let router = api::build_router(state.clone());

    // 有效模型以合并 settings 后为准(「Kedai server starting」行的 env model 可能因设置页覆盖而失真)
    let effective_model = state
        .model
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    tracing::info!(
        url = format!("http://{addr}/").as_str(),
        model = effective_model.as_str(),
        "Kedai 已启动"
    );
    println!("[OK] Kedai 已启动 → http://{addr}/");

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(|e| format!("服务运行出错: {e}"))?;

    // 优雅关闭兜底:显式关停 MCP 子进程(各句柄 kill_on_drop 双保险,防孤儿)
    state.mcp.shutdown().await;
    Ok(())
}

/// 优雅关闭:Ctrl+C(命令行场景)。GUI 进程无控制台信号,此 future 不会完成。
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("收到退出信号,正在关闭");
}
