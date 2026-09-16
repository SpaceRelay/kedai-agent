// 核心类型定义,与 Node 版 server/src/models/types.ts 契约一一对应
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------- 角色 ----------
#[derive(Debug, Clone, Serialize)]
pub struct CharacterRecord {
    pub id: String,
    /// 原始文件名(去扩展名,非法字符替换为 _)
    pub name: String,
    /// V2 规范 name
    pub chara_name: String,
    pub description: String,
    pub file_path: String,
    pub avatar_path: Option<String>,
    /// 完整 V2 JSON,未知字段无损保留;列表接口序列化为 null 时省略
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_raw: Option<Value>,
    /// 开场白(first_mes,从 data_raw 提取,供编辑弹窗)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_mes: Option<String>,
    /// 备用开场列表(alternate_greetings,从 data_raw 提取;多开场切换用)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternate_greetings: Option<Vec<String>>,
    /// 角色卡正则脚本(从 data_raw 提取,前端可选 HTML 渲染)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regex_scripts: Option<Vec<crate::parsing::regex_script::RegexScript>>,
    /// 角色卡内嵌插件检测结果(酒馆助手等;仅详情接口填充,列表不携带)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_plugins: Option<Vec<crate::parsing::assistant::CardPluginInfo>>,
    /// 卡元数据(从 data_raw 提取,列表也携带):远程资源卡(酒馆助手式资源页)
    /// 需要 creator/character_version/creator_notes 推导资源包口令(见 resource_frame 模板
    /// 的 TavernHelper shim),体积小,列表/详情均返回
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator_notes: Option<String>,
    pub created_at: String,
}

// ---------- 会话 / 消息 ----------
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub character_id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 会话 + 角色名(聊天记录面板用)
#[derive(Debug, Clone, Serialize)]
pub struct SessionWithCharacter {
    pub id: String,
    pub character_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_name: Option<String>,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

// ---------- 世界书 ----------
/// 世界书记录(对应 world_books 表)
#[derive(Debug, Clone, Serialize)]
pub struct WorldBookRecord {
    pub id: String,
    pub name: String,
    /// 绑定角色 id;None = 全局
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,
    /// 绑定角色名(联表查询填充,列表展示用)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_name: Option<String>,
    pub enabled: bool,
    /// upload=独立上传;character_card=来自角色卡内嵌
    pub source: String,
    /// 有效条目数
    pub entry_count: i64,
    /// 原始 JSON 完整保留(无损)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_raw: Option<Value>,
    /// 上传时的自动转换统计(仅上传响应返回;不落库)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversion: Option<crate::parsing::world_book_convert::ConversionReport>,
    pub created_at: String,
}

/// 世界书条目编辑视图(API 预览/编辑返回;可 Deserialize 回写 data_raw)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorldBookEntryView {
    /// 数值 id:优先 uid/id,缺失时用序号
    #[serde(default)]
    pub id: i64,
    /// 注释/标题(如 "地点"、"世界观")
    #[serde(default)]
    pub comment: String,
    /// 触发关键字(子串匹配,大小写不敏感)
    #[serde(default)]
    pub keys: Vec<String>,
    /// 副关键词(酒馆 keysecondary / secondary_keys)
    #[serde(default)]
    pub keys_secondary: Vec<String>,
    /// 可选正则表达式(use_regex=true 时优先于 keys 匹配)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// 是否启用正则匹配
    #[serde(default)]
    pub use_regex: bool,
    /// constant=true 时始终注入,不受 key 匹配限制(酒馆「全局」)
    #[serde(default)]
    pub constant: bool,
    /// 是否启用(酒馆「激发状态」)
    #[serde(default)]
    pub enabled: bool,
    pub content: String,
    /// 注入位置权重(0=before_char ... 4=after_char;本项目统一并入 system,仅用于排序)
    #[serde(default)]
    pub position: i64,
    /// 扫描深度(酒馆 depth):从最新消息往前扫最近 N 条(0 = 全部)
    #[serde(default)]
    pub depth: i64,
    /// 注入顺序(酒馆 order / insertion_order):同 position 内先后
    #[serde(default)]
    pub order: i64,
    /// 关键词大小写敏感(酒馆 caseSensitive)
    #[serde(default)]
    pub case_sensitive: bool,
    /// 粘性(酒馆 sticky):命中后接下来 N 条消息持续注入
    #[serde(default)]
    pub sticky: i64,
    /// 冷却(酒馆 cooldown):命中后 N 条消息内不再次触发
    #[serde(default)]
    pub cooldown: i64,
    /// 命中概率%(酒馆 probability + useProbability)
    #[serde(default)]
    pub probability: i64,
    /// 是否启用概率
    #[serde(default)]
    pub use_probability: bool,
    /// 注入消息角色(system / user / assistant;None = 缺省按位置决定:常驻→system,激发→user)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    /// 自增数字 id
    pub id: i64,
    pub session_id: String,
    pub role: String, // user | assistant | system
    pub content: String,
    pub extra: Value,
    pub created_at: String,
}

/// 导入导出用的 SillyTavern 兼容消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

// ---------- Agent ----------
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSessionRecord {
    pub id: String,
    pub session_id: String,
    pub state: String,
    pub plan: Vec<String>,
    pub steps: Vec<ToolCallRecord>,
    pub step_index: i64,
    pub agent_mode: String,
    pub started_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub agent_session_id: String,
    pub name: String,
    pub input: Value,
    pub output: Value,
    pub duration_ms: i64,
    pub created_at: String,
}

// ---------- Agent 计划 ----------
/// 执行步骤(三模式静态模板 与 自定义流程共用):
/// 自定义流程(custom 模式)由用户在设置中编辑,extra 字段缺省时行为与三模式模板一致。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanStep {
    /// 自定义流程步骤 id(模板步骤为空串)
    #[serde(default)]
    pub id: String,
    /// 步骤名称(自定义流程展示用;模板步骤为空串)
    #[serde(default)]
    pub name: String,
    /// 是否启用(仅配置层语义;make_custom_plan 已过滤,引擎执行时恒为启用步骤)
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub goal: String,
    /// direct | reflect(校验见 `services/agent_flow_service.rs`;`tool` 不在支持范围)
    #[serde(default)]
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generates: Option<bool>,
    /// 步骤级系统提示词(追加到 system 末尾,支持酒馆宏;None = 不追加)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// 步骤级温度覆盖(0.0-2.0;None = 用全局参数)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// 步骤级输出上限覆盖(1-32768;None = 用全局参数)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// 步骤级工具:None=不使用工具;Some([])=全部工具;Some(list)=白名单
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// 步骤级工具选择策略:auto | none | required | function;None = auto
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    /// tool_choice=function 时指定的工具名
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice_function: Option<String>,
    /// 是否允许模型单轮返回多个工具调用;None = 使用后端默认
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub steps: Vec<PlanStep>,
    pub summary: String,
}

// ---------- Token ----------
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub context_tokens: i64,
    /// prompt 缓存命中 token(DeepSeek 等提供商的 prompt_cache_hit_tokens,
    /// 用于前端「当前命中率」展示;无缓存字段的提供商恒为 0)
    #[serde(default)]
    pub prompt_cache_hit_tokens: i64,
    /// prompt 缓存未命中 token(DeepSeek prompt_cache_miss_tokens;OpenAI 风格由
    /// prompt_tokens - cached_tokens 推导;无缓存字段的提供商恒为 0)
    #[serde(default)]
    pub prompt_cache_miss_tokens: i64,
}

// ---------- LLM ----------
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String, // system | user | assistant | tool
    pub content: String,
    /// 思考模式(thinking)的推理内容;DeepSeek 系后端要求多轮工具调用时原样回传,否则 400
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// assistant 消息携带的工具调用(function calling);None = 普通消息
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallArgs>>,
    /// role="tool" 消息对应的工具调用 id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl LlmMessage {
    /// 构造普通消息(system/user/assistant)
    pub fn plain(role: &str, content: &str) -> Self {
        LlmMessage {
            role: role.to_string(),
            content: content.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }
}

// ---------- 提示词楼层(L1:与 DB/服务无关,parsing 预设导入与 services 注入共用) ----------

/// 楼层注入位置(与 SillyTavern Prompt Manager 语义对齐)
///
/// **注意**:引擎注入路径已统一归位「系统提示词内」——`Before`/`After`/`Depth`
/// 不再参与注入(见 `agents/engine/messages/build.rs` 位置4 注释)。三个变体仅保留
/// 解析能力,用于兼容导入的酒馆预设(`parsing/preset.rs`),新配置应一律用 `System`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FloorPosition {
    /// 拼入系统提示词末尾
    #[default]
    System,
    /// 对话历史最前(开场白之前)——已废弃,不参与注入
    Before,
    /// 对话历史最后(最新消息之后)——已废弃,不参与注入
    After,
    /// 深度:从历史末尾往前数第 N 条之后插入(0 = 最新消息后)——已废弃,不参与注入
    Depth,
}

/// 楼层消息角色(自由选择)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FloorRole {
    #[default]
    System,
    User,
    Assistant,
}

impl FloorRole {
    /// LLM 消息角色字符串(system/user/assistant)
    pub fn as_str(&self) -> &'static str {
        match self {
            FloorRole::System => "system",
            FloorRole::User => "user",
            FloorRole::Assistant => "assistant",
        }
    }
}

/// 单条提示词楼层
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptFloor {
    pub id: String,
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub role: FloorRole,
    #[serde(default)]
    pub position: FloorPosition,
    /// position = depth 时的深度(0 = 最新消息后)
    #[serde(default)]
    pub depth: usize,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 拖拽排序序号(同级内按 order 升序)
    #[serde(default)]
    pub order: usize,
}

fn default_true() -> bool {
    true
}

/// 楼层 ID 生成(uuid v4,与 skill_service 一致)
pub fn new_floor_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 工具选择策略(function calling):决定模型是否/如何调用工具。
/// OpenAI 兼容语义:auto = 模型自行决定;none = 禁止调用;required = 强制调用(至少一个);
/// function(name) = 强制调用指定工具。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Function(String),
}

#[derive(Debug, Clone)]
pub struct GenerationParams {
    pub temperature: f64,
    pub top_p: f64,
    pub max_tokens: u32,
    pub stop: Option<Vec<String>>,
    /// 工具定义(function calling);空 = 不启用工具
    pub tools: Vec<ToolDefinition>,
    /// 工具循环轮次上限(None = 用引擎默认 32)。当前轮工具调用照常执行,
    /// 达到上限后不再发起新的模型请求;避免 AGENT 模式长链路任务被硬编码 8 轮截断。
    pub max_tool_rounds: Option<u32>,
    /// 工具选择策略;默认 Auto(保持原行为)。None/Required/Function 仅 tools 非空时生效。
    pub tool_choice: ToolChoice,
    /// 是否允许模型单轮返回多个工具调用(None = 使用后端默认)
    pub parallel_tool_calls: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallArgs {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// 连接器流式产出块
#[derive(Debug, Clone)]
pub enum LlmStreamChunk {
    Token(String),
    ToolCall(ToolCallArgs),
    /// 思考模式(thinking)的 reasoning_content,多轮工具调用时必须随 assistant 消息回传
    Reasoning(String),
    Usage {
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
        /// prompt 缓存命中 token(DeepSeek 等提供商;其余为 0)
        prompt_cache_hit_tokens: i64,
        /// prompt 缓存未命中 token(DeepSeek 风格原样透传;OpenAI 风格由推导得出)
        prompt_cache_miss_tokens: i64,
        /// completion 中推理消耗的 token(DeepSeek 推理模型 completion_tokens_details.
        /// reasoning_tokens;无此字段的提供商为 0)。诊断「空输出 = 推理耗尽预算」的关键证据
        reasoning_tokens: i64,
    },
    /// 流正常结束时的 finish_reason(stop/length/content_filter 等;tool_calls 不单独产出)。
    /// 任务模式据此区分「真空响应」与「max_tokens 截断」,决定是否提高上限重试
    Finish {
        reason: String,
    },
}

// ---------- SSE 事件 ----------
/// 任务事件分类(`SseEvent::Task.kind`,WP4 / 批次 4 / 批次 R4)。
/// 序列化为 snake_case,与前端手写 union `TaskEventKind`(`web/src/api/types.ts`)
/// 逐值对齐——改此处须同步前端 union 与 `tools/check-contract.mjs` 的映射表。
///
/// 用枚举而非 `String`:后端拼错分类名从前只会在前端静默失效(未知 kind 不刷新),
/// 现由编译器拦住;线格式与字符串时代逐字节一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventKind {
    /// 任务已创建
    Created,
    /// 任务状态迁移
    Status,
    /// 执行计划更新
    Plan,
    /// 子任务创建 / 状态更新
    Subtask,
    /// token 用量落库
    Usage,
    /// 任务已删除
    Deleted,
    /// LLM 调用落库(批次 3 调用追踪)
    LlmCall,
    /// 主 / 子 agent 状态迁移(批次 4)
    AgentStatus,
    /// 计划待批准(批次 4 plan 模式)
    ApprovalRequired,
    /// 流式正文增量(批次 R4;暂态事件不落库)
    Delta,
}

/// SSE 事件,serde 序列化为 {"type":"...", ...}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SseEvent {
    Token {
        text: String,
    },
    Step {
        step: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// 自定义流程(custom 模式)步骤进度;其余模式不携带
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<usize>,
    },
    ToolCall {
        name: String,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        /// 工具展示意图(render intent):前端据此分派渲染,缺省走 JSON 直出
        #[serde(skip_serializing_if = "Option::is_none")]
        render_kind: Option<String>,
    },
    /// 工具未获授权，因此没有执行；前端可据此提供会话/角色授权入口。
    ToolAuthorizationRequired {
        name: String,
        risk: crate::tools::permissions::ToolRisk,
        reason: String,
        run_id: String,
        call_id: String,
    },
    ToolResult {
        name: String,
        output: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        /// 工具展示意图(render intent):前端据此分派渲染,缺省走 JSON 直出
        #[serde(skip_serializing_if = "Option::is_none")]
        render_kind: Option<String>,
    },
    /// 酒馆助手变量树同步(每次应用 <UpdateVariable> 补丁后推送最新 stat_data)
    Vars {
        stat_data: Value,
    },
    Interrupted,
    /// 顶层生成错误终态:模型失败/上游错误时发送,取代「空内容 finish 伪装正常结束」。
    /// 前端据此展示错误而非「完成」。
    Error {
        /// 错误码(如 upstream_error / request_timeout / tool_loop_failed)
        code: String,
        message: String,
        /// 是否可重试(网络类错误可重试,鉴权/参数类不可)
        retryable: bool,
    },
    Finish {
        usage: TokenUsage,
        content: String,
        /// 上游 finish_reason(stop / length / content_filter 等;可观测性问题①)。
        /// "length" = 输出被 max_tokens 截断(推理模型常见:reasoning 吃光预算)。
        /// 任务模式早已据此落库并展示「截断」徽标,聊天路径此前完全丢弃该字段——
        /// 前端把半截回复当正常完成渲染,用户无从察觉。
        /// None = 未下发/不适用,序列化时省略(旧客户端忽略即可,线格式向后兼容)。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
    },
    /// 任务模式(task 工作台)事件(WP4):任务生命周期广播,由 TaskService 的
    /// broadcast 通道推送,GET /api/tasks/events 转发为 SSE,取代前端 1s REST 轮询。
    /// 除 task_id 外全部可选,旧客户端缺字段即忽略(向后兼容)。
    Task {
        task_id: String,
        /// 事件分类:created | status | plan | subtask | usage | llm_call | deleted |
        /// agent_status | approval_required | delta
        ///(llm_call:批次 3 调用追踪,task_llm_calls 落库成功后发射,面板据此重拉;
        ///  delta:批次 R4 流式输出,LLM 正文增量经攒批后透出,暂态事件不落库——
        ///  权威数据以 llm_call 落库行/calls 端点为准,见 events.rs emit_delta)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<TaskEventKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<TaskStatus>,
        /// 简短中文说明(供前端事件监控面板展示);kind=delta 时为攒批后的正文增量文本
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// 上游 finish_reason(仅 kind=llm_call 且成功调用携带;可观测性问题①:
        /// "length" 即 max_tokens 截断标记)。None = 不适用/未知,序列化时省略。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
        /// 调用归属阶段(批次 R4;仅 kind=delta/llm_call 携带:planner | step |
        /// summarize | summary | agent | subagent | audit | final_audit 等,
        /// 与 task_llm_calls.phase 同口径;前端流式缓冲 key 的前半)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        /// 调用归属步骤下标(0 起,与 task_llm_calls.step_index 同口径;
        /// 仅步骤类调用携带,非步骤阶段 None 省略;前端缓冲 key 的后半)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_index: Option<usize>,
    },
}

// ---------- 工具 ----------
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub character_id: String,
    /// 子智能体嵌套深度(主 Agent 为 0;子任务内再派发时 +1)。
    /// 深度守卫:子 agent 以 `agent_depth + 1` 运行,达 `subagent_max_depth` 后不再派发
    /// (见 `tools/agent_tools_agent.rs`)。
    pub agent_depth: u32,
}

/// 工具执行器签名（**L1 契约**：工具注册表与各 L3 加载器共用）。
///
/// 2026-09-14 从 `tools/registry.rs` 下沉至此。理由：`plugins/`（L3 插件加载器）与
/// `mcp/`（L3 客户端）都需要**构造**执行器来注册工具，若该类型留在 L2 的 `tools/`，
/// 两者都会构成 `L3→L2` 越代依赖。类型别名是纯契约、无实现，与
/// `ToolDefinition` / `ToolContext` 同处 L1 最合适。
pub type ToolExecutor = std::sync::Arc<
    dyn Fn(Value, ToolContext) -> futures::future::BoxFuture<'static, Result<String, String>>
        + Send
        + Sync,
>;

/// 已装载的脚本执行体（**L1 契约**）。
///
/// 2026-09-14 从 `scripts/loader.rs`（L3）下沉至此。理由：`services::script_authorization_service`
/// （L2）需要用它计算脚本内容哈希（授权门），若类型留在 L3，就构成 `L2→L3` 越代依赖
/// （规则 J 实测检出）。它是纯数据形状、无行为，与 `ToolDefinition` 同类。
///
/// 字段语义与 `scripts::loader::collect_enabled_scripts` 的产出保持一致。
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedScript {
    pub id: String,
    pub name: String,
    pub content: String,
    /// script 作用域变量（data）
    pub data: Value,
}

/// 工具注册能力的最小接口（**L1 契约**，宿主注入用）。
///
/// 2026-09-14 新增。理由：L3 的 `mcp/` 需要把发现的远端工具登记进工具表，
/// 但**不能**直接依赖 `tools::registry::ToolRegistry`（L2 设施，会构成 `L3→L2`）。
/// 故定义此窄接口，由 L2 的注册表实现、由组合根把 `&dyn ToolRegistrar` 注入给 L3。
///
/// **这是「青层能力经显式接缝注入」的标准形态**，与 `task_core::TaskBackend`
/// 断开 `task_engine→task_service` 是同一手法（见 `docs/契约-架构与数据.md` §3）。
pub trait ToolRegistrar: Send + Sync {
    /// 注册一个**外部来源**工具（插件 / MCP）。实现方须据此把参数视为不可信。
    fn register_external(
        &self,
        definition: ToolDefinition,
        execute: ToolExecutor,
        timeout: Option<std::time::Duration>,
        origin: crate::models::tool_policy::ToolOrigin,
    );

    /// 注销工具（插件删除 / MCP 服务器退出时用）；不存在时静默忽略。
    fn unregister(&self, name: &str);
}

/// 让 `Arc<T>` 可直接当 `&dyn ToolRegistrar` 使用（组合根通常持有 `Arc<ToolRegistry>`）。
///
/// 没有它，`self.mcp.start(servers, &self.tool_registry)` 会因
/// `Arc<ToolRegistry>` 未实现该 trait 而无法编译——宿主不得不先 deref 或改持裸引用，
/// 反而增加装配摩擦。转发实现保持行为零变化。
impl<T: ToolRegistrar + ?Sized> ToolRegistrar for std::sync::Arc<T> {
    fn register_external(
        &self,
        definition: ToolDefinition,
        execute: ToolExecutor,
        timeout: Option<std::time::Duration>,
        origin: crate::models::tool_policy::ToolOrigin,
    ) {
        (**self).register_external(definition, execute, timeout, origin);
    }

    fn unregister(&self, name: &str) {
        (**self).unregister(name);
    }
}

// ---------- Skill 库 ----------
/// 提示词技能(skill):read 工具按名/关键词读取,可注入上下文。
/// 渐进披露(落地项 3):system 仅注入 name+description 紧凑清单,
/// 正文按需经 read(type=skill) 读取;allowed_tools/run_as_subagent/model
/// 为调度增强预留元数据(旧库缺列由迁移补默认)。
#[derive(Debug, Clone, Serialize)]
pub struct SkillRecord {
    pub id: String,
    pub name: String,
    pub description: String,
    pub content: String,
    pub enabled: bool,
    pub created_at: String,
    /// 工具白名单(JSON 数组字符串;空数组 = 不限制)
    pub allowed_tools: String,
    /// 是否可作为子智能体技能派发(0/1;默认 false)
    pub run_as_subagent: bool,
    /// 可选模型名覆盖(空 = 用当前模型)
    pub model: String,
}

// ---------- 子智能体任务(agentgo / agentend) ----------
#[derive(Debug, Clone, Serialize)]
pub struct AgentSubtaskRecord {
    pub id: String,
    pub session_id: String,
    pub character_id: String,
    pub name: String,
    pub instruction: String,
    /// pending | running | done | error | ended
    pub status: String,
    pub result: String,
    pub error: String,
    pub created_at: String,
    pub updated_at: String,
    /// 首次进入终态(done/error/ended)的时刻;pending/running 期间为空串。
    /// 有了它,调用方不必再靠 `status` 反推「何时完成」,也不必用 `ended` 兼表中断与完成
    /// (见 docs/功能-变更史.md)。
    pub finished_at: String,
}

// ---------- 任务模式(task 工作台) ----------
/// 任务执行模式(tasks.task_mode 列,批次 4 六模式)。序列化/落盘均为 snake_case
/// 文本;旧行缺省 'legacy',行为与六模式引入前逐字节一致。语义见 docs/功能.md。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunMode {
    /// 三段式:规划 → 逐步执行 → 汇总(既有行为,默认)
    Legacy,
    /// 单主 agent 工具自循环(run_tool_loop,工具集按 task_tool_policy 编译)
    Solo,
    /// solo + 子 agent 工具化(agent_depth+1 深度守卫)
    Multi,
    /// 只规划不执行:产出计划进 planned 待批准,批准后按计划逐步骤续跑
    Plan,
    /// 多主 agent 分工(2~4 主,每主 1~4 子目标)+ 审计终审升华
    Team,
    /// 自定义流程(AgentFlowConfig 步骤序列,轻量 step 执行器)
    Custom,
}

impl TaskRunMode {
    /// 文本形态(与 serde 输出一致;DB 参数化写入与日志用)
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Solo => "solo",
            Self::Multi => "multi",
            Self::Plan => "plan",
            Self::Team => "team",
            Self::Custom => "custom",
        }
    }

    /// DB 读取容错:未知值(未来版本/异常行)记 warn 回退 Legacy,不 panic 不丢行。
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "legacy" => Self::Legacy,
            "solo" => Self::Solo,
            "multi" => Self::Multi,
            "plan" => Self::Plan,
            "team" => Self::Team,
            "custom" => Self::Custom,
            _ => {
                tracing::warn!(value = s, "未知 task_mode,回退 legacy");
                Self::Legacy
            }
        }
    }

    /// 严格解析(API 入参用):未知值返回 None,由调用方回 400「未知任务模式」;
    /// DB 读取请用 from_str_lossy(容错回退,不丢行)。
    pub fn from_str_strict(s: &str) -> Option<Self> {
        match s {
            "legacy" => Some(Self::Legacy),
            "solo" => Some(Self::Solo),
            "multi" => Some(Self::Multi),
            "plan" => Some(Self::Plan),
            "team" => Some(Self::Team),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// TaskRecord.task_mode 的 serde 缺省(旧 JSON 无此字段 = legacy)
fn task_default_run_mode() -> TaskRunMode {
    TaskRunMode::Legacy
}

/// 终态追加指令的作用模式(批次 R2b+,2026-09-10 实跑修复 F5)。
/// 序列化为 snake_case 文本;`append`(默认)为历史行为,`replace` 用新产出整体
/// 替换原 result,支持「压缩 / 重写 / 改前面」这类 append 无法表达的指令。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFollowupMode {
    /// 追加:新产出以「追加 N」段附加到 result 末尾(历史行为,默认)
    Append,
    /// 替换:新产出整体替换 result(段标「修订 N」)
    Replace,
}

impl TaskFollowupMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Append => "append",
            Self::Replace => "replace",
        }
    }

    /// 严格解析(API 入参用):缺省/空 = Append;未知值返回 None 由调用方回 400。
    /// 与 TaskRunMode 口径一致:入参严格、无容错回退。
    pub fn from_str_strict(s: &str) -> Option<Self> {
        match s {
            "append" => Some(Self::Append),
            "replace" => Some(Self::Replace),
            _ => None,
        }
    }
}

/// 任务状态(tasks.status 列)。序列化输出与 DB 落盘均为 snake_case 文本,
/// 与历史 String 形态逐字节一致(API JSON 与存量数据零变化;TS 侧同名 union 对齐)。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    /// LLM 规划拆解中(run 入口至首步执行前)
    Planning,
    Running,
    /// 计划已产出、待用户批准(plan 模式特有;批准后转 planning 续跑,放弃走 stop)
    Planned,
    Done,
    /// 部分完成(含 error 步骤但成果已产出)
    Partial,
    Error,
    /// 用户停止(stop)
    Ended,
}

impl TaskStatus {
    /// 文本形态(与 serde 输出一致;DB 参数化写入与日志用)
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Planning => "planning",
            Self::Running => "running",
            Self::Planned => "planned",
            Self::Done => "done",
            Self::Partial => "partial",
            Self::Error => "error",
            Self::Ended => "ended",
        }
    }

    /// DB 读取容错:历史/异常行的未知值记 warn 并回退 Pending,不 panic 不丢行。
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "planning" => Self::Planning,
            "running" => Self::Running,
            "planned" => Self::Planned,
            "done" => Self::Done,
            "partial" => Self::Partial,
            "error" => Self::Error,
            "ended" => Self::Ended,
            _ => {
                tracing::warn!(
                    r#type = "task_status",
                    value = s,
                    "任务状态未知值,回退 pending"
                );
                Self::Pending
            }
        }
    }
}

/// 任务计划步骤状态(tasks.plan JSON 数组内 TaskStep.status)。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStepStatus {
    Pending,
    Running,
    Done,
    Error,
}

impl TaskStepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Error => "error",
        }
    }

    /// 容错同 TaskStatus::from_str_lossy;plan JSON 反序列化经 de_step_status_lossy 走此处。
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "done" => Self::Done,
            "error" => Self::Error,
            _ => {
                tracing::warn!(
                    r#type = "task_step_status",
                    value = s,
                    "任务状态未知值,回退 pending"
                );
                Self::Pending
            }
        }
    }
}

/// 任务子任务状态(task_subtasks.status 列)。pending 无写入点,仅 stop 的防御性比较保留。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSubtaskStatus {
    Pending,
    Running,
    Done,
    Error,
    Ended,
}

impl TaskSubtaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Error => "error",
            Self::Ended => "ended",
        }
    }

    /// 是否终态(done/error/ended):终态不再被后续状态写入改写,首次进入时记 finished_at。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Error | Self::Ended)
    }

    /// 容错同 TaskStatus::from_str_lossy。
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "done" => Self::Done,
            "error" => Self::Error,
            "ended" => Self::Ended,
            _ => {
                tracing::warn!(
                    r#type = "task_subtask_status",
                    value = s,
                    "任务状态未知值,回退 pending"
                );
                Self::Pending
            }
        }
    }
}

/// TaskStep.status 容错反序列化:LLM 产出的 plan JSON 里 status 偶发未知值
/// (或旧版执行期写入的中间态),严格 derive 会让整个 plan 解析失败;
/// 先读 String 再 from_str_lossy 回退 Pending。字段缺失仍走 #[serde(default)]。
fn de_step_status_lossy<'de, D>(deserializer: D) -> Result<TaskStepStatus, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Ok(TaskStepStatus::from_str_lossy(&s))
}

/// 任务计划步骤(plan 以 JSON 数组存于 tasks.plan 列)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStep {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub goal: String,
    /// pending | running | done | error(类型化为 TaskStepStatus;未知值容错回退 Pending)
    #[serde(
        default = "task_step_default_status",
        deserialize_with = "de_step_status_lossy"
    )]
    pub status: TaskStepStatus,
    #[serde(default)]
    pub result: String,
}

fn task_step_default_status() -> TaskStepStatus {
    TaskStepStatus::Pending
}

impl Default for TaskStep {
    fn default() -> Self {
        TaskStep {
            name: String::new(),
            goal: String::new(),
            status: TaskStepStatus::Pending,
            result: String::new(),
        }
    }
}

/// 任务记录(任务工作台的一等公民)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub title: String,
    /// 状态机见 TaskStatus doc:pending | planning | running | done | partial | error | ended
    #[serde(default = "task_default_status")]
    pub status: TaskStatus,
    #[serde(default)]
    pub plan: Vec<TaskStep>,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub error: String,
    /// 执行者人设角色 id(空 = 通用执行者)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// 执行模式(批次 4;缺省 legacy,旧客户端/旧行零变化)
    #[serde(default = "task_default_run_mode")]
    pub task_mode: TaskRunMode,
}

fn task_default_status() -> TaskStatus {
    TaskStatus::Pending
}

/// 任务子任务(独立于 agent_subtasks:任务 id 非 sessions 外键,故单独建表)
#[derive(Debug, Clone, Serialize)]
pub struct TaskSubtaskRecord {
    pub id: String,
    pub task_id: String,
    pub name: String,
    pub instruction: String,
    /// pending | running | done | error | ended
    pub status: TaskSubtaskStatus,
    pub result: String,
    pub error: String,
    pub created_at: String,
    pub updated_at: String,
    /// 首次进入终态(done/error/ended)的时刻;pending/running 期间为空串。
    pub finished_at: String,
}

/// 任务 LLM 调用追踪行(task_llm_calls 表;批次 3「调用情况」面板时间线数据源)。
/// phase:planner | step | summarize | agent | subagent | audit;
/// status:ok | empty | error;摘要为截断文本(不携带全量上下文,面板可展开查看)。
/// finish_reason(可观测性问题①):上游 stop/length/content_filter 等;
/// '' = 未知/未下发(旧行默认值)。length = max_tokens 截断——修复前截断调用
/// 与正常完成同为 status=ok,面板无从区分。
#[derive(Debug, Clone, Serialize)]
pub struct TaskLlmCallRecord {
    pub id: String,
    pub task_id: String,
    pub phase: String,
    pub step_index: Option<i64>,
    pub model: String,
    pub prompt_summary: String,
    pub response_summary: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub reasoning_tokens: i64,
    pub elapsed_ms: i64,
    pub status: String,
    /// 上游 finish_reason(stop/length/...);'' = 未知/未下发(旧行兼容)
    pub finish_reason: String,
    pub created_at: String,
}

/// 任务消息(task_messages 表;批次 R2 多轮用户输入):任务全程的用户输入
///(followup 终态追加指令 / plan_chat 批准环节对话)与助手产出按行落库,
/// GET /api/tasks/{id} 详情响应的 messages 数组(created_at 升序)数据源。
/// 序列化字段 snake_case,与 TaskRecord/TaskSubtaskRecord/TaskLlmCallRecord 同风格;
/// 任务删除经外键 ON DELETE CASCADE 一并清除。
#[derive(Debug, Clone, Serialize)]
pub struct TaskMessageRecord {
    pub id: String,
    pub task_id: String,
    /// user | assistant(建表 CHECK 约束)
    pub role: String,
    /// normal | followup | plan_chat(旧行默认 normal;读取侧不做严格校验,宽容演进)
    pub kind: String,
    pub content: String,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// serde 快照(批次 R2):TaskMessageRecord 序列化键集合与线格式
    /// (snake_case,与 TaskRecord/TaskLlmCallRecord 同风格;前端 TaskMessage 类型对齐)。
    #[test]
    fn task_message_record_serde_snapshot() {
        let rec = TaskMessageRecord {
            id: "m1".into(),
            task_id: "t1".into(),
            role: "user".into(),
            kind: "followup".into(),
            content: "再补充一点".into(),
            created_at: "2026-09-03T00:00:00.000Z".into(),
        };
        let v = serde_json::to_value(&rec).unwrap();
        // serde_json 未启用 preserve_order:Map 键按字典序,断言键集合(排序后)
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["content", "created_at", "id", "kind", "role", "task_id"],
            "字段集合快照: {keys:?}"
        );
        assert_eq!(v["role"], "user");
        assert_eq!(v["kind"], "followup");
        assert_eq!(v["content"], "再补充一点");
    }

    /// serde 快照:三个状态枚举序列化输出必须与历史 String 形态(API JSON/DB 落盘)
    /// 逐字节一致;as_str 与 from_str_lossy 同表往返。
    #[test]
    fn task_status_serde_snapshot() {
        let cases = [
            (TaskStatus::Pending, "pending"),
            (TaskStatus::Planning, "planning"),
            (TaskStatus::Running, "running"),
            (TaskStatus::Done, "done"),
            (TaskStatus::Partial, "partial"),
            (TaskStatus::Error, "error"),
            (TaskStatus::Ended, "ended"),
        ];
        for (status, text) in cases {
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{text}\"")
            );
            assert_eq!(status.as_str(), text);
            assert_eq!(TaskStatus::from_str_lossy(text), status);
        }
    }

    /// serde 快照:llm_call 事件线格式(批次 3 新增 kind)。kind 为 Option<TaskEventKind>,
    /// 本测试锁定「type=task + kind=llm_call」帧形态(None 字段省略),防线格式漂移。
    #[test]
    fn sse_task_llm_call_event_wire_format() {
        let ev = SseEvent::Task {
            task_id: "t1".into(),
            kind: Some(TaskEventKind::LlmCall),
            title: None,
            status: None,
            detail: Some("step #1 · mock · 5 tokens".into()),
            finish_reason: None,
            phase: None,
            step_index: None,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "type": "task",
                "task_id": "t1",
                "kind": "llm_call",
                "detail": "step #1 · mock · 5 tokens"
            })
        );
    }

    /// serde 快照:llm_call 事件携带 finish_reason(可观测性问题①,截断标记透出)。
    /// Some 时字段出现,None 时省略(旧客户端兼容);同时锁定 TaskLlmCallRecord
    /// 的 finish_reason 字段出现在 calls 端点 JSON(旧行为 '' 空串)。
    #[test]
    fn sse_task_llm_call_event_with_finish_reason_wire_format() {
        let ev = SseEvent::Task {
            task_id: "t1".into(),
            kind: Some(TaskEventKind::LlmCall),
            title: None,
            status: None,
            detail: Some("agent · mock · 5 tokens".into()),
            finish_reason: Some("length".into()),
            phase: None,
            step_index: None,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["finish_reason"], serde_json::json!("length"));

        let rec = TaskLlmCallRecord {
            id: "c1".into(),
            task_id: "t1".into(),
            phase: "agent".into(),
            step_index: None,
            model: "m".into(),
            prompt_summary: String::new(),
            response_summary: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            reasoning_tokens: 0,
            elapsed_ms: 0,
            status: "ok".into(),
            finish_reason: String::new(),
            created_at: "c".into(),
        };
        let v = serde_json::to_value(&rec).unwrap();
        assert_eq!(
            v["finish_reason"],
            serde_json::json!(""),
            "旧行(未知)应以空串透出,不等于 stop"
        );
    }

    /// serde 快照:聊天终态 Finish 事件携带 finish_reason(可观测性问题①,2026-09-15)。
    /// Some 时字段出现,None 时省略——旧客户端忽略未知字段即可,线格式向后兼容。
    /// 锁定聊天路径不再丢弃截断信息(此前 SseEvent::Finish 只有 usage/content)。
    #[test]
    fn sse_finish_event_carries_finish_reason_wire_format() {
        let truncated = SseEvent::Finish {
            usage: TokenUsage::default(),
            content: "半截回复".into(),
            finish_reason: Some("length".into()),
        };
        let v = serde_json::to_value(&truncated).unwrap();
        assert_eq!(v["type"], serde_json::json!("finish"));
        assert_eq!(v["finish_reason"], serde_json::json!("length"));

        let normal = SseEvent::Finish {
            usage: TokenUsage::default(),
            content: "完整回复".into(),
            finish_reason: Some("stop".into()),
        };
        let v = serde_json::to_value(&normal).unwrap();
        assert_eq!(v["finish_reason"], serde_json::json!("stop"));

        // 未下发 finish_reason 时字段省略(不污染线格式,旧客户端兼容)
        let unknown = SseEvent::Finish {
            usage: TokenUsage::default(),
            content: "x".into(),
            finish_reason: None,
        };
        let v = serde_json::to_value(&unknown).unwrap();
        assert!(v.get("finish_reason").is_none(), "None 应省略字段: {v}");
    }

    /// serde 快照:delta 事件线格式(批次 R4 任务模式流式输出)。kind=delta +
    /// detail 攒批文本 + phase/step_index 调用归属标识;暂态事件不落库(纪律例外,
    /// 见 events.rs emit_delta),None 字段序列化省略,旧客户端缺字段即忽略。
    #[test]
    fn sse_task_delta_event_wire_format() {
        let ev = SseEvent::Task {
            task_id: "t1".into(),
            kind: Some(TaskEventKind::Delta),
            title: None,
            status: None,
            detail: Some("攒批后的正文增量".into()),
            finish_reason: None,
            phase: Some("step".into()),
            step_index: Some(0),
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "type": "task",
                "task_id": "t1",
                "kind": "delta",
                "detail": "攒批后的正文增量",
                "phase": "step",
                "step_index": 0
            })
        );
    }

    /// serde 快照:llm_call 事件携带 phase/step_index(批次 R4:前端据此清对应
    /// 流式缓冲,delta 暂态数据由落库行取代对齐权威)。非步骤类阶段(planner 等)
    /// step_index 为 None,序列化省略该字段。
    #[test]
    fn sse_task_llm_call_event_with_phase_wire_format() {
        let ev = SseEvent::Task {
            task_id: "t1".into(),
            kind: Some(TaskEventKind::LlmCall),
            title: None,
            status: None,
            detail: Some("step #1 · mock · 5 tokens".into()),
            finish_reason: None,
            phase: Some("step".into()),
            step_index: Some(0),
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["phase"], serde_json::json!("step"));
        assert_eq!(v["step_index"], serde_json::json!(0));

        let ev = SseEvent::Task {
            task_id: "t1".into(),
            kind: Some(TaskEventKind::LlmCall),
            title: None,
            status: None,
            detail: Some("planner · mock · 5 tokens".into()),
            finish_reason: None,
            phase: Some("planner".into()),
            step_index: None,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["phase"], serde_json::json!("planner"));
        assert!(
            v.get("step_index").is_none(),
            "step_index 为 None 时应省略: {v}"
        );
    }

    /// serde 快照:TaskRunMode 六模式枚举(批次 4 契约)。线格式 snake_case;
    /// 旧行/旧配置缺省 = legacy;DB 未知值 from_str_lossy 容错回退 Legacy 不 panic。
    #[test]
    fn task_run_mode_serde_snapshot() {
        let cases = [
            (TaskRunMode::Legacy, "legacy"),
            (TaskRunMode::Solo, "solo"),
            (TaskRunMode::Multi, "multi"),
            (TaskRunMode::Plan, "plan"),
            (TaskRunMode::Team, "team"),
            (TaskRunMode::Custom, "custom"),
        ];
        for (mode, text) in cases {
            assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{text}\""));
            assert_eq!(mode.as_str(), text);
            assert_eq!(TaskRunMode::from_str_lossy(text), mode);
        }
        assert_eq!(
            TaskRunMode::from_str_lossy("未来未知模式"),
            TaskRunMode::Legacy
        );
    }

    /// TaskStatus::Planned(plan 模式待批准态)线格式 + as_str/from_str_lossy 往返。
    #[test]
    fn task_status_planned_serde() {
        assert_eq!(
            serde_json::to_string(&TaskStatus::Planned).unwrap(),
            "\"planned\""
        );
        assert_eq!(TaskStatus::Planned.as_str(), "planned");
        assert_eq!(TaskStatus::from_str_lossy("planned"), TaskStatus::Planned);
    }

    /// TaskRecord 缺省 task_mode = legacy:旧 API 体/旧 tasks 行 JSON 反序列化零迁移。
    #[test]
    fn task_record_task_mode_defaults_legacy() {
        let t: TaskRecord = serde_json::from_value(serde_json::json!({
            "id": "t", "title": "x", "created_at": "c", "updated_at": "u"
        }))
        .unwrap();
        assert_eq!(t.task_mode, TaskRunMode::Legacy);
    }

    /// TaskStepStatus 快照(序列化/as_str/往返)
    #[test]
    fn task_step_status_serde_snapshot() {
        let cases = [
            (TaskStepStatus::Pending, "pending"),
            (TaskStepStatus::Running, "running"),
            (TaskStepStatus::Done, "done"),
            (TaskStepStatus::Error, "error"),
        ];
        for (status, text) in cases {
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{text}\"")
            );
            assert_eq!(status.as_str(), text);
            assert_eq!(TaskStepStatus::from_str_lossy(text), status);
        }
    }

    /// TaskSubtaskStatus 快照(序列化/as_str/往返)
    #[test]
    fn task_subtask_status_serde_snapshot() {
        let cases = [
            (TaskSubtaskStatus::Pending, "pending"),
            (TaskSubtaskStatus::Running, "running"),
            (TaskSubtaskStatus::Done, "done"),
            (TaskSubtaskStatus::Error, "error"),
            (TaskSubtaskStatus::Ended, "ended"),
        ];
        for (status, text) in cases {
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{text}\"")
            );
            assert_eq!(status.as_str(), text);
            assert_eq!(TaskSubtaskStatus::from_str_lossy(text), status);
        }
    }

    /// from_str_lossy 未知值回退 Pending(记 warn,不 panic)
    #[test]
    fn status_from_str_lossy_fallback() {
        assert_eq!(TaskStatus::from_str_lossy("进行中"), TaskStatus::Pending);
        assert_eq!(TaskStatus::from_str_lossy(""), TaskStatus::Pending);
        assert_eq!(
            TaskStepStatus::from_str_lossy("ended"),
            TaskStepStatus::Pending
        );
        assert_eq!(
            TaskSubtaskStatus::from_str_lossy("PARTIAL"),
            TaskSubtaskStatus::Pending
        );
    }

    /// plan JSON 反序列化容错:未知 status 不破坏整个 plan 解析(回退 Pending);
    /// 字段缺失走 serde default;合法值正常解析。
    #[test]
    fn task_step_status_deserialize_lossy() {
        let step: TaskStep =
            serde_json::from_str(r#"{"name":"一","goal":"g","status":"进行中"}"#).unwrap();
        assert_eq!(step.status, TaskStepStatus::Pending);
        let step: TaskStep = serde_json::from_str(r#"{"name":"一","goal":"g"}"#).unwrap();
        assert_eq!(step.status, TaskStepStatus::Pending);
        let step: TaskStep =
            serde_json::from_str(r#"{"name":"一","goal":"g","status":"done"}"#).unwrap();
        assert_eq!(step.status, TaskStepStatus::Done);
        // 序列化回写仍是 snake_case 文本(plan 落盘形态不变)
        let json = serde_json::to_string(&step).unwrap();
        assert!(
            json.contains(r#""status":"done""#),
            "plan JSON 形态: {json}"
        );
    }
}
