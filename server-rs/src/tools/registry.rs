// 工具注册表(与 Node 版 tools/registry.ts 对齐)
// 执行器为 async(sleep/agentgo/search 等工具需要异步能力),签名:
//   Fn(Value, ToolContext) -> BoxFuture<Result<String, String>>
use crate::models::types::{ToolContext, ToolDefinition};
use crate::tools::action_class::ToolOrigin;
use crate::tools::permissions::{PermissionDecision, ToolPermissionManager};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 工具执行超时(防止 sleep/插件/网络工具无限阻塞;超时按失败回填模型)
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(30);
/// 工具结果最大字节数(超出截断,避免超长输出撑爆上下文与前端渲染)
const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

/// 工具执行器类型别名**已下沉到 L1**（`crate::models::types::ToolExecutor`，2026-09-14）。
///
/// 理由：`plugins/`（L3）与 `mcp/`（L3）都需要**构造**执行器，若类型留在本模块（L2），
/// 两者都会构成 `L3→L2` 越代依赖。此处仅重导出，服务 `tools/` 内部既有 `use`。
pub use crate::models::types::ToolExecutor;

#[derive(Clone)]
pub struct RegisteredTool {
    pub definition: ToolDefinition,
    pub execute: ToolExecutor,
    /// 该工具的执行超时;None = 跟随注册表 tool_timeout(默认 30s,测试可缩短)。
    /// MCP 等慢外部工具经 register_with_timeout 显式放宽(批次 6.2)。
    pub timeout: Option<Duration>,
    /// 工具来源(内置/插件/MCP):三档授权模式据此判定路径区域可信度
    /// (内置文件工具受角色文件区沙箱约束,外部工具的参数对引擎不透明)。
    pub origin: ToolOrigin,
}

pub struct ToolRegistry {
    tools: Mutex<HashMap<String, RegisteredTool>>,
    permissions: ToolPermissionManager,
    /// 工具执行超时(测试可缩短,生产默认 30s)
    tool_timeout: Duration,
    /// 回退快照服务(批次 6.1「undo」):由 app_state 在工具注册后经 set_undo 注入
    /// (OnceLock:UndoService 依赖的 Db/SessionService 早于注册表存在,但装配顺序
    /// 上注册表先构造,故后注;与 ToolDeps.engine/tasks 的 OnceLock 后注同范式)。
    /// 未注入(单元测试 ToolRegistry::new())时写工具不产快照,行为与旧版一致。
    undo: std::sync::OnceLock<Arc<crate::services::undo_service::UndoService>>,
    /// 工具定义快照缓存(2026-09-14 性能):`list_definitions` 此前每次调用都在锁内
    /// 全量 clone 每个工具定义再排序。该函数在请求热路径上(agent 模式每请求一次、
    /// custom 模式每 step 一次、任务策略每次 compile 一次),工具多时是无谓的重复开销。
    /// 改为「按需构建 + 写路径失效」:注册/注销时置 None,下次读取重建一次。
    /// 失效点只有 register_external 与 unregister 两处,覆盖插件加载与 MCP 动态注册。
    definitions_cache: Mutex<Option<Arc<Vec<ToolDefinition>>>>,
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
            undo: std::sync::OnceLock::new(),
            definitions_cache: Mutex::new(None),
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

    /// 注入回退快照服务(批次 6.1;app_state 装配时调用一次,重复 set 忽略)
    pub fn set_undo(&self, undo: Arc<crate::services::undo_service::UndoService>) {
        let _ = self.undo.set(undo);
    }

    pub fn register(&self, definition: ToolDefinition, execute: ToolExecutor) {
        self.register_with_timeout(definition, execute, None);
    }

    /// 带自定义执行超时的注册(批次 6.2 MCP 外部工具用,如 120s);
    /// timeout=None 与 register 完全一致(跟随注册表 tool_timeout,生产 30s),
    /// 既有调用方行为零变化。
    pub fn register_with_timeout(
        &self,
        definition: ToolDefinition,
        execute: ToolExecutor,
        timeout: Option<Duration>,
    ) {
        self.register_external(definition, execute, timeout, ToolOrigin::Builtin);
    }

    /// 带来源标记的注册:插件与 MCP 工具须经此登记 origin,
    /// 否则三档授权模式会把外部工具误当作受沙箱约束的内置工具。
    pub fn register_external(
        &self,
        definition: ToolDefinition,
        execute: ToolExecutor,
        timeout: Option<Duration>,
        origin: ToolOrigin,
    ) {
        let mut g = self.tools.lock().unwrap_or_else(|e| e.into_inner());
        g.insert(
            definition.name.clone(),
            RegisteredTool {
                definition,
                execute,
                timeout,
                origin,
            },
        );
        drop(g);
        self.invalidate_definitions_cache();
    }

    /// 失效工具定义快照(2026-09-14)。**所有写路径必须调用**——漏调会导致
    /// 新注册的工具(插件/MCP)不下发给模型。当前写路径只有 register_external 与 unregister。
    fn invalidate_definitions_cache(&self) {
        *self
            .definitions_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// 工具来源;未注册返回 None。裁决时用于区分沙箱内外的路径可信度。
    pub fn origin_of(&self, name: &str) -> Option<ToolOrigin> {
        self.tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .map(|t| t.origin)
    }

    /// 是否内置工具(受角色文件区沙箱约束)
    pub fn is_builtin(&self, name: &str) -> bool {
        self.origin_of(name) == Some(ToolOrigin::Builtin)
    }
}

/// 实现 L1 的 [`crate::models::types::ToolRegistrar`] 窄接口：让 L3（`mcp/`）能经
/// `&dyn ToolRegistrar` 注册工具，而**不必** `use crate::tools::...`（否则构成 L3→L2
/// 越代依赖）。方法体直接委托到本结构体的同名固有方法，行为零变化。
impl crate::models::types::ToolRegistrar for ToolRegistry {
    fn register_external(
        &self,
        definition: ToolDefinition,
        execute: ToolExecutor,
        timeout: Option<Duration>,
        origin: ToolOrigin,
    ) {
        ToolRegistry::register_external(self, definition, execute, timeout, origin);
    }

    fn unregister(&self, name: &str) {
        ToolRegistry::unregister(self, name);
    }
}

impl ToolRegistry {
    pub fn unregister(&self, name: &str) {
        self.tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name);
        self.invalidate_definitions_cache();
    }

    pub fn get(&self, name: &str) -> Option<RegisteredTool> {
        self.tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    pub fn list_definitions(&self) -> Vec<ToolDefinition> {
        self.definitions_snapshot().as_ref().clone()
    }

    /// 工具定义的共享快照(2026-09-14 性能)。
    /// 首次调用构建一次(锁内取全部定义 → 排序),之后返回同一 `Arc` 直到注册表变化。
    /// 热路径若只需遍历/过滤,用本方法避免 `list_definitions` 的整体 clone。
    pub fn definitions_snapshot(&self) -> Arc<Vec<ToolDefinition>> {
        if let Some(hit) = self
            .definitions_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .cloned()
        {
            return hit;
        }
        let mut definitions: Vec<_> = self
            .tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|t| t.definition.clone())
            .collect();
        definitions.sort_by(|a, b| a.name.cmp(&b.name));
        let snapshot = Arc::new(definitions);
        *self
            .definitions_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(snapshot.clone());
        snapshot
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
            .ok_or_else(|| format!("未注册的工具:{name}。请先用 todo 工具查看可用能力,或改用已注册的工具名(区分大小写)"))?;
        let decision = self.permissions.decide(name, &ctx);
        if !decision.allowed {
            return Err(format!(
                "工具 \"{name}\" 未执行:{}。下一步:在授权弹窗中允许,或改用无需授权的只读工具(如 read/todo)",
                decision.reason
            ));
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
            return Err(format!(
                "工具 \"{name}\" 未执行:{}。下一步:检查步骤白名单配置,或改用白名单内的工具",
                decision.reason
            ));
        }
        let tool = self
            .get(name)
            .ok_or_else(|| format!("未注册的工具:{name}。请先用 todo 工具查看可用能力,或改用已注册的工具名(区分大小写)"))?;
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
        // 补回 serde 原错(含行列位置):模型给出的坏 JSON 常是截断/多余逗号,
        // 位置信息是判断「预算不足」还是「语法错误」的关键
        let args: Value = serde_json::from_str(args_json).map_err(|e| {
            format!("工具 \"{name}\" 参数解析失败({e}):{args_json}。下一步:改为合法 JSON 对象,键名与类型对照工具定义的 parameters")
        })?;
        // 批次 6.1 回退快照(两段式):写工具执行前取逆操作负载暂存;执行成功 commit
        // 落库,失败 discard 丢弃。快照构建含角色文件区文件读取与同步 DB 查询,
        // 经 spawn_blocking 挪出 tokio worker(B-1:消除 async 热路径同步 IO);
        // 构建失败/任务取消返回 None——快照 best-effort,绝不挡工具执行。
        // undo_enabled 开关在 UndoService 内读取(全局开关,直接读基础值)。
        let stager = match self.undo.get() {
            Some(undo) => {
                let undo = undo.clone();
                let snap_name = name.clone();
                let snap_args = args.clone();
                let snap_ctx = ctx.clone();
                tokio::task::spawn_blocking(move || {
                    undo.snapshot_before(&snap_name, &snap_args, &snap_ctx)
                })
                .await
                .ok()
                .flatten()
            }
            None => None,
        };
        let fut = (tool.execute)(args, ctx);
        // 单工具自定义超时(MCP 等慢外部工具)优先,否则跟随注册表档(生产 30s)
        let timeout = tool.timeout.unwrap_or(self.tool_timeout);
        let output = match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                if let Some(s) = stager {
                    s.discard();
                }
                return Err(e);
            }
            Err(_) => {
                if let Some(s) = stager {
                    s.discard();
                }
                return Err(format!(
                    "工具 \"{name}\" 执行超时({}s)。下一步:缩小参数范围(如减少读取量)后重试,或改用 agentgo 后台执行",
                    timeout.as_secs()
                ));
            }
        };
        // 执行成功:补记执行后信息并落库(须在下方截断之前,截断后输出不再是合法 JSON)
        if let Some(s) = stager {
            s.commit(&output);
        }
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
            agent_depth: 0,
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

    /// P3-1(2026-09-14):工具定义快照的**失效正确性**。
    /// 这是本优化唯一的风险点——漏失效会让新注册的工具(插件/MCP)不下发给模型。
    /// 断言:① 重复读取命中同一 Arc(缓存生效);② 注册后缓存失效、新工具可见;
    /// ③ 注销后同样失效。
    #[test]
    fn definitions_snapshot_invalidates_on_register_and_unregister() {
        let reg = ToolRegistry::new();
        let def = |n: &str| ToolDefinition {
            name: n.into(),
            description: n.into(),
            parameters: serde_json::json!({}),
        };
        let ex = || -> ToolExecutor { Arc::new(|_, _| Box::pin(async { Ok("ok".into()) }) as _) };

        reg.register(def("alpha"), ex());
        let first = reg.definitions_snapshot();
        let second = reg.definitions_snapshot();
        assert!(
            Arc::ptr_eq(&first, &second),
            "注册表未变时快照应命中同一 Arc(缓存生效)"
        );
        assert_eq!(first.len(), 1);

        // 注册新工具 → 快照必须失效
        reg.register(def("beta"), ex());
        let after_register = reg.definitions_snapshot();
        assert_eq!(after_register.len(), 2, "注册后快照必须失效并包含新工具");
        assert!(
            after_register.iter().any(|d| d.name == "beta"),
            "新注册工具必须出现在快照里"
        );
        assert!(
            !Arc::ptr_eq(&first, &after_register),
            "注册后应返回新构建的快照"
        );

        // 注销 → 快照必须失效
        reg.unregister("alpha");
        let after_unregister = reg.definitions_snapshot();
        assert_eq!(after_unregister.len(), 1, "注销后快照必须失效");
        assert_eq!(after_unregister[0].name, "beta");
    }

    /// 通过 ToolRegistrar 窄接口注册(插件/MCP 的实际路径)同样触发失效
    #[test]
    fn definitions_snapshot_invalidates_via_registrar_trait() {
        use crate::models::types::ToolRegistrar;
        let reg = ToolRegistry::new();
        reg.register(
            ToolDefinition {
                name: "builtin".into(),
                description: "内置".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| Box::pin(async { Ok("ok".into()) })),
        );
        assert_eq!(reg.definitions_snapshot().len(), 1);

        // 模拟 MCP 动态注册(经 &dyn ToolRegistrar)
        let registrar: &dyn ToolRegistrar = &reg;
        registrar.register_external(
            ToolDefinition {
                name: "mcp_demo_tool".into(),
                description: "外部".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| Box::pin(async { Ok("ok".into()) })),
            None,
            ToolOrigin::Mcp,
        );
        let snap = reg.definitions_snapshot();
        assert_eq!(snap.len(), 2, "经窄接口注册后快照必须失效");
        assert!(snap.iter().any(|d| d.name == "mcp_demo_tool"));
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

    /// 工具治理:两次构建注册表,工具定义序列化逐字节一致——
    /// HashMap 无序,若 list_definitions 不排序,下发给模型的工具定义数组顺序会跨启动漂移,
    /// 击穿 DeepSeek 逐字节前缀缓存。此测试守护该顺序稳定性。
    #[test]
    fn tool_definitions_serialize_identically_across_builds() {
        fn build_registry() -> ToolRegistry {
            let reg = ToolRegistry::new();
            // 注册顺序刻意乱序,验证输出不依赖插入顺序
            for name in ["zeta", "alpha", "middle", "read", "write"] {
                reg.register(
                    ToolDefinition {
                        name: name.into(),
                        description: format!("{name} 工具"),
                        parameters: serde_json::json!({"type":"object"}),
                    },
                    Arc::new(|_, _| Box::pin(async { Ok("ok".into()) })),
                );
            }
            reg
        }
        let first = serde_json::to_string(&build_registry().list_definitions()).unwrap();
        let second = serde_json::to_string(&build_registry().list_definitions()).unwrap();
        assert_eq!(first, second, "两次构建的工具定义 JSON 必须逐字节一致");
        assert!(first.contains("alpha"), "定义序列化应含工具名: {first}");
    }

    /// 工具治理:错误文案可操作化——裸错误一律附「下一步」指引,模型可直接照做
    #[tokio::test]
    async fn error_messages_carry_next_step_guidance() {
        let reg = ToolRegistry::new().with_tool_timeout(Duration::from_millis(50));
        reg.register(
            ToolDefinition {
                name: "write".into(),
                description: "写入".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok("不应到达".into())
                })
            }),
        );
        // 未注册工具:补「查看可用能力/核对工具名」指引
        let missing = reg.execute("nope", "{}", ctx()).await.unwrap_err();
        assert!(
            missing.contains("todo") && missing.contains("区分大小写"),
            "未注册错误应含指引: {missing}"
        );
        // 权限拒绝:补「授权或改用只读工具」指引
        let denied = reg.execute("write", "{}", ctx()).await.unwrap_err();
        assert!(
            denied.contains("授权弹窗") && denied.contains("只读工具"),
            "权限拒绝应含指引: {denied}"
        );
        // 参数解析失败:补「对照 parameters 修正 JSON」指引
        reg.permissions()
            .authorize("write", "session", "s")
            .unwrap();
        let bad_args = reg.execute("write", "不是json", ctx()).await.unwrap_err();
        assert!(
            bad_args.contains("合法 JSON") && bad_args.contains("parameters"),
            "参数解析失败应含指引: {bad_args}"
        );
        // 超时:补「缩小范围重试/转后台」指引
        let timeout_err = reg.execute("write", "{}", ctx()).await.unwrap_err();
        assert!(
            timeout_err.contains("缩小参数范围") && timeout_err.contains("agentgo"),
            "超时错误应含指引: {timeout_err}"
        );
    }

    /// 批次 6.2:register_with_timeout 单工具超时覆盖注册表档——
    /// 慢外部工具(MCP)可放宽到 120s 而不影响其余工具的 30s 默认。
    #[tokio::test]
    async fn register_with_timeout_overrides_registry_default() {
        let reg = ToolRegistry::new().with_tool_timeout(Duration::from_millis(50));
        // 自定义 5s 超时:执行器睡 100ms(超过注册表档 50ms)应成功——单工具档生效
        reg.register_with_timeout(
            ToolDefinition {
                name: "slow-mcp".into(),
                description: "慢外部工具".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok("慢但完成".into())
                })
            }),
            Some(Duration::from_secs(5)),
        );
        reg.permissions()
            .authorize("slow-mcp", "session", "s")
            .unwrap();
        let out = reg.execute("slow-mcp", "{}", ctx()).await.unwrap();
        assert_eq!(out, "慢但完成", "单工具 5s 档应覆盖注册表 50ms 档");

        // 对照:普通 register 仍跟随注册表档(50ms),睡 100ms 应超时
        reg.register(
            ToolDefinition {
                name: "slow-default".into(),
                description: "默认档慢工具".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Ok("不应到达".into())
                })
            }),
        );
        reg.permissions()
            .authorize("slow-default", "session", "s")
            .unwrap();
        let err = reg.execute("slow-default", "{}", ctx()).await.unwrap_err();
        assert!(err.contains("执行超时"), "默认档应超时: {err}");
    }

    /// 批次 6.2:缺省路径零变化——register 与 register_with_timeout(None) 等价(30s 生产档)
    #[test]
    fn register_default_timeout_unchanged() {
        assert_eq!(
            DEFAULT_TOOL_TIMEOUT,
            Duration::from_secs(30),
            "生产默认工具超时不得漂移(30s)"
        );
        let reg = ToolRegistry::new();
        reg.register(
            ToolDefinition {
                name: "plain".into(),
                description: "普通注册".into(),
                parameters: serde_json::json!({}),
            },
            Arc::new(|_, _| Box::pin(async { Ok("ok".into()) })),
        );
        assert!(
            reg.get("plain").unwrap().timeout.is_none(),
            "register 不应携带单工具超时(None = 跟随注册表档)"
        );
    }
}
