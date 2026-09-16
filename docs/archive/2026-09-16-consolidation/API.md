# Kedai API 文档

基地址:开发模式 `http://127.0.0.1:3001`(前端经 Vite 代理 `/api`);生产模式同域。

所有请求/响应均为 `application/json`(除文件上传与 SSE 流)。除 `/api/health`、`/api/bootstrap` 与 `/api/avatars/*` 外,API 请求必须携带服务启动时生成或由 `KEDAI_API_TOKEN` 固定配置的 Bearer token;非 loopback 监听还必须显式设置 `KEDAI_ALLOW_REMOTE=1`,且固定 token 至少 32 字符。运行时主 Agent 提示词统一来自 `DATA_DIR/AGENTS_RUNTIME.md`,不是仓库开发用 `AGENTS.md`。

---

## 聊天

### POST `/api/chat/send` — 发送消息,触发 Agent 全流程

**请求体**

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `message` | string | ✅ | 用户消息(去空) |
| `session_id` | string | 条件 | 目标会话;与 `character_id` 二选一(缺省时取该角色首个会话/自动创建) |
| `character_id` | string | 条件 | 目标角色 |
| `agent_mode` | `"fast" \| "deep" \| "agent" \| "custom"` | 否 | 默认 `fast`;`deep` 启用完整计划-执行-反思;`agent` 同 deep + 工具循环;`custom` 按「Agent 执行流程」配置的步骤序列执行(需先在设置中启用并保存,否则 400) |
| `temperature` | number | 否 | 默认取运行期设置(初始 0.8) |
| `top_p` | number | 否 | 默认取运行期设置(初始 0.9) |
| `max_tokens` | number | 否 | 默认取运行期设置(初始 1024);simple 字数注入启用且字数要求更高时自动上调(需 = min(字数×2+512, 8192)) |
| `resend_message_id` | integer | 否 | 编辑后重发锚点;服务端仅接受当前会话最后一条、角色为 `user` 且正文与 `message` 一致的消息,否则返回 409 |

**响应**:`text/event-stream` SSE。事件类型:

| 事件 | 负载 | 说明 |
|---|---|---|
| `step` | `{step, detail?}` | Agent 步骤(计划中/执行中/生成中/反思/出错…);custom 模式额外携带 `{index, total}`(第几步/共几步),fast/deep/agent 不带 |
| `token` | `{text}` | 文本片段(逐字渲染) |
| `tool_call` | `{name, input}` | 工具调用预告 |
| `tool_result` | `{name, output}` | 工具结果 |
| `vars` | `{stat_data}` | 酒馆助手变量树同步:模型回复中的 `<UpdateVariable>` 补丁被应用后推送最新 `stat_data`(仅变量更新时出现,先于 `finish`) |
| `interrupted` | `{}` | 被停止 |
| `finish` | `{usage, content}` | 结束,含 usage(`prompt/completion/total/context_tokens`);`content` 已剥离 `<UpdateVariable>` 协议块 |

**错误**:400 消息为空;404 会话不存在;409 会话正在生成中;400 custom 模式未启用/流程未保存或非法。

### POST `/api/chat/stop` — 停止当前生成

请求:`{ "session_id": string }` → `{ "ok": true }`

### GET `/api/chat/history?session_id=…` — 消息历史

响应:`{ "messages": [{ id, session_id, role, content, extra, created_at }] }`

> 酒馆助手:assistant 消息 `extra.mvu = { stat_data }` 携带当轮更新后的变量树快照(仅变量更新时),前端按消息顺序回放即可得到会话最新状态;`content` 为剥离 `<UpdateVariable>` 块后的干净文本。**纯变量更新消息(正文为空)同样落库**(带快照),保证刷新/回放不把变量树回滚到更新前。

### PUT `/api/chat/messages/:id?session_id=…` — 编辑消息

请求:`{ "content": string }` → 更新后的消息

### DELETE `/api/chat/messages/:id?session_id=…` — 删除消息

成功 → `204`

### POST `/api/chat/clear` — 清空会话(保留角色定义)

请求:`{ "session_id": string }` → `{ "ok": true }`

---

## 会话

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/chat/sessions?character_id=…` | 某角色的会话列表(按更新时间倒序) |
| POST | `/api/chat/sessions` | 创建会话,`{ character_id, title? }` → 201 |
| DELETE | `/api/chat/sessions/:id` | 删除会话 → 204 |

---

## 角色卡

### GET `/api/characters` — 列表(摘要,不含 data_raw)

### POST `/api/characters/upload` — 上传角色卡

`multipart/form-data`,字段名 `file`,支持 `.png`(V2 tEXt 内嵌)或 `.json`。

成功 → 201 完整记录(含 `data_raw` 全部 V2 字段与未知字段)。

### GET `/api/characters/:id` — 详情(含完整 `data_raw`)

响应除列表字段外含 `data_raw`、`regex_scripts`,以及 **`card_plugins`**(角色卡内嵌插件检测结果,仅详情返回):
`[{ id: "sillytavern-assistant", name, name_en, enabled, source: "character_card", description, features: [{ id, label, detected }] }]` ——
检测基于 `character_book` 条目文本([InitVar] / EJS 模板 `<%` / `getvar` / `{{format_message_variable}}` / `<UpdateVariable>` 等特征);普通角色卡无该字段。

### PUT `/api/characters/:id` — 更新

请求:`{ "chara_name"?, "description"? }`

### DELETE `/api/characters/:id` — 删除(级联删除会话与消息)→ 204

### GET `/api/avatars/:file` — 头像静态资源

---

## 设置

### GET `/api/settings` — 读取运行期设置(API Key 脱敏)

响应:`{ openai_base_url, api_key_masked, has_api_key, model, default_temperature, default_top_p, default_max_tokens, max_context_tokens, agent_system_prompt, search_endpoint, mvu_vars_position, reflect_prompt }`

### PUT `/api/settings` — 更新运行期设置(部分字段)

请求(任意子集):`{ openai_base_url?, openai_api_key?, model?, default_temperature?, default_top_p?, default_max_tokens?, max_context_tokens?, agent_system_prompt?, search_endpoint?, mvu_vars_position?, reflect_prompt? }`

- `openai_base_url` / `openai_api_key` 变更 → **立即重建连接器**(Key 非空才替换,空/缺省保持不变)
- `model` 变更 → 立即切换当前模型
- `mvu_vars_position`:`system`(默认,世界书并入 system 提示词)/ `user_tail`(追加到最新用户消息尾部,变量更新不再使 system + 早期历史前缀缓存整体失效,提示词缓存命中率更高);非法值忽略
- `reflect_prompt`:反思提示词,可清空。空 = 反思步骤用内置机械规则(空/短输出、截断标点、提问未答)检查;非空 = deep/agent/custom 模式的反思步骤改为调用一次 LLM,按本提示词判定草稿质量,输出须以 `PASS` 或 `FAIL` 开头,失败自动重新生成(最多 3 次);模型输出无法解析或调用失败时自动回退机械规则,反思重试仍受硬上限约束
- 持久化到 `data/settings.json`,重启后仍生效;优先级高于 `.env`。Windows 使用当前用户 DPAPI;非 Windows 无安全凭据后端时默认拒绝保存非空 Key并返回明确错误,仅显式设置 `KEDAI_ALLOW_INSECURE_PLAINTEXT_SECRETS=1` 才允许不安全明文持久化

响应:`{ ok, settings: { …同上 } }`

### POST `/api/settings/connect` — 测试后端连接

响应:`{ ok, message, models: string[] }`

### GET `/api/settings/models` — 可用模型列表

响应:`{ models: string[] }`

### GET `/api/settings/info` — 当前连接信息(不含密钥)

`{ connector, model, models, availableConnectors }`

### GET `/api/settings/model` / PUT `/api/settings/model` — 获取 / 切换当前模型

### POST `/api/settings/refresh-models` — 重新拉取模型列表

### GET / PUT `/api/settings/agent-prompt` — 读取 / 写入运行时主 Agent 提示词(`DATA_DIR/AGENTS_RUNTIME.md`)

### POST `/api/settings/prompt-preview` — 预览最终提示词(不发起生成)

### GET `/api/diagnostics/cache` — 缓存命中率 / 费用估算 / 四级水位诊断

### GET `/api/token/session-total` / `/api/token/global-total` — 会话级 / 全局 token 累计

---

## Token 计数

### POST `/api/token/count`

请求:`{ messages: [{role, content}], model? }` → `{ total, model }`

按模型自动选择分词器(`o200k_base` / `cl100k_base` / `p50k_base`),未知模型回退 `cl100k_base`。

---

## 提示词注入

### GET `/api/prompt-inject` — 读取注入配置

响应:`{ ok, config: { mode: "simple"|"complex", simple: {…}, floors: [{id, name, content, role, position, depth, enabled, order}] } }`

`simple` 为 `SimplePromptConfig`:

| 字段 | 类型 | 说明 |
|---|---|---|
| `word_count_enabled` / `word_count` | bool / number | 字数上限(启用且 >0 生效) |
| `paraphrase_enabled` | bool | 转述 |
| `dialogue_enabled` | bool | 对话 |
| `perspective_enabled` / `perspective` | bool / string | 视角(第一/二/三人称) |
| `order` | string[] | 注入顺序(空 = 默认) |
| `banned_words_enabled` | bool | 禁词库总开关 |
| `banned_words` | `[{word, replacement}]` | 禁词 → 替换词映射表;输出含禁词时所有模式注入自省提示词,deep/agent/custom 另由引擎收尾调 `censor_text` 工具同义替换(替换词留空 = 删除) |

### PUT `/api/prompt-inject` — 全量覆写注入配置

请求:`{ config: {…同上} }` → `{ ok, config }`,持久化到 `data/prompt_floors.json`。新字段(`banned_words_*`)向后兼容——旧 JSON 缺字段自动取默认值。

### POST `/api/prompt-inject/import` — 导入酒馆(SillyTavern)预设

请求:`multipart/form-data`,`file` 字段为 ST 预设 JSON(20MB 上限),可选 `mode` 字段(`replace` 默认,清空替换全部楼层并自动切复杂模式;`append` 接续现有楼层)。

- 顶层 `prompts` 数组映射为楼层:identifier→id、role 直映(非法→user)、enabled/depth/order 保留;position 映射 `system_prompt=true`→System、`injection_position` 0→System / 1→Before / 2→(depth>0 ? Depth : After)、仅 attach 条目→After;顺序按 `prompt_order` 首个组定序(跳过 worldInfoBefore/chatHistory 等锚点),禁用的条目保留 `enabled=false`。

响应:`{ ok, imported: N, config }`;非有效预设(缺 `prompts` 数组 / 无楼层)→ 400 中文错误。

### GET `/api/chat/history?session_id=…` — 消息历史(显示层宏展开)

每条消息返回 `content`(存储原文,编辑用)与 `content_display`(渲染用):对每条消息按「之前的消息」构造 history_before 做只读宏展开(`{{char}}`/`{{personality}}`/`{{scenario}}`/`{{getvar}}`/`{{lastMessage}}` 等),vars 使用副本,`{{setvar}}`/`{{addvar}}` 只影响本轮后续消息显示、**不落库**。

---

## Agent 执行流程(custom 模式)

流程为**全局流程库**(持久化 `data/agent_flows.json`):可保存多个流程、切换当前选中、导入导出。所有响应统一携带:

- `library`: `{ current_flow_id: string|null, flows: [流程…] }` — 完整流程库
- `config`: 当前选中流程(无选中时为 `null`,兼容旧调用方)

单个流程:`{ id, name, description?, enabled: bool, steps: […] }`;步骤字段:`{id, name, enabled, goal, action, generates?, system_prompt?, temperature?, max_tokens?, tools?}`。

`tools` 语义:`null`/缺省 = 该步不使用工具;`[]` = 全部工具;`[name…]` = 白名单(按注册名过滤)。

### GET `/api/agent-flows` — 读取流程库

响应:`{ ok, library, config }`。

### PUT `/api/agent-flows` — 保存(创建或更新)流程并设为当前选中

请求:`{ config: {…单个流程} }` → `{ ok, library, config }`。`config.id` 为空/缺省视为**新建**(后端分配 id);id 已存在则覆盖该流程。适合「保存」「新建」「导入」「复制」。

### POST `/api/agent-flows/select` — 切换当前选中流程

请求:`{ id }` → `{ ok, library, config }`;id 不存在 → 400。

### DELETE `/api/agent-flows/{id}` — 删除流程

→ `{ ok, library, config }`;删除当前选中流程时自动回退到第一个;id 不存在 → 400。

校验(`enabled=true` 时):步骤非空且至少一条启用;至少一个 `action=direct` 且 `generates=true`;`action` 仅 `direct|reflect`;reflect 步不得带 `generates=true` 与 `system_prompt`;温度 0–2;输出上限 1–32768。失败 → 400 中文错误;`enabled=false` 时允许保存任意状态(先编辑后启用)。

### 数据迁移

旧版单流程格式 `{ enabled, steps }` 首次加载自动迁移为流程库(旧步骤保留,`name="默认流程"`);从未编辑过的空配置替换为内置「文学创作协调流程」。文件缺失时同样注入内置流程,开箱即用。

### custom 模式执行语义

- 步骤按 `enabled=true` 顺序执行;每步 `system_prompt` 经酒馆宏展开后追加 `[本步指令]` 到 system 末尾(独立消息视图)。
- `{{setvar}}`/`{{addvar}}` 在每步生成后持久化到会话变量,后续步骤可用 `{{getvar}}` 读取(跨步骤状态传递)。
- 步骤级 `temperature`/`max_tokens` 覆盖全局参数;`tools` 决定该步工具可见性(agent 模式整体下发全部工具,custom 按步控制)。
- `POST /api/agent/plan` 支持 `agent_mode=custom`:返回流程摘要「自定义流程:共 N 步」与各步骤名称。

---

## 导入 / 导出

### GET `/api/export/chat?session_id=…` — 导出

响应:`{ session_id, messages }`,`messages` 为 SillyTavern 完全兼容的消息数组。

### POST `/api/import/chat` — 导入(替换当前会话)

请求:`{ session_id, messages: [{role, content, extra?}] }` → `{ ok, imported }`
非法消息格式 → 400。

---

## 世界书(World Info / Lorebook)

### GET `/api/world-books` — 独立世界书列表(不含 data_raw)

### POST `/api/world-books/upload` — 上传独立世界书

`multipart/form-data`:`file`(JSON)+ 可选 `character_id`(绑定角色;缺省全局)→ 201 记录。

上传时自动执行**酒馆兼容转换**(data_raw 存转换后结果):
- 关键词:兼容 `keys/key/keywords/keyword` 字段名;逗号/中文逗号/顿号/换行分隔的字符串拆分为数组,过滤空项;
- 常态/激发:兼容字符串 `"true"/"false"` 与数字 `1/0` 形态的 `constant`;缺失时自动判定(无关键词且无正则 → 常驻,否则 → 激发);
- 激活状态:兼容 `enabled/disable/disabled` 字段,缺失默认启用;
- 属性默认自动:注入角色 `role` 仅保留 `system/user/assistant`,其余/缺失一律移除(按 常驻→system、激发→user 自动分配)。

响应在记录上附带 `conversion` 统计:`{ total_count, converted_count, constant_auto_count, key_normalized_count }`(未转换时为 0)。

### GET `/api/world-books/auto-assign-check` — 自动分配属性机制自检

可选 query `character_id`(指定角色时额外校验该角色/全局世界书条目解析链路)。

响应 `{ ok, checks, summary }`;`checks` 各项:`parse`(条目解析链路)、`auto_role`(role=自动 → 常驻 system / 激发 user 实测)、`convert`(转换机制变体规范化实测)、`ui_auto`(前端「自动」选项)。全为 `ok` 时 `ok:true`。

### PUT `/api/world-books/:id` — 更新独立世界书

请求:`{ enabled?, character_id?, name? }`;`character_id: ""` 清除绑定转全局。

### DELETE `/api/world-books/:id` — 删除 → 204

### GET `/api/world-books/:id/entries` — 条目编辑视图

响应:`{ id, entries: [{id, comment, keys, keys_secondary, regex?, use_regex, constant, enabled, content, position, depth, order, case_sensitive, sticky, cooldown, probability, use_probability}] }`,按 position → order → id 稳定排序。

### PUT `/api/world-books/:id/entries` — 全量回写条目

请求:`{ entries: [...] }`(字段同上);写回 data_raw.entries(map 按 uid 覆盖/追加,数组整组替换)。保存后下次发送生效。

### POST `/api/world-books/:id/entries` — 新增空条目

返回 `{ entry }`(分配新 uid;空内容/关键词条目按约定不参与注入,编辑填入后生效)。

### GET `/api/characters/:id/world-entries` — 角色卡内嵌世界书条目

响应:`{ character_id, entries: [...] }`(无内嵌返回空数组)。角色卡上传时同样自动执行世界书兼容转换(见上传节)。

### PUT `/api/characters/:id/world-entries` — 回写角色卡内嵌世界书

请求:`{ entries: [...] }` → 写回 `data_raw.character_book.entries`(兼容 V2 顶层 / V3 data 子对象)。角色卡无内嵌世界书 → 400。

**注入语义**(完整兼容酒馆 SillyTavern World Info):引擎每轮收集「角色卡内嵌 + 独立世界书(绑定角色或全局且 enabled)」条目 → 常驻(`constant=true`)始终注入;非常驻按主关键词 `keys` + 副关键词 `keys_secondary` 对**最近 `depth` 条用户消息**(0=全部历史)做子串匹配(`case_sensitive` 控制大小写,`regex`+`use_regex` 优先);`use_probability` 时按 `probability%` 随机决定命中后是否注入 → 命中文本按 `position` → `order` 排序拼入系统提示词(`{{world_info}}` 占位符或内置模板追加)。`position` 兼容数字 0-4 与字符串 `before_char/top/normal/bottom/after_char`。

---

## Agent 控制

### POST `/api/agent/plan` — 预览行动计划(不执行)

请求:`{ message, agent_mode?, session_id? }`
响应:`{ plan, summary, tools, history? }`;`agent_mode=custom` 时 `summary` 为「自定义流程:共 N 步」,`plan.steps` 含步骤 `name`;流程未启用/非法 → 400 中文错误。

### POST `/api/agent/execute` — 手动执行指定步骤(历史桩,返回 501 未实现)

请求:`{ session_id, step? }`(MVP 阶段由引擎自动编排)

### POST `/api/agent/interrupt` — 中断当前执行

请求:`{ session_id }` → `{ ok: true }`

### GET / POST / DELETE `/api/agent/tool-permissions` — 工具授权查询 / 授予 / 撤销

查询响应含 `tools`(每项 `name/description/risk/allowed/reason`)与 `grants`(该会话/角色的
显式授权:`{ session: string[], role: string[] }`)。授权作用域 `scope` 仅支持 `session` 与 `role`;
空 `scope_id`(匿名会话)会被拒绝,避免跨匿名会话串权。

### POST `/api/agent/tool-permissions/resolve` — 处理「工具未授权」事件(once/session/role/deny)

### 授权模式与任务工具策略(settings 字段)

| 字段 | 默认 | 说明 |
|---|---|---|
| `authorization_mode` | `loose` | `strict` / `loose` / `bypass`;非法值 400。判定矩阵见 [docs/授权模式.md](docs/授权模式.md) |
| `bypass_blacklist` | `[]` | 「始终需授权」清单(三档下都需授权) |
| `tool_authorization_timeout_secs` | `300` | 授权等待超时(30..=1800) |
| `task_tool_policy` | `deny_dangerous` | `all` / `deny_dangerous` / `allowlist`;任务模式工具集合 |
| `task_tool_allowlist` | `[]` | `task_tool_policy=allowlist` 时的白名单 |
| `task_persona_full` | `false` | 执行者人设:`false` 精简(description+personality)/`true` 完整;仅任务模式生效 |
| `task_prompt_inject_enabled` | `false` | 任务模式是否继承 `prompt_floors.json` 提示词注入(2026-09-10 实跑修复):`false` 隔离(默认,避免角色扮演文章要求污染任务)/`true` 沿用旧行为 |
| `bypass_mode` | `false` | 已废弃;`true` → `bypass`,`false` → `strict` |

任务模式无 UI 授权上下文,未放行工具不进入授权等待,而是立即回灌
`{"error": ..., "code": "tool_policy_denied"}`。

---

## 任务模式(task 工作台)

任务运行模式为六值枚举 `TaskRunMode`:`legacy`(三段式,默认)/ `solo` / `multi` / `plan` / `team` / `custom`,语义见 [docs/任务引擎六模式.md](docs/任务引擎六模式.md)。

### GET `/api/tasks` — 任务列表

### POST `/api/tasks` — 新建任务

### GET `/api/tasks/{id}` — 任务详情(含子任务、阶段消息)

### DELETE `/api/tasks/{id}` — 删除任务

### POST `/api/tasks/{id}/run` — 启动任务

### POST `/api/tasks/{id}/stop` — 停止任务

### POST `/api/tasks/{id}/approve` — 批准计划(plan 模式:planned 态批准后可携修改后计划,solo 续跑)

### POST `/api/tasks/{id}/followup` — 终态追加指令(done/partial/error/ended 可追加,solo 续跑续写成果)

请求体 `{ content, mode? }`:`mode` 缺省/空 = `append`(新产出以「追加 N」段附加进
`result`,保留原文);`replace` = 用新产出整体替换 `result`(段标「修订 N」,用于
「压缩/重写/改前面」类指令);未知值 400(VALIDATION)。

### POST `/api/tasks/{id}/plan-chat` — 批准环节规划对话(planned 态按反馈修订计划)

### GET `/api/tasks/{id}/calls` — 任务 LLM 调用追踪(「调用情况」面板)

### GET `/api/tasks/events` — **任务事件 SSE 流**

取代前端 REST 轮询;事件 `kind` 取值:`created` | `status` | `plan` | `subtask` | `usage` | `llm_call` | `deleted` | `agent_status` | `approval_required` | `delta`(`delta` 为流式正文增量暂态事件,不落库;权威数据以 `llm_call` 落库行 / `calls` 端点为准)。

### GET `/api/tasks/usage-total` — 全部任务 token 用量累计

---

## 记忆库(跨会话记忆蒸馏)

`memory_entries` 表按角色维度存储,十端点:

### POST `/api/memory/distill` — 蒸馏指定会话(需开启 `memory_distill_enabled`)

响应含 `{ ok, inserted, skipped, character_id }`。

### GET `/api/memory?character_id=…` — 按角色列出全部记忆(最新在前)

条目字段含 `selected`(是否参与注入)与 `pinned`(常驻置顶,最高注入优先级)。

### GET `/api/memory/search?character_id=&q=&limit=` — 检索记忆条目

FTS5 全文检索(BM25 排序);查询短于 3 字符时后端回退 `LIKE`(trigram 分词器限制)。
`limit` 默认 20、上限 100。响应 `{ memories: [...] }`。

### POST `/api/memory/prune` — 精简记忆条目(硬删除该角色 `selected=0` 的归档条目)

请求 `{ character_id }`;响应 `{ ok, removed }`。

### GET `/api/memory/embedding-status` — 向量索引状态

响应 `{ enabled, model, configured_dim, status: { total, embedded, dim, dim_mismatch } }`。
`dim_mismatch` 非空表示换过模型/维度,需调用重建。

### POST `/api/memory/rebuild-embeddings` — 手动重建向量索引

清空向量表后按批回填全部记忆;需先开启 `embedding_enabled` 且凭据可用。
响应 `{ ok, embedded, dim }`;中途失败返回 `{ error, done }`(已处理条数)。

### POST `/api/memory` — 手动添加(kind='manual')

### PATCH `/api/memory/{id}` — 编辑 content / selected / pinned

### DELETE `/api/memory/{id}` — 删除(204 无正文)

### POST `/api/settings/embedding/test` — 测试向量化连接

嵌入一条固定文本,验证地址/Key/模型是否可用。响应 `{ ok, dim?, latency_ms?, message }`;
成功时回传**实际维度**(可用于回填 `embedding_dim`)。不写库、不改配置。

---

## 插件 / 技能 / 其他

### GET / POST / DELETE `/api/plugins/tools*` — 自定义工具插件(列表 / 重载 / 上传 / 删除)

### GET / POST / PUT / DELETE `/api/skills*` — 技能库(列表 / 导入 / 更新 / 删除)

`SkillRecord` 含 `allowed_tools`(工具白名单)、`run_as_subagent`(是否可作为子智能体技能派发)、`model`(模型覆盖)元数据。

### GET / PUT `/api/audio` — 音频播放器(bgm/ambient 双通道)

### GET `/api/scripts/tree` — 用户脚本树

### GET `/api/slash/commands` — slash 命令列表

### POST `/api/macros/expand` — 宏展开预览

### GET / POST / PATCH / DELETE `/api/quick-replies*` — 快速回复管理

### GET `/api/resource/proxy` — 角色卡远程资源界面代理(https + SSRF 防护)

### GET `/api/repo-index` — 仓库索引(开发辅助)

---

## 会话补充端点

### POST `/api/chat/sessions/{id}/truncate` — 保留 anchor 消息,删除其后所有消息(编辑用户消息后重发)

### POST `/api/chat/sessions/{id}/regreet` — 重新生成开场白

### PUT `/api/chat/sessions/{id}/assistant-vars` — 写入酒馆助手变量树

### PATCH `/api/chat/messages/{id}/variables` — 保存消息级变量

### POST `/api/chat/messages/{id}/swipe` — 切换消息 swipe 版本(extra.swipes)

### GET `/api/chat/init-vars` — 初始化变量

### POST `/api/chat/generate-raw` — 角色卡资源页作者脚本的自由生成桥(一次性、非流式、不写会话)

请求:`{ messages: [{role, content}], character_id?, temperature?, top_p?, max_tokens? }`
(role 仅 `system`/`user`/`assistant`;条数 ≤200、单条 ≤64KB、总长 ≤256KB)。
提供 `character_id` 时按该角色世界书做关键字匹配注入(常驻条目置顶、触发条目插到最后
一条 user 之前)。

响应:`{ ok: true, text, injected: string[] }`。`injected` 为本次注入的世界书条目
comment 清单——卡片诊断时用它区分「世界书未注入(`injected` 为空)」与「输出被截断
(`text` 偏短)」两类不同故障。

**预算与截断自愈**(常量见 `server-rs/src/api/chat.rs`):
- 未显式指定 `max_tokens` 时取「用户设置 vs 8192(`GENERATE_RAW_MIN_TOKENS)」的较大者
  ——结构化输出下限:卡片要求「整个回复有且仅有一个 JSON」,1024 默认值会把 JSON 腰斩
  (2026-09-13 Android「掉格式」根因);显式值原样尊重并钳在 `1..=65536`,
  `max_tokens: 0` 返回 400 `VALIDATION`;
- 上游 `finish_reason=length` 时预算翻倍重发(封顶 32768,最多 2 次);
- **重发失败或自愈用尽时仍返回 200 + 最后一次(半截)文本**:卡片自带解析器可从半截
  文本尽力提取,整体报错反而更无用;此时错误只留服务端日志
  (`generate_raw_retry_failed_fallback_partial` / `generate_raw_still_truncated`)。

**已知限制**:客户端断开不取消在途重发(最多 3 次上游调用);本端点不落 usage/审计,
重发消耗只能从服务端日志统计。

### POST `/api/chat/compact` — 触发上下文压缩(manual)

### POST `/api/chat/compact/clear` — 清除压缩摘要

### GET / PUT / PATCH `/api/variables` — 7 作用域变量(读整树 / 整树覆写 / JSON Patch 子集)

### GET `/api/chat/sessions/{id}/undo` — 回退快照列表

### POST `/api/undo/{id}/restore` — 按快照恢复

---

## 契约引擎 HTTP 出口(方案 A)

供 ST 前端与外部工具调用同一契约引擎(与 Agent 多步工具 `apply_patch` 共用同一管线,单库无分叉)。契约不存在时补丁原样放行(存量卡零行为变化)。

> 并发语义:本出口与 Agent 生成共用存储;外部更新应避免与进行中的生成并发调用,并发时运行态快照以引擎收尾为准(变量表不丢,快照可能短暂回退)。`writer` 是子系统标识而非身份认证——持有 bearer token 的调用方可任意声明 `writer`(含 `"agent"`);所有权校验防的是「子系统误写」,认证边界在 token(与前端同级信任)。

### POST `/api/variable/update` — 契约引擎统一写入

请求:`{ session_id, patches: [{ op, path, value?, from?, confidence? }], writer? }`

- `patches`:JSON Patch 子集(replace/set/insert/delta/remove/move);`confidence` 可选("low"/"medium"/"high",非法值按 low 处理——不写入、进待复核队列);
- `writer`:写者子系统 id,缺省 `"external"`;契约 `updateRules` 的 `ownership.writers` 须包含该 id 或 `"*"`(默认 `[agent, manual]`),否则 `not_owner` 拒绝;
- 门控:未声明字段 `unknown_field` 拒绝、低置信入 pending、契约生效时逐 op 留痕(kaleido_changelog)并维护 meta.pending。

响应(部分被拦仍 200,已生效部分生效):`{ ok, stat_data, entries, warnings?, breaker_hashes? }`;`ok=false` 表示全部被拦(树未变;低置信场景提议已入队,全部被拒场景无任何写入)。请求体结构不符 → 422。

### GET `/api/variable/state?session_id=…` — 契约运行态整行

`{ session_id, contract_version, stat_data, meta, revision_seq, revision_hash, updated_at }`;尚无契约运行态 → 404。

### GET `/api/variable/changelog?session_id=…&limit=50` — 逐 op 变更流水

`{ entries: [ChangelogEntry] }`(最新在前,limit 夹取 [1, 1000])。

---

## 健康检查

### GET `/api/health`

`{ ok: true, ts }`
