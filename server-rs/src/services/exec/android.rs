// Android 执行器:经 Kotlin 桥执行(阶段 C/D)。
//
// 为什么必须在 Kotlin 侧执行(见 docs/计划.md 阶段 3 第 2 条):
// 进程派生、`su` 弹窗、Shizuku binder 都是 Java 层 API;Rust 虽能 `Command::new`,
// 但普通应用 UID 受 SELinux 约束,拿不到 root/ADB 权限。故 Rust 只做字符串进出
// (与 native_bridge_android.rs 同一模式),执行逻辑在 Kotlin `ShellExecutorBridge`。
//
// 入参编码约定(沿用 `\u{1f}` 单元分隔符,避免额外 JNI 签名):
//   exec:       command ␟ cwd ␟ timeoutMs        →  exitCode ␟ stdout ␟ stderr
//   detectTier: ""                               →  root|shizuku|sandbox|disabled
//   requestShizukuPermission: ""                 →  ""(成功)或错误消息
#![cfg(target_os = "android")]

use super::{truncate_output, ExecRequest, ExecResult, ShellTier};
use crate::services::jni_bridge::call_string_static;

/// Kotlin 桥类全限定名(JNI 用 `/` 分隔)
const EXEC_CLASS: &str = "com/kedai/app/ShellExecutorBridge";

/// 单元分隔符(US):与 Kotlin 侧 SEP 一致
const SEP: char = '\u{1f}';

/// 探测当前可用执行器等级(Kotlin 侧按 ROOT → Shizuku → Sandbox 顺序探测)。
/// 探测失败一律回退 Disabled:宁可不可用,不给「看似可用实则失败」的假象。
pub fn detect_tier() -> ShellTier {
    match call_string_static(EXEC_CLASS, "detectTier", "") {
        Ok(s) => ShellTier::from_str_lossy(s.trim()),
        Err(e) => {
            tracing::warn!(error = %e, "探测 Android 执行器等级失败,回退 disabled");
            ShellTier::Disabled
        }
    }
}

/// 经 Kotlin 桥执行命令。
/// 注意:授权与风险确认已在 bash 工具层完成,本函数只负责执行与结果解析。
pub async fn execute(req: ExecRequest) -> Result<ExecResult, String> {
    let tier = detect_tier();
    if tier == ShellTier::Disabled {
        return Err("Android 执行层不可用:请在设置中开启执行通道(沙箱 / Shizuku / ROOT)".into());
    }

    let timeout_ms = super::clamp_timeout(req.timeout_ms);
    let payload = format!(
        "{}{SEP}{}{SEP}{}",
        req.command,
        req.cwd.unwrap_or_default(),
        timeout_ms
    );

    // JNI 调用是同步阻塞的,放到阻塞线程池避免占用 async 运行时
    let raw = tokio::task::spawn_blocking(move || call_string_static(EXEC_CLASS, "exec", &payload))
        .await
        .map_err(|e| format!("执行任务调度失败: {e}"))??;

    parse_result(&raw, tier)
}

/// 解析 Kotlin 回传的 `exitCode␟stdout␟stderr`。
/// 三字段缺失时给出可诊断的错误(而非静默当空输出)。
fn parse_result(raw: &str, tier: ShellTier) -> Result<ExecResult, String> {
    let mut parts = raw.splitn(3, SEP);
    let code = parts
        .next()
        .ok_or_else(|| format!("执行器返回格式异常(缺 exitCode): {raw}"))?;
    // 有意丢弃 ParseIntError:唯一信息是「不是整数」,原始 code 已在消息中
    let exit_code: i32 = code
        .trim()
        .parse()
        .map_err(|_| format!("执行器返回的 exitCode 非法: {code}"))?;
    let stdout = parts.next().unwrap_or("");
    let stderr = parts.next().unwrap_or("");
    Ok(ExecResult {
        exit_code,
        stdout: truncate_output(stdout),
        stderr: truncate_output(stderr),
        tier,
        timed_out: false,
    })
}

/// 请求 Shizuku 权限(由前端按钮触发;首次会弹系统授权框)。
pub fn request_shizuku_permission() -> Result<(), String> {
    call_string_static(EXEC_CLASS, "requestShizukuPermission", "").map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_result() {
        let raw = format!("0{SEP}hello{SEP}");
        let r = parse_result(&raw, ShellTier::Shizuku).expect("应解析成功");
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.stdout, "hello");
        assert_eq!(r.stderr, "");
        assert_eq!(r.tier, ShellTier::Shizuku);
    }

    #[test]
    fn parses_result_with_stderr() {
        let raw = format!("1{SEP}out{SEP}err message");
        let r = parse_result(&raw, ShellTier::Root).expect("应解析成功");
        assert_eq!(r.exit_code, 1);
        assert_eq!(r.stdout, "out");
        assert_eq!(r.stderr, "err message");
    }

    #[test]
    fn rejects_malformed_exit_code() {
        let raw = format!("notanumber{SEP}x{SEP}y");
        let err = parse_result(&raw, ShellTier::Root).unwrap_err();
        assert!(err.contains("非法"), "{err}");
    }

    #[test]
    fn truncates_long_output() {
        let long = "x".repeat(super::super::MAX_OUTPUT_CHARS + 50);
        let raw = format!("0{SEP}{long}{SEP}");
        let r = parse_result(&raw, ShellTier::Sandbox).expect("应解析成功");
        assert!(r.stdout.contains("已截断"));
    }
}
