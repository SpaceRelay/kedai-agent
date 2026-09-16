// 命令风险分级(执行类工具的判定依据)。
//
// 定位(与 docs/契约-协议与配置.md 一致):
// - 本模块只做**语义标注**(这条命令属于哪一级),不构成安全边界。
//   命令字符串可被混淆/拼接/编码绕过,静态匹配天然不完备——真正的边界是
//   「三档授权矩阵 + 高风险逐条确认 + 审计留痕」,本分级只决定「要不要额外确认」。
// - 分级结果用于:① 决定是否走「独立于三档的强制确认」;② 写入审计行的 risk 字段;
//   ③ 确认卡与审计面板向用户展示风险标签。
//
// 四级(危险度递增):Safe < Sensitive < Destructive < Admin。
//   Safe        只读检视类,不修改任何状态
//   Sensitive   工作区内写(建文件/复制/移动/就地编辑/重定向)
//   Destructive 可能造成不可逆数据丢失(删除、格式化、覆写设备)
//   Admin       提权或系统级控制(su/sudo/服务管理/网络配置/包管理/设备控制)
//
// 判定顺序:Admin → Destructive → Sensitive → Safe(取最高危)。多命令串联
// (`&&`/`;`/`|`)逐段判定后取最高危,避免「安全命令 && 危险命令」被降级放行。

/// 命令风险级别。
/// 命令风险级别**已下沉到 L1**（`crate::models::tool_policy::CommandRisk`，2026-09-14）。
///
/// 本模块的职责边界随之澄清：
/// - **保留在此（L3）**：分级表（`ADMIN_COMMANDS` 等）与 `classify_command` 算法
///   ——这是**业务判定**，依赖具体命令语义；
/// - **已下沉（L1）**：`CommandRisk` 枚举本身——它是被 `services/exec/audit.rs`（审计落库）
///   等 L2 代码共享的**领域词汇**。
///
/// 若枚举留在本模块，`services/exec/audit.rs:11` 就会构成 L2→L3 越代依赖。
/// 详见 `models/tool_policy.rs` 头部与 `docs/契约-架构与数据.md` §2.2。
pub use crate::models::tool_policy::CommandRisk;

/// 提权或系统级控制的命令名(取最高危,先判)。
const ADMIN_COMMANDS: &[&str] = &[
    "su",
    "sudo",
    "doas",
    "pkexec",
    // 服务 / 内核 / 网络 / 挂载
    "systemctl",
    "service",
    "insmod",
    "rmmod",
    "modprobe",
    "iptables",
    "ip6tables",
    "nft",
    "ufw",
    "firewall-cmd",
    "mount",
    "umount",
    "swapon",
    "swapoff",
    "sysctl",
    // 账户 / 权限
    "useradd",
    "userdel",
    "usermod",
    "groupadd",
    "passwd",
    "chown",
    "chgrp",
    "visudo",
    // 包管理(会改系统状态)
    "apt",
    "apt-get",
    "dpkg",
    "yum",
    "dnf",
    "rpm",
    "pacman",
    "apk",
    "brew",
    "winget",
    "choco",
    // Android 系统控制
    "pm",
    "am",
    "settings",
    // Windows 系统级
    "reg",
    "sc",
    "net",
    "bcdedit",
    "diskpart",
    "takeown",
    "icacls",
];

/// 破坏性命令名(不可逆数据丢失或覆写)。
const DESTRUCTIVE_COMMANDS: &[&str] = &[
    "rm",
    "rmdir",
    "shred",
    "truncate",
    "dd",
    "mkfs",
    "mke2fs",
    "fdisk",
    "parted",
    "format",
    "wipefs",
    "blkdiscard",
    "kill",
    "pkill",
    "killall",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "chmod",
    "chattr",
];

/// 写类命令名(工作区内修改,可逆性中等)。
const SENSITIVE_COMMANDS: &[&str] = &[
    "mkdir",
    "touch",
    "cp",
    "mv",
    "rename",
    "ln",
    "tee",
    "sed",
    "perl",
    "awk",
    "gzip",
    "gunzip",
    "tar",
    "zip",
    "unzip",
    "curl",
    "wget",
    "git",
    "npm",
    "pnpm",
    "yarn",
    "pip",
    "pip3",
    "cargo",
    "make",
    "cmake",
    "node",
    "python",
    "python3",
    "java",
    "go",
    "dotnet",
    "powershell",
    "pwsh",
    "reg",
    "set",
    "copy",
    "xcopy",
    "robocopy",
    "del",
    "erase",
    "ren",
    "move",
    "md",
];

/// 只读安全命令名。
const SAFE_COMMANDS: &[&str] = &[
    "ls",
    "dir",
    "pwd",
    "cd",
    "cat",
    "type",
    "head",
    "tail",
    "wc",
    "echo",
    "printf",
    "grep",
    "egrep",
    "fgrep",
    "find",
    "which",
    "where",
    "whoami",
    "id",
    "date",
    "env",
    "printenv",
    "uname",
    "hostname",
    "df",
    "du",
    "stat",
    "file",
    "tree",
    "sort",
    "uniq",
    "cut",
    "tr",
    "diff",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "true",
    "false",
    "test",
    "expr",
    "history",
    "ps",
    "top",
    "free",
    "uptime",
    "who",
    "w",
    "less",
    "more",
    "man",
    "help",
    "ver",
    "systeminfo",
    "tasklist",
];

/// 取命令行中首个 token 的命令名(处理绝对路径:`/usr/bin/rm` → `rm`;
/// Windows `C:\Windows\System32\cmd.exe` → `cmd`)。
fn basename_of(token: &str) -> String {
    let t = token.trim_matches(|c: char| c == '"' || c == '\'');
    let last = t.rsplit(['/', '\\']).next().unwrap_or(t);
    // 去扩展名(.exe/.cmd/.bat)
    let stem = last.rsplit_once('.').map(|(a, _)| a).unwrap_or(last);
    stem.to_ascii_lowercase()
}

/// 把整条命令按连接符拆成若干段(逐段判定,取最高危)。
/// 覆盖:`&&`、`||`、`;`、`|`、换行、Windows 的 `&`。
fn split_segments(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '&' | '|' => {
                // 吞掉第二个(& 或 |),保留单字符与双字符等价处理
                if chars.peek() == Some(&c) {
                    chars.next();
                }
                out.push(std::mem::take(&mut cur));
            }
            ';' | '\n' | '\r' => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out.retain(|s| !s.trim().is_empty());
    out
}

/// 判定单个命令段的风险级。
fn classify_segment(seg: &str) -> CommandRisk {
    let trimmed = seg.trim();
    if trimmed.is_empty() {
        return CommandRisk::Safe;
    }
    // 重定向写出(`>`/`>>`)本身就是写操作;若已含写类命令以更高危为准
    let has_write_redirect = trimmed.contains('>');

    // 取首个非环境变量赋值的 token 作为命令名
    let mut first = String::new();
    for tok in trimmed.split_whitespace() {
        if tok.contains('=') && !tok.starts_with('-') && !first.is_empty() {
            continue;
        }
        if tok.contains('=') && tok.starts_with(|c: char| c.is_ascii_uppercase() || c == '_') {
            continue; // 形如 FOO=bar 的前置赋值
        }
        first = tok.to_string();
        break;
    }
    if first.is_empty() {
        return CommandRisk::Safe;
    }
    let name = basename_of(&first);

    // 提权:Admin
    if ADMIN_COMMANDS.contains(&name.as_str()) {
        return CommandRisk::Admin;
    }
    // 破坏性:Destructive;chmod/chattr 仅在其参数含广泛开放位时视为破坏性
    if DESTRUCTIVE_COMMANDS.contains(&name.as_str()) {
        if name == "chmod" || name == "chattr" {
            let risky =
                trimmed.contains("777") || trimmed.contains("666") || trimmed.contains("+s");
            return if risky {
                CommandRisk::Destructive
            } else {
                CommandRisk::Sensitive
            };
        }
        return CommandRisk::Destructive;
    }
    // find/grep 等只读命令带破坏性子选项时升级
    if name == "find"
        && (trimmed.contains("-delete") || trimmed.contains("-exec") || trimmed.contains("-ok"))
    {
        return CommandRisk::Destructive;
    }
    if name == "git" {
        // git 的只读子命令保持 Safe;其余(commit/push/reset/clean/checkout)按写处理;
        // reset --hard / clean -fd 具破坏性
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("reset --hard")
            || lower.contains("clean -")
            || lower.contains("push --force")
        {
            return CommandRisk::Destructive;
        }
        if lower.contains(" status")
            || lower.contains(" log")
            || lower.contains(" diff")
            || lower.contains(" show")
            || lower.contains(" branch")
            || lower.contains(" remote")
        {
            return CommandRisk::Safe;
        }
        return CommandRisk::Sensitive;
    }
    // 写类
    if SENSITIVE_COMMANDS.contains(&name.as_str()) {
        return CommandRisk::Sensitive;
    }
    // 只读类
    if SAFE_COMMANDS.contains(&name.as_str()) {
        // 只读命令却带写重定向 → 至少 Sensitive
        return if has_write_redirect {
            CommandRisk::Sensitive
        } else {
            CommandRisk::Safe
        };
    }
    // 未识别的命令:保守归 Sensitive(可能是自定义脚本/工具),
    // 但不擅自升到 Destructive(否则大量正常命令都要额外确认,确认疲劳反而降低安全性)。
    // 带写重定向同样落此级,故无需再分支。
    CommandRisk::Sensitive
}

/// 判定整条命令的风险级(多段取最高危)。
/// `shell` 保留参数:不同 shell 的语义差异(如 PowerShell `Remove-Item`)后续可细分;
/// 当前判定以命令名为主,与 shell 无关。
pub fn classify_command(cmd: &str, _shell: &str) -> CommandRisk {
    let segs = split_segments(cmd);
    if segs.is_empty() {
        return CommandRisk::Safe;
    }
    segs.iter()
        .map(|s| classify_segment(s))
        .max()
        .unwrap_or(CommandRisk::Safe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_readonly_commands() {
        for c in [
            "ls -la",
            "pwd",
            "cat README.md",
            "grep -n todo src/lib.rs",
            "echo hello",
        ] {
            assert_eq!(
                classify_command(c, "sh"),
                CommandRisk::Safe,
                "应为只读安全: {c}"
            );
        }
    }

    #[test]
    fn write_commands_are_sensitive() {
        for c in [
            "mkdir -p a/b",
            "touch f.txt",
            "cp a.txt b.txt",
            "sed -i s/a/b/ f.txt",
        ] {
            assert_eq!(
                classify_command(c, "sh"),
                CommandRisk::Sensitive,
                "应为写入: {c}"
            );
        }
    }

    #[test]
    fn destructive_commands_detected() {
        for c in [
            "rm -rf build",
            "rmdir old",
            "dd if=/dev/zero of=/dev/sda",
            "mkfs.ext4 /dev/sdb1",
            "shutdown -h now",
            "pkill -9 node",
            "chmod 777 /etc/passwd",
        ] {
            assert_eq!(
                classify_command(c, "sh"),
                CommandRisk::Destructive,
                "应为破坏性: {c}"
            );
        }
    }

    #[test]
    fn admin_commands_detected() {
        for c in [
            "sudo apt-get install vim",
            "su -c id",
            "systemctl restart nginx",
            "iptables -L",
            "pm list packages",
            "am start -n a/b",
        ] {
            assert_eq!(
                classify_command(c, "sh"),
                CommandRisk::Admin,
                "应为提权/系统: {c}"
            );
        }
    }

    #[test]
    fn chained_commands_take_highest_risk() {
        // 「安全命令 && 危险命令」不得被降级放行
        assert_eq!(
            classify_command("echo start && rm -rf /tmp/x", "sh"),
            CommandRisk::Destructive
        );
        assert_eq!(
            classify_command("pwd; sudo reboot", "sh"),
            CommandRisk::Admin
        );
        assert_eq!(classify_command("ls | grep foo", "sh"), CommandRisk::Safe);
        assert_eq!(
            classify_command("cat f > out.txt", "sh"),
            CommandRisk::Sensitive
        );
    }

    #[test]
    fn absolute_path_and_windows_extensions_normalized() {
        assert_eq!(
            classify_command("/usr/bin/rm -rf /tmp", "sh"),
            CommandRisk::Destructive
        );
        assert_eq!(
            classify_command(r"C:\Windows\System32\cmd.exe /c dir", "cmd"),
            CommandRisk::Sensitive
        );
        assert_eq!(classify_command("ls.exe -la", "cmd"), CommandRisk::Safe);
    }

    #[test]
    fn find_with_delete_upgraded() {
        assert_eq!(
            classify_command("find . -name '*.log' -delete", "sh"),
            CommandRisk::Destructive
        );
        assert_eq!(
            classify_command("find . -name '*.log'", "sh"),
            CommandRisk::Safe
        );
    }

    #[test]
    fn git_subcommands_graded() {
        assert_eq!(classify_command("git status", "sh"), CommandRisk::Safe);
        assert_eq!(
            classify_command("git log --oneline", "sh"),
            CommandRisk::Safe
        );
        assert_eq!(
            classify_command("git commit -m x", "sh"),
            CommandRisk::Sensitive
        );
        assert_eq!(
            classify_command("git reset --hard HEAD~1", "sh"),
            CommandRisk::Destructive
        );
    }

    #[test]
    fn unknown_command_is_sensitive_not_destructive() {
        // 未识别命令归 Sensitive:既保守也不制造确认疲劳
        assert_eq!(
            classify_command("./my-script.sh", "sh"),
            CommandRisk::Sensitive
        );
    }

    #[test]
    fn confirm_requirement_matrix() {
        assert!(!CommandRisk::Safe.requires_explicit_confirm());
        assert!(!CommandRisk::Sensitive.requires_explicit_confirm());
        assert!(CommandRisk::Destructive.requires_explicit_confirm());
        assert!(CommandRisk::Admin.requires_explicit_confirm());
    }

    #[test]
    fn ordering_is_by_severity() {
        assert!(CommandRisk::Safe < CommandRisk::Sensitive);
        assert!(CommandRisk::Sensitive < CommandRisk::Destructive);
        assert!(CommandRisk::Destructive < CommandRisk::Admin);
    }

    #[test]
    fn empty_and_whitespace() {
        assert_eq!(classify_command("", "sh"), CommandRisk::Safe);
        assert_eq!(classify_command("   ", "sh"), CommandRisk::Safe);
    }
}
