// MCP 服务器子进程托管(批次 6.2,L3 隔离)。
//
// 生命周期设计(保守:仓库首个 Command/Child 先例):
// - spawn 时 stdin/stdout 管道接 McpClient,stderr 捕获逐行进日志(不静默吞掉);
// - Child 以 kill_on_drop(true) 创建:任何路径 Drop 都会尝试 kill,防孤儿进程;
// - 显式 kill() 供「握手失败/装配失败即禁用该服务器」路径使用,kill 后 wait 回收僵尸;
// - Drop 内只做同步 best-effort kill(start_kill),异步 wait 由运行时回收兜底。
//
// 注意:kill 只杀直接子进程;若服务器命令是 cmd/sh 包装再启孙进程,孙进程不保证回收
// (v1 保守语义,注释留痕;如需进程组级清理另起批次)。
use crate::services::settings_service::McpServerConfig;
// Android 上 spawn 被平台门控(直接返回 Err),这些仅桌面/服务端派生进程所需
#[cfg(not(target_os = "android"))]
use tokio::io::{AsyncBufReadExt, BufReader};
#[cfg(not(target_os = "android"))]
use tokio::process::Command;
use tokio::process::Child;

use super::client::McpClient;

pub struct McpProcess {
    /// 服务器配置名(日志标签用)
    name: String,
    /// kill_on_drop(true):句柄析构即杀进程,是孤儿进程的最后兜底
    child: Child,
    /// stderr 捕获任务:把服务器 stderr 行转发到 kedai 日志
    stderr_task: Option<tokio::task::JoinHandle<()>>,
}

impl McpProcess {
    /// 启动子进程并把 stdout/stdin 接到新 McpClient。
    /// spawn 失败(命令不存在等)直接 Err,由调用方记 warn 并禁用该服务器。
    ///
    /// Android 门控:移动端沙箱内没有 npx/uvx/node/python 等可执行环境,也没有可用的
    /// 进程派生模型(spawn 必然失败),直接返回带明确说明的错误,避免用户只看到一句
    /// 含义不明的 spawn 失败。MCP 在 Android 后续应改为内置执行器实现(见
    /// docs/android-port-plan.md 的移动专项待办)。
    #[cfg(target_os = "android")]
    pub fn spawn(cfg: &McpServerConfig) -> Result<(Self, McpClient), String> {
        Err(format!(
            "MCP 服务器 \"{}\" 在 Android 上暂不支持:移动端无法派生外部命令({}),\
             请改用内置工具或在桌面端使用 MCP。",
            cfg.name, cfg.command
        ))
    }

    /// 启动子进程并把 stdout/stdin 接到新 McpClient。
    /// spawn 失败(命令不存在等)直接 Err,由调用方记 warn 并禁用该服务器。
    #[cfg(not(target_os = "android"))]
    pub fn spawn(cfg: &McpServerConfig) -> Result<(Self, McpClient), String> {
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // 句柄 Drop 即杀进程:防孤儿兜底(配合显式 kill 双保险)
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            format!(
                "MCP 服务器 \"{}\" 启动失败({}): {e}。下一步:核对设置里的 command/args 是否存在于本机",
                cfg.name, cfg.command
            )
        })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "MCP 子进程 stdout 管道缺失".to_string())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "MCP 子进程 stdin 管道缺失".to_string())?;

        // stderr 捕获:转发到 kedai 日志(带服务器名标签),便于排查服务器自身报错
        let stderr_task = child.stderr.take().map(|stderr| {
            let tag = cfg.name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::warn!(
                        server = tag.clone(),
                        line = line.chars().take(500).collect::<String>(),
                        "MCP 服务器 stderr"
                    );
                }
            })
        });

        let client = McpClient::new(Box::new(stdout), Box::new(stdin));
        Ok((
            McpProcess {
                name: cfg.name.clone(),
                child,
                stderr_task,
            },
            client,
        ))
    }

    /// 显式终止:kill 后 wait 回收,避免僵尸;stderr 任务随管道 EOF 自行结束(超时兜底 abort)。
    pub async fn kill(&mut self) {
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
        // 已退出的进程 kill 会报错,属正常(目标本就是「确保不在跑」),不向上传播
        let _ = self.child.kill().await;
    }

    /// 进程是否仍在运行(测试与诊断用)
    #[cfg(test)]
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
        // kill_on_drop(true) 下 Child 析构已会 start_kill;这里显式再调一次是
        // 幂等保险(进程已退出时 start_kill 返回 Err,忽略)。
        let _ = self.child.start_kill();
        tracing::info!(server = self.name.clone(), "MCP 服务器进程已终止");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(command: &str, args: &[&str]) -> McpServerConfig {
        McpServerConfig {
            name: "test".into(),
            command: command.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            enabled: true,
        }
    }

    /// 短命进程:spawn 成功、自然退出、Drop 不 panic 不悬挂。
    /// 只用 `cmd /c exit 0` 级别的即时退出命令——不起长驻进程,避免 CI 不稳定与孤儿。
    #[cfg(windows)]
    #[tokio::test]
    async fn spawn_short_lived_process_and_drop_cleanly() {
        let (mut proc, _client) =
            McpProcess::spawn(&cfg("cmd", &["/c", "exit 0"])).expect("cmd 应可启动");
        // 进程可能已退出;kill 对已退出进程幂等(忽略错误)
        proc.kill().await;
        assert!(!proc.is_running(), "kill 后进程不应在运行");
        drop(proc); // Drop 路径:不 panic
    }

    /// spawn 不存在的命令:返回带「下一步」指引的错误,不 panic
    #[cfg(windows)]
    #[tokio::test]
    async fn spawn_missing_command_errors_gracefully() {
        let result = McpProcess::spawn(&cfg("kedai-no-such-command-9z8y7x", &[]));
        let err = result.err().expect("不存在的命令应启动失败");
        assert!(err.contains("启动失败"), "错误应含上下文: {err}");
        assert!(err.contains("下一步"), "错误应含指引: {err}");
    }

    /// 非 Windows 平台不强行起进程:仅编译级覆盖(类型/路径可编译),
    /// 理由:本套件目标平台为 Windows,CI 若跨平台跑也不应因起进程而飘红。
    #[cfg(not(windows))]
    #[test]
    fn compile_level_coverage_on_non_windows() {
        let _ = cfg("true", &[]);
    }
}
