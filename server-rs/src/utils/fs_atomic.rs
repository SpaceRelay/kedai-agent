// 原子文件写入:先写同目录临时文件,再原子替换目标文件,进程中断/崩溃也不会在
// 目标留下半截内容。JSON sidecar(settings.json / tool_permissions.json /
// prompt_floors.json / audio.json / agent_flows.json 等)统一走这里(N3)。
//
// 替换语义(实现移植自 tools/permissions.rs 的成熟先例):
//   Windows 用 ReplaceFileW——std::fs::rename 在目标已存在时会失败,
//     ReplaceFileW 才能原子替换;目标不存在(首次写入)时退化为 rename(同目录原子)。
//   其他平台用 std::fs::rename(POSIX 同目录 rename 原子)。
// 临时文件与目标同目录(保证同卷,替换才原子),文件名带 uuid,
// 并发写同一目标时各自的临时文件互不覆盖;替换失败会尽力清理临时文件。
use std::path::Path;

/// 把 contents 原子写入 path;父目录不存在时先创建。
/// 失败返回 io 错误:目标文件保持旧内容(或不存在),不会残留半截文件。
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("atomic-write");
    let temp_path = path.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temp_path, contents)?;
    if let Err(error) = replace_file(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, path: &Path) -> std::io::Result<()> {
    std::fs::rename(temp_path, path)
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    #[link(name = "Kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
    }

    // ReplaceFileW 要求目标已存在;首次写入退化为 rename(同目录原子)
    if !path.exists() {
        return std::fs::rename(temp_path, path);
    }
    let replaced: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let replacement: Vec<u16> = temp_path.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_support::TempDataDir;

    /// 隔离临时目录(uuid 唯一 + 作用域结束自动清理)
    fn temp_dir(tag: &str) -> TempDataDir {
        TempDataDir::new(&format!("fs-atomic-{tag}"))
    }

    /// 目录下不应残留任何原子写临时文件(.{name}.*.tmp)
    fn assert_no_temp_residue(dir: &Path) {
        let entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            entries
                .iter()
                .all(|n| !n.starts_with('.') || !n.ends_with(".tmp")),
            "原子写成功后不应残留临时文件,实际: {entries:?}"
        );
    }

    #[test]
    fn write_new_file_creates_parent_and_content() {
        let dir = temp_dir("new");
        let target = dir.join("nested").join("data.json");
        write_atomic(&target, br#"{"a":1}"#).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), br#"{"a":1}"#);
        assert_no_temp_residue(&dir.join("nested"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overwrite_existing_file_replaces_content() {
        let dir = temp_dir("overwrite");
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("data.json");
        std::fs::write(&target, b"old-content").unwrap();
        write_atomic(&target, b"new-content").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new-content");
        assert_no_temp_residue(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_write_leaves_target_untouched_and_no_residue() {
        // 父路径被文件占用(无法建目录)→ 写失败;不产生临时文件,目标侧无变化
        let dir = temp_dir("fail");
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("blocked");
        std::fs::write(&blocker, b"i am a file").unwrap();
        let target = blocker.join("data.json");
        assert!(write_atomic(&target, b"x").is_err());
        assert_eq!(std::fs::read(&blocker).unwrap(), b"i am a file");
        assert_no_temp_residue(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
