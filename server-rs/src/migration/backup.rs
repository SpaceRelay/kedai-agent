// 数据库快照备份:经 SQLite Online Backup API 生成一致性快照(含未 checkpoint 的 WAL)。
use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags};
use std::fs;
use std::path::{Path, PathBuf};

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

/// 升级前自动备份的落点目录名(相对 data_dir)
const PREUPGRADE_DIR: &str = "backups";
/// 升级前备份的文件名前缀;`snapshot_database` 会删同名目标,故必须带时间戳保证唯一
const PREUPGRADE_PREFIX: &str = "kedai-preupgrade-";
/// 保留的升级前备份份数(按 mtime 保留最新)
const PREUPGRADE_KEEP: usize = 3;

/// 在 schema 升级**之前**对现有库做一次快照(DB-2 / 计划批次)。
///
/// 为什么需要:升级链虽已事务化(失败整体回滚),但**没有**任何前置快照——一旦用户需要
/// 退回旧版 exe,只能靠自己事前的备份。此处补上「升级前自动留档」,成本极低而覆盖所有
/// 降级路径(比逐版本写 down 迁移划算得多,见 `遗留.md` L25 / `展望.md` DB-2)。
///
/// 落点与命名:`<data_dir>/backups/kedai-preupgrade-v<旧版本>-<YYYYMMDD-HHMMSS>.db`。
/// 带时间戳是**必需**的:底层 `snapshot_database` 会先删除同名目标文件,固定名会互相覆盖,
/// 无法留下多份历史。
///
/// 返回值:成功返回快照路径;失败返回 `Err`——**调用方必须只记 warn 并继续升级**,
/// 备份失败绝不能让「本来能正常升级」的库变成「应用打不开」。
pub fn snapshot_before_upgrade(
    db_path: &Path,
    data_dir: &Path,
    old_version: i32,
) -> Result<PathBuf, String> {
    let dir = data_dir.join(PREUPGRADE_DIR);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let dest = dir.join(format!("{PREUPGRADE_PREFIX}v{old_version}-{stamp}.db"));
    snapshot_database(db_path, &dest)?;
    prune_preupgrade_backups(&dir);
    Ok(dest)
}

/// 只保留最新的 PREUPGRADE_KEEP 份升级前备份,其余按 mtime 从旧到新删除。
/// best-effort:单个文件删除失败不影响其它,也不向调用方报错(它本就只是收紧磁盘占用)。
fn prune_preupgrade_backups(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with(PREUPGRADE_PREFIX) || !name.ends_with(".db") {
                return None;
            }
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((mtime, e.path()))
        })
        .collect();
    if files.len() <= PREUPGRADE_KEEP {
        return;
    }
    // 新的在前;跳过前 PREUPGRADE_KEEP 份,其余删除
    files.sort_by_key(|b| std::cmp::Reverse(b.0));
    for (_, path) in files.into_iter().skip(PREUPGRADE_KEEP) {
        let _ = fs::remove_file(path);
    }
}
