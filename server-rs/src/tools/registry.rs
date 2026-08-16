// 工具注册表(与 Node 版 tools/registry.ts 对齐)
// 执行器为 async(sleep/agentgo/search 等工具需要异步能力),签名:
//   Fn(Value, ToolContext) -> BoxFuture<Result<String, String>>
use crate::models::types::{ToolContext, ToolDefinition};
use crate::tools::permissions::{PermissionDecision, ToolPermissionManager};
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 工具执行超时(防止 sleep/插件/网络工具无限阻塞;超时按失败回填模型)
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(30);
/// 工具结果最大字节数(超出截断,避免超长输出撑爆上下文与前端渲染)
const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

/// 工具执行器类型别名(register_multistep_tools 等外部构造闭包时需要显式标注)
pub type ToolExecutor =
    Arc<dyn Fn(Value, ToolContext) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

#[derive(Clone)]
pub struct RegisteredTool {
    pub definition: ToolDefinition,
    pub execute: ToolExecutor,
}

pub struct ToolRegistry {
    tools: Mutex<HashMap<String, RegisteredTool>>,
    permissions: ToolPermissionManager,
    /// 工具执行超时(测试可缩短,生产默认 30s)
    tool_timeout: Duration,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::with_permissions(ToolPermissionManager::in_memory())
    }

    pub fn with_permissions(permissions: ToolPermissionManager) -> Self {
        ToolRegistry {
            tools: Mutex::new(HashMap::new()),
            permissions,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
        }
    }

    /// 自定义工具执行超时(测试用;生产保持默认)
    #[cfg(test)]
    fn with_tool_timeout(mut self, timeout: Duration) -> Self {
        self.tool_timeout = timeout;
        self
    }

    pub fn permissions(&self) -> &ToolPermissionManager {
        &self.permissions
    }

    pub fn register(&self, definition: ToolDefinition, execute: ToolExecutor) {
        let mut g = self.tools.lock().unwrap_or_else(|e| e.into_inner());
        g.insert(
            definition.name.clone(),
            RegisteredTool {
                definition,
                execute,
            },
        );
    }

    pub fn unregister(&self, name: &str) {
        self.tools.lock().unwrap_or_else(|e| e.into_inner()).remove(name);
    }

    pub fn get(&self, name: &str) -> Option<RegisteredTool> {
        self.tools.lock().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    pub fn list_definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions: Vec<_> = self
            .tools
            .lock().unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|t| t.definition.clone())
            .collect();
        definitions.sort_by(|a, b| a.name.cmp(&b.name));
        definitions
    }

    /// 生成给模型看的工具使用指南(自然语言清单:名称 + 一句话功能 + 何时调用)。
    /// 与 list_definitions() 同源(始终与实际注册工具同步),供 agent 系统提示词动态注入;
    /// 调用时机对内置工具用静态映射,未知名(脚本插件工具)回退其 description。
    pub fn tool_guidance(&self) -> String {
        self.tool_guidance_for(&self.list_definitions())
    }

    /// 为本次请求实际下发的工具定义生成指南。Custom 步骤必须使用此接口,
    /// 避免提示词列出步骤不可见的注册工具。
    pub fn tool_guidance_for(&self, defs: &[ToolDefinition]) -> String {
        if defs.is_empty() {
            return String::new();
        }
        let mut defs = defs.to_vec();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        let mut out =
            String::from("你可以通过 function calling 调用以下工具,需要时再调用,不必每轮都调:");
        for d in &defs {
            let when = tool_when(&d.name);
            if when.is_empty() {
                // 未知工具(脚本插件):无内置「何时调用」映射,仅列名称与功能描述
                out.push_str(&format!("\n- {}:{}", d.name, d.description.trim()));
            } else {
                out.push_str(&format!(
                    "\n- {}:{}。{}",
                    d.name,
                    d.description.trim(),
                    when
                ));
            }
        }
        out
    }

    /// 最终执行边界:查工具 → 权限裁决 → JSON.parse 参数 → 调用(async,带超时)。
    /// 所有调用方都必须经过这里，避免 executor 或插件路径绕过授权。
    pub async fn execute(
        &self,
        name: &str,
        args_json: &str,
        ctx: ToolContext,
    ) -> Result<String, String> {
        let tool = self
            .get(name)
            .ok_or_else(|| format!("未注册的工具:{name}"))?;
        let decision = self.permissions.decide(name, &ctx);
        if !decision.allowed {
            return Err(format!("工具 \"{name}\" 未执行:{}", decision.reason));
        }
        self.run_tool(tool, args_json, ctx).await
    }

    /// 已裁决执行:跳过权限裁决,直接按给定 decision 语义执行(仍查工具存在性)。
    /// 供 Custom 白名单等已显式放行的路径使用——避免「外层放行、execute 二次裁决拒绝」
    /// 的双层裁决冲突;权限模型依旧唯一(ToolPermissionManager),此处只是信任上游裁决。
    pub async fn execute_with_decision(
        &self,
        name: &str,
        args_json: &str,
        ctx: ToolContext,
        decision: &PermissionDecision,
    ) -> Result<String, String> {
        if !decision.allowed {
            return Err(format!("工具 \"{name}\" 未执行:{}", decision.reason));
        }
        let tool = self
            .get(name)
            .ok_or_else(|| format!("未注册的工具:{name}"))?;
        self.run_tool(tool, args_json, ctx).await
    }

    /// 参数解析 + 带超时执行 + 结果截断
    async fn run_tool(
        &self,
        tool: RegisteredTool,
        args_json: &str,
        ctx: ToolContext,
    ) -> Result<String, String> {
        let name = tool.definition.name.clone();
        let args: Value = serde_json::from_str(args_json)
            .map_err(|_| format!("工具 \"{name}\" 参数解析失败:{args_json}"))?;
        let fut = (tool.execute)(args, ctx);
        let timeout = self.tool_timeout;
        let output = match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(format!("工具 \"{name}\" 执行超时({}s)", timeout.as_secs())),
        };
        // 超长结果截断(保留字节计数元数据,调用方按字符串回填模型/前端)
        if output.len() > MAX_TOOL_OUTPUT_BYTES {
            let mut boundary = MAX_TOOL_OUTPUT_BYTES;
            while boundary > 0 && !output.is_char_boundary(boundary) {
                boundary -= 1;
            }
            let mut truncated = output[..boundary].to_string();
            truncated.push_str(&format!(
                "\n...\n[输出已截断:原始 {} 字节,保留前 {} 字符]",
                output.len(),
                MAX_TOOL_OUTPUT_BYTES
            ));
            return Ok(truncated);
        }
        Ok(output)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 各内置工具的「何时调用」说明(注入系统提示词用);未知名(脚本插件工具)回退 description。
fn tool_when(name: &str) -> String {
    let s = match name {
        "read" => "需要读取世界书条目/角色提示词/技能库/角色文件/子任务结果时调用。",
        "role" => "需要随机数或掷骰(决定走向、概率事件、生成随机值)时调用。",
        "write" => "需要把内容写入当前对话气泡或角色文件区文件时调用。",
        "replace" => "需要修改对话气泡或角色文件区的已有内容(追加/插入/替换/删除)时调用。",
        "create" => "需要在角色文件区新建文件夹或文件时调用。",
        "search" => "需要联网查询最新信息、资料或实时内容时调用。",
        "todo" => "需要查看当前执行计划、子智能体任务状态或最近工具调用历史时调用。",
        "agentgo" => "需要把可并行的子任务(如多段资料整理、分头起草)交给子智能体后台执行时调用,返回 task_id 供后续 read/todo 轮询。",
        "agentend" => "需要取消或结束已派出的子智能体任务时调用(传入 task_ids)。",
        "sleep" => "需要等待子任务或外部过程完成时调用(毫秒,上限 60000)。",
        "calculator" => "需要进行四则运算或数值计算时调用。",
        "censor_text" => "需要把文本中的禁词替换为更得体表达时调用。",
        "memory_read" => "需要读取本会话长期记忆中的既有记录时调用。",
        "memory_write" => "需要把值得长期记住的信息写入本会话记忆时调用。",
        "update_variables" => "需要更新当前会话的 stat_data 变量树时调用。",
        _ => return String::new(),
    };
    s.to_string()
}

/// 内置工具的展示意图(render intent),供 SSE ToolCall/ToolResult 透传、前端分派渲染。
/// 取值:read(读文件/条目,代码块展示)、search(搜索结果,列表化)、generic(其余通用)。
/// 未知名(脚本插件工具)返回 None,前端回退 JSON 直出。
pub fn render_kind_for(name: &str) -> Option<&'static str> {
    match name {
        "read" => Some("read"),
        "search" => Some("search"),
        "calculator" | "role" | "write" | "replace" | "create" | "todo" | "agentgo"
        | "agentend" | "sleep" | "censor_text" | "memory_read" | "memory_write"
        | "update_variables" | "get_state" | "apply_patch" => Some("generic"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "s".into(),
            character_id: "c".into(),
        }
    }

    #[test]
    fn list_definitions_is_stable_by_name() {
        let reg = ToolRegistry::new();
        for name in ["zeta", "alpha", "middle"] {
            reg.register(
                ToolDefinition {
                    name: name.into(),
                    description: name.into(),
                    parameters: serde_json::json!({}),
                },
                Arc::new(|_, _| Box::pin(async { Ok("ok".into()) })),
            );
        }

        let names: Vec<String> = reg
            .list_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        assert_eq!(names, vec!["alpha", "middle", "zeta"]);
    }

    #[test]
    fn tool_guidance_lists_builtin_with_when() {
        let reg = ToolRegistry::new();
        for (name, desc) in [("read", "读取资料"), ("role", "生成随机数")] {
            reg.register(
                ToolDefinition {
                    name: name.into(),
                    description: desc.into(),
                    parameters: serde_json::json!({}),
                },
                Arc::new(|_, _| Box::pin(async { Ok("ok".into()) })),
            );
        }
        let g = reg.tool_guidance();
        assert!(g.contains("read"), "应含 read: {g}");
        assert!(g.contains("role"), "应含 role: {g}");
        assert!(g.contains("function calling"), "应说明调用方式: {g}");
        assert!(g.contains("随机数"), "应含 role 的调用时机: {g}");
    }

    #[test]
    fn tool_guidance_for_only_lists_effective_definitions() {
        let reg = ToolRegistry::new();
        for name in ["read", "search"] {
            reg.register(
                ToolDefinition {
                    name: name.into(),
                    description: format!("{name} 功能"),
                    parameters: serde_json::json!({}),
                },
                Arc::new(|_, _| Box::pin(async { Ok("ok".into()) })),
            );
        }
        let effective = vec![reg.get("read").unwrap().definition];
        let guidance = reg.tool_guidance_for(&effective);
        assert!(guidance.contains("read"));
        assert!(
            !guidance.contains("search"),
            "不应泄露不可见工具: {guidance}"
        );
    }

    #[test]
    fn tool_guidance_unknown_tool_falls_back_to_description() {
        let reg = ToolRegistry::new();
        reg.register(
            ToolDefinition {
                name: "my_plugin".into(),
                description: "自定义插件功能".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| Box::pin(async { Ok("ok".into()) })),
        );
        let g = reg.tool_guidance();
        assert!(g.contains("my_plugin"), "应列名: {g}");
        assert!(g.contains("自定义插件功能"), "应回退 description: {g}");
    }

    #[test]
    fn tool_guidance_empty_registry_returns_empty() {
        let reg = ToolRegistry::new();
        assert!(reg.tool_guidance().is_empty(), "空注册表应返回空串");
    }

    #[tokio::test]
    async fn dangerous_tool_never_executes_without_explicit_authorization() {
        let reg = ToolRegistry::new();
        reg.register(
            ToolDefinition {
                name: "write".into(),
                description: "写入".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| Box::pin(async { Ok("不应执行".into()) })),
        );

        let error = reg.execute("write", "{}", ctx()).await.unwrap_err();
        assert!(error.to_string().contains("需要显式授权"));
    }

    #[tokio::test]
    async fn session_authorization_allows_sensitive_tool() {
        let reg = ToolRegistry::new();
        reg.register(
            ToolDefinition {
                name: "memory_write".into(),
                description: "写记忆".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| Box::pin(async { Ok("已执行".into()) })),
        );
        reg.permissions()
            .authorize("memory_write", "session", "s")
            .unwrap();

        let output = reg.execute("memory_write", "{}", ctx()).await.unwrap();
        assert_eq!(output, "已执行");
    }

    #[tokio::test]
    async fn test_register_and_execute() {
        let reg = ToolRegistry::new();
        reg.register(
            ToolDefinition {
                name: "calculator".into(),
                description: "calculator".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|args, _ctx| {
                Box::pin(async move { Ok(serde_json::to_string(&args).unwrap()) })
            }),
        );
        let out = reg
            .execute("calculator", r#"{"x":1}"#, ctx())
            .await
            .unwrap();
        assert!(out.contains("x"));
        assert!(reg.execute("nope", "{}", ctx()).await.is_err());
    }

    /// M2:工具执行超时——执行器超过 tool_timeout 时按失败返回,不再无限阻塞
    #[tokio::test]
    async fn tool_execution_times_out() {
        let reg = ToolRegistry::new().with_tool_timeout(Duration::from_millis(50));
        reg.register(
            ToolDefinition {
                name: "slow".into(),
                description: "慢工具".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok("太慢".into())
                })
            }),
        );
        // slow 为未注册权限工具 → 权限裁决会拒绝,先授权会话放行
        reg.permissions().authorize("slow", "session", "s").unwrap();
        let err = reg.execute("slow", "{}", ctx()).await.unwrap_err();
        assert!(err.contains("执行超时"), "应返回超时错误,实际: {err}");
    }

    /// M2:已裁决执行接口(execute_with_decision)——跳过二次权限裁决,白名单放行路径可用。
    /// 修复 Custom 白名单「外层放行、execute 内部再裁决拒绝」的双层冲突。
    #[tokio::test]
    async fn execute_with_decision_bypasses_second_permission_check() {
        let reg = ToolRegistry::new();
        reg.register(
            ToolDefinition {
                name: "write".into(),
                description: "写入".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| Box::pin(async { Ok("已写入".into()) })),
        );
        // 危险工具未授权:普通 execute 必须拒绝
        let err = reg.execute("write", "{}", ctx()).await.unwrap_err();
        assert!(err.contains("未执行"), "危险工具未授权时应拒绝: {err}");
        // 已裁决执行(白名单放行语义):应成功
        let decision = crate::tools::permissions::PermissionDecision {
            allowed: true,
            risk: crate::tools::permissions::ToolRisk::Dangerous,
            reason: "步骤白名单工具,自动放行".into(),
        };
        let out = reg
            .execute_with_decision("write", "{}", ctx(), &decision)
            .await
            .unwrap();
        assert_eq!(out, "已写入");
    }

    #[tokio::test]
    async fn execute_with_decision_rejects_denied_decision() {
        let reg = ToolRegistry::new();
        reg.register(
            ToolDefinition {
                name: "calculator".into(),
                description: "计算".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| Box::pin(async { Ok("不应执行".into()) })),
        );
        let denied = PermissionDecision {
            allowed: false,
            risk: crate::tools::permissions::ToolRisk::Safe,
            reason: "测试拒绝".into(),
        };
        let error = reg
            .execute_with_decision("calculator", "{}", ctx(), &denied)
            .await
            .unwrap_err();
        assert!(error.contains("测试拒绝"));
    }

    #[tokio::test]
    async fn tool_output_truncates_on_valid_utf8_boundary() {
        let reg = ToolRegistry::new().with_tool_timeout(Duration::from_secs(5));
        reg.register(
            ToolDefinition {
                name: "unicode-big".into(),
                description: "中文输出".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| Box::pin(async { Ok("中文😀".repeat(20 * 1024)) })),
        );
        reg.permissions()
            .authorize("unicode-big", "session", "s")
            .unwrap();
        let out = reg.execute("unicode-big", "{}", ctx()).await.unwrap();
        assert!(out.contains("输出已截断"));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    /// M2:工具输出超长截断——超限输出带截断标记,不整段撑爆上下文
    #[tokio::test]
    async fn tool_output_is_truncated_when_oversized() {
        let reg = ToolRegistry::new().with_tool_timeout(Duration::from_secs(5));
        reg.register(
            ToolDefinition {
                name: "big".into(),
                description: "大输出".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| Box::pin(async { Ok("x".repeat(80 * 1024)) })),
        );
        reg.permissions().authorize("big", "session", "s").unwrap();
        let out = reg.execute("big", "{}", ctx()).await.unwrap();
        assert!(
            out.contains("输出已截断"),
            "超长输出应带截断标记,实际长度 {}",
            out.len()
        );
        assert!(
            out.len() < 70 * 1024,
            "截断后应远小于原始 80KB: {}",
            out.len()
        );
    }
}
