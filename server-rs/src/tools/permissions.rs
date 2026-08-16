// Agent 工具最终裁决：按风险等级与会话/角色授权决定是否允许执行。
use crate::models::types::ToolContext;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRisk {
    Safe,
    Sensitive,
    Dangerous,
}

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
        self.decide_with_policy(tool, ctx, true, false, false, false)
    }

    /// 最终权限裁决。优先级固定为:未知工具拒绝 > Custom 白名单 > 显式授权 >
    /// 安全工具 > bypass 非黑名单 > 等待授权。`tools=[]` 由调用方传入 custom_authorized=true。
    pub fn decide_with_policy(
        &self,
        tool: &str,
        ctx: &ToolContext,
        registered: bool,
        custom_authorized: bool,
        bypass_mode: bool,
        bypass_blacklisted: bool,
    ) -> PermissionDecision {
        let risk = self.risk_for(tool);
        if !registered {
            return PermissionDecision {
                allowed: false,
                risk,
                reason: "工具未注册,永久拒绝".into(),
            };
        }
        if custom_authorized {
            return PermissionDecision::allowed(risk, "Custom 步骤白名单永久授权".into());
        }
        let config = self.config.lock().unwrap_or_else(|e| e.into_inner());
        let explicitly_allowed = config
            .session_grants
            .get(&ctx.session_id)
            .is_some_and(|set| set.contains(tool))
            || config
                .role_grants
                .get(&ctx.character_id)
                .is_some_and(|set| set.contains(tool));
        drop(config);
        if explicitly_allowed {
            return PermissionDecision::allowed(risk, "已获得显式授权".into());
        }
        if risk == ToolRisk::Safe {
            return PermissionDecision::allowed(risk, "安全工具自动执行".into());
        }
        if bypass_mode && !bypass_blacklisted {
            return PermissionDecision::allowed(risk, "放行模式自动授权".into());
        }
        let reason = if bypass_mode && bypass_blacklisted {
            "工具位于放行模式黑名单,需要授权"
        } else {
            match risk {
                ToolRisk::Sensitive => "敏感工具等待用户授权",
                ToolRisk::Dangerous => "危险工具需要显式授权,等待用户决定",
                ToolRisk::Safe => unreachable!(),
            }
        };
        PermissionDecision {
            allowed: false,
            risk,
            reason: reason.into(),
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

    pub fn begin_wait(
        &self,
        run_id: &str,
        call_id: &str,
        session_id: &str,
        tool: &str,
    ) -> oneshot::Receiver<PendingAuthorizationDecision> {
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(
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
        let item = pending.remove(&key).unwrap();
        item.sender
            .send(decision)
            .map_err(|_| "待授权调用已断开".to_string())
    }

    pub fn cancel_wait(&self, run_id: &str, call_id: &str) {
        self.pending
            .lock().unwrap_or_else(|e| e.into_inner())
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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建权限配置目录失败: {e}"))?;
        }
        let raw =
            serde_json::to_string_pretty(config).map_err(|e| format!("序列化权限配置失败: {e}"))?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("tool_permissions.json");
        let temp_path = path.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temp_path, raw).map_err(|e| format!("写入权限配置临时文件失败: {e}"))?;
        if let Err(error) = replace_file(&temp_path, path) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(format!("原子替换权限配置失败: {error}"));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(temp_path: &std::path::Path, path: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(temp_path, path)
}

#[cfg(windows)]
fn replace_file(temp_path: &std::path::Path, path: &std::path::Path) -> std::io::Result<()> {
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

fn default_risk(tool: &str) -> ToolRisk {
    match tool {
        "calculator" | "memory_read" | "read" | "role" | "todo" | "censor_text" | "revise_passage" => {
            ToolRisk::Safe
        }
        "search" | "sleep" | "agentgo" => ToolRisk::Sensitive,
        "memory_write" | "update_variables" | "write" | "replace" | "create" | "agentend" => {
            ToolRisk::Dangerous
        }
        // 未知工具(包括用户插件)按危险处理,避免新增工具绕过裁决。
        _ => ToolRisk::Dangerous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::types::ToolContext;

    fn context() -> ToolContext {
        ToolContext {
            session_id: "s".into(),
            character_id: "r".into(),
        }
    }

    #[test]
    fn failed_authorize_does_not_change_memory() {
        let root =
            std::env::temp_dir().join(format!("kedai-permission-fail-{}", uuid::Uuid::new_v4()));
        std::fs::write(&root, "not a directory").unwrap();
        let manager = ToolPermissionManager::load(root.join("permissions.json"));
        assert!(manager.authorize("search", "session", "s").is_err());
        assert!(!manager.decide("search", &context()).allowed);
        let _ = std::fs::remove_file(root);
    }

    #[test]
    fn failed_revoke_keeps_existing_memory_grant() {
        let root =
            std::env::temp_dir().join(format!("kedai-permission-revoke-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("permissions.json");
        let manager = ToolPermissionManager::load(path.clone());
        manager.authorize("search", "session", "s").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&root).unwrap();
        std::fs::write(&root, "block directory").unwrap();
        assert!(manager.revoke("search", "session", "s").is_err());
        assert!(manager.decide("search", &context()).allowed);
        let _ = std::fs::remove_file(root);
    }
}
