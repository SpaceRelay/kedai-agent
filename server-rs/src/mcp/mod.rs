// MCP stdio 客户端管理(批次 6.2;L3 隔离;docs/契约-架构与数据.md §2.5)。
//
// 边界:v1 只做 stdio transport + tools(不做 SSE/HTTP,不做 resources/prompts);
// 仅在启动时装配(mcp_enabled=false 时完全跳过:零进程、零注册),运行期改设置
// 不回溯重连,重启后生效。
//
// 权限:MCP 工具名带 mcp_ 前缀,不命中任何内建分类——permissions.rs default_risk
// 的「未知工具按 Dangerous 处理」自动覆盖全部 MCP 工具,无需额外分类工作。
//
// ## 代际纪律(L3 青层·活;2026-09-14 依赖倒置)
//
// 本模块**只依赖 L1**:配置词汇(`models::tool_policy::McpServerConfig`)、
// 工具定义与注册窄接口(`models::types::{ToolDefinition, ToolExecutor, ToolRegistrar}`)。
// 它**不再** `use crate::services::` 或 `use crate::tools::`——L3 不得依赖 L2。
//
// 配置的获取方式随之改变:宿主(组合根)读设置后,把 `Vec<McpServerConfig>` **传参**进来,
// 而不是让本模块自己去读 `RuntimeSettings`（后者是 L2 类型）。注册工具则经
// `&dyn ToolRegistrar` 由宿主注入,而非直接持有 L2 的 `ToolRegistry`。
// 这与 `task_core::TaskBackend` 断开 `task_engine→task_service` 是同一手法。
pub mod client;
pub mod process;

use crate::models::tool_policy::McpServerConfig;
use crate::models::types::{ToolDefinition, ToolExecutor, ToolRegistrar};
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use client::McpClient;
use process::McpProcess;

/// MCP 工具执行超时(注册表单工具档):外部服务器冷调用/大数据可能慢于内建工具,
/// 放宽到 120s;客户端内部 tools/call 超时 110s 略小于本档,保证先拿到可读错误。
pub const MCP_TOOL_TIMEOUT: Duration = Duration::from_secs(120);

/// 单个已装配服务器:client 供执行器转发,process 句柄 Drop 即杀(防孤儿)。
struct ServerHandle {
    /// 句柄表持有一份 client Arc(执行器闭包另持 Arc 转发调用);
    /// shutdown 经此 Arc 主动 close——执行器持有的 Arc 会让 client 存活,
    /// 没有主动 close 的话,关停后的残留注册项调用会悬挂而非快速失败。
    client: Arc<McpClient>,
    /// None 仅出现于内存管测试(无真实进程)
    _process: Option<McpProcess>,
}

/// MCP 管理器:服务器句柄表。内部可变,start/shutdown 只持 std Mutex 同步取放,
/// 不跨 .await(锁内无异步调用)。
pub struct McpManager {
    servers: Mutex<HashMap<String, ServerHandle>>,
}

impl McpManager {
    /// 空管理器(mcp_enabled=false 或缺省路径:零进程、零注册)
    pub fn empty() -> Self {
        McpManager {
            servers: Mutex::new(HashMap::new()),
        }
    }

    /// 已装配服务器数(启动日志/测试断言用)
    pub fn server_count(&self) -> usize {
        self.servers.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// 启动装配:遍历给定的启用服务器,逐个 spawn → 握手 → tools/list → 注册工具。
    ///
    /// **入参而非自读设置**:`servers` 由宿主(组合根)从 `RuntimeSettings` 过滤 `enabled`
    /// 后传入;`registry` 是 L1 的窄接口 `&dyn ToolRegistrar`,由 L2 注册表实现。
    /// 这样本模块（L3）不依赖任何 L2 类型。
    ///
    /// 单台失败(spawn 失败/握手超时/协议错误)记 warn 并禁用该台,不影响其余服务器,
    /// 不 panic、不阻断整体启动(单台最坏耗时 = initialize 30s 超时,有界)。
    pub async fn start(&self, servers: Vec<McpServerConfig>, registry: &dyn ToolRegistrar) {
        if servers.is_empty() {
            return;
        }
        for cfg in servers {
            let tag = cfg.name.clone();
            match McpProcess::spawn(&cfg) {
                Ok((process, client)) => {
                    match self
                        .attach(cfg.name.clone(), client, Some(process), registry)
                        .await
                    {
                        Ok(n) => tracing::info!(server = tag, tools = n, "MCP 服务器已装配"),
                        Err(e) => tracing::warn!(
                            server = tag,
                            error = e,
                            "MCP 服务器装配失败,已禁用该服务器"
                        ),
                    }
                }
                Err(e) => {
                    tracing::warn!(server = tag, error = e, "MCP 服务器启动失败,已禁用该服务器")
                }
            }
        }
    }

    /// 装配单台:握手(initialize)→ tools/list → 逐工具注册。
    /// 返回注册工具数;任何一步失败即 kill 进程并 Err(调用方记 warn 禁用)。
    /// process 为 None 是内存管测试路径(无进程可杀)。
    /// registry 为 L1 窄接口,由宿主注入(见 [`crate::models::types::ToolRegistrar`])。
    async fn attach(
        &self,
        name: String,
        client: McpClient,
        process: Option<McpProcess>,
        registry: &dyn ToolRegistrar,
    ) -> Result<usize, String> {
        let client = Arc::new(client);
        let tools = async {
            client.initialize().await?;
            client.list_tools().await
        }
        .await;
        let tools = match tools {
            Ok(t) => t,
            Err(e) => {
                // 装配失败:显式 kill(Drop 的 kill_on_drop 是兜底,这里立即回收)
                if let Some(mut p) = process {
                    p.kill().await;
                }
                return Err(e);
            }
        };

        let sanitized = sanitize_server_name(&name);
        let mut registered = 0usize;
        for tool in &tools {
            let full_name = prefixed_tool_name(&sanitized, &tool.name);
            let definition = ToolDefinition {
                name: full_name.clone(),
                description: if tool.description.is_empty() {
                    format!("[MCP·{sanitized}] {}", tool.name)
                } else {
                    format!("[MCP·{sanitized}] {}", tool.description)
                },
                parameters: tool.input_schema.clone(),
            };
            let call_client = client.clone();
            let remote_name = tool.name.clone();
            let execute: ToolExecutor = Arc::new(
                move |args: Value, _ctx| -> BoxFuture<'static, Result<String, String>> {
                    let call_client = call_client.clone();
                    let remote_name = remote_name.clone();
                    Box::pin(async move { call_client.call_tool(&remote_name, args).await })
                },
            );
            // MCP 工具标记为外部来源:参数对引擎不透明,三档授权模式按系统路径扫描处理
            registry.register_external(
                definition,
                execute,
                Some(MCP_TOOL_TIMEOUT),
                crate::models::tool_policy::ToolOrigin::Mcp,
            );
            registered += 1;
        }

        // 同名(sanitize 后冲突)服务器后装者覆盖先装者;句柄表同键替换,
        // 被替换的句柄 Drop 即杀旧进程,不泄漏。
        self.servers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                sanitized,
                ServerHandle {
                    client,
                    _process: process,
                },
            );
        Ok(registered)
    }

    /// 关闭全部服务器:先主动 close 协议层(在飞/后续请求快速失败,不悬挂),
    /// 再清空句柄表——各 ServerHandle Drop 时 kill_on_drop 杀进程。
    /// 进程退出路径(run_server 优雅关闭后)调用;v1 无热重连,不提供单台关停 API。
    pub async fn shutdown(&self) {
        let handles: Vec<ServerHandle> = self
            .servers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, h)| h)
            .collect();
        for h in &handles {
            h.client.close();
        }
        drop(handles); // 显式落 Drop 语义(kill 进程、abort stderr 任务)
    }
}

/// 服务器名 sanitize 为 [a-z0-9_](小写;非法字符归并为单个 _):
/// 作为工具名前缀段,需满足 function calling 工具名约束且不与内建/插件冲突。
pub fn sanitize_server_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_underscore = false;
    for ch in name.trim().chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            last_underscore = false;
        } else if !last_underscore && !out.is_empty() {
            out.push('_');
            last_underscore = true;
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        "server".to_string() // 全非法字符的兜底名(如纯中文名)
    } else {
        out
    }
}

/// MCP 侧工具名净化:function calling 允许 [a-zA-Z0-9_-],其余字符替换为 _。
fn sanitize_tool_part(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "tool".to_string()
    } else {
        out
    }
}

/// 完整注册名:mcp_{server}_{tool},总长钳 64(OpenAI function 名上限)。
fn prefixed_tool_name(sanitized_server: &str, tool: &str) -> String {
    let mut name = format!("mcp_{}_{}", sanitized_server, sanitize_tool_part(tool));
    if name.len() > 64 {
        name.truncate(64);
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::tool_policy::ToolRisk;
    use crate::models::types::ToolContext;
    // 测试自建真实注册表(McpManager::start 的装配行为需断言真实注册表状态);
    // 生产代码不依赖 tools —— 见本文件头部「代际纪律」。
    use crate::services::settings_service::RuntimeSettings;
    use crate::tools::permissions::PermissionDecision;
    use crate::tools::registry::ToolRegistry;
    use serde_json::json;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::{duplex, AsyncWriteExt, BufReader, DuplexStream};

    #[test]
    fn sanitize_server_name_normalizes_to_prefix_charset() {
        assert_eq!(sanitize_server_name("File System"), "file_system");
        assert_eq!(sanitize_server_name("fs-2.0"), "fs_2_0");
        assert_eq!(sanitize_server_name("  EDGE--case  "), "edge_case");
        assert_eq!(sanitize_server_name("文件系统"), "server", "全非法字符兜底");
        assert_eq!(sanitize_server_name(""), "server");
    }

    #[test]
    fn prefixed_tool_name_has_mcp_prefix_and_length_cap() {
        assert_eq!(prefixed_tool_name("fs", "read_file"), "mcp_fs_read_file");
        let long = prefixed_tool_name("s", &"x".repeat(100));
        assert!(long.len() <= 64, "工具名应钳到 64 字符: {}", long.len());
        assert!(long.starts_with("mcp_s_"));
    }

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "s".into(),
            character_id: "c".into(),
            agent_depth: 0,
        }
    }

    fn allow() -> PermissionDecision {
        PermissionDecision {
            allowed: true,
            risk: ToolRisk::Dangerous,
            reason: "测试放行".into(),
        }
    }

    /// 内存管假服务器:initialize/tools/list/tools/call 固定响应(与 client.rs 测试同范式)
    fn mock_server(mut io: DuplexStream) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut reader = BufReader::new(&mut io);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let Ok(req) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                let Some(id) = req.get("id").cloned() else {
                    continue;
                };
                let result = match req["method"].as_str().unwrap_or("") {
                    "initialize" => {
                        json!({ "protocolVersion": "2024-11-05", "capabilities": {}, "serverInfo": { "name": "mock", "version": "1" } })
                    }
                    "tools/list" => json!({ "tools": [
                        { "name": "read_file", "description": "读文件", "inputSchema": { "type": "object" } },
                    ]}),
                    "tools/call" => {
                        json!({ "content": [{ "type": "text", "text": "文件内容-甲乙丙" }] })
                    }
                    _ => json!({}),
                };
                let resp = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
                if reader
                    .get_mut()
                    .write_all(format!("{resp}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
                let _ = reader.get_mut().flush().await;
            }
        })
    }

    fn client_pair() -> (McpClient, DuplexStream) {
        let (a, b) = duplex(64 * 1024);
        let (ar, aw) = tokio::io::split(a);
        (McpClient::new(Box::new(ar), Box::new(aw)), b)
    }

    /// 空配置/全禁用:start 零注册、零服务器(默认关路径的零副作用保证)
    ///
    /// 代际倒置后 `start` 收「已过滤的服务器列表」,故本测试分两段断言:
    /// ① 空列表(等价于 `mcp_enabled=false` 或设置里无服务器)→ 零副作用;
    /// ② **宿主过滤语义**:`enabled=false` 的条目必须被宿主滤掉,不得进入 `start`。
    ///    过滤动作在组合根(`api/app_state.rs`),此处以同一规则复现并锁定契约。
    #[tokio::test]
    async fn start_with_empty_or_disabled_config_is_noop() {
        let reg = ToolRegistry::new();
        let mgr = McpManager::empty();
        let mut settings = RuntimeSettings::from_config(&crate::config::AppConfig::from_env());
        settings.mcp_servers = vec![McpServerConfig {
            name: "off".into(),
            command: "cmd".into(),
            args: vec![],
            enabled: false, // 显式禁用:不应起进程
        }];
        // 宿主过滤规则(与 api/app_state.rs 的启动装配一致)
        let servers: Vec<McpServerConfig> = settings
            .mcp_servers
            .iter()
            .filter(|s| s.enabled)
            .cloned()
            .collect();
        assert!(servers.is_empty(), "enabled=false 的服务器应被宿主滤除");
        mgr.start(servers, &reg).await;
        assert_eq!(mgr.server_count(), 0);
        assert!(reg.list_definitions().is_empty(), "不应注册任何工具");
    }

    /// 装配链路:握手 + tools/list → 工具以 mcp_{server}_{tool} 注册,
    /// 执行器经注册表(120s 档)转发 tools/call 并拼接 text 结果。
    #[tokio::test]
    async fn attach_registers_prefixed_tools_that_forward_calls() {
        let reg = ToolRegistry::new();
        let mgr = McpManager::empty();
        let (client, server_io) = client_pair();
        let server_task = mock_server(server_io);

        let n = mgr
            .attach("File System".into(), client, None, &reg)
            .await
            .expect("装配应成功");
        assert_eq!(n, 1);
        assert_eq!(mgr.server_count(), 1);

        let defs = reg.list_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "mcp_file_system_read_file", "工具名应带前缀");
        assert!(defs[0].description.contains("[MCP·file_system]"));
        // MCP 档超时 120s(register_with_timeout 单工具档)
        let tool = reg.get("mcp_file_system_read_file").unwrap();
        assert_eq!(tool.timeout, Some(MCP_TOOL_TIMEOUT));

        let out = reg
            .execute_with_decision(
                "mcp_file_system_read_file",
                r#"{"path":"a.txt"}"#,
                ctx(),
                &allow(),
            )
            .await
            .expect("转发调用应成功");
        assert_eq!(out, "文件内容-甲乙丙");

        // 关闭后:句柄清空,残留注册项调用报「连接已关闭」而不是悬挂
        mgr.shutdown().await;
        assert_eq!(mgr.server_count(), 0);
        let err = reg
            .execute_with_decision(
                "mcp_file_system_read_file",
                r#"{"path":"a.txt"}"#,
                ctx(),
                &allow(),
            )
            .await
            .unwrap_err();
        assert!(err.contains("连接已关闭"), "关闭后调用应明确失败: {err}");

        // 注册表执行器仍持有 client Arc(写端未关),假服务器等不到 EOF——直接 abort
        server_task.abort();
    }

    /// 握手失败的台:attach 返回 Err 且不残留句柄/注册项(调用方据此禁用)
    #[tokio::test]
    async fn attach_failure_leaves_no_trace() {
        let reg = ToolRegistry::new();
        let mgr = McpManager::empty();
        let (client, server_io) = client_pair();
        drop(server_io); // 对端直接断开 → initialize 必失败
        let err = mgr
            .attach("broken".into(), client, None, &reg)
            .await
            .unwrap_err();
        assert!(!err.is_empty());
        assert_eq!(mgr.server_count(), 0);
        assert!(reg.list_definitions().is_empty());
    }
}
