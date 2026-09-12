// API 类型定义(与后端契约一致;各域 API 模块共用)

export interface CharacterRecord {
  id: string;
  name: string;
  chara_name: string;
  description: string;
  file_path: string;
  avatar_path: string | null;
  data_raw?: Record<string, unknown>;
  /** 开场白(first_mes) */
  first_mes?: string;
  /** 备用开场列表(alternate_greetings;与 first_mes 组成多开场) */
  alternate_greetings?: string[];
  /** 角色卡正则脚本(用于消息 HTML 渲染) */
  regex_scripts?: RegexScript[];
  /** 角色卡内嵌插件检测结果(酒馆助手等;仅详情接口返回) */
  card_plugins?: CardPluginInfo[];
  /** 卡元数据(远程资源页 TavernHelper shim 口令推导用;列表/详情均携带) */
  creator?: string;
  character_version?: string;
  creator_notes?: string;
  created_at: string;
}

/** 角色卡内嵌插件能力点 */
export interface PluginFeature {
  id: string;
  label: string;
  detected: boolean;
}

/** 角色卡内嵌插件(如酒馆助手 SillyTavern-Assistant) */
export interface CardPluginInfo {
  id: string;
  name: string;
  name_en: string;
  /** 检测到即视为启用(kedai 内置实现,无需安装) */
  enabled: boolean;
  /** 来源:character_card(角色卡内嵌) */
  source: string;
  description: string;
  features: PluginFeature[];
}

/** 角色卡正则脚本(extensions.regex_scripts) */
export interface RegexScript {
  id: string;
  script_name: string;
  find_regex: string;
  replace_string: string;
  markdown_only: boolean;
  enabled: boolean;
  /** 酒馆楼层深度下限:仅对深度 >= min_depth 的消息生效(0 = 最新一条);缺省不限 */
  min_depth?: number | null;
  /** 酒馆楼层深度上限:仅对深度 <= max_depth 的消息生效;缺省不限 */
  max_depth?: number | null;
}

export interface SessionInfo {
  id: string;
  character_id: string;
  title: string;
  created_at: string;
  updated_at: string;
}

/** 会话 + 角色名 + 消息数(聊天记录面板) */
export interface SessionWithCharacter extends SessionInfo {
  character_name?: string | null;
  message_count?: number;
}

export interface ChatMessage {
  id: number;
  session_id: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  /** 服务端宏展开后的显示文本({{char}}/{{getvar}} 等);缺省时前端回退 content */
  content_display?: string;
  extra: Record<string, unknown>;
  created_at: string;
}

/** 任务事件分类(WP4 后端推送):创建 / 状态变化 / 计划 / 子任务 / token 累计 / 删除 / LLM 调用落库
 *  批次 4 六模式追加:agent_status(主/子 agent 状态迁移)/ approval_required(plan 模式计划待批准)
 *  批次 R4 流式输出追加:delta(LLM 正文攒批增量,暂态不落库;权威数据以 llm_call 落库行为准) */
export type TaskEventKind = 'created' | 'status' | 'plan' | 'subtask' | 'usage' | 'deleted' | 'llm_call' | 'agent_status' | 'approval_required' | 'delta';

/**
 * 任务模式(task 工作台)事件(对齐 server-rs SseEvent::Task):
 * WP5 起由 GET /api/tasks/events 真实 SSE 推送,取代前端 1s REST 轮询与本地合成伪事件。
 * 除 task_id 外全部可选;kind 缺失(旧服务端/未知分类)时仅透传事件监控面板,不触发刷新。
 */
export type TaskEvent = {
  type: 'task';
  task_id: string;
  kind?: TaskEventKind;
  title?: string;
  /** 任务状态(snake_case,见 TaskStatus) */
  status?: string;
  /** 简短中文说明(事件监控面板展示用);kind=delta 时为攒批后的正文增量文本 */
  detail?: string;
  /** 上游 finish_reason(仅 kind=llm_call 且成功调用携带,可观测性问题①截断标记;
   *  'length' = max_tokens 截断;缺省 = 不适用/未知,旧客户端直接忽略) */
  finish_reason?: string;
  /** 调用归属阶段(批次 R4;仅 kind=delta/llm_call 携带,与 task_llm_calls.phase 同口径;
   *  流式缓冲 key 前半:`${phase}:${step_index ?? ''}`) */
  phase?: string;
  /** 调用归属步骤下标(0 起;仅步骤类调用携带;流式缓冲 key 后半) */
  step_index?: number;
};

export type SseEvent =
  | { type: 'token'; text: string }
  | { type: 'step'; step: string; detail?: string; index?: number; total?: number }
  | { type: 'tool_call'; name: string; input: unknown; call_id?: string; render_kind?: string }
  | { type: 'tool_authorization_required'; name: string; risk: ToolRisk; reason: string; run_id: string; call_id: string }
  | { type: 'tool_result'; name: string; output: unknown; call_id?: string; render_kind?: string }
  | { type: 'vars'; stat_data: Record<string, unknown> }
  | { type: 'interrupted' }
  | { type: 'error'; code: string; message: string; retryable: boolean }
  | { type: 'finish'; usage: TokenUsage; content: string }
  | TaskEvent;

export type ToolRisk = 'safe' | 'sensitive' | 'dangerous';

/** 授权模式三档(与后端 AuthorizationMode 一致)。
 *  strict:读/写/删文件都需授权;
 *  loose:读/写文件放行,删文件需授权(默认);
 *  bypass:除「系统路径(C 盘)写/删」外一律放行。 */
export type AuthorizationMode = 'strict' | 'loose' | 'bypass';

export interface ToolPermission {
  name: string;
  description: string;
  risk: ToolRisk;
  allowed: boolean;
  reason: string;
}

export interface TokenUsage {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  context_tokens: number;
  /** prompt 缓存命中 token(DeepSeek 等提供商;无缓存字段时为 0),用于计算当前命中率 */
  prompt_cache_hit_tokens: number;
  /** prompt 缓存未命中 token(命中 + 未命中 = prompt_tokens) */
  prompt_cache_miss_tokens: number;
}

export interface StMessage {
  id?: number;
  role: 'user' | 'assistant' | 'system';
  content: string;
  extra?: Record<string, unknown>;
}

/** mvu 变量快照(消息级) */
export interface MvuSnapshot {
  stat_data?: Record<string, unknown>;
  display_data?: Record<string, unknown>;
}

/** 消息记录(含 extra,可含 mvu 快照) */
export interface MessageRecord {
  id: number;
  session_id: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  extra?: Record<string, unknown>;
  created_at: string;
}

/** 世界书上传时的自动转换统计(后端返回;未发生转换时为 null/缺失) */
export interface WorldBookConversion {
  /** 处理的条目总数 */
  total_count: number;
  /** 实际发生转换的条目数 */
  converted_count: number;
  /** 缺失 constant 时自动判定为常态(常驻)的条数 */
  constant_auto_count: number;
  /** 关键词经字段变体/逗号拆分归一的条数 */
  key_normalized_count: number;
}

/** 世界书记录(列表/详情) */
export interface WorldBookRecord {
  id: string;
  name: string;
  /** 绑定角色 id;缺省 = 全局 */
  character_id?: string | null;
  character_name?: string | null;
  enabled: boolean;
  /** upload=独立上传;character_card=角色卡内嵌 */
  source: string;
  entry_count: number;
  data_raw?: Record<string, unknown>;
  /** 上传时的自动转换统计(仅上传响应返回) */
  conversion?: WorldBookConversion | null;
  created_at: string;
}

/** 快速回复记录(阶段四 4b;后端 QuickReplyRecord 契约) */
export interface QuickReplyRecord {
  id: number;
  name: string;
  label: string;
  content: string;
  enabled: boolean;
  /** 注入位置权重(0-4,对齐酒馆 injection position;当前仅用于排序) */
  position: number;
  /** 组内排序(升序) */
  sort_order: number;
  created_at?: string;
  updated_at?: string;
}

/** slash 命令元信息(阶段四 4a;后端 GET /api/slash/commands) */
export interface SlashCommandMeta {
  name: string;
  description: string;
  params: string;
}

/** 世界书条目(预览/编辑;可与后端互写 data_raw) */
export interface WorldBookEntry {
  id: number;
  comment: string;
  /** 触发关键词(子串匹配) */
  keys: string[];
  /** 副关键词(酒馆 keysecondary) */
  keys_secondary?: string[];
  regex?: string | null;
  /** 是否用正则匹配(优先于关键词) */
  use_regex?: boolean;
  /** 常驻注入(酒馆「全局」) */
  constant: boolean;
  /** 激活状态(总开关:激活才参与注入) */
  enabled: boolean;
  content: string;
  /** 注入位置权重(0-4,酒馆 position) */
  position?: number;
  /** 扫描深度(酒馆 depth:从最新消息往前扫 N 条,0=全部) */
  depth?: number;
  /** 注入顺序(酒馆 order) */
  order?: number;
  /** 关键词大小写敏感 */
  case_sensitive?: boolean;
  /** 粘性(命中后接下来 N 条持续注入) */
  sticky?: number;
  /** 冷却(命中后 N 条内不再次触发) */
  cooldown?: number;
  /** 命中概率%(酒馆 probability) */
  probability?: number;
  /** 是否启用概率 */
  use_probability?: boolean;
  /** 注入消息角色(system / user / assistant;缺省:常驻→system,激发→user) */
  role?: 'system' | 'user' | 'assistant' | null;
}

// ---- 用户脚本(ScriptTree,阶段三;对齐 JS-Slash-Runner「酒馆助手」存储格式) ----

/** 脚本按钮(脚本侧可见的自定义按钮) */
export interface ScriptButton {
  name: string;
  visible?: boolean;
}

/** 单个脚本:可执行体,存于角色卡 extensions.tavern_helper 或全局脚本表 */
export interface ScriptNode {
  type: 'script';
  enabled: boolean;
  name: string;
  /** uuid */
  id: string;
  /** 脚本正文 */
  content: string;
  /** 脚本说明 */
  info?: string;
  button?: {
    enabled: boolean;
    buttons: ScriptButton[];
  };
  /** script 作用域变量(阶段三执行层读取) */
  data?: Record<string, unknown>;
  /** 随卡导出开关(酒馆 export_with) */
  export_with?: {
    data: boolean;
    button: boolean;
  };
}

/** 文件夹:可嵌套一层 script 节点 */
export interface ScriptFolder {
  type: 'folder';
  enabled: boolean;
  name: string;
  /** uuid */
  id: string;
  icon?: string;
  color?: string;
  scripts: ScriptTreeNode[];
}

/** 脚本树节点(顶层数组元素) */
export type ScriptTreeNode = ScriptNode | ScriptFolder;

/** 脚本树:顶层数组(可含 script / folder,文件夹内可再含 script) */
export type ScriptTree = ScriptTreeNode[];

export interface ConnectorInfo {
  connector: string;
  model: string;
  models: string[];
  availableConnectors: Array<{ type: string; label: string }>;
}

/** 运行期设置(API 连接 + 生成参数);api_key 仅回传脱敏值 */
export interface RuntimeSettings {
  openai_base_url: string;
  api_key_masked: string;
  has_api_key: boolean;
  model: string;
  default_temperature: number;
  default_top_p: number;
  default_max_tokens: number;
  max_context_tokens: number;
  /** Agent 系统提示词(空 = 内置默认),支持 {{character_name}} 等占位符 */
  agent_system_prompt: string;
  /** 联网搜索端点(空 = 默认 DuckDuckGo HTML 接口) */
  search_endpoint: string;
  /** mvu 变量状态注入位置:system(默认,世界书并入 system)/ user_tail(追加到最新用户消息尾,前缀缓存友好) */
  mvu_vars_position: string;
  /** 反思提示词(空 = 机械规则检查;非空 = 反思步骤调用 LLM 判定) */
  reflect_prompt: string;
  /** 复杂模式预设尾部提示词(空 = 禁用;位置0 尾部) */
  preset_tail_prompt: string;
  /** 预设尾部提示词注入角色(user / assistant) */
  preset_tail_role: string;
  /** 反思失败建议的可选补充说明(主体建议由引擎自动生成 ≤200 token,注入位置0 内预设尾部之前;空 = 仅自动建议) */
  reflect_advice_prompt: string;
  /** 反思失败建议提示词注入角色(user / assistant) */
  reflect_advice_role: string;
  /** 授权模式(三档):strict=读/写/删文件都需授权;loose=读/写放行、删需授权;bypass=除系统路径(C 盘)写/删外全放行 */
  authorization_mode: AuthorizationMode;
  /** @deprecated 旧放行模式开关;读 authorization_mode 代替。true 等价 bypass,false 等价 strict */
  bypass_mode: boolean;
  /** 「始终需授权」清单:名单内工具在三档模式下都需授权 */
  bypass_blacklist: string[];
  /** 授权等待超时(秒;30..=1800,默认 300) */
  tool_authorization_timeout_secs: number;
  /** 任务模式工具策略:all=全量、deny_dangerous=拒绝危险工具(默认)、allowlist=白名单 */
  task_tool_policy: 'all' | 'deny_dangerous' | 'allowlist';
  /** 任务模式工具白名单(task_tool_policy=allowlist 时生效) */
  task_tool_allowlist: string[];
  /** AGENT/CUSTOM 模式工具循环轮次上限(默认 32) */
  max_tool_rounds: number;
  /** 工具历史保留的最近完整轮数(1..=32;默认 4;超出后最老轮摘要化) */
  tool_history_keep_rounds: number;
  /** 工具历史 token 预算(0 = 禁用预算闸门;否则 1024..=1M,默认 16384) */
  tool_history_budget_tokens: number;
  /** HTML 渲染开关(状态栏脚本执行前置条件):true = 开启安全 HTML 渲染 */
  render_html: boolean;
  /** 上下文压缩模式:off(不压缩)/ manual(手动触发)/ auto(token 超阈值自动压缩) */
  compaction_mode: string;
  /** 上下文压缩触发阈值(0.5..=0.95,默认 0.8):auto 模式下历史 token 占比达到该值即压缩 */
  compaction_threshold: number;
  /** 压缩后保留的最近消息条数(2..=200,默认 4;缓存感知管线) */
  compaction_keep_recent: number;
  /** snip 零成本裁剪的超长消息阈值(字节;0 = 禁用,上限 1MB,默认 8192) */
  compaction_snip_bytes: number;
  /** LLM 请求快照开关(第四点·主题 A):true = 每次下发前把完整消息数组落库供回放调试 */
  llm_request_log: boolean;
  /** 跨会话记忆蒸馏开关:true = 允许 POST /api/memory/distill 蒸馏会话为角色记忆 */
  memory_distill_enabled: boolean;
  /** 每次注入提示词的记忆条数上限(0..=50;0 = 不注入) */
  memory_inject_limit: number;
  /** 记忆槽字符预算(0..=20000;0 = 不限制,默认 2000):按精选排序累积到预算即停 */
  memory_inject_char_budget: number;
  /** 每角色记忆容量上限(0..=10000;0 = 不淘汰,默认 200):超出后最低分条目置 selected=0 */
  memory_max_entries: number;
  /** 向量化(embedding)开关:开启后记忆写入生成向量、召回走「向量+Jaccard」混合打分 */
  embedding_enabled: boolean;
  /** embedding 服务地址(OpenAI 兼容 /embeddings;独立于聊天 Base URL) */
  embedding_base_url: string;
  /** embedding API Key 脱敏展示(仅后 4 位;不回显明文) */
  embedding_api_key_masked: string;
  /** 是否已配置 embedding API Key */
  has_embedding_api_key: boolean;
  /** embedding 模型名(如 text-embedding-3-small / embedding-3 / bge-m3) */
  embedding_model: string;
  /** 向量维度(0 = 由测试连接自动探测并回填) */
  embedding_dim: number;
  /** 技能渐进披露开关(true = system 只注入「名称:用途」清单,正文按需 read;默认 true) */
  skill_progressive_disclosure: boolean;
  /** 子智能体最大嵌套深度(1..=4;默认 2) */
  subagent_max_depth: number;
  /** 子智能体最大并发数(1..=16;默认 6) */
  subagent_max_concurrency: number;
  /** 子智能体结果最大字符数(500..=8000;默认 2000,超出截断带尾注) */
  subagent_result_max_chars: number;
  /** 回退快照(undo)开关(批次 6.1b;默认 true):开时写工具执行前自动存档,可回退 */
  undo_enabled: boolean;
  /** MCP stdio 客户端总开关(批次 6.2;默认 false;仅启动时装配,改后重启生效) */
  mcp_enabled: boolean;
  /** MCP 服务器列表(stdio 托管子进程;默认空) */
  mcp_servers: McpServerConfig[];
  /** 执行者人设完整开关(R3a;默认 false = 精简:仅 description+personality;true = 完整:再加 scenario+mes_example)。仅任务模式生效 */
  task_persona_full: boolean;
  /** 任务模式是否继承提示词注入(2026-09-10 实跑修复;默认 false = 隔离,不注入 prompt_floors.json)。仅任务模式生效 */
  task_prompt_inject_enabled: boolean;
}

/** MCP 服务器配置(批次 6.2):name 会 sanitize 为工具名前缀段([a-z0-9_]) */
export interface McpServerConfig {
  /** 服务器名(工具注册为 mcp_{name}_{tool}) */
  name: string;
  /** 可执行命令(如 npx / node / 某个 exe) */
  command: string;
  /** 命令行参数 */
  args: string[];
  /** 是否启用(默认 true;false = 保留配置但不装配) */
  enabled: boolean;
}

export interface PromptPreviewLayer {
  source: string;
  role: string;
  layer: number;
  order: number;
  content: string;
}

export interface PromptPreview {
  ok: boolean;
  note: string;
  layers: PromptPreviewLayer[];
}

export interface RuntimeSettingsPatch {
  openai_base_url?: string;
  openai_api_key?: string;
  model?: string;
  default_temperature?: number;
  default_top_p?: number;
  default_max_tokens?: number;
  max_context_tokens?: number;
  agent_system_prompt?: string;
  search_endpoint?: string;
  mvu_vars_position?: string;
  reflect_prompt?: string;
  preset_tail_prompt?: string;
  preset_tail_role?: string;
  reflect_advice_prompt?: string;
  reflect_advice_role?: string;
  /** @deprecated 用 authorization_mode;true → bypass,false → strict */
  bypass_mode?: boolean;
  /** 授权模式三档 */
  authorization_mode?: AuthorizationMode;
  /** 「始终需授权」清单 */
  bypass_blacklist?: string[];
  /** 授权等待超时(秒;30..=1800) */
  tool_authorization_timeout_secs?: number;
  /** 任务模式工具策略 */
  task_tool_policy?: 'all' | 'deny_dangerous' | 'allowlist';
  /** 任务模式工具白名单 */
  task_tool_allowlist?: string[];
  max_tool_rounds?: number;
  /** 工具历史保留轮数(1..=32) */
  tool_history_keep_rounds?: number;
  /** 工具历史 token 预算(0 = 禁用;否则 1024..=1048576) */
  tool_history_budget_tokens?: number;
  render_html?: boolean;
  compaction_mode?: string;
  compaction_threshold?: number;
  compaction_keep_recent?: number;
  compaction_snip_bytes?: number;
  llm_request_log?: boolean;
  memory_distill_enabled?: boolean;
  memory_inject_limit?: number;
  /** 记忆槽字符预算(0..=20000;0 = 不限制) */
  memory_inject_char_budget?: number;
  /** 每角色记忆容量上限(0..=10000;0 = 不淘汰) */
  memory_max_entries?: number;
  /** 向量化开关 */
  embedding_enabled?: boolean;
  /** embedding 服务地址(自动补协议与 /v1) */
  embedding_base_url?: string;
  /** embedding API Key(留空 = 保持现有不变更) */
  embedding_api_key?: string;
  /** embedding 模型名 */
  embedding_model?: string;
  /** 向量维度(0 = 自动探测) */
  embedding_dim?: number;
  skill_progressive_disclosure?: boolean;
  subagent_max_depth?: number;
  subagent_max_concurrency?: number;
  subagent_result_max_chars?: number;
  undo_enabled?: boolean;
  mcp_enabled?: boolean;
  mcp_servers?: McpServerConfig[];
  /** 执行者人设完整开关(R3a;仅任务模式生效) */
  task_persona_full?: boolean;
  /** 任务模式是否继承提示词注入(2026-09-10 实跑修复;默认 false = 隔离) */
  task_prompt_inject_enabled?: boolean;
}

// ===== 音频播放器(阶段五 5a;契约对齐酒馆助手 audio.d.ts) =====

/** 音频曲目(仅 URL 播放,本地文件上传留扩展位) */
export interface AudioTrack {
  /** 标题 */
  title: string;
  /** 音频的网络链接 */
  url: string;
}

/** 播放模式(与酒馆助手 AudioSettings.mode 对齐) */
export type AudioMode = 'repeat_one' | 'repeat_all' | 'shuffle' | 'play_one_and_stop';

/** 单通道设置(部分字段合并更新时全部可选) */
export interface AudioChannelSettings {
  /** 是否启用 */
  enabled: boolean;
  /** 当前播放模式 */
  mode: AudioMode;
  /** 是否静音 */
  muted: boolean;
  /** 当前音量 (0-100) */
  volume: number;
}

/** 音频通道标识 */
export type AudioChannelType = 'bgm' | 'ambient';

/** 双通道全量状态 */
export interface AudioState {
  bgm: AudioChannelState;
  ambient: AudioChannelState;
}

export interface AudioChannelState extends AudioChannelSettings {
  playlist: AudioTrack[];
}

/** 沙箱内 getCurrentAudio 返回(当前选中/播放的曲目信息) */
export interface CurrentAudio {
  /** 当前选中/正在播放的音频链接,未选中曲目时为空字符串 */
  src: string;
  /** 当前选中音频的标题,未匹配到时为空字符串 */
  title: string;
  /** 是否正在播放 */
  playing: boolean;
  /** 播放进度 (0-100) */
  progress: number;
}

/** 插件工具信息(自定义工具插件) */
export interface ToolPluginInfo {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
}

export interface PluginToolsStatus {
  tools: ToolPluginInfo[];
  files: string[];
}

/** 提示词注入模式:简单模式 / 复杂模式(楼层系统) */
export type InjectMode = 'simple' | 'complex';
export type FloorPosition = 'system' | 'before' | 'after' | 'depth';
export type FloorRole = 'system' | 'user' | 'assistant';

/** 禁词条目:输出中出现 word 时注入自省提示词(所有模式),deep/agent/custom 另由引擎工具替换为 replacement */
export interface BannedWordEntry {
  /** 禁词(精确子串匹配) */
  word: string;
  /** 替换词(含义相近、更得体;空串 = 直接删除) */
  replacement: string;
}

/** 简单模式:字数/转述/对话/视角 四项 + 禁词库 */
export interface SimplePromptConfig {
  word_count_enabled: boolean;
  word_count: number;
  paraphrase_enabled: boolean;
  dialogue_enabled: boolean;
  perspective_enabled: boolean;
  perspective: string;
  /** 注入顺序(合成文本按此顺序拼接;空 = 默认 字数→转述→对话→视角) */
  order?: string[];
  /** 禁词库总开关 */
  banned_words_enabled?: boolean;
  /** 禁词提示词(纯文本,发给 AI 的自省提示;新格式) */
  banned_prompt?: string;
  /** 禁词 → 替换词 映射表(旧格式兼容,仅保留字段不再用于注入) */
  banned_words?: BannedWordEntry[];
}

/** 单条提示词楼层(仿 SillyTavern Prompt Manager) */
export interface PromptFloor {
  id: string;
  name: string;
  content: string;
  role: FloorRole;
  position: FloorPosition;
  /** 深度:从历史末尾往前数第 N 条之后插入(0 = 最新消息后;仅 position=depth 生效) */
  depth: number;
  enabled: boolean;
  order: number;
}

/** 提示词注入整体配置(全局,所有会话生效) */
export interface PromptInjectConfig {
  mode: InjectMode;
  simple: SimplePromptConfig;
  floors: PromptFloor[];
}

/** 导入结果(替换现有楼层) */
export interface PromptPresetImportResult {
  ok: boolean;
  imported: number;
  config: PromptInjectConfig;
}

/** 技能记录(提示词技能) */
export interface SkillRecord {
  id: string;
  name: string;
  description: string;
  content: string;
  enabled: boolean;
  created_at: string;
  /** 技能工具白名单(空 = 不限);经 read(type=skill) 加载该技能时可用的工具集合 */
  allowed_tools: string[];
  /** 是否允许作为子智能体派发(agentgo 链路) */
  run_as_subagent: boolean;
  /** 技能级模型覆盖(空 = 沿用当前连接器模型) */
  model: string;
}

export interface SkillImportItem {
  name: string;
  description?: string;
  content?: string;
}

export interface AgentPlan {
  plan: {
    steps: Array<{ goal: string; action: string; generates?: boolean; name?: string }>;
    summary: string;
  };
  summary: string;
  tools: string[];
  history: { state: string; plan: string[]; step_index: number } | null;
}

export type AgentMode = 'fast' | 'deep' | 'agent' | 'custom';

/** 单步执行流程(与后端 PlanStep 对齐;tools:null=不使用,[]=全部,列表=白名单) */
export interface AgentFlowStep {
  id: string;
  name: string;
  enabled: boolean;
  goal: string;
  /** direct=执行/生成, reflect=反思(不生成) */
  action: 'direct' | 'reflect';
  /** 是否生成正文(direct 步骤必选;reflect 步骤必须缺省) */
  generates?: boolean;
  /** 步骤级系统提示词(支持酒馆宏),追加到 system 末尾,带 [本步指令] 标记 */
  system_prompt?: string | null;
  temperature?: number | null;
  max_tokens?: number | null;
  tools?: string[] | null;
  /** 工具选择策略;function 时需同时填写 tool_choice_function */
  tool_choice?: 'auto' | 'none' | 'required' | 'function' | null;
  tool_choice_function?: string | null;
  parallel_tool_calls?: boolean | null;
}

/** 自定义执行流程配置(单个流程,data/agent_flows.json 流程库中的一项) */
export interface AgentFlowConfig {
  /** 流程 id(库内唯一;空 = 新建,由后端分配) */
  id?: string;
  /** 流程名称(选择器展示) */
  name?: string;
  /** 流程说明 */
  description?: string | null;
  enabled: boolean;
  steps: AgentFlowStep[];
}

/** 流程库(全局):当前选中的流程 + 全部流程 */
export interface AgentFlowLibrary {
  current_flow_id: string | null;
  flows: AgentFlowConfig[];
}

// ===== 宏调试(阶段六 6b) =====

/** 历史消息条目({{lastMessage}}/{{firstMessage}} 等宏读取) */
export interface MacroHistoryItem {
  role: string;
  content: string;
}

/** 宏展开上下文(全可选;缺省即空/默认,与服务端引擎缺省语义一致) */
export interface MacroExpandCtx {
  character_name?: string;
  character_description?: string;
  personality?: string;
  scenario?: string;
  user_name?: string;
  user_input?: string;
  history?: MacroHistoryItem[];
  /** 扁平变量 map({{getvar::key}}/{{var::key}} 读取;值为显示字符串) */
  vars?: Record<string, string | number | boolean>;
}

/** 宏展开结果 */
export interface MacroExpandResult {
  expanded: string;
}

// ===== 任务模式(task 工作台) =====

/**
 * 任务计划步骤状态(与 server-rs task_service 实际写入值对齐):
 * pending=待执行(parse 时 serde 默认值) / running=执行中 / done=完成 / error=失败
 * 写入点:executor.rs 步骤循环(running→done|error)、mod.rs/parse.rs(初始 pending)
 */
export type TaskStepStatus = 'pending' | 'running' | 'done' | 'error';

/** 任务计划步骤 */
export interface TaskStep {
  name: string;
  goal: string;
  status: TaskStepStatus;
  result: string;
}

/** 任务状态:待执行/规划中/执行中/计划待批准(plan 模式)/完成/部分完成(含失败步骤但成果已产出)/出错/已停止 */
export type TaskStatus = 'pending' | 'planning' | 'running' | 'planned' | 'done' | 'partial' | 'error' | 'ended';

/** 任务执行模式(批次 4 六模式,对齐 server-rs TaskRunMode):
 *  legacy 三段式(默认) / solo 单主工具循环 / multi 多agent / plan 先规划后批准 / team 多主+审计 / custom 自定义流程 */
export type TaskRunMode = 'legacy' | 'solo' | 'multi' | 'plan' | 'team' | 'custom';

/** 任务记录 */
export interface TaskRecord {
  id: string;
  title: string;
  status: TaskStatus;
  plan: TaskStep[];
  result: string;
  error: string;
  character_id?: string | null;
  created_at: string;
  updated_at: string;
  /** 执行模式(批次 4;旧服务端不带此字段,消费侧按 legacy 处理) */
  task_mode?: TaskRunMode;
}

/**
 * 任务子任务状态(与 server-rs task_service 实际写入值对齐):
 * running=执行中(create_subtask 插入即 running) / done=完成 / error=失败 / ended=已停止(取消)
 * pending=待执行:当前无写入点,但 cancel 逻辑对其做防御性判断,保留在值域内
 */
export type TaskSubtaskStatus = 'pending' | 'running' | 'done' | 'error' | 'ended';

/** 任务子任务 */
export interface TaskSubtask {
  id: string;
  task_id: string;
  name: string;
  instruction: string;
  status: TaskSubtaskStatus;
  result: string;
  error: string;
  created_at: string;
  updated_at: string;
}

/** 任务 token 累计(规划/步骤/汇总各次 LLM 调用落库聚合) */
export interface TaskUsageTotal {
  prompt_tokens: number;
  completion_tokens: number;
  reasoning_tokens: number;
}

/** 任务消息角色(批次 R2):user=用户指令 / assistant=执行者产出 */
export type TaskMessageRole = 'user' | 'assistant';

/**
 * 任务消息种类(批次 R2;对齐 server-rs task_messages.kind):
 * goal=创建时的用户目标 / result=首轮成果(实跑问题 1 起落库)
 * / normal=普通(旧行默认值) / followup=终态追加指令(R2a) / plan_chat=批准环节规划对话(R2b)
 */
export type TaskMessageKind = 'normal' | 'goal' | 'result' | 'followup' | 'plan_chat';

/** 任务消息(批次 R2 多轮用户输入;详情响应 messages 数组,created_at 升序) */
export interface TaskMessage {
  id: string;
  task_id: string;
  role: TaskMessageRole;
  kind: TaskMessageKind;
  content: string;
  created_at: string;
}

/** 任务详情(含子任务与该任务 token 累计;批次 R2 起携 messages 用户指令历史) */
export interface TaskDetail {
  task: TaskRecord;
  subtasks: TaskSubtask[];
  usage_total: TaskUsageTotal;
  /** 用户指令历史(followup 追加 / plan_chat 规划对话);旧服务端无此字段,读取须容错 */
  messages?: TaskMessage[];
}

/**
 * 任务单次 LLM 调用记录(批次 3 L3 调用追踪;GET /api/tasks/{id}/calls,按 created_at,id 升序)。
 * phase:planner=规划 / step=步骤 / summarize=汇总 / agent=主Agent / subagent=子Agent / audit=审计;
 * status:ok=正常 / empty=空响应 / error=错误。
 */
export interface TaskLlmCall {
  id: string;
  task_id: string;
  phase: string;
  /** step 阶段的步骤序号(0 起,展示时 +1);其余阶段为 null */
  step_index: number | null;
  model: string;
  /** 提示词摘要(面板展开查看) */
  prompt_summary: string;
  /** 响应摘要(面板展开查看) */
  response_summary: string;
  prompt_tokens: number;
  completion_tokens: number;
  reasoning_tokens: number;
  elapsed_ms: number;
  status: string;
  /**
   * 上游 finish_reason(stop / length / content_filter 等;可观测性问题①截断标记)。
   * 'length' = max_tokens 截断(响应为半截文本);'' = 未知/未下发(旧行兼容值)。
   * 旧服务端无此列,字段可能缺省,读取须容错。
   */
  finish_reason?: string;
  created_at: string;
}
