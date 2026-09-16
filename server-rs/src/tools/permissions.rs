// Agent 工具最终裁决:按授权模式(严格/宽松/放行)、操作类型与会话/角色授权决定是否允许执行。
use crate::models::types::ToolContext;
use crate::tools::action_class::{PathZone, ToolAction, ToolOp};
use crate::tools::command_risk::CommandRisk;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::sync::oneshot;

/// 工具风险等级与授权模式**已下沉到 L1**（`crate::models::tool_policy`，2026-09-14），
/// 此处仅重导出以保持 `tools/` 内部既有 `use` 路径可用。
///
/// **下沉理由**：`services/`（L2）需要读授权档位与风险等级，若词汇留在 `tools/`（L3），
/// 就形成 L2→L3 的越代依赖。详见 `models/tool_policy.rs` 头部与
/// `docs/契约-架构与数据.md` §2.2。
///
/// **L2 代码请直接 `use crate::models::tool_policy::{...}`**，不要经本模块转发——
/// 转发仍会构成跨代依赖边（规则 J 会如实检出）。本重导出只服务 `tools/` 内部。
pub use crate::models::tool_policy::{AuthorizationMode, ToolRisk};

#[derive(Debug, Clone, Serialize)]
pub struct PermissionDecision {
    pub allowed: bool,
    pub risk: ToolRisk,
    pub reason: String,
}

impl PermissionDecision {
    /// 构造自动放行的决策(白名单工具用)
    pub fn allowed(risk: ToolRisk, reason: String) -> Self {
        Self {
            allowed: true,
            risk,
            reason,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PermissionConfig {
    #[serde(default)]
    classifications: BTreeMap<String, ToolRisk>,
    #[serde(default)]
    session_grants: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    role_grants: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingAuthorizationDecision {
    AllowOnce,
    AllowSession,
    AllowRole,
    Deny,
}

struct PendingAuthorization {
    session_id: String,
    tool: String,
    sender: oneshot::Sender<PendingAuthorizationDecision>,
}

pub struct ToolPermissionManager {
    path: Option<PathBuf>,
    config: Mutex<PermissionConfig>,
    pending: Mutex<HashMap<(String, String), PendingAuthorization>>,
}

impl ToolPermissionManager {
    pub fn in_memory() -> Self {
        Self {
            path: None,
            config: Mutex::new(PermissionConfig::default()),
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn load(path: PathBuf) -> Self {
        let config = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self {
            path: Some(path),
            config: Mutex::new(config),
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn risk_for(&self, tool: &str) -> ToolRisk {
        let config = self.config.lock().unwrap_or_else(|e| e.into_inner());
        config
            .classifications
            .get(tool)
            .copied()
            .unwrap_or_else(|| default_risk(tool))
    }

    pub fn decide(&self, tool: &str, ctx: &ToolContext) -> PermissionDecision {
        // 无操作分类信息时的保守回退:按宽松模式 + 非文件操作裁决
        let action = ToolAction {
            op: ToolOp::Other,
            zone: PathZone::Opaque,
            target: None,
            reason: String::new(),
            exec_risk: None,
        };
        self.decide_with_policy(
            tool,
            ctx,
            true,
            false,
            AuthorizationMode::Loose,
            &action,
            false,
        )
    }

    /// 最终权限裁决。优先级固定为:未注册拒绝 > 白名单授权 > 显式授权(会话/角色)
    /// > 始终需授权清单 > 三档文件规则 > 原风险裁决(安全工具 > 放行模式 > 等待授权)。
    ///
    /// `custom_authorized`:调用方传入的白名单命中(聊天 custom 步骤 / 任务策略闸门)。
    /// `mode`:三档授权模式;`action` 为本次调用的操作/区域分类;`always_required` 表示
    /// 该工具位于「始终需授权」清单(settings.bypass_blacklist,重定义后对三档都生效)。
    #[allow(clippy::too_many_arguments)] // 裁决输入维度天然多(工具/上下文/三档/清单),拆分反而降低可读性
    pub fn decide_with_policy(
        &self,
        tool: &str,
        ctx: &ToolContext,
        registered: bool,
        custom_authorized: bool,
        mode: AuthorizationMode,
        action: &ToolAction,
        always_required: bool,
    ) -> PermissionDecision {
        let risk = self.risk_for(tool);
        // ★★ 命令执行高危硬门(必须在任何自动放行之前判定)★★
        // 为什么优先级最高:bash 是**单一工具名**,工具级授权(白名单/会话授权/角色授权)
        // 无法区分「同一工具这次跑的是 ls 还是 rm -rf」。若让工具级授权先放行,用户一旦
        // 授权过 bash,高危命令就会静默执行(2026-09-13 冒烟实测复现过该缺口)。
        // 故 Destructive/Admin 命令一律返回未授权,强制走引擎的授权等待:
        //   - 聊天模式:弹确认卡(展示命令原文 + 风险级),逐条由用户决定;
        //   - 任务模式:no_ui_authorization=true → 直接拒绝(无 UI 通道,不能假装问过);
        //   - 「放行」模式与「始终需授权」清单在此都不适用(硬门在它们之前)。
        if action.is_exec_op() {
            let cmd_risk = action.exec_risk.unwrap_or(CommandRisk::Admin);
            if cmd_risk.requires_explicit_confirm() {
                return PermissionDecision {
                    allowed: false,
                    risk,
                    reason: format!(
                        "高危命令({})需逐条确认,不受任何授权豁免;任务模式不支持高危命令",
                        cmd_risk.label()
                    ),
                };
            }
        }
        if !registered {
            return PermissionDecision {
                allowed: false,
                risk,
                reason: "工具未注册,永久拒绝".into(),
            };
        }
        if custom_authorized {
            return PermissionDecision::allowed(risk, "白名单授权".into());
        }
        let config = self.config.lock().unwrap_or_else(|e| e.into_inner());
        // 匿名会话(character_id 为空)不查角色授权:否则多个无角色会话会共享同一份
        // role_grants[""] 而互相串权。
        let explicitly_allowed = config
            .session_grants
            .get(&ctx.session_id)
            .is_some_and(|set| set.contains(tool))
            || (!ctx.character_id.trim().is_empty()
                && config
                    .role_grants
                    .get(&ctx.character_id)
                    .is_some_and(|set| set.contains(tool)));
        drop(config);
        if explicitly_allowed {
            return PermissionDecision::allowed(risk, "已获得显式授权".into());
        }
        // 「始终需授权」清单:对三档模式一律拦截(用户在设置中显式钉死的高危工具)
        if always_required {
            return PermissionDecision {
                allowed: false,
                risk,
                reason: "该工具位于「始终需授权」清单,需手动授权".into(),
            };
        }
        // 非高危命令执行(只读/写入类):按三档常规处理
        // (高危已在函数开头硬门拦截,能到这里说明是 safe/sensitive 命令)
        if action.is_exec_op() {
            let cmd_risk = action.exec_risk.unwrap_or(CommandRisk::Admin);
            if mode == AuthorizationMode::Bypass {
                return PermissionDecision::allowed(
                    risk,
                    format!("放行模式:{}命令已放行", cmd_risk.label()),
                );
            }
            return PermissionDecision {
                allowed: false,
                risk,
                reason: format!("{}命令需授权", cmd_risk.label()),
            };
        }
        // 三档文件规则(仅对文件操作或外部工具的系统路径调用生效)
        if let Some(decision) = self.file_rule(mode, action, risk) {
            return decision;
        }
        // 非文件操作:沿用原风险裁决(安全工具自动执行;放行模式放行其余)
        if risk == ToolRisk::Safe {
            return PermissionDecision::allowed(risk, "安全工具自动执行".into());
        }
        if mode == AuthorizationMode::Bypass {
            return PermissionDecision::allowed(risk, "放行模式自动授权".into());
        }
        let reason = match risk {
            ToolRisk::Sensitive => "敏感工具等待用户授权",
            ToolRisk::Dangerous => "危险工具需要显式授权,等待用户决定",
            ToolRisk::Safe => unreachable!(),
        };
        PermissionDecision {
            allowed: false,
            risk,
            reason: reason.into(),
        }
    }

    /// 三档文件规则:返回 Some(决策) 表示该调用由文件规则裁定;None 表示非文件操作,
    /// 交回原风险裁决。规则见 AuthorizationMode 文档注释与 docs。
    fn file_rule(
        &self,
        mode: AuthorizationMode,
        action: &ToolAction,
        risk: ToolRisk,
    ) -> Option<PermissionDecision> {
        let op = action.op;
        // 命令执行不适用文件矩阵:已由 decide_with_policy 的 Exec 分支处理。
        // 此处显式返回 None 而非落入下方 match,是为了让「Exec 不走文件规则」这一
        // 契约在类型层面可见(而非依赖 zone 恰好是 Opaque 的偶然性)。
        if action.is_exec_op() {
            return None;
        }
        if !action.is_file_op() {
            // 外部工具(插件/MCP)参数中出现系统路径,但无法判定读写:按最严处理,
            // 三档一律要求授权(宁可多拦,不可静默写删系统文件)。
            if action.zone == PathZone::SystemPath {
                return Some(self.needs_auth(mode, action, risk));
            }
            return None;
        }
        match action.zone {
            PathZone::SystemPath => match op {
                // 读系统路径:严格模式需授权;宽松/放行放行
                ToolOp::ReadFile => {
                    if mode == AuthorizationMode::Strict {
                        Some(self.needs_auth(mode, action, risk))
                    } else {
                        Some(PermissionDecision::allowed(
                            risk,
                            "读取系统路径已放行".into(),
                        ))
                    }
                }
                // 写/删系统路径:三档硬底线,一律需授权
                _ => Some(self.needs_auth(mode, action, risk)),
            },
            // 内置工具受角色文件区沙箱约束,恒为 Sandbox;Opaque 也按沙箱口径处理
            PathZone::Sandbox | PathZone::Opaque => match op {
                ToolOp::ReadFile | ToolOp::WriteFile => match mode {
                    AuthorizationMode::Strict => Some(self.needs_auth(mode, action, risk)),
                    AuthorizationMode::Loose | AuthorizationMode::Bypass => Some(
                        PermissionDecision::allowed(risk, "该操作在当前授权模式下已放行".into()),
                    ),
                },
                ToolOp::DeleteFile => match mode {
                    AuthorizationMode::Strict | AuthorizationMode::Loose => {
                        Some(self.needs_auth(mode, action, risk))
                    }
                    AuthorizationMode::Bypass => Some(PermissionDecision::allowed(
                        risk,
                        "放行模式已放行删除".into(),
                    )),
                },
                ToolOp::Other => None,
                // Exec 已在上方提前返回;此 arm 仅为 match 穷尽性
                ToolOp::Exec => None,
            },
        }
    }

    /// 需要授权:理由优先用分类器给出的具体操作描述(如「写入角色文件 笔记/a.md」),
    /// 否则回退风险级别的通用文案。
    fn needs_auth(
        &self,
        mode: AuthorizationMode,
        action: &ToolAction,
        risk: ToolRisk,
    ) -> PermissionDecision {
        let reason = if !action.reason.trim().is_empty() {
            format!("{}({}模式需授权)", action.reason, mode_label(mode))
        } else {
            format!("{}模式需授权", mode_label(mode))
        };
        PermissionDecision {
            allowed: false,
            risk,
            reason,
        }
    }

    pub fn authorize(&self, tool: &str, scope: &str, scope_id: &str) -> Result<(), String> {
        if tool.trim().is_empty() || scope_id.trim().is_empty() {
            return Err("tool 与 scope_id 不能为空".into());
        }
        let mut current = self.config.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = current.clone();
        let grants = match scope {
            "session" => &mut next.session_grants,
            "role" => &mut next.role_grants,
            _ => return Err("scope 仅支持 session 或 role".into()),
        };
        grants
            .entry(scope_id.into())
            .or_default()
            .insert(tool.into());
        self.save(&next)?;
        *current = next;
        Ok(())
    }

    pub fn revoke(&self, tool: &str, scope: &str, scope_id: &str) -> Result<(), String> {
        let mut current = self.config.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = current.clone();
        let grants = match scope {
            "session" => &mut next.session_grants,
            "role" => &mut next.role_grants,
            _ => return Err("scope 仅支持 session 或 role".into()),
        };
        if let Some(set) = grants.get_mut(scope_id) {
            set.remove(tool);
            if set.is_empty() {
                grants.remove(scope_id);
            }
        }
        self.save(&next)?;
        *current = next;
        Ok(())
    }

    /// 整段撤销某个授权作用域的全部授权(会话删除/角色清理时调用)。
    /// 幂等:作用域不存在时直接成功(无需清理)。
    pub fn revoke_scope(&self, scope: &str, scope_id: &str) -> Result<(), String> {
        if scope_id.trim().is_empty() {
            return Ok(()); // 空作用域 id 本就查不到授权,视为无操作
        }
        let mut current = self.config.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = current.clone();
        let grants = match scope {
            "session" => &mut next.session_grants,
            "role" => &mut next.role_grants,
            _ => return Err("scope 仅支持 session 或 role".into()),
        };
        if grants.remove(scope_id).is_none() {
            return Ok(()); // 幂等:无该作用域授权
        }
        self.save(&next)?;
        *current = next;
        Ok(())
    }

    /// 清理指向已不存在会话的孤儿会话授权(启动装配时调用,返回清理条数)。
    /// 只清理会话授权;角色授权随角色生命周期,不在启动时判定。
    pub fn prune_orphan_session_grants(
        &self,
        existing_session_ids: &std::collections::HashSet<String>,
    ) -> Result<usize, String> {
        let mut current = self.config.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = current.clone();
        let before = next.session_grants.len();
        next.session_grants
            .retain(|sid, _| existing_session_ids.contains(sid));
        let removed = before - next.session_grants.len();
        if removed == 0 {
            return Ok(0);
        }
        self.save(&next)?;
        *current = next;
        Ok(removed)
    }

    pub fn begin_wait(
        &self,
        run_id: &str,
        call_id: &str,
        session_id: &str,
        tool: &str,
    ) -> oneshot::Receiver<PendingAuthorizationDecision> {
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                (run_id.into(), call_id.into()),
                PendingAuthorization {
                    session_id: session_id.into(),
                    tool: tool.into(),
                    sender,
                },
            );
        receiver
    }

    pub fn resolve_wait(
        &self,
        run_id: &str,
        call_id: &str,
        session_id: &str,
        tool: &str,
        decision: PendingAuthorizationDecision,
    ) -> Result<(), String> {
        let key = (run_id.to_string(), call_id.to_string());
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let Some(item) = pending.get(&key) else {
            return Err("待授权调用不存在或已结束".into());
        };
        if item.session_id != session_id || item.tool != tool {
            return Err("待授权调用与会话或工具不匹配".into());
        }
        // 上方 get 已确认键存在且持锁期间无并发移除,remove 必然为 Some
        let item = pending.remove(&key).expect("键已确认存在,移除必然成功");
        // 有意丢弃 SendError:接收端已 drop 即原因本身(用户离开/会话结束),
        // 文案已等价表达,且此处决策值无需回传给调用方
        item.sender
            .send(decision)
            .map_err(|_| "待授权调用已断开".to_string())
    }

    pub fn cancel_wait(&self, run_id: &str, call_id: &str) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(run_id.to_string(), call_id.to_string()));
    }

    pub fn grants_for(&self, session_id: &str, role_id: &str) -> serde_json::Value {
        let config = self.config.lock().unwrap_or_else(|e| e.into_inner());
        serde_json::json!({
            "session": config.session_grants.get(session_id).cloned().unwrap_or_default(),
            "role": config.role_grants.get(role_id).cloned().unwrap_or_default(),
        })
    }

    fn save(&self, config: &PermissionConfig) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let raw =
            serde_json::to_string_pretty(config).map_err(|e| format!("序列化权限配置失败: {e}"))?;
        // 统一原子写(utils::fs_atomic):写临时文件 + 原子替换,崩溃不留半截配置
        crate::utils::fs_atomic::write_atomic(path, raw.as_bytes())
            .map_err(|e| format!("保存权限配置失败: {e}"))
    }
}

fn mode_label(mode: AuthorizationMode) -> &'static str {
    match mode {
        AuthorizationMode::Strict => "严格",
        AuthorizationMode::Loose => "宽松",
        AuthorizationMode::Bypass => "放行",
    }
}

fn default_risk(tool: &str) -> ToolRisk {
    match tool {
        "calculator" | "memory_read" | "read" | "role" | "todo" | "censor_text"
        | "revise_passage" => ToolRisk::Safe,
        // agentend 是子智能体编排收尾(结束任务),不改数据,归敏感级;
        // 若归危险级,任务模式默认策略(拒绝危险工具)会打断子智能体流程。
        "search" | "sleep" | "agentgo" | "agentend" => ToolRisk::Sensitive,
        "memory_write" | "update_variables" | "write" | "replace" | "create" => ToolRisk::Dangerous,
        // 命令执行恒危险级:与文件写工具同级,但额外走「命令级风险强制确认」
        // (tools/command_risk.rs)。显式登记而非依赖下面的通配兜底,便于后续审查。
        "bash" => ToolRisk::Dangerous,
        // 未知工具(包括用户插件)按危险处理,避免新增工具绕过裁决。
        _ => ToolRisk::Dangerous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::types::ToolContext;
    use crate::utils::test_support::TempDataDir;

    fn context() -> ToolContext {
        ToolContext {
            session_id: "s".into(),
            character_id: "r".into(),
            agent_depth: 0,
        }
    }

    #[test]
    fn failed_authorize_does_not_change_memory() {
        let root = TempDataDir::new("permission-fail");
        // 父路径故意做成一个「文件」而非目录:permissions.json 无法在其下创建,
        // 借此模拟落盘失败。放在 root 内一层是为了让 TempDataDir 仍能整体清理
        //(直接把 root 本身变成文件会让守卫的 remove_dir_all 静默失败、留下残留)。
        let blocked = root.join("blocked");
        std::fs::write(&blocked, "not a directory").unwrap();
        let manager = ToolPermissionManager::load(blocked.join("permissions.json"));
        assert!(manager.authorize("search", "session", "s").is_err());
        assert!(!manager.decide("search", &context()).allowed);
    }

    #[test]
    fn failed_revoke_keeps_existing_memory_grant() {
        let root = TempDataDir::new("permission-revoke");
        let dir = root.join("cfg");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("permissions.json");
        let manager = ToolPermissionManager::load(path.clone());
        manager.authorize("search", "session", "s").unwrap();
        std::fs::remove_file(&path).unwrap();
        // 把承载目录换成文件(同上一用例的机制),使 revoke 的落盘必然失败
        std::fs::remove_dir(&dir).unwrap();
        std::fs::write(&dir, "block directory").unwrap();
        assert!(manager.revoke("search", "session", "s").is_err());
        assert!(manager.decide("search", &context()).allowed);
    }

    // ==================== 三档授权模式矩阵 ====================

    fn file_action(op: ToolOp, zone: PathZone, target: &str) -> ToolAction {
        ToolAction {
            op,
            zone,
            target: Some(target.into()),
            reason: format!("测试 {target}"),
            exec_risk: None,
        }
    }

    fn other_action() -> ToolAction {
        ToolAction {
            op: ToolOp::Other,
            zone: PathZone::Opaque,
            target: None,
            reason: String::new(),
            exec_risk: None,
        }
    }

    fn decide_with(
        manager: &ToolPermissionManager,
        tool: &str,
        mode: AuthorizationMode,
        action: &ToolAction,
    ) -> PermissionDecision {
        manager.decide_with_policy(tool, &context(), true, false, mode, action, false)
    }

    /// 严格模式:读/写/删文件一律需授权
    #[test]
    fn strict_mode_requires_auth_for_all_file_ops() {
        let m = ToolPermissionManager::in_memory();
        for (op, tool) in [
            (ToolOp::ReadFile, "read"),
            (ToolOp::WriteFile, "write"),
            (ToolOp::DeleteFile, "replace"),
        ] {
            let a = file_action(op, PathZone::Sandbox, "a.md");
            let d = decide_with(&m, tool, AuthorizationMode::Strict, &a);
            assert!(!d.allowed, "严格模式下 {tool} 应需授权");
            assert!(d.reason.contains("严格"), "理由应含模式名: {}", d.reason);
        }
    }

    /// 宽松模式:读/写放行,删需授权
    #[test]
    fn loose_mode_allows_read_write_but_not_delete() {
        let m = ToolPermissionManager::in_memory();
        let r = decide_with(
            &m,
            "read",
            AuthorizationMode::Loose,
            &file_action(ToolOp::ReadFile, PathZone::Sandbox, "a.md"),
        );
        assert!(r.allowed);
        let w = decide_with(
            &m,
            "write",
            AuthorizationMode::Loose,
            &file_action(ToolOp::WriteFile, PathZone::Sandbox, "a.md"),
        );
        assert!(w.allowed);
        let d = decide_with(
            &m,
            "replace",
            AuthorizationMode::Loose,
            &file_action(ToolOp::DeleteFile, PathZone::Sandbox, "a.md"),
        );
        assert!(!d.allowed, "宽松模式下删除仍需授权");
    }

    /// 放行模式:沙箱内读写删全放行
    #[test]
    fn bypass_mode_allows_all_sandbox_file_ops() {
        let m = ToolPermissionManager::in_memory();
        for op in [ToolOp::ReadFile, ToolOp::WriteFile, ToolOp::DeleteFile] {
            let d = decide_with(
                &m,
                "replace",
                AuthorizationMode::Bypass,
                &file_action(op, PathZone::Sandbox, "a.md"),
            );
            assert!(d.allowed, "放行模式下沙箱 {op:?} 应放行");
        }
    }

    /// 系统路径(C 盘)写/删:三档模式一律需授权(硬底线)
    #[test]
    fn system_path_write_delete_requires_auth_in_all_modes() {
        let m = ToolPermissionManager::in_memory();
        for mode in [
            AuthorizationMode::Strict,
            AuthorizationMode::Loose,
            AuthorizationMode::Bypass,
        ] {
            let a = file_action(ToolOp::WriteFile, PathZone::SystemPath, "C:\\Windows\\x");
            let d = decide_with(&m, "my_plugin", mode, &a);
            assert!(!d.allowed, "{mode:?} 下写系统路径应需授权");
            let a = file_action(ToolOp::DeleteFile, PathZone::SystemPath, "C:\\Windows\\x");
            let d = decide_with(&m, "my_plugin", mode, &a);
            assert!(!d.allowed, "{mode:?} 下删系统路径应需授权");
        }
    }

    /// 外部工具参数含系统路径但操作未知(Other):三档一律需授权
    #[test]
    fn opaque_external_with_system_path_requires_auth_in_all_modes() {
        let m = ToolPermissionManager::in_memory();
        let a = ToolAction {
            op: ToolOp::Other,
            zone: PathZone::SystemPath,
            target: Some("C:\\x".into()),
            reason: "外部工具参数含系统路径 C:\\x".into(),
            exec_risk: None,
        };
        for mode in [
            AuthorizationMode::Strict,
            AuthorizationMode::Loose,
            AuthorizationMode::Bypass,
        ] {
            let d = decide_with(&m, "my_plugin", mode, &a);
            assert!(!d.allowed, "{mode:?} 下外部工具系统路径调用应需授权");
        }
    }

    /// 非文件操作(写对话气泡、查世界书)不受文件规则影响,走原风险裁决
    #[test]
    fn non_file_ops_follow_risk_policy() {
        let m = ToolPermissionManager::in_memory();
        // write target=bubble → Other + Safe 风险(read 是 Safe;write 是 Dangerous)
        let safe = decide_with(&m, "read", AuthorizationMode::Strict, &other_action());
        assert!(safe.allowed, "安全工具在严格模式下仍自动执行");
        // Dangerous 工具在严格/宽松下等待授权,放行模式下放行
        let strict = decide_with(
            &m,
            "memory_write",
            AuthorizationMode::Strict,
            &other_action(),
        );
        assert!(!strict.allowed);
        let bypass = decide_with(
            &m,
            "memory_write",
            AuthorizationMode::Bypass,
            &other_action(),
        );
        assert!(bypass.allowed);
    }

    /// 「始终需授权」清单对三档都生效,即使显式授权之前
    #[test]
    fn always_required_overrides_mode() {
        let m = ToolPermissionManager::in_memory();
        let a = file_action(ToolOp::WriteFile, PathZone::Sandbox, "a.md");
        for mode in [
            AuthorizationMode::Strict,
            AuthorizationMode::Loose,
            AuthorizationMode::Bypass,
        ] {
            let d = m.decide_with_policy("write", &context(), true, false, mode, &a, true);
            assert!(!d.allowed, "{mode:?} 下始终需授权清单应拦截");
            assert!(d.reason.contains("始终需授权"));
        }
    }

    /// 显式授权优先于文件规则:严格模式下已授权 read 可直接读文件
    #[test]
    fn explicit_grant_beats_strict_file_rule() {
        let m = ToolPermissionManager::in_memory();
        m.authorize("read", "session", "s").unwrap();
        let a = file_action(ToolOp::ReadFile, PathZone::Sandbox, "a.md");
        let d = decide_with(&m, "read", AuthorizationMode::Strict, &a);
        assert!(d.allowed);
        assert!(d.reason.contains("显式授权"));
    }

    /// 匿名会话(character_id 为空)不命中角色授权,防止串权。
    /// 场景:历史配置或手改的 tool_permissions.json 里存在 role_grants[""]。
    #[test]
    fn anonymous_session_does_not_match_role_grants() {
        let root = TempDataDir::new("permission-anon");
        let path = root.join("permissions.json");
        std::fs::write(
            &path,
            r#"{"classifications":{},"session_grants":{},"role_grants":{"":["memory_write"]}}"#,
        )
        .unwrap();
        let m = ToolPermissionManager::load(path);
        let anon = ToolContext {
            session_id: "s1".into(),
            character_id: String::new(),
            agent_depth: 0,
        };
        let d = m.decide("memory_write", &anon);
        assert!(!d.allowed, "空角色不应命中 role_grants[\"\"]");
    }

    /// authorize 拒绝空 scope_id(旧实现会写入 role_grants[""],造成跨匿名会话串权)
    #[test]
    fn authorize_rejects_empty_scope_id() {
        let m = ToolPermissionManager::in_memory();
        assert!(m.authorize("read", "role", "").is_err());
        assert!(m.authorize("read", "session", "  ").is_err());
    }

    /// revoke_scope 幂等且清空整段
    #[test]
    fn revoke_scope_is_idempotent() {
        let m = ToolPermissionManager::in_memory();
        m.authorize("search", "session", "s").unwrap();
        m.authorize("read", "session", "s").unwrap();
        m.revoke_scope("session", "s").unwrap();
        let d = m.decide("search", &context());
        assert!(!d.allowed);
        // 再次调用仍成功(幂等)
        m.revoke_scope("session", "s").unwrap();
    }

    /// 孤儿会话授权清理
    #[test]
    fn prune_orphan_session_grants_removes_stale_entries() {
        let m = ToolPermissionManager::in_memory();
        m.authorize("search", "session", "alive").unwrap();
        m.authorize("search", "session", "dead").unwrap();
        let mut existing = std::collections::HashSet::new();
        existing.insert("alive".to_string());
        assert_eq!(m.prune_orphan_session_grants(&existing).unwrap(), 1);
        let alive = ToolContext {
            session_id: "alive".into(),
            character_id: "r".into(),
            agent_depth: 0,
        };
        assert!(m.decide("search", &alive).allowed);
        let dead = ToolContext {
            session_id: "dead".into(),
            character_id: "r".into(),
            agent_depth: 0,
        };
        assert!(!m.decide("search", &dead).allowed);
    }

    // ==================== 命令执行(Exec)授权矩阵 ====================

    /// 构造一个命令执行类 ToolAction(风险级由 classify 内部判定)。
    fn exec_action(cmd: &str) -> ToolAction {
        crate::tools::action_class::classify(
            "bash",
            &format!(r#"{{"command":"{cmd}"}}"#),
            crate::tools::action_class::ToolOrigin::Builtin,
        )
    }

    /// **核心安全契约**:破坏性/提权命令在**任何**授权模式下都不被放行
    /// (含 Bypass)——否则放行模式等于「静默执行任意命令」。
    #[test]
    fn destructive_and_admin_commands_never_auto_allowed() {
        let m = ToolPermissionManager::in_memory();
        for cmd in [
            "rm -rf /tmp/x",
            "sudo reboot",
            "dd if=/dev/zero of=/dev/sda",
            "pm list packages",
        ] {
            let a = exec_action(cmd);
            assert!(a.is_exec_op(), "{cmd} 应归类为 Exec");
            for mode in [
                AuthorizationMode::Strict,
                AuthorizationMode::Loose,
                AuthorizationMode::Bypass,
            ] {
                let d = decide_with(&m, "bash", mode, &a);
                assert!(
                    !d.allowed,
                    "{cmd} 在 {mode:?} 下不应自动放行(高危命令硬门);理由:{}",
                    d.reason
                );
                assert!(
                    d.reason.contains("逐条确认"),
                    "理由应说明需确认:{}",
                    d.reason
                );
            }
        }
    }

    /// 只读命令:严格/宽松下需授权,放行模式下放行(非高危,不制造确认疲劳)。
    #[test]
    fn safe_command_follows_three_mode_matrix() {
        let m = ToolPermissionManager::in_memory();
        let a = exec_action("ls -la");
        assert!(!decide_with(&m, "bash", AuthorizationMode::Strict, &a).allowed);
        assert!(!decide_with(&m, "bash", AuthorizationMode::Loose, &a).allowed);
        assert!(decide_with(&m, "bash", AuthorizationMode::Bypass, &a).allowed);
    }

    /// 写入类命令(非高危):放行模式放行,严格模式需授权。
    #[test]
    fn sensitive_command_not_treated_as_destructive() {
        let m = ToolPermissionManager::in_memory();
        let a = exec_action("mkdir -p build");
        assert!(!decide_with(&m, "bash", AuthorizationMode::Strict, &a).allowed);
        assert!(decide_with(&m, "bash", AuthorizationMode::Bypass, &a).allowed);
    }

    /// bash 的风险级恒为 Dangerous(显式登记,不依赖通配兜底)。
    #[test]
    fn bash_registered_as_dangerous() {
        let m = ToolPermissionManager::in_memory();
        assert_eq!(m.risk_for("bash"), ToolRisk::Dangerous);
    }

    /// ★ 回归测试(2026-09-13 冒烟实测发现的缺口):
    /// **工具级显式授权不得豁免命令级高危硬门**。
    /// 背景:bash 是单一工具名,用户授权 bash 后,同一工具既能跑 ls 也能跑 rm -rf;
    /// 修复前 explicitly_allowed 在 exec 硬门之前判定,导致授权 bash 后 rm -rf 直接执行。
    #[test]
    fn explicit_tool_grant_does_not_bypass_destructive_command_gate() {
        let m = ToolPermissionManager::in_memory();
        // 授予会话级 bash 授权(模拟用户点过「本次授权」)
        m.authorize("bash", "session", "s").unwrap();
        let a = exec_action("rm -rf /important");
        for mode in [
            AuthorizationMode::Strict,
            AuthorizationMode::Loose,
            AuthorizationMode::Bypass,
        ] {
            let d = decide_with(&m, "bash", mode, &a);
            assert!(
                !d.allowed,
                "会话已授权 bash,但高危命令仍必须逐条确认(而非直接执行);模式 {mode:?}"
            );
        }
        // 提权命令同理
        let a2 = exec_action("sudo rm -rf /");
        assert!(!decide_with(&m, "bash", AuthorizationMode::Bypass, &a2).allowed);
    }

    /// ★ 任务模式白名单(custom_authorized=true,即 ToolGate 名单内工具)不得豁免
    /// 命令级高危硬门:无人值守任务没有 UI 确认通道,rm -rf/sudo 必须直接拒绝;
    /// 普通命令仍经白名单放行(证明硬化断言非恒真)。
    #[test]
    fn task_whitelist_does_not_bypass_destructive_command_gate() {
        let m = ToolPermissionManager::in_memory();
        let ctx = ToolContext {
            session_id: "task:s1".into(),
            character_id: String::new(),
            agent_depth: 0,
        };
        for cmd in ["rm -rf /important", "sudo reboot"] {
            let a = exec_action(cmd);
            let d = m.decide_with_policy(
                "bash",
                &ctx,
                true,
                true,
                AuthorizationMode::Loose,
                &a,
                false,
            );
            assert!(
                !d.allowed,
                "任务白名单不得放行高危命令 {cmd};理由:{}",
                d.reason
            );
        }
        let a = exec_action("ls -la");
        let d = m.decide_with_policy(
            "bash",
            &ctx,
            true,
            true,
            AuthorizationMode::Loose,
            &a,
            false,
        );
        assert!(d.allowed, "普通命令应经任务白名单放行:{}", d.reason);
    }

    /// 非高危命令仍受工具级授权放行(授权 bash 后 ls 不再重复问)。
    #[test]
    fn explicit_tool_grant_still_applies_to_safe_commands() {
        let m = ToolPermissionManager::in_memory();
        m.authorize("bash", "session", "s").unwrap();
        let a = exec_action("ls -la");
        assert!(
            decide_with(&m, "bash", AuthorizationMode::Strict, &a).allowed,
            "只读命令在已授权后应放行,避免每次都弹确认"
        );
    }

    /// 「始终需授权」清单对 bash 同样生效(用户可再加一道锁)。
    #[test]
    fn bash_respects_always_required_list() {
        let m = ToolPermissionManager::in_memory();
        let a = exec_action("ls");
        let d = m.decide_with_policy(
            "bash",
            &context(),
            true,
            false,
            AuthorizationMode::Bypass,
            &a,
            true,
        );
        assert!(!d.allowed, "在始终需授权清单中即使放行模式也不得放行");
    }
}
