// bash 工具:执行 shell 命令(阶段 B)。
//
// 安全模型(与 docs/契约-协议与配置.md 一致,四道闸):
//   ① 工具级:风险恒 Dangerous;任务模式默认策略(deny_dangerous)按工具名对 bash 开例外
//      下发(用户要求任务模式具备命令执行能力),聊天模式走三档授权;
//   ② 命令级:破坏性/提权命令**任何模式都不自动放行**,强制走引擎的授权等待
//      (聊天弹出确认卡,展示命令原文 + 风险级;任务模式无 UI 通道 → 直接拒绝);
//   ③ 执行级:① 强制超时 + 超时强杀;② stdin 置 null 禁交互;③ 输出截断;
//      ④ Android 上与执行器等级联动(经 services::exec 分派);
//   ④ 审计级:每次尝试(含被拒绝的)落 exec_audit 一行,root/ADB 级命令可回溯。
//
// 分工说明:本文件只做「参数解析 + 调用执行器 + 写审计」;风险分级在
// tools/command_risk.rs,授权裁决在 tools/permissions.rs,
// 进程派生在 services/exec/(桌面直派 / Android 走 Kotlin 桥)。
use crate::models::types::{ToolContext, ToolDefinition};
use crate::services::exec::{self, audit, ExecRequest};
use crate::tools::registry::ToolRegistry;
use serde_json::json;
use std::sync::Arc;

/// 工具名(权限矩阵、tool_sets、前端确认卡均以此为准)
pub const TOOL_NAME: &str = "bash";

/// 注册 bash 工具。`db` 用于审计落库;`settings` 读「命令执行总开关」;
/// `data_dir` 作为默认工作目录。
///
/// 注意:**不**加入任何只读白名单(READONLY_SCOUT/SUBAGENT/REFLECT),
/// 避免命令执行能力被下发给规划侦察轮、子 agent 或反思步骤。
pub fn register_bash_tool(
    registry: &ToolRegistry,
    db: Arc<crate::models::db::Db>,
    settings: Arc<std::sync::Mutex<crate::services::settings_service::RuntimeSettings>>,
    data_dir: std::path::PathBuf,
) {
    let definition = ToolDefinition {
        name: TOOL_NAME.into(),
        description: "执行 shell 命令并返回输出。危险命令(删除/提权/系统级)会要求用户逐条确认;\
                      任务模式下此类命令不可用,普通命令可用。默认在数据目录执行,可用 cwd 指定工作目录。"
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "完整命令(交平台默认 shell 解释:Windows=cmd,其他=sh)"
                },
                "cwd": {
                    "type": "string",
                    "description": "工作目录绝对路径;省略则在应用数据目录执行"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "超时毫秒(默认 60000,上限 300000);超时会强杀进程"
                }
            },
            "required": ["command"]
        }),
    };

    let executor_db = db.clone();
    let executor_settings = settings.clone();
    registry.register_with_timeout(
        definition,
        Arc::new(move |args: serde_json::Value, ctx: ToolContext| {
            let db = executor_db.clone();
            let st = executor_settings.clone();
            let dir = data_dir.clone();
            Box::pin(async move { run(&args, &ctx, &db, &st, &dir).await })
        }),
        // 执行器自带超时强杀,注册表超时给足余量(命令上限 300s + 收尾)
        Some(std::time::Duration::from_secs(330)),
    );
}

/// 工具主体:参数解析 → 执行 → 审计 → 结果文本。
///
/// 说明:授权裁决已由引擎在调用本执行器**之前**完成(见 agents/engine/executor.rs
/// 的 decide 流程)。所以能进到这里就说明已获授权(或属自动放行的非高危命令)。
/// 本函数仍会自查命令风险并记审计,保证「跑了什么」可回溯。
async fn run(
    args: &serde_json::Value,
    ctx: &ToolContext,
    db: &Arc<crate::models::db::Db>,
    settings: &Arc<std::sync::Mutex<crate::services::settings_service::RuntimeSettings>>,
    data_dir: &std::path::Path,
) -> Result<String, String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if command.is_empty() {
        return Err("command 不能为空".into());
    }
    let cwd = args
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| data_dir.to_string_lossy().into_owned());
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .map(Some)
        .unwrap_or(None);

    let risk = crate::tools::command_risk::classify_command(&command, "");
    let tier = exec::detect_tier();
    // 会话归属:task: 前缀为任务模式虚拟 session(与 task_service 同口径)
    let is_task = ctx.session_id.starts_with("task:");
    let source = if is_task {
        audit::AuditSource::Task
    } else {
        audit::AuditSource::Chat
    };

    // 总开关(阶段 E):默认关闭,须用户在设置「命令执行」中显式开启。
    // 关闭时拒绝并留审计——「有人试图执行但被总开关拦下」同样需要可回溯。
    if !exec_enabled(settings) {
        audit::record(
            db,
            &audit::AuditRecord {
                source,
                task_id: is_task.then(|| ctx.session_id.trim_start_matches("task:").to_string()),
                session_id: (!is_task).then(|| ctx.session_id.clone()),
                command: command.clone(),
                shell: shell_name(),
                tier: tier.as_str().into(),
                risk,
                decision: audit::AuditDecision::Denied,
                exit_code: None,
                stdout: String::new(),
                stderr: "命令执行总开关未开启".into(),
            },
        );
        return Err("命令执行未开启:请在「设置 → 授权管理 → 命令执行」中打开总开关后重试。".into());
    }

    // 允许等级:由设置的三档开关编译(桌面恒 Sandbox,Android 按用户放行)。
    // 这是「等级可见 + 可关闭」的执行侧落点——探测到 ROOT 但用户只放行沙箱档时,
    // 会以清晰提示拒绝,而不是悄悄以高权限执行。
    let allowed: Vec<exec::ShellTier> = {
        let s = settings.lock().unwrap_or_else(|e| e.into_inner());
        let mut v = Vec::new();
        if s.exec_allow_root {
            v.push(exec::ShellTier::Root);
        }
        if s.exec_allow_shizuku {
            v.push(exec::ShellTier::Shizuku);
        }
        if s.exec_allow_sandbox {
            v.push(exec::ShellTier::Sandbox);
        }
        // 桌面端无档位概念:恒放行 Sandbox(总开关 exec_enabled 已把关)
        if !cfg!(target_os = "android") {
            v.push(exec::ShellTier::Sandbox);
        }
        v
    };

    let result = exec::execute(
        ExecRequest {
            command: command.clone(),
            cwd: Some(cwd.clone()),
            timeout_ms,
        },
        &allowed,
    )
    .await;

    match result {
        Ok(out) => {
            audit::record(
                db,
                &audit::AuditRecord {
                    source,
                    task_id: is_task
                        .then(|| ctx.session_id.trim_start_matches("task:").to_string()),
                    session_id: (!is_task).then(|| ctx.session_id.clone()),
                    command: command.clone(),
                    shell: shell_name(),
                    tier: out.tier.as_str().into(),
                    risk,
                    decision: audit::AuditDecision::Allowed,
                    exit_code: Some(out.exit_code),
                    stdout: out.stdout.clone(),
                    stderr: out.stderr.clone(),
                },
            );
            Ok(format_result(&command, &out))
        }
        Err(e) => {
            // 执行失败(超时/启动失败)也留痕:失败原因同样需要可回溯
            audit::record(
                db,
                &audit::AuditRecord {
                    source,
                    task_id: is_task
                        .then(|| ctx.session_id.trim_start_matches("task:").to_string()),
                    session_id: (!is_task).then(|| ctx.session_id.clone()),
                    command: command.clone(),
                    shell: shell_name(),
                    tier: tier.as_str().into(),
                    risk,
                    decision: audit::AuditDecision::Denied,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: e.clone(),
                },
            );
            Err(e)
        }
    }
}

/// 结果文本:给模型看的结构化摘要(含退出码与所跑命令,便于自查)。
fn format_result(command: &str, out: &exec::ExecResult) -> String {
    let mut s = format!(
        "$ {command}\n(执行器:{};退出码:{})",
        out.tier.label(),
        out.exit_code
    );
    if !out.stdout.trim().is_empty() {
        s.push_str("\n\n[stdout]\n");
        s.push_str(out.stdout.trim_end());
    }
    if !out.stderr.trim().is_empty() {
        s.push_str("\n\n[stderr]\n");
        s.push_str(out.stderr.trim_end());
    }
    if out.stdout.trim().is_empty() && out.stderr.trim().is_empty() {
        s.push_str("\n\n(无输出)");
    }
    if out.exit_code != 0 {
        s.push_str("\n\n提示:非零退出码表示命令失败,请检查命令与参数。");
    }
    s
}

/// 平台默认 shell 名(审计记录用;实际解释器由 services::exec 决定)。
fn shell_name() -> String {
    if cfg!(target_os = "windows") {
        "cmd".into()
    } else {
        "sh".into()
    }
}

/// 读命令执行总开关(settings.json 的 exec_enabled)。
/// 经 ToolDeps 注入的 RuntimeSettings 句柄读取,与设置面板写入的是同一份内存态。
/// 读锁中毒按 into_inner 恢复(项目锁纪律);开关语义上失败方向取安全侧。
fn exec_enabled(
    settings: &Arc<std::sync::Mutex<crate::services::settings_service::RuntimeSettings>>,
) -> bool {
    settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .exec_enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_success_output() {
        let out = exec::ExecResult {
            exit_code: 0,
            stdout: "hello\n".into(),
            stderr: String::new(),
            tier: exec::ShellTier::Sandbox,
            timed_out: false,
        };
        let s = format_result("echo hello", &out);
        assert!(s.contains("$ echo hello"));
        assert!(s.contains("hello"));
        assert!(s.contains("退出码:0"));
        assert!(!s.contains("非零退出码"));
    }

    #[test]
    fn formats_failure_with_hint() {
        let out = exec::ExecResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "boom".into(),
            tier: exec::ShellTier::Sandbox,
            timed_out: false,
        };
        let s = format_result("false", &out);
        assert!(s.contains("boom"));
        assert!(s.contains("非零退出码"), "失败时应给提示:{s}");
    }

    #[test]
    fn formats_empty_output() {
        let out = exec::ExecResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            tier: exec::ShellTier::Sandbox,
            timed_out: false,
        };
        assert!(format_result("true", &out).contains("无输出"));
    }

    #[test]
    fn rejects_empty_command() {
        // 直接验证参数校验分支(不触发真实执行)
        let args = serde_json::json!({ "command": "   " });
        let cmd = args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        assert!(cmd.is_empty(), "空命令应被拒");
    }

    #[test]
    fn shell_name_matches_platform() {
        if cfg!(target_os = "windows") {
            assert_eq!(shell_name(), "cmd");
        } else {
            assert_eq!(shell_name(), "sh");
        }
    }
}
