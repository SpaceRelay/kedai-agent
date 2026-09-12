// 数据库快照备份:经 SQLite Online Backup API 生成一致性快照(含未 checkpoint 的 WAL)。
use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags};
use std::fs;
use std::path::Path;

pub fn snapshot_database(source: &Path, destination: &Path) -> Result<(), String> {
    if !source.is_file() {
        return Err(format!("数据库不存在: {}", source.display()));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建快照目录失败: {e}"))?;
    }
    if destination.exists() {
        fs::remove_file(destination).map_err(|e| format!("移除旧快照失败: {e}"))?;
    }
    let source = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打开源数据库失败: {e}"))?;
    source
        .busy_timeout(std::time::Duration::from_secs(30))
        .map_err(|e| format!("设置快照超时失败: {e}"))?;
    let mut destination =
        Connection::open(destination).map_err(|e| format!("创建数据库快照失败: {e}"))?;
    let backup = Backup::new(&source, &mut destination)
        .map_err(|e| format!("初始化 SQLite backup 失败: {e}"))?;
    backup
        .run_to_completion(128, std::time::Duration::from_millis(10), None)
        .map_err(|e| format!("生成 SQLite 快照失败: {e}"))?;
    Ok(())
}
