//! 工具策略领域词汇（L1 契约层）。
//!
//! ## 为什么这些枚举在 L1 而不是 tools/（2026-09-14 下沉）
//!
//! 按三结合「代际判定程序」（见 `docs/契约-架构与数据.md` §2.2）：
//! - **P2 依赖方向**：`services/`（L2 编排层）要读授权档位与风险等级。若这些词汇留在
//!   `tools/`（名义 L3），就形成 `L2 → L3` 的越代依赖——编排层反向依赖青层能力。
//! - **P5 复用度**：授权档位被 `settings_service`（配置读写）、`task_engine::tool_policy`
//!   （任务工具策略）、`exec::audit`（审计落库）等多处复用，是**共享的领域词汇**，
//!   而非某一层私有的实现细节。
//!
//! 这与当年 `PromptFloor` 从 `services` 下沉到 `models/types.rs` 解决
//! `contracts/registry.rs` 反向依赖是**同一手法**：把被上层共享的词汇放到最底层，
//! 让依赖方向恢复单向。
//!
//! ## 边界：这里只放「词汇」，不放「判定逻辑」
//!
//! - 本文件放：枚举定义 + 其自带的纯函数（解析、标签、默认值）。
//! - **不放**：命令分级表与分级算法（`tools/command_risk.rs` 的 `classify_command`）、
//!   工具裁决矩阵（`tools/permissions.rs` 的 `decide_with_policy`）。那是**业务判定**，
//!   属 L2；它们随批次 3 迁往 `services/tool_runtime/`。
//!
//! 线格式不变：三个枚举的 `serde(rename_all = "snake_case")` 与磁盘/API 表示逐字保持，
//! 本次下沉**只改 `use` 路径，不改任何对外契约**。

use serde::{Deserialize, Serialize};

/// 工具风险等级（工具粒度的粗分级）。
///
/// 用途：`ToolPermissionManager` 的裁决兜底与确认卡展示。
/// 与 [`CommandRisk`] 的区别：本枚举是**工具**级别（如 `bash` 整体为 `Dangerous`），
/// `CommandRisk` 是**命令**级别（同一条 `bash` 里 `ls` 为 Safe、`rm` 为 Destructive），
/// 后者更细，用于执行类工具的逐条确认判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRisk {
    Safe,
    Sensitive,
    Dangerous,
}

/// 授权模式（三档；settings 以 snake_case 字符串落盘，旧配置缺省经迁移映射）。
///
/// - `Strict`：读/写/删文件都需授权；其他工具走原风险裁决。
/// - `Loose`：读/写文件放行，删文件需授权；其他工具走原风险裁决。
/// - `Bypass`：除「系统路径（C 盘）写/删」外一律放行。
///
/// 三档下「写/删系统路径」**始终**需授权，这是模式的硬底线（见
/// `tools/permissions.rs` 的 `file_rule`，该硬门优先于一切自动放行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationMode {
    Strict,
    Loose,
    Bypass,
}

impl Default for AuthorizationMode {
    /// 新装默认宽松（可用性优先：读写放行、仅删除需授权）。
    fn default() -> Self {
        AuthorizationMode::Loose
    }
}

impl AuthorizationMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthorizationMode::Strict => "strict",
            AuthorizationMode::Loose => "loose",
            AuthorizationMode::Bypass => "bypass",
        }
    }

    /// 解析模式字符串；非法值返回 None（调用方决定是 400 还是回退）。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "strict" => Some(AuthorizationMode::Strict),
            "loose" => Some(AuthorizationMode::Loose),
            "bypass" => Some(AuthorizationMode::Bypass),
            _ => None,
        }
    }
}

/// 命令风险级别（执行类工具的判定依据）。
///
/// 四级，危险度递增：`Safe < Sensitive < Destructive < Admin`。
/// - `Safe`：只读检视类，不修改任何状态
/// - `Sensitive`：工作区内写（建文件/复制/移动/就地编辑/重定向）
/// - `Destructive`：可能造成不可逆数据丢失（删除、格式化、覆写设备）
/// - `Admin`：提权或系统级控制（su/sudo/服务管理/网络配置/包管理/设备控制）
///
/// **定位**：本枚举只做**语义标注**，不构成安全边界。命令字符串可被混淆/拼接/编码绕过，
/// 静态匹配天然不完备——真正的边界是「三档授权矩阵 + 高风险逐条确认 + 审计留痕」，
/// 本分级只决定「要不要额外确认」。派生顺序由 `PartialOrd/Ord` 提供（取最高危）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CommandRisk {
    Safe,
    Sensitive,
    Destructive,
    Admin,
}

impl CommandRisk {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Sensitive => "sensitive",
            Self::Destructive => "destructive",
            Self::Admin => "admin",
        }
    }

    /// 是否必须逐条确认（不受授权模式影响）。
    /// 这是风险分级与权限裁决之间的**唯一接口契约**：危险级永远要人点头。
    pub fn requires_explicit_confirm(&self) -> bool {
        matches!(self, Self::Destructive | Self::Admin)
    }

    /// 面向用户的中文标签（确认卡与审计面板用）。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Safe => "只读",
            Self::Sensitive => "写入",
            Self::Destructive => "破坏性",
            Self::Admin => "提权/系统",
        }
    }
}

/// 工具来源：决定授权裁决时的**路径区域可信度**。
///
/// 内置工具（`Builtin`）受沙箱约束、参数可信；插件（`Plugin`）与 MCP（`Mcp`）工具的
/// 参数对引擎**不透明**（可能含系统路径），故授权裁决按外部来源处理
/// （见 `tools/permissions.rs` 的 `default_risk`：未登记的外部工具一律落 `Dangerous`）。
///
/// **为何在 L1**：这是被 `plugins/`（L3 加载器）与 `mcp/`（L3 客户端）共同标注的
/// **来源词汇**；若留在 `tools/`，两个 L3 模块都会构成 `L3→L2` 越代依赖。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOrigin {
    Builtin,
    Plugin,
    Mcp,
}

/// MCP 服务器配置（stdio 托管子进程）。
///
/// **为何在 L1**（2026-09-14 下沉）：`mcp/`（L3 客户端）需要读它来 spawn 子进程，
/// 若留在 `services/settings_service`（L2），`mcp/` 就构成 `L3→L2` 越代依赖。
/// 它是被 L2 设置层与 L3 客户端共享的配置词汇。
///
/// v1 仅在启动时装配（改设置后重启生效，无热重连）。serde 表示与线格式保持不变
/// （`settings.json` 的 `mcp_servers` 数组形状不变）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq)]
pub struct McpServerConfig {
    /// 服务器名（工具名前缀来源，装配时 sanitize 为 `[a-z0-9_]`）
    #[serde(default)]
    pub name: String,
    /// 可执行命令（如 npx / node / 某个 exe）
    #[serde(default)]
    pub command: String,
    /// 命令行参数
    #[serde(default)]
    pub args: Vec<String>,
    /// 该服务器是否启用（默认 true；false = 保留配置但启动时不装配）
    #[serde(default = "default_mcp_server_enabled")]
    pub enabled: bool,
}

/// 旧配置缺 `enabled` 字段时的默认值（保持「默认启用」的历史行为）。
fn default_mcp_server_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 线格式冻结：这三个枚举是设置落盘与 API 响应的对外契约，
    /// 下沉到 L1 时**不得**改变序列化表示（否则旧配置文件读不出来）。
    #[test]
    fn vocabulary_serde_representation_is_frozen() {
        assert_eq!(
            serde_json::to_string(&AuthorizationMode::Loose).unwrap(),
            "\"loose\""
        );
        assert_eq!(
            serde_json::to_string(&ToolRisk::Dangerous).unwrap(),
            "\"dangerous\""
        );
        assert_eq!(
            serde_json::to_string(&CommandRisk::Destructive).unwrap(),
            "\"destructive\""
        );
    }

    #[test]
    fn authorization_mode_parse_roundtrip() {
        for mode in [
            AuthorizationMode::Strict,
            AuthorizationMode::Loose,
            AuthorizationMode::Bypass,
        ] {
            assert_eq!(AuthorizationMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(AuthorizationMode::parse("bogus"), None);
    }

    /// 默认宽松是可用性决策，不是安全决策——本测试锁定该语义不被误改。
    #[test]
    fn default_mode_is_loose() {
        assert_eq!(AuthorizationMode::default(), AuthorizationMode::Loose);
    }

    /// 危险级必须逐条确认；这是「危险级永远要人点头」的最小断言。
    #[test]
    fn only_destructive_and_admin_require_explicit_confirm() {
        assert!(!CommandRisk::Safe.requires_explicit_confirm());
        assert!(!CommandRisk::Sensitive.requires_explicit_confirm());
        assert!(CommandRisk::Destructive.requires_explicit_confirm());
        assert!(CommandRisk::Admin.requires_explicit_confirm());
    }

    /// 派生序用于「多命令串联取最高危」，顺序不可调换。
    #[test]
    fn severity_order_is_ascending() {
        assert!(CommandRisk::Safe < CommandRisk::Sensitive);
        assert!(CommandRisk::Sensitive < CommandRisk::Destructive);
        assert!(CommandRisk::Destructive < CommandRisk::Admin);
    }
}
