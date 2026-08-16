// Kedai 桌面壳(Tauri 2)
// 职责:统一桌面数据目录 → 首次幂等迁移旧数据 → 启动内嵌后端
// → 健康检查通过后再导航并展示窗口。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::Manager;

const READY_TIMEOUT: Duration = Duration::from_secs(60);
const READY_INTERVAL: Duration = Duration::from_millis(250);

pub fn run() {
    tauri::Builder::default()
        // 导出保存对话框 + 文件写入(用户自由选择导出位置)
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            let app_data = app.path().app_data_dir().unwrap_or_else(|_| {
                PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("com.kedai.app")
            });
            let data_dir = app_data.join("data");
            let log_dir = app_data.join("logs");

            std::fs::create_dir_all(&log_dir)?;
            if let Some(project_data) = find_project_data_dir() {
                if let Err(error) = migrate_data_if_needed(&project_data, &data_dir) {
                    write_start_error(&log_dir, &format!("旧数据迁移失败: {error}"));
                }
            }

            std::env::set_var("DATA_DIR", &data_dir);
            std::env::set_var("LOG_DIR", &log_dir);
            eprintln!("[信息] DATA_DIR={}", data_dir.display());

            let config = kedai_server::config::AppConfig::from_env();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = start_and_wait_ready(config, app_handle.clone()).await {
                    write_start_error(&log_dir, &error);
                    eprintln!("[错误] {error}");
                    app_handle.exit(1);
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("构建 Kedai 桌面应用失败")
        .run(|_, _| {});
}

async fn start_and_wait_ready(
    config: kedai_server::config::AppConfig,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let service_url = service_url(&config);

    // 桌面版必须使用本进程内、指向统一 AppData 的后端,不能复用项目目录中的浏览器服务。
    if health_ok(&config).await {
        return Err(format!(
            "端口 {} 已有 Kedai 服务运行,请先关闭浏览器开发服务后再启动桌面版",
            config.port
        ));
    }

    let (server_error_tx, mut server_error_rx) = tokio::sync::oneshot::channel();
    let server_config = config.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = kedai_server::run_server(server_config).await {
            let _ = server_error_tx.send(error);
        }
    });

    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let Ok(error) = server_error_rx.try_recv() {
            return Err(format!("Kedai 后端启动失败: {error}"));
        }
        if health_ok(&config).await {
            let window = app
                .get_webview_window("main")
                .ok_or_else(|| "找不到主窗口".to_string())?;
            window
                .navigate(url::Url::parse(&service_url).map_err(|e| format!("服务地址非法: {e}"))?)
                .map_err(|e| format!("主窗口导航失败: {e}"))?;
            window.show().map_err(|e| format!("主窗口显示失败: {e}"))?;
            window
                .set_focus()
                .map_err(|e| format!("主窗口聚焦失败: {e}"))?;
            return Ok(());
        }
        tokio::time::sleep(READY_INTERVAL).await;
    }

    Err(format!(
        "Kedai 后端在 {} 秒内未就绪: {service_url}",
        READY_TIMEOUT.as_secs()
    ))
}

fn service_url(config: &kedai_server::config::AppConfig) -> String {
    let host = match config.host.as_str() {
        "0.0.0.0" | "::" => "127.0.0.1",
        host => host,
    };
    format!("http://{host}:{}/", config.port)
}

async fn health_ok(config: &kedai_server::config::AppConfig) -> bool {
    let url = format!("{}api/health", service_url(config));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(1200))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    match client.get(url).send().await {
        Ok(response) if response.status().is_success() => matches!(
            response.json::<serde_json::Value>().await,
            Ok(body) if body.get("ok").and_then(|value| value.as_bool()).unwrap_or(false)
        ),
        _ => false,
    }
}

fn find_project_data_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut current = exe.parent()?.to_path_buf();
    for _ in 0..6 {
        let data = current.join("data");
        if data.is_dir() && (current.join("web").is_dir() || current.join("Kedai.exe").is_file()) {
            return Some(data);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// 仅当目标没有任何用户数据时执行复制。源目录始终保留,迁移可重复调用且不会覆盖目标。
fn migrate_data_if_needed(source: &Path, target: &Path) -> Result<usize, String> {
    if source == target || !has_migratable_data(source) || !target_is_pristine(target) {
        return Ok(0);
    }

    std::fs::create_dir_all(target).map_err(|e| format!("创建数据目录失败: {e}"))?;
    let copied = copy_tree(source, target).map_err(|e| format!("复制数据失败: {e}"))?;
    let source_db = source.join("kedai.db");
    if source_db.is_file() {
        kedai_server::migration::snapshot_database(&source_db, &target.join("kedai.db"))?;
    }
    rewrite_avatar_paths(target, source)?;

    let marker = format!(
        "source={}\nmigrated_at={}\nfiles={}\n",
        source.display(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or(0),
        copied
    );
    std::fs::write(target.join(".migration-v1"), marker)
        .map_err(|e| format!("写入迁移标记失败: {e}"))?;
    Ok(copied)
}

fn has_migratable_data(path: &Path) -> bool {
    path.join("kedai.db").is_file()
        || path.join("settings.json").is_file()
        || path.join("characters").is_dir()
        || path.join("avatars").is_dir()
}

fn target_is_pristine(path: &Path) -> bool {
    if !path.exists() {
        return true;
    }
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        if entry.file_name() == "builtin-system.json" {
            continue;
        }
        return false;
    }
    true
}

fn copy_tree(source: &Path, target: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(target)?;
    let mut copied = 0;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if name_text == "kedai.db"
            || name_text.ends_with("-wal")
            || name_text.ends_with("-shm")
            || name_text.ends_with(".log")
            || name_text.contains(".bak-")
            || name_text.starts_with(".verify")
        {
            continue;
        }
        let destination = target.join(name);
        if entry.path().is_dir() {
            copied += copy_tree(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), destination)?;
            copied += 1;
        }
    }
    Ok(copied)
}

fn rewrite_avatar_paths(target: &Path, old_data_dir: &Path) -> Result<(), String> {
    let database = target.join("kedai.db");
    if !database.is_file() {
        return Ok(());
    }
    let connection = rusqlite::Connection::open(&database)
        .map_err(|e| format!("打开迁移后的数据库失败: {e}"))?;
    let has_characters = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='characters')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|e| format!("检查角色表失败: {e}"))?;
    if !has_characters {
        return Ok(());
    }
    let old_prefix = old_data_dir.to_string_lossy();
    let new_prefix = target.to_string_lossy();
    connection
        .execute(
            "UPDATE characters SET avatar_path = replace(avatar_path, ?1, ?2) WHERE avatar_path LIKE ?3",
            rusqlite::params![old_prefix.as_ref(), new_prefix.as_ref(), format!("{}%", old_prefix)],
        )
        .map_err(|e| format!("修正头像路径失败: {e}"))?;
    Ok(())
}

fn write_start_error(log_dir: &Path, message: &str) {
    let _ = std::fs::create_dir_all(log_dir);
    let _ = std::fs::write(
        log_dir.join("tauri-start-error.log"),
        format!("{message}\n"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("kedai-desktop-{tag}-{stamp}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn migration_copies_once_and_keeps_source() {
        let source = temp_dir("source");
        let target = temp_dir("target");
        std::fs::write(source.join("settings.json"), "{}").unwrap();
        std::fs::create_dir_all(source.join("characters")).unwrap();
        std::fs::write(source.join("characters/card.json"), "{}").unwrap();
        let source_db = source.join("kedai.db");
        let connection = rusqlite::Connection::open(&source_db).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE snapshot_test(value TEXT); INSERT INTO snapshot_test VALUES('WAL 中的数据');")
            .unwrap();

        assert_eq!(migrate_data_if_needed(&source, &target).unwrap(), 2);
        assert!(source.join("settings.json").is_file(), "迁移不得删除源数据");
        assert!(target.join("settings.json").is_file());
        assert!(target.join("characters/card.json").is_file());
        let migrated = rusqlite::Connection::open(target.join("kedai.db")).unwrap();
        assert_eq!(
            migrated
                .query_row("SELECT COUNT(*) FROM snapshot_test", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1,
            "迁移必须通过 backup API 包含尚在 WAL 中的数据"
        );
        assert!(target.join(".migration-v1").is_file());

        std::fs::write(source.join("new.json"), "new").unwrap();
        assert_eq!(migrate_data_if_needed(&source, &target).unwrap(), 0);
        assert!(
            !target.join("new.json").exists(),
            "重复运行不得覆盖或追加目标数据"
        );

        std::fs::remove_dir_all(source).ok();
        std::fs::remove_dir_all(target).ok();
    }

    #[test]
    fn migration_never_overwrites_existing_target_data() {
        let source = temp_dir("used-source");
        let target = temp_dir("used-target");
        std::fs::write(source.join("settings.json"), "source").unwrap();
        std::fs::write(target.join("settings.json"), "target").unwrap();

        assert_eq!(migrate_data_if_needed(&source, &target).unwrap(), 0);
        assert_eq!(
            std::fs::read_to_string(target.join("settings.json")).unwrap(),
            "target"
        );

        std::fs::remove_dir_all(source).ok();
        std::fs::remove_dir_all(target).ok();
    }

    #[test]
    fn service_url_uses_loopback_for_unspecified_bind_host() {
        let mut config = kedai_server::config::AppConfig::from_env();
        config.host = "0.0.0.0".into();
        config.port = 4321;
        assert_eq!(service_url(&config), "http://127.0.0.1:4321/");
    }
}
