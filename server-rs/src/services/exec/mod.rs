// 命令执行抽象层(阶段 C):bash 工具与 Android 执行层共用的单一执行入口。
//
// 分层动机(见 docs/计划.md 决策 4):
// - 桌面/服务端:`std::process` 直接派生(desktop.rs);
// - Android:进程派生、su 弹窗、Shizuku binder 均为 Java API,必须经 Kotlin 桥
//   (android.rs),Rust 侧只做字符串进出。
// 执行器按 ROOT → Shizuku → Sandbox 逐级探测;探测结果暴露给前端展示(等级可见)。
//
// 与权限的关系:本层**不做授权判定**。授权/确认在调用方(bash 工具)经
// `tools::permissions` 与「命令级风险强制确认」完成后才落到这里执行。
// 本层只负责:按等级执行、限时、强杀、输出截断。

use serde::Serialize;

/// 执行器等级(危险度递减)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellTier {
    /// root 提权(su / Magisk / KernelSU)
    Root,
    /// Shizuku:以 ADB shell(UID 2000)权限执行,无需 root
    Shizuku,
    /// 沙箱:应用自身 UID,能力等于本进程
    Sandbox,
    /// 不可用(Android 未开任何通道 / 探测失败)
    Disabled,
}

impl ShellTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Shizuku => "shizuku",
            Self::Sandbox => "sandbox",
            Self::Disabled => "disabled",
        }
    }

    /// 面向用户的中文等级名(前端展示与确认卡)
    pub fn label(&self) -> &'static str {
        match self {
            Self::Root => "ROOT 提权",
            Self::Shizuku => "Shizuku(ADB 权限)",
            Self::Sandbox => "沙箱(应用自身权限)",
            Self::Disabled => "已禁用",
        }
    }

    /// 从线格式字符串解析(前端/设置侧传入的等级开关校验用)。
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "root" => Self::Root,
            "shizuku" => Self::Shizuku,
            "sandbox" => Self::Sandbox,
            _ => Self::Disabled,
        }
    }
}

/// 一次执行请求。
#[derive(Debug, Clone)]
pub struct ExecRequest {
    /// 完整命令原文(交给 shell 解释;调用方已完成风险分级与授权)
    pub command: String,
    /// 工作目录(None = 进程默认 cwd)
    pub cwd: Option<String>,
    /// 超时毫秒(调用方已夹取上限;None = 默认)
    pub timeout_ms: Option<u64>,
}

/// 一次执行结果。
#[derive(Debug, Clone, Serialize)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    /// 实际使用的执行器等级(供审计与前端展示)
    pub tier: ShellTier,
    /// 是否因超时被强杀
    pub timed_out: bool,
}

/// 输出保留上限(字符):超出部分截断并附说明。
/// 与 tools/registry 的 64KB 结果截断同量级,避免一条命令把上下文灌满。
pub const MAX_OUTPUT_CHARS: usize = 32_768;

/// 默认与上限超时(毫秒)。上限防止模型给出超大值导致挂起。
pub const DEFAULT_TIMEOUT_MS: u64 = 60_000;
pub const MAX_TIMEOUT_MS: u64 = 300_000;

/// 夹取超时到 [1s, MAX_TIMEOUT_MS]。
pub fn clamp_timeout(ms: Option<u64>) -> u64 {
    ms.unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(1_000, MAX_TIMEOUT_MS)
}

/// 截断输出并附省略说明(按字符,避免切坏多字节)。
pub fn truncate_output(s: &str) -> String {
    let count = s.chars().count();
    if count <= MAX_OUTPUT_CHARS {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}\n…(输出超 {MAX_OUTPUT_CHARS} 字符已截断,完整内容见审计日志)")
}

/// 命令执行的单一入口:按当前可用等级执行。
/// 平台实现见 desktop.rs / android.rs;本函数只做等级分派与结果归一。
pub async fn execute(req: ExecRequest, allowed_tiers: &[ShellTier]) -> Result<ExecResult, String> {
    // 「等级可见 + 可关闭」的落点:探测到的等级必须被用户显式放行。
    // 桌面端恒为 Sandbox,由 bash 工具的 exec_enabled 总开关把关,此处一并生效。
    let tier = detect_tier();
    if !allowed_tiers.contains(&tier) {
        return Err(format!(
            "当前执行器等级「{}」未在设置中开启;请在「设置 → 授权管理 → 命令执行」放行该等级。",
            tier.label()
        ));
    }
    #[cfg(target_os = "android")]
    {
        return android::execute(req).await;
    }
    #[cfg(not(target_os = "android"))]
    {
        desktop::execute(req).await
    }
}

/// 探测当前可用等级(供前端展示与设置校验)。
pub fn detect_tier() -> ShellTier {
    #[cfg(target_os = "android")]
    {
        android::detect_tier()
    }
    #[cfg(not(target_os = "android"))]
    {
        // 桌面/服务端:可直接派生子进程,恒为 Sandbox(无提权语义)
        ShellTier::Sandbox
    }
}

pub mod audit;

#[cfg(not(target_os = "android"))]
pub mod desktop;

#[cfg(target_os = "android")]
pub mod android;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_clamped_to_bounds() {
        assert_eq!(clamp_timeout(None), DEFAULT_TIMEOUT_MS);
        assert_eq!(clamp_timeout(Some(1)), 1_000, "低于下限抬到 1s");
        assert_eq!(
            clamp_timeout(Some(10_000_000)),
            MAX_TIMEOUT_MS,
            "超上限夹到 300s"
        );
        assert_eq!(clamp_timeout(Some(5_000)), 5_000);
    }

    #[test]
    fn truncate_keeps_short_output_intact() {
        assert_eq!(truncate_output("hello"), "hello");
    }

    #[test]
    fn truncate_marks_long_output() {
        let long = "字".repeat(MAX_OUTPUT_CHARS + 100);
        let out = truncate_output(&long);
        assert!(out.starts_with('字'));
        assert!(out.contains("已截断"));
        // 截断后总长受控
        assert!(out.chars().count() < long.chars().count());
    }

    #[test]
    fn tier_roundtrip() {
        for (s, t) in [
            ("root", ShellTier::Root),
            ("shizuku", ShellTier::Shizuku),
            ("sandbox", ShellTier::Sandbox),
            ("disabled", ShellTier::Disabled),
            ("garbage", ShellTier::Disabled),
        ] {
            assert_eq!(ShellTier::from_str_lossy(s), t, "解析 {s}");
            // 未知值回退 disabled,故仅对已知值断言原文往返
            if s != "garbage" {
                assert_eq!(t.as_str(), s, "往返 {s}");
            }
        }
    }
}
