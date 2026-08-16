// 运行时主 Agent 提示词文件服务：统一 DATA_DIR 路径、兼容旧文件迁移与原子写入。
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

pub const RUNTIME_PROMPT_FILE: &str = "AGENTS_RUNTIME.md";
pub const MAX_RUNTIME_PROMPT_BYTES: u64 = 512 * 1024;

#[derive(Debug)]
pub struct RuntimePromptService {
    path: PathBuf,
    legacy_path: Option<PathBuf>,
}

impl RuntimePromptService {
    pub fn new(data_dir: PathBuf, legacy_path: Option<PathBuf>) -> Self {
        Self {
            path: data_dir.join(RUNTIME_PROMPT_FILE),
            legacy_path,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取 DATA_DIR 中的运行时提示词。新文件缺失时，安全读取旧文件并尝试一次性迁移；
    /// 迁移失败仍返回已校验的旧内容，避免升级后提示词突然丢失。
    pub fn read(&self) -> Result<String, String> {
        match read_checked(&self.path) {
            Ok(content) => return Ok(content),
            Err(ReadError::NotFound) => {}
            Err(e) => return Err(e.message(&self.path, "读取")),
        }

        let legacy = self
            .legacy_path
            .as_ref()
            .filter(|p| p.as_path() != self.path.as_path())
            .ok_or_else(|| format!("运行时提示词不存在：{}", self.path.display()))?;
        let content = read_checked(legacy).map_err(|e| e.message(legacy, "读取旧版"))?;
        // 不覆盖并发创建的新文件。AlreadyExists 表示其他写入者已先完成，改读新文件。
        match self.create_if_absent(content.as_bytes()) {
            Ok(true) => Ok(content),
            Ok(false) => {
                read_checked(&self.path).map_err(|e| e.message(&self.path, "读取并发迁移后的"))
            }
            Err(_) => Ok(content),
        }
    }

    pub fn read_optional(&self) -> Result<Option<String>, String> {
        match self.read() {
            Ok(content) if content.trim().is_empty() => Ok(None),
            Ok(content) => Ok(Some(content)),
            Err(e) if e.starts_with("运行时提示词不存在：") => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn write(&self, content: &str) -> Result<(), String> {
        validate_bytes(content.as_bytes()).map_err(|e| e.message(&self.path, "写入"))?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| format!("运行时提示词路径无父目录：{}", self.path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|e| format!("创建运行时提示词目录失败 {}：{e}", parent.display()))?;
        let temp = temp_path(&self.path);
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|e| format!("创建运行时提示词临时文件失败 {}：{e}", temp.display()))?;
            file.write_all(content.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|e| format!("写入运行时提示词临时文件失败 {}：{e}", temp.display()))?;
            drop(file);
            atomic_replace(&temp, &self.path)
                .map_err(|e| format!("原子替换运行时提示词失败 {}：{e}", self.path.display()))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    fn create_if_absent(&self, bytes: &[u8]) -> Result<bool, String> {
        validate_bytes(bytes).map_err(|e| e.message(&self.path, "迁移"))?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "运行时提示词路径无父目录".to_string())?;
        fs::create_dir_all(parent).map_err(|e| format!("创建运行时提示词目录失败：{e}"))?;
        let temp = temp_path(&self.path);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| format!("创建迁移临时文件失败：{e}"))?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("写入迁移临时文件失败：{e}"))?;
        match fs::hard_link(&temp, &self.path) {
            Ok(()) => {
                let _ = fs::remove_file(&temp);
                Ok(true)
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temp);
                Ok(false)
            }
            Err(e) => {
                let _ = fs::remove_file(&temp);
                Err(format!("迁移运行时提示词失败：{e}"))
            }
        }
    }
}

#[derive(Debug)]
enum ReadError {
    NotFound,
    TooLarge(u64),
    InvalidUtf8(String),
    Io(String),
}

impl ReadError {
    fn message(&self, path: &Path, action: &str) -> String {
        match self {
            Self::NotFound => format!("运行时提示词不存在：{}", path.display()),
            Self::TooLarge(size) => format!(
                "{action}运行时提示词失败 {}：文件大小 {size} 字节，超过上限 {MAX_RUNTIME_PROMPT_BYTES} 字节",
                path.display()
            ),
            Self::InvalidUtf8(e) => format!("{action}运行时提示词失败 {}：内容不是有效 UTF-8（{e}）", path.display()),
            Self::Io(e) => format!("{action}运行时提示词失败 {}：{e}", path.display()),
        }
    }
}

fn validate_bytes(bytes: &[u8]) -> Result<(), ReadError> {
    if bytes.len() as u64 > MAX_RUNTIME_PROMPT_BYTES {
        return Err(ReadError::TooLarge(bytes.len() as u64));
    }
    std::str::from_utf8(bytes)
        .map(|_| ())
        .map_err(|e| ReadError::InvalidUtf8(e.to_string()))
}

fn read_checked(path: &Path) -> Result<String, ReadError> {
    let metadata = fs::metadata(path).map_err(|e| {
        if e.kind() == ErrorKind::NotFound {
            ReadError::NotFound
        } else {
            ReadError::Io(e.to_string())
        }
    })?;
    if metadata.len() > MAX_RUNTIME_PROMPT_BYTES {
        return Err(ReadError::TooLarge(metadata.len()));
    }
    let bytes = fs::read(path).map_err(|e| ReadError::Io(e.to_string()))?;
    validate_bytes(&bytes)?;
    String::from_utf8(bytes).map_err(|e| ReadError::InvalidUtf8(e.to_string()))
}

fn temp_path(path: &Path) -> PathBuf {
    let suffix = uuid::Uuid::new_v4();
    path.with_file_name(format!(".{RUNTIME_PROMPT_FILE}.{suffix}.tmp"))
}

#[cfg(not(windows))]
fn atomic_replace(temp: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(temp, target)
}

#[cfg(windows)]
fn atomic_replace(temp: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
    if !target.exists() {
        return fs::rename(temp, target);
    }
    let target_w: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let temp_w: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe {
        ReplaceFileW(
            target_w.as_ptr(),
            temp_w.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
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

    fn dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kedai-runtime-prompt-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn migrates_legacy_once_without_overwriting_new_file() {
        let root = dir("migration");
        let data = root.join("data");
        fs::create_dir_all(&data).unwrap();
        let legacy = root.join(RUNTIME_PROMPT_FILE);
        fs::write(&legacy, "旧提示词").unwrap();
        let service = RuntimePromptService::new(data.clone(), Some(legacy));
        assert_eq!(service.read().unwrap(), "旧提示词");
        assert_eq!(
            fs::read_to_string(data.join(RUNTIME_PROMPT_FILE)).unwrap(),
            "旧提示词"
        );
        fs::write(data.join(RUNTIME_PROMPT_FILE), "新提示词").unwrap();
        assert_eq!(service.read().unwrap(), "新提示词");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_oversized_and_invalid_utf8_content() {
        let root = dir("validation");
        let service = RuntimePromptService::new(root.clone(), None);
        let large = "x".repeat(MAX_RUNTIME_PROMPT_BYTES as usize + 1);
        assert!(service.write(&large).unwrap_err().contains("超过上限"));
        fs::write(service.path(), [0xff, 0xfe]).unwrap();
        assert!(service.read().unwrap_err().contains("不是有效 UTF-8"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_write_replaces_existing_content() {
        let root = dir("replace");
        let service = RuntimePromptService::new(root.clone(), None);
        service.write("第一版").unwrap();
        service.write("第二版").unwrap();
        assert_eq!(service.read().unwrap(), "第二版");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1, "不应残留临时文件");
        let _ = fs::remove_dir_all(root);
    }
}
