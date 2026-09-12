// 运行时主 Agent 提示词文件服务：统一 DATA_DIR 路径、兼容旧文件迁移与原子写入。
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

pub const RUNTIME_PROMPT_FILE: &str = "AGENTS_RUNTIME.md";
pub const MAX_RUNTIME_PROMPT_BYTES: u64 = 512 * 1024;

/// 主 Agent 运行时提示词的内置默认(新装/首次运行、数据目录尚无该文件时兜底)。
///
/// 存在意义:此前该提示词只以「用户数据文件」形态存在,全新安装(含 Android 首装)
/// 不含此文件 → 引擎读到空 → 主 Agent 缺少角色定位/创作原则/工具使用原则/输出纪律
/// 这段最高层约定,行为与 Win 端不一致。内置后两端开箱一致,用户仍可在设置里覆盖
/// (保存即落盘为 AGENTS_RUNTIME.md,文件优先于本默认)。
///
/// 内容与 Win 端调好的版本一致:`{{char}}` 由引擎按角色名替换(见 messages/context.rs)。
pub const DEFAULT_RUNTIME_PROMPT: &str = "\
# 主 Agent 提示词（运行时）

## 角色定位

你是 Kedai 的主 Agent：以角色「{{char}}」的扮演者与文学创作者身份，与用户进行沉浸式角色扮演 / 文学创作。你的正文是小说文本而非聊天记录，以“能否被称为一段好小说”为最低验收标准。

## 创作原则

1. 严格以系统要求的视角与口吻输出，不出现旁白标题、“以上是回复”“作为AI”等元文本；角色设定与世界观保持一致。
2. 用户指令优先：用户提出的字数、风格、视角、情节走向要求必须服从；用户提问必须正面回答，不回避。
3. 不转述：用户输入的动作与对话视为已经发生，直接从其后无缝衔接继续创作。
4. 推进节奏：一次输出不把事件推进至末尾，适当拆分、合理安排节奏，正文末尾为用户留下可互动的窗口。

## 工具使用原则

- 你可以通过 function calling 调用工具（可用清单由系统注入，见 system 消息末尾的【可用工具】段）。需要时再调用，不必每轮都调；工具是辅助手段，不要因调用工具而中断创作节奏。
- 需要随机数 / 掷骰决定走向时用 `role`；需要联网查最新信息时用 `search`；需要读取世界书、角色设定、长期记忆等资料时用 `read` / `memory_read`。
- 需要把内容写入对话气泡或角色文件时用 `write` / `replace` / `create`；需要把可并行的子任务交给后台处理时用 `agentgo`，并用 `todo` / `read` 轮询结果，用 `agentend` 结束任务。
- 变量状态更新优先经 function calling 工具完成；仅在未走工具时才使用 `<UpdateVariable>` 文本协议作为回退。除该块外不使用任何自定义标签。

## 输出纪律

一次只输出角色回应本身；若需分段，使用空行，不使用 Markdown 标题。工具调用与正文创作分离：先按需调用工具获取资料或写入状态，再输出正文。
";

#[derive(Debug)]
pub struct RuntimePromptService {
    path: PathBuf,
    legacy_path: Option<PathBuf>,
    /// 文件与旧文件都不存在时是否回退内置默认。
    /// 显式指定提示词目录(`KEDAI_RUNTIME_PROMPT_DIR`,测试与自定义部署用)时置 false:
    /// 该变量语义是「只从这里读」,空目录即视为无提示词,不注入内置默认——
    /// 测试据此隔离真实默认,避免 mock 断言被内置文本干扰。
    builtin_default: bool,
}

impl RuntimePromptService {
    pub fn new(data_dir: PathBuf, legacy_path: Option<PathBuf>) -> Self {
        Self {
            path: data_dir.join(RUNTIME_PROMPT_FILE),
            legacy_path,
            builtin_default: true,
        }
    }

    /// 显式指定提示词目录(关闭内置默认回退)。来源:`KEDAI_RUNTIME_PROMPT_DIR`。
    pub fn with_dir(dir: PathBuf, legacy_path: Option<PathBuf>) -> Self {
        Self {
            path: dir.join(RUNTIME_PROMPT_FILE),
            legacy_path,
            builtin_default: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取 DATA_DIR 中的运行时提示词。新文件缺失时，安全读取旧文件并尝试一次性迁移；
    /// 迁移失败仍返回已校验的旧内容，避免升级后提示词突然丢失。
    /// 文件与旧文件都不存在时回退内置默认(见 `DEFAULT_RUNTIME_PROMPT`)。
    pub fn read(&self) -> Result<String, String> {
        match read_checked(&self.path) {
            Ok(content) => return Ok(content),
            Err(ReadError::NotFound) => {}
            Err(e) => return Err(e.message(&self.path, "读取")),
        }

        // 旧版项目根文件:仅作一次性迁移来源。不存在则继续走内置默认。
        let legacy = self
            .legacy_path
            .as_ref()
            .filter(|p| p.as_path() != self.path.as_path());
        if let Some(legacy) = legacy {
            match read_checked(legacy) {
                Ok(content) => {
                    // 不覆盖并发创建的新文件。AlreadyExists 表示其他写入者已先完成，改读新文件。
                    return match self.create_if_absent(content.as_bytes()) {
                        Ok(true) => Ok(content),
                        Ok(false) => read_checked(&self.path)
                            .map_err(|e| e.message(&self.path, "读取并发迁移后的")),
                        Err(_) => Ok(content),
                    };
                }
                Err(ReadError::NotFound) => {}
                Err(e) => return Err(e.message(legacy, "读取旧版")),
            }
        }

        // 两者都不存在:回退内置默认;显式指定目录时视为无提示词(供测试/自定义部署隔离)。
        if self.builtin_default {
            return Ok(DEFAULT_RUNTIME_PROMPT.to_string());
        }
        Err(format!("运行时提示词不存在：{}", self.path.display()))
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

    /// 全新安装(数据目录无该文件、项目根也无旧文件)回退内置默认:
    /// Android/Win 首装开箱即含主 Agent 提示词,不再依赖用户手工放置文件。
    #[test]
    fn falls_back_to_builtin_default_when_no_file() {
        let root = dir("builtin");
        let service = RuntimePromptService::new(root.clone(), None);
        let content = service.read().unwrap();
        assert!(content.contains("主 Agent 提示词"), "内置默认应含标题: {content}");
        assert!(content.contains("工具使用原则"), "内置默认应含工具使用原则");
        assert!(
            content.contains("{{char}}"),
            "内置默认保留角色宏,由引擎按角色名替换"
        );
        assert_eq!(
            service.read_optional().unwrap().as_deref(),
            Some(content.as_str()),
            "read_optional 也应拿到内置默认(否则引擎不注入)"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// 文件优先于内置默认:用户保存过的内容不被默认覆盖
    #[test]
    fn file_wins_over_builtin_default() {
        let root = dir("builtin-override");
        let service = RuntimePromptService::new(root.clone(), None);
        service.write("我的自定义主提示词").unwrap();
        assert_eq!(service.read().unwrap(), "我的自定义主提示词");
        // 空文件 = 显式不注入(read_optional 视空为 None)
        service.write("   ").unwrap();
        assert_eq!(service.read_optional().unwrap(), None);
        let _ = fs::remove_dir_all(root);
    }

    /// 显式指定目录(KEDAI_RUNTIME_PROMPT_DIR)关闭内置默认:
    /// 空目录视为无提示词,测试据此隔离真实默认、避免 mock 断言被内置文本干扰。
    #[test]
    fn with_dir_disables_builtin_default() {
        let root = dir("builtin-off");
        let service = RuntimePromptService::with_dir(root.clone(), None);
        assert!(service.read().unwrap_err().contains("不存在"));
        assert_eq!(service.read_optional().unwrap(), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_oversized_and_invalid_utf8_content() {        let root = dir("validation");
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
