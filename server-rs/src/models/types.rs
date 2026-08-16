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
    /// direct | tool | reflect
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
    },
}

// ---------- SSE 事件 ----------
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
}

// ---------- Skill 库 ----------
/// 提示词技能(skill):read 工具按名/关键词读取,可注入上下文
#[derive(Debug, Clone, Serialize)]
pub struct SkillRecord {
    pub id: String,
    pub name: String,
    pub description: String,
    pub content: String,
    pub enabled: bool,
    pub created_at: String,
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
}
