// 生成参数:模式隔离类型(Roleplay/Task 提示词、ModeSettings 覆盖层)、serde 默认值函数、
// from_config 默认构建与 for_mode 按模式覆盖合并。
use crate::config::AppConfig;
// 授权档位词汇已下沉 L1(2026-09-14):L2 直连 models,不经 tools 转发。
use crate::models::tool_policy::AuthorizationMode;
use serde::{Deserialize, Serialize};

use super::connection::DEFAULT_SEARCH_ENDPOINT;
use super::{AppMode, RuntimeSettings};

/// 角色扮演 Agent 系统提示词(类型级模式隔离,WP7 抗多模式提示词混淆):
/// RuntimeSettings 扁平字段的权威类型。serde transparent = 序列化为裸字符串,
/// settings.json 与 /api/settings JSON 线格式逐字节不变。
/// 语义:空串 = 使用内置默认角色扮演模板。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(transparent)]
pub struct RoleplayPromptConfig(pub String);

/// 任务模式 Agent 系统提示词覆盖值(类型级模式隔离,WP7)。
/// 仅出现在 task 覆盖层(`Option<TaskPromptConfig>`),三态语义与旧 Option<String> 同构:
/// 缺字段/null → None(沿用,for_mode 注入内置任务向默认词);
/// `""` → Some(空串)(显式清空,不注入);`"v"` → Some(v)(覆盖)。
/// serde transparent = 序列化为裸字符串;None 由外层 Option 的 skip_serializing_if 省略,
/// 不落 null,settings.json 与 API 线格式零变化。
/// 与 RoleplayPromptConfig 类型不同源:task 覆盖值无法被误赋给 roleplay 扁平字段,
/// 「task 不继承 roleplay 人设词」由类型系统而非注释约定保证(for_mode 是唯一转换点)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct TaskPromptConfig(pub String);

/// MCP 服务器配置**已下沉到 L1**（`crate::models::tool_policy::McpServerConfig`，2026-09-14）。
///
/// 理由：`mcp/`（L3）需要读取该配置来 spawn 子进程。若类型留在 `settings_service`（L2），
/// `mcp/` 就构成 `L3→L2` 越代依赖（规则 J 检出的 `mcp→services`）。
/// 它是被 L2 设置层与 L3 客户端共享的**配置词汇**，放 L1 最合适。此处仅重导出。
pub use crate::models::tool_policy::McpServerConfig;

/// 按模式的设置覆盖项:所有字段 `Option`,`Some` 表示覆盖共享默认,`None` 表示沿用共享值。
/// 仅覆盖生成参数与 Agent 配置;连接信息(openai_base_url/openai_api_key/model)始终共享。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModeSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u32>,
    /// task 覆盖层的 Agent 系统提示词(类型化为 TaskPromptConfig,与 roleplay 扁平值类型隔离)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_system_prompt: Option<TaskPromptConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mvu_vars_position: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflect_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_tail_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_tail_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflect_advice_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflect_advice_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bypass_mode: Option<bool>,
    /// task 覆盖层的授权模式(三档);None 沿用扁平值
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_mode: Option<AuthorizationMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bypass_blacklist: Option<Vec<String>>,
    /// task 覆盖层的授权等待超时(秒)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_authorization_timeout_secs: Option<u32>,
    /// task 覆盖层的任务工具策略
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_tool_policy: Option<String>,
    /// task 覆盖层的任务工具白名单
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_tool_allowlist: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_html: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_threshold: Option<f32>,
    /// 压缩后保留的最近消息条数(缓存感知管线,默认 4)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_keep_recent: Option<u32>,
    /// snip 零成本裁剪的消息长度阈值(字节;0 = 禁用 snip,默认 8192)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_snip_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_request_log: Option<bool>,
    /// 跨会话记忆蒸馏开关(落地项 2;默认关闭)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_distill_enabled: Option<bool>,
    /// 记忆槽注入条数上限(0 = 关闭注入;默认 8,钳 0..=50)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_inject_limit: Option<u32>,
    /// 记忆槽字符预算(通道 1+2 合计,默认 2000;0 = 不限制,钳 0..=20000)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_inject_char_budget: Option<u32>,
    /// 每角色记忆容量上限(默认 200;0 = 不淘汰,钳 0..=10000)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_max_entries: Option<u32>,
    /// 技能渐进披露开关(落地项 3;默认 true)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_progressive_disclosure: Option<bool>,
    /// 回退快照开关(批次 6.1;默认 true):写工具执行前留逆操作快照,可「回退到此处」
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_enabled: Option<bool>,
    /// 子智能体最大嵌套深度(默认 2,钳 1..=4;主 Agent 为第 0 层)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_max_depth: Option<u32>,
    /// 子智能体并发上限(默认 6,钳 1..=16;顺序执行下为在飞计数守卫)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_max_concurrency: Option<u32>,
    /// 子智能体结果最大字符数(默认 2000,钳 500..=8000;超出截断并附尾注)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_result_max_chars: Option<u32>,
    /// MCP stdio 客户端总开关(批次 6.2;默认关,仅启动时装配,改后重启生效)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_enabled: Option<bool>,
    /// MCP 服务器列表(覆盖语义与 bypass_blacklist 一致)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<McpServerConfig>>,
    /// 执行者人设完整开关(R3a):None/false = 精简(description+personality 两段),
    /// true = 完整(再加 scenario+mes_example)。仅任务模式人设拼装消费;
    /// roleplay 引擎侧无人设注入点,扁平值仅作 task 覆盖层 None 时的沿用值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_persona_full: Option<bool>,
    /// 任务模式是否继承提示词注入(2026-09-10 实测修复):None 沿用扁平值(默认 false = 隔离),
    /// true = 任务侧注入 prompt_floors.json(旧行为)。仅任务模式 system 拼装消费。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_prompt_inject_enabled: Option<bool>,
}

/// 默认工具循环轮次上限
pub(super) fn default_max_tool_rounds() -> u32 {
    32
}

/// 默认工具循环历史保留轮数(R3b):最近 4 轮完整,更早轮摘要化
pub(super) fn default_tool_history_keep_rounds() -> u32 {
    4
}

/// 默认工具循环历史 token 预算(R3b):16K 估算 token,覆盖主流模型单轮工具结果规模;
/// 实测膨胀形态(3 步 26K+)在此预算内被收敛到最近几轮
pub(super) fn default_tool_history_budget_tokens() -> u32 {
    16_384
}

/// 默认放行模式黑名单
/// 授权模式默认值:新装为宽松(读写放行、仅删除需授权)。
/// 旧配置的迁移见 `migrate_authorization_mode`(按旧 bypass_mode 修正)。
pub(super) fn default_authorization_mode() -> AuthorizationMode {
    AuthorizationMode::Loose
}

/// 授权等待超时默认 300 秒(与旧硬编码一致,行为不变);钳制 30..=1800。
pub(super) fn default_tool_authorization_timeout_secs() -> u32 {
    300
}

/// 任务模式工具策略默认:拒绝危险工具(无人值守任务不默认放行写类操作)。
pub(super) fn default_task_tool_policy() -> String {
    "deny_dangerous".to_string()
}

/// 「始终需授权」清单默认值:空。
/// 原默认值 delete_file/format_disk/modify_system/registry_write 都不是真实注册工具名,
/// 形同虚设;现重定义为「始终需授权」的用户可选清单,默认空以保证放行模式语义
/// (放行模式下除系统路径写/删外一律放行)。需要的用户可手动钉死高危工具。
pub(super) fn default_bypass_blacklist() -> Vec<String> {
    Vec::new()
}

/// 旧配置迁移:旧版只有 bypass_mode 布尔值,映射到三档模式。
/// - bypass_mode=true  → Bypass(旧行为:除黑名单外全放行)
/// - bypass_mode=false → Strict(旧行为:非安全工具都需授权,Strict 最贴近)
///
/// 仅当配置中确实出现旧字段时调用,避免把「新装默认 loose」误改。
pub(super) fn migrate_authorization_mode(legacy_bypass_mode: bool) -> AuthorizationMode {
    if legacy_bypass_mode {
        AuthorizationMode::Bypass
    } else {
        AuthorizationMode::Strict
    }
}

/// 默认尾部注入角色(user;旧配置缺省保持 user 行为)
pub(super) fn default_user_role() -> String {
    "user".to_string()
}

/// 默认上下文压缩模式(off = 不压缩,保持既有行为)
pub(super) fn default_compaction_mode() -> String {
    "off".to_string()
}

/// 默认上下文压缩触发阈值(0.8 = 历史 token 达上下文窗口 80% 时自动压缩)
pub(super) fn default_compaction_threshold() -> f32 {
    0.8
}

/// 默认压缩后保留的最近消息条数(缓存感知管线;沿用原 KEEP_RECENT_MESSAGES 常量值)
pub(super) fn default_compaction_keep_recent() -> u32 {
    4
}

/// 默认 snip 零成本裁剪消息长度阈值(8KB)
pub(super) fn default_compaction_snip_bytes() -> u32 {
    8192
}

/// 默认记忆槽注入条数上限(落地项 2)
pub(super) fn default_memory_inject_limit() -> u32 {
    8
}

/// 默认记忆槽字符预算(升级工作流 B2)
pub(super) fn default_memory_inject_char_budget() -> u32 {
    2000
}

/// 默认每角色记忆容量上限(升级工作流 B3)
pub(super) fn default_memory_max_entries() -> u32 {
    200
}

/// 默认开启技能渐进披露(落地项 3)
pub(super) fn default_skill_progressive_disclosure() -> bool {
    true
}

/// 默认开启回退快照(批次 6.1)
pub(super) fn default_undo_enabled() -> bool {
    true
}

/// 默认子智能体最大嵌套深度(落地项 3)
pub(super) fn default_subagent_max_depth() -> u32 {
    2
}

/// 默认子智能体并发上限(落地项 3)
pub(super) fn default_subagent_max_concurrency() -> u32 {
    6
}

/// 默认子智能体结果最大字符数(落地项 3)
pub(super) fn default_subagent_result_max_chars() -> u32 {
    2000
}

// MCP 服务器条目默认启用的辅助函数已随 `McpServerConfig` 下沉到 L1
// （`models/tool_policy.rs` 的私有 `default_mcp_server_enabled`，2026-09-14）。
// 此处不再保留副本——否则 serde 默认值会与 L1 定义分叉。

/// 任务模式缺省 Agent 系统提示词(任务向,与 task_service 执行者指令互补)。
/// 仅 task 覆盖层未显式设置(None)时使用;显式清空(Some(""))表示不注入。
///
/// 定位:这是「用户可编辑」的任务向系统提示词,注入执行者/汇总者的 system
/// (经 untrusted 边界包裹);规划器不注入。与固定的规划器/执行者/汇总者内置指令
/// (task_service/prompt.rs)互补——内置指令管「这一步产出什么形态」,本提示词管
/// 「以什么姿态、按什么标准做事」,故此处不重复 JSON 契约等步骤级形态要求。
///
/// 约束(有测试锁,改文本时勿破坏):
/// - 必须保留「任务执行智能体」字样(server-rs/tests/tasks.rs 断言缺省已注入默认词);
/// - 不得出现 `{{char}}` 等角色扮演宏(settings_service 断言:task 默认词不得继承人设词)。
///
/// 可用占位符:`{{user}}`(用户)、`{{lastUserMessage}}`(本轮任务目标,经 render_agent_prompt)。
pub fn default_task_agent_prompt() -> String {
    "你是高效的任务执行智能体,直接、准确地完成用户给出的目标。\n\
     \n\
     【工作方式】\n\
     1. 先准确理解目标:明确交付物是什么、面向谁、有无格式/字数/范围约束。目标含糊时按最合理解读推进,并在结果开头用一句话说明所采用的假设,不要因小歧义停下来反问。\n\
     2. 需要事实、资料或现状时先用工具查证(读取文件、搜索、查询记忆),不要凭印象编造;工具结果与记忆冲突时以工具结果为准。\n\
     3. 能一次做对就不拆步;确需多步时先在心里定好顺序再动手,不要边做边改方向。\n\
     4. 完成后自查一遍:目标是否逐条覆盖、有无事实或逻辑错误、有无被截断或遗漏。\n\
     \n\
     【质量标准】\n\
     1. 结果必须完整可用:宁可范围收窄后做扎实,也不要面面俱到却处处空洞。\n\
     2. 事实、数字、引用要可靠;不确定的信息要标注「不确定」或说明依据,不得编造来源、数据与结论。\n\
     3. 输出语言跟随用户目标;专有名词、代码、路径、命令保留原文。\n\
     \n\
     【输出纪律】\n\
     1. 只输出结果本身:不复述指令、不解释执行过程、不模拟对话、不以角色扮演口吻写作。\n\
     2. 不使用「以下是」「综上所述」这类元文本开场或收尾。\n\
     3. 除非用户明确要求,不使用 Markdown 标题;列表与代码块按内容需要正常使用。"
        .into()
}

/// 角色扮演 Agent 系统提示词的内置默认(新装/首次运行、数据目录尚无 settings.json 时兜底)。
///
/// 存在意义:此前引擎侧只在 agents/engine/messages/build.rs 有一份措辞较早的兜底模板,
/// 而设置层 `from_config` 与 serde 默认给的是空串——全新安装(含 Android 首装)的面板与
/// /api/settings 恒显示为空,与 Win 端用户已调优并保存的版本措辞不一致。内置后两端开箱一致;
/// 用户仍可在设置里覆盖(保存即写入 settings.json 扁平字段,文件值优先于本默认)。
///
/// 文本与 Win 端正用版逐字一致;`{{char}}` / `{{personality}}` / `{{scenario}}` / `{{world_info}}`
/// 由引擎宏系统按角色展开,故本文本**必须保留**这些占位符(与 `default_task_agent_prompt` 相反,
/// 后者是任务向默认词,明确不得含角色扮演宏)。
pub fn default_roleplay_agent_prompt() -> String {
    r#"你是 {{char}} 的扮演者与文学创作者,与用户进行沉浸式角色扮演 / 文学创作。
背景设定:{{personality}}
{{scenario}}
世界书(当前已命中的设定,必须严格遵循,冲突时以世界书为准):
{{world_info}}

【创作总纲】
本会话定位为文学创作系统:你的正文是小说文本而非聊天记录,以"能否被称为一段好小说"为最低验收标准。描写应当可朗读、可回味、经得起推敲。

【角色扮演规则】
1. 视角与口吻:严格以提示词要求的视角和口吻输出,不要出现旁白标题、「以上是回复」「作为AI」等元文本;角色设定与世界观保持一致。
2. 用户指令优先:用户提出的字数、风格、视角、情节走向要求必须服从;用户提问必须在本轮正面回答,不允许回避。
3. 不转述:用户输入的动作与对话视为已经发生,不得复述或引用,直接从其后无缝衔接继续创作新的剧情。
4. 主角与独立角色:用户所操控角色是剧情主角,但所有角色都是独立的人,有自己的价值观、思考方式与喜好,不会无条件依附用户,好感度不会因小事凭空上涨或下降。
5. 推进节奏:一次输出不得把当前事件直接推进至末尾,应当适当拆分事件、合理安排节奏,在正文末尾为用户留下可互动的窗口。

【写作要求】
1. 句式:长短句合理搭配;段落之间空一行;适当插入短段,不得连续堆叠对话;各段长度合理交错。
2. 描写:区分周围描写(环境)与聚焦描写(重点物),合理穿插;相同环境描写不得重复提及;拒绝"自然式"滥用与过度精确化的机械描写;比喻须贴切,不把物比作与其无关的事物。
3. 对话:口语化,话题连贯不重复;所有说出口的对话必须用中文双引号"..."包裹。
4. 规避:禁止先否后肯句式(不是……而是……)、动物比喻、元评论(用括号解释正文)、夸张化描写;非必要不描写回忆,尤其不得复述与上文类似的回忆。
5. 开头结尾:每次输出的开头与结尾都应当新颖,不得与上一次输出类似或重复,不得强行升华。
6. 不得以任何方式描述任何角色的具体年龄。
7. 角色对话必须符合其性格与对话示例,禁止刻板印象化、指导式、刻薄式、油腻式语言;该爆发时爆发,该平静时平静。
8. 逻辑一致:严格遵守时间/对话/行为/变量/剧情的逻辑;角色不得知晓不该知道的设定信息,不得出现"根据XX的设定"这类作者视角发言。

【剧情结构(模块化)】
每次输出将剧情分为三个剧情模块和一个结尾模块,模块之间以过渡段承转;模块类型在【环境/推进/插入】中选择,同类型不得连续出现三次;模块与结尾的格式、内容不得与上一次输出相似或雷同;禁止在正文中对模块或过渡部分做任何标注。
【输出纪律】
一次只输出角色回应本身;若需要分段,使用空行,不使用 Markdown 标题。"#
        .into()
}

/// 角色扮演模式缺省反思提示词(deep / agent 模式的反思步骤用;空 = 走机械规则)。
///
/// 与 Win 端调好的版本一致,使全新安装(含 Android 首装)开箱即有 LLM 质量判定,
/// 而非仅靠机械规则(非空/非截断/无提问即通过)。用户可在「Agent 设置 › 反思提示词」
/// 覆盖;显式清空即回到机械规则。
///
/// 代价提示:非空时反思步骤会额外调用一次 LLM(判定失败可重试,存在有界次数上限)。
/// 文本内不使用 `{{char}}` 等占位符(反思提示词不由宏系统展开),角色信息只以中性词指代。
pub fn default_reflect_prompt() -> String {
    r#"你是 Kedai 的草稿质量检查员。你会收到 [用户输入] 与 [模型草稿] 两部分,请只判定草稿是否合格,不要修改或重写草稿。

按以下标准逐项检查:
1. 需求符合度:草稿是否服从用户提出的字数(如1200字)、风格、视角、情节走向要求;用户提问是否被正面回答,未回答即不合格。
2. 人设与世界观:是否与角色设定、世界书内容一致,有无明显冲突或臆造事实。
3. 输出纪律:是否遵循提示词要求的视角(用户指定视角时按要求叙述,未指定时默认角色视角);有无旁白标题、「以上是回复」「作为AI」等元文本;有无除 <UpdateVariable> 外的自定义 XML/HTML 标签;是否包含反思或推理过程本身。
4. 完整性:草稿是否被截断(结尾半句、段落不完整);若上下文存在「当前状态」且状态发生变化,是否已输出格式正确的 <UpdateVariable> 块(<UpdateVariable><JSONPatch>[JSON 数组]</JSONPatch></UpdateVariable>);状态无变化时是否画蛇添足输出空块。
5. 语言质量:有无明显语病、错别字、前后矛盾。

判定输出(严格遵守):
- 不要让正文出现草稿。
- 全部达标:第一行输出 PASS 或「通过」。
- 任一不达标:第一行输出 FAIL 或「不通过」,第二行起用一句话指出主要问题(如:字数不足 / 未答疑问 / 视角漂移 / 结尾截断 / 状态未更新 / 非法标签 / 人设冲突),供修订参考。
- 第一行只能是 PASS / 通过 / FAIL / 不通过 之一,否则视为无效判定。"#
        .into()
}

impl RuntimeSettings {
    /// 从环境配置构建默认设置
    pub fn from_config(cfg: &AppConfig) -> Self {
        RuntimeSettings {
            openai_base_url: cfg.openai_base_url.clone(),
            openai_api_key: cfg.openai_api_key.clone(),
            model: cfg.openai_model.clone(),
            default_temperature: cfg.default_temperature,
            default_top_p: cfg.default_top_p,
            default_max_tokens: cfg.default_max_tokens,
            max_context_tokens: cfg.default_max_context_tokens,
            agent_system_prompt: RoleplayPromptConfig(default_roleplay_agent_prompt()),
            search_endpoint: DEFAULT_SEARCH_ENDPOINT.to_string(),
            mvu_vars_position: "system".to_string(),
            mvu_temperature: None,
            mvu_model: None,
            reflect_prompt: default_reflect_prompt(),
            preset_tail_prompt: String::new(),
            preset_tail_role: "user".to_string(),
            reflect_advice_prompt: String::new(),
            reflect_advice_role: "user".to_string(),
            bypass_mode: false,
            authorization_mode: default_authorization_mode(),
            bypass_blacklist: default_bypass_blacklist(),
            tool_authorization_timeout_secs: default_tool_authorization_timeout_secs(),
            task_tool_policy: default_task_tool_policy(),
            task_tool_allowlist: Vec::new(),
            max_tool_rounds: default_max_tool_rounds(),
            tool_history_keep_rounds: default_tool_history_keep_rounds(),
            tool_history_budget_tokens: default_tool_history_budget_tokens(),
            render_html: false,
            compaction_mode: default_compaction_mode(),
            compaction_threshold: default_compaction_threshold(),
            compaction_keep_recent: default_compaction_keep_recent(),
            compaction_snip_bytes: default_compaction_snip_bytes(),
            llm_request_log: false,
            memory_distill_enabled: false,
            memory_inject_limit: default_memory_inject_limit(),
            memory_inject_char_budget: default_memory_inject_char_budget(),
            memory_max_entries: default_memory_max_entries(),
            embedding_enabled: false,
            embedding_base_url: String::new(),
            embedding_api_key: String::new(),
            embedding_model: String::new(),
            embedding_dim: 0,
            skill_progressive_disclosure: default_skill_progressive_disclosure(),
            undo_enabled: default_undo_enabled(),
            subagent_max_depth: default_subagent_max_depth(),
            subagent_max_concurrency: default_subagent_max_concurrency(),
            subagent_result_max_chars: default_subagent_result_max_chars(),
            mcp_enabled: false,
            mcp_servers: Vec::new(),
            // 命令执行(阶段 E):全部默认关闭,须用户显式开启(合规:root 能力不进 Play)
            exec_enabled: false,
            exec_allow_root: false,
            exec_allow_shizuku: false,
            exec_allow_sandbox: false,
            task_persona_full: false,
            // 默认隔离:任务模式不继承 prompt_floors.json 注入(2026-09-10 实测修复)
            task_prompt_inject_enabled: false,
            task: ModeSettings::default(),
        }
    }

    /// 返回指定模式的合并后有效设置。扁平字段即 roleplay 权威值(引擎直接读);
    /// task 模式则把 task 覆盖层(Some)替换到扁平字段上,None 沿用扁平值。
    /// 旧 settings.json 无覆盖层时 task 返回扁平值,行为与改造前一致。
    pub fn for_mode(&self, mode: AppMode) -> RuntimeSettings {
        let ov = match mode {
            AppMode::Roleplay => return self.clone(),
            AppMode::Task => &self.task,
        };
        let mut out = self.clone();
        if let Some(v) = ov.default_temperature {
            out.default_temperature = v;
        }
        if let Some(v) = ov.default_top_p {
            out.default_top_p = v;
        }
        if let Some(v) = ov.default_max_tokens {
            out.default_max_tokens = v;
        }
        if let Some(v) = ov.max_context_tokens {
            out.max_context_tokens = v;
        }
        // agent_system_prompt 不回退扁平值:扁平值(RoleplayPromptConfig)多为角色扮演人设词,
        // 直接继承会污染任务执行;None 注入内置任务向默认词,Some("") 尊重用户显式留空。
        // TaskPromptConfig → RoleplayPromptConfig 的显式构造是本隔离的唯一转换点(类型不同源,
        // 绕过本 match 的隐式继承无法通过编译)。
        out.agent_system_prompt = match &ov.agent_system_prompt {
            Some(v) => RoleplayPromptConfig(v.0.clone()),
            None => RoleplayPromptConfig(default_task_agent_prompt()),
        };
        if let Some(v) = &ov.search_endpoint {
            out.search_endpoint = v.clone();
        }
        if let Some(v) = &ov.mvu_vars_position {
            out.mvu_vars_position = v.clone();
        }
        if let Some(v) = &ov.reflect_prompt {
            out.reflect_prompt = v.clone();
        }
        if let Some(v) = &ov.preset_tail_prompt {
            out.preset_tail_prompt = v.clone();
        }
        if let Some(v) = &ov.preset_tail_role {
            out.preset_tail_role = v.clone();
        }
        if let Some(v) = &ov.reflect_advice_prompt {
            out.reflect_advice_prompt = v.clone();
        }
        if let Some(v) = &ov.reflect_advice_role {
            out.reflect_advice_role = v.clone();
        }
        if let Some(v) = ov.bypass_mode {
            out.bypass_mode = v;
        }
        if let Some(v) = ov.authorization_mode {
            out.authorization_mode = v;
        }
        if let Some(v) = &ov.bypass_blacklist {
            out.bypass_blacklist = v.clone();
        }
        if let Some(v) = ov.tool_authorization_timeout_secs {
            out.tool_authorization_timeout_secs = v;
        }
        if let Some(v) = &ov.task_tool_policy {
            out.task_tool_policy = v.clone();
        }
        if let Some(v) = &ov.task_tool_allowlist {
            out.task_tool_allowlist = v.clone();
        }
        if let Some(v) = ov.max_tool_rounds {
            out.max_tool_rounds = v;
        }
        if let Some(v) = ov.render_html {
            out.render_html = v;
        }
        if let Some(v) = &ov.compaction_mode {
            out.compaction_mode = v.clone();
        }
        if let Some(v) = ov.compaction_threshold {
            out.compaction_threshold = v;
        }
        if let Some(v) = ov.compaction_keep_recent {
            out.compaction_keep_recent = v;
        }
        if let Some(v) = ov.compaction_snip_bytes {
            out.compaction_snip_bytes = v;
        }
        if let Some(v) = ov.llm_request_log {
            out.llm_request_log = v;
        }
        if let Some(v) = ov.memory_distill_enabled {
            out.memory_distill_enabled = v;
        }
        if let Some(v) = ov.memory_inject_limit {
            out.memory_inject_limit = v;
        }
        if let Some(v) = ov.memory_inject_char_budget {
            out.memory_inject_char_budget = v;
        }
        if let Some(v) = ov.memory_max_entries {
            out.memory_max_entries = v;
        }
        if let Some(v) = ov.skill_progressive_disclosure {
            out.skill_progressive_disclosure = v;
        }
        if let Some(v) = ov.undo_enabled {
            out.undo_enabled = v;
        }
        if let Some(v) = ov.subagent_max_depth {
            out.subagent_max_depth = v;
        }
        if let Some(v) = ov.subagent_max_concurrency {
            out.subagent_max_concurrency = v;
        }
        if let Some(v) = ov.subagent_result_max_chars {
            out.subagent_result_max_chars = v;
        }
        if let Some(v) = ov.mcp_enabled {
            out.mcp_enabled = v;
        }
        if let Some(v) = &ov.mcp_servers {
            out.mcp_servers = v.clone();
        }
        if let Some(v) = ov.task_persona_full {
            out.task_persona_full = v;
        }
        if let Some(v) = ov.task_prompt_inject_enabled {
            out.task_prompt_inject_enabled = v;
        }
        out
    }
}
