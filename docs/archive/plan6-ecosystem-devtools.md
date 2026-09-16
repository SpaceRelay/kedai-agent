# Kedai「计划六:开发者工具 + 优化面板 + 脚本导入导出 + swipe + TavernHelper 更多 API + RENDER」详细设计

> 兼容对象:JS-Slash-Runner「酒馆助手」4.9.1(TavernHelper 完整 API:生成/导入/扩展管理;事件系统 tavern_events;ScriptTree 导入导出;swipe 每页独立变量表)与 ST-Prompt-Template(RENDER 标签语义、宏/变量调试入口)。
> 前置:计划一(EJS 上下文真实化)、计划二(7 作用域变量)、计划三(角色卡脚本 iframe 沙箱、EvalBridge)、计划四(slash/快速回复)、计划五(音频 + 渲染面板)已完成;阶段五遗留「RENDER 标签引擎跳过」与「{{input}}/{{pipe}}/{{first}}/{{last}} 宏」中前者待补、后者已落地。
> 目标(路线图原文):「开发者工具(事件监听/宏调试)、优化面板、脚本导入导出、swipe 变量同步、TavernHelper 更多 API(生成/导入/扩展管理)。」+ 阶段五遗留 RENDER 标签执行(世界书条目 `[RENDER:BEFORE/AFTER]`)。
> 硬约束:不引入 eval/新依赖;注释/文档用简体中文;不提交 git;swipe 与事件系统改动不破坏既有聊天历史/消息 API(向后兼容);扩展管理无真实安装基础设施,以「只读视图 + 明确不支持」落地。
> 本设计中「事实」指代码中可直接引证的行为(附 文件:行号);「建议」指基于经验的判断(标注 [建议])。

---

## 1. 背景结论(基于探索)

| 候选项 | 现状(事实) | 缺口 | 参考实现 |
|---|---|---|---|
| RENDER 标签 | 解析/渲染已完成,仅 engine 丢弃渲染结果(`agents/engine/mod.rs:916-919`) | 补 match 分支并入生成注入 | ST-Prompt-Template `handler.ts:476-580`(before+正文+after 拼接) |
| 宏调试 | `expand_macros` 仅内部调用(`macros.rs:33-80`),无预览 API;{{input}}/{{pipe}}/{{first}}/{{last}} 已实现(`:119-127`) | 新建展开预览端点 + 前端面板 | ST `/ejs` 命令(`command.ts:10-65`) |
| 事件监听 | SSE 单次流不落库、无缓冲(`api/chat.rs:234,258-264`;`send_event` `mod.rs:233-249`);前端仅 Mvu eventBus 一条事件(`mvu/host.ts:30-41,132-139`) | 后端事件缓冲 + recent API;前端事件日志面板 | 酒馆 `tavern_events` 90+ 事件(`@types/iframe/event.d.ts:188-276`) |
| 优化面板 | prompt-preview 已存在(分层脱敏,`api/settings.rs:412-589`),前端埋在 agent 分区(`SettingsModal.vue:381-397`) | 独立面板 + 一键推荐设置 + token 估算 | 酒馆 `Optimize.vue`(一键开关)+ `PromptViewer`(dry-run) |
| 脚本导入导出 | ScriptTree 仅 global/character 两级(`api/user_scripts.rs:23-50,52-86`);无导出端点;前端 ScriptsModal 无按钮 | 导出端点 + 导入(复用 PUT)+ 前端按钮 | 酒馆脚本导出 JSON(`@types/function/script.d.ts:11-37`) |
| swipe | 后端 `MessageRecord` 无多版本字段(`models/types.rs:140-147`);前端零 swipe 代码 | 消息模型扩 swipes + 端点 + 前端滑动 + 每页变量 | 酒馆 `ChatMessageSwiped`(`chat_message.d.ts:11-20`)、`msg.variables[swipe_id]` |
| TavernHelper 更多 API | 桥已有变量/slash/eventOn(内存表未接引擎,`bridge.rs:75-144`);音频 8 个仅前端沙箱 | 生成类(接引擎)、导入类(映射现有 import 端点)、扩展管理(只读) | `generate.d.ts` / `import_raw.d.ts` / `extension.d.ts` |

**总判断**:阶段六 7 个子项里,RENDER、宏调试、脚本导入导出、优化面板为「补强现有生态」;事件监听、swipe、TavernHelper 生成类为「从零新建」且涉及引擎/消息模型改造。按「先补漏、再补强、后新建」顺序分 6a~6g 七条独立可交付线推进。

---

## 2. 交付物总览

| 子阶段 | 交付物 | 规模 | 独立可测 |
|---|---|---|---|
| 6a RENDER 标签 | engine match 补 RenderBefore/RenderAfter 分支(并入 generate_before/after) | 小 | ✅ engine 集成测试 |
| 6b 宏调试 | `POST /api/macros/expand` + 前端宏调试面板 | 小-中 | ✅ 后端单测 + 前端单测 |
| 6c 事件监听 | 后端事件环形缓冲 + `GET /api/events/recent` + 前端事件日志面板 | 中 | ✅ 后端集成 + 前端单测 |
| 6d 优化面板 | 独立优化面板(提示词检查 + 一键推荐设置 + 上下文健康度) | 中 | ✅ 前端单测 |
| 6e 脚本导入导出 | `GET /api/scripts/export` + 导入复用 PUT + ScriptsModal 按钮 | 小-中 | ✅ 后端集成 + 前端单测 |
| 6f swipe | 消息模型扩 swipes + swipe 端点 + 前端滑动 + 每页变量同步 | 大 | ✅ 后端集成 + 前端单测 |
| 6g TavernHelper 更多 API | 生成类(静默生成)+ 导入类(importRaw*)+ 扩展管理(只读) | 中-大 | ✅ 桥单测 + 集成 |
| 文档 | `docs/archive/plan6-ecosystem-devtools.md`(本文件) | — | — |

---

## 3. 6a RENDER 标签执行(阶段五遗留补漏)

### 3.1 现状

`InjectTag` 枚举已含 `RenderBefore/RenderAfter`(`parsing/assistant/inject_tag.rs:18-35`),`parse_inject_tag` 可解析 `[RENDER:BEFORE]/[RENDER:AFTER]`(`:39-59,92-99`),`collect_generate_entries_with` 已渲染条目内容(`:131-158`);但 engine 的 GENERATE 分类 match(`agents/engine/mod.rs:882-921`)把 `RenderBefore/RenderAfter` 与 `InitialVariables` 一起丢进 `_` 分支(`:916-919`)。

### 3.2 设计

- `agents/engine/mod.rs:886-921` 的 match 补两分支:`InjectTag::RenderBefore => generate_before.push(ge.rendered.clone())`、`InjectTag::RenderAfter => generate_after.push(...)`(与 `GenerateBefore/After` 同路);`InitialVariables` 保持跳过(已由 init 处理)。
- 语义说明(事实):Kedai 无独立「显示渲染阶段」(消息显示走 markdown/渲染面板,世界书条目不进显示管道),故 RENDER 并入生成注入(system 首/尾),与酒馆「仅影响显示、不影响生成」语义有差异——**写入风险表**,前端显示渲染留扩展位。
- 渲染深度/容错沿用现有 `render_assistant_content_with`(`inject_tag.rs:146`),无需新基础设施。

### 3.3 测试

`tests/assistant.rs` 或 worldbook 相关用例补一条:RENDER:BEFORE 条目内容出现在 system 开头、RENDER:AFTER 出现在末尾(复用 `worldbook_decorators_gate_injection_and_write_vars` 模式);`InitialVariables` 仍不注入。

---

## 4. 6b 宏调试(展开预览 API + 面板)

### 4.1 现状

`expand_macros(text, &mut MacroCtx)` 单遍扫描(`macros.rs:33-80`),`MacroCtx`(`:13-30`)含 character/user/history/vars/scopes;调用方为引擎消息构建层与聊天历史显示层(`api/sessions.rs:282`)。无对外预览端点。

### 4.2 后端

- 新增 `api/macros.rs`:`POST /api/macros/expand`,body `{text, ctx?}`(ctx 可选:character_name/user_name/history 摘要/当前会话变量快照;缺省空);内部 `expand_macros` 展开后返回 `{expanded}`。未知宏保持原样(既有策略),错误(如深递归)回退原文。
- 数据来源:ctx 由调用方传(不自动读引擎内部,避免耦合);会话变量快照可经现有 `ScopeVars::view`(`bridge.rs:58-69` 同源)读取。**脱敏**:不返回历史正文,只接受调用方显式传入的 ctx 字段。
- 路由:`api/mod.rs` 注册(挂在 `/api/scripts/tree` 附近)。

### 4.3 前端

- `api/macros.ts`:`expandMacros(text, ctx?)`;types 加 `MacroExpandRequest/Result`。
- 宏调试面板(开发者工具内,见 6c):模板输入框 + 实时展开预览(防抖 300ms 自动请求)+ 错误/未知宏提示 + 常用宏速查(复用 `MACRO_HINTS`)。

### 4.4 测试

后端:`macros.rs` 已有 `st_compat_macros`(`:474-498`)回归;新增 handler 单测(展开成功/未知宏原样/ctx 传参)。前端:api 封装单测 + 面板展开逻辑纯函数单测。

---

## 5. 6c 事件监听面板

### 5.1 现状

`SseEvent` 枚举 10 种(`models/types.rs:334-380`),发射统一走 `send_event(event, tx, abort, flag)`(`mod.rs:233-249`),但 SSE 是单次请求流(`api/chat.rs:258-264`),事件不落库、无缓冲、无多播。前端 `Mvu.on/off/emit` eventBus(`mvu/host.ts:30-41,132-139`)仅 `mag_variable_updated` 一条,无 UI 订阅。

### 5.2 后端

- 事件缓冲:`AppState` 加 `events: Arc<Mutex<EventLog>>`,`EventLog { buf: VecDeque<(ts, SseEvent)>, cap: 200 }`(写满丢弃最旧);`send_event` 追加写缓冲(原子,失败不阻断 SSE)。
- 新端点 `GET /api/events/recent?limit=50` → `{events: [{ts, type, payload}]}`(payload 按类型序列化;Token 事件截断到 200 字符防刷屏)。
- **不做实时多播订阅**(SSE 订阅留扩展位,风险表);前端 1s 轮询 recent,配合 chat/send 的实时 SSE 已有感知。

### 5.3 前端

- 开发者工具面板(新 Modal,仿 ScriptsModal 左右分栏 `:332-337` 布局):tab1 事件日志(时间/类型/载荷 JSON,类型过滤、清空、暂停轮询)、tab2 宏调试(6b)、tab3 沙箱状态(可选)。
- 挂载:App.vue 加 `store.devtoolsOpen` 开关 + Sidebar/SettingsHub 入口(仿 quickActions `SettingsHub.vue:30-41`)。
- 事件类型名映射到中文标签(如 `Token→文本流`)。

### 5.4 测试

后端:`/api/events/recent` 集成测试(空缓冲/写入后读取/limit/cap 截断)。前端:事件过滤/格式化纯函数单测。

---

## 6. 6d 优化面板

### 6.1 现状

prompt-preview 已可用(`api/settings.rs:412-589`,分层脱敏输出),前端在 SettingsModal agent 分区(`SettingsModal.vue:381-397`);token 估算 API 已有(`api/tokens.rs` `countTokens`);顶栏已有缓存命中率(`ChatWindow.vue:519-523`);RuntimeSettings 全量字段(`settings_service.rs:12-62`)经 `PUT /api/settings` 部分 patch 保存(`api/settings.rs:21-73`)。

### 6.2 设计(前端为主,后端零新增或仅静态推荐定义)

- 新 Modal「优化面板」(`store.optimizeOpen` + App.vue 挂载,仿 SettingsHub 分区壳):
  - **提示词检查**:完整 prompt-preview 分层展示(迁移 agent 分区),每层附 token 估算(调 `countTokens`)+ 层来源/顺序,保留脱敏说明。
  - **一键推荐设置**:一组 Kedai 实际设置项的推荐开关(对齐酒馆 Optimize 模式):`mvu_vars_position=system`(缓存友好)、`render_html`、`bypass_mode`、`max_tool_rounds`、`max_context_tokens` 上限提示——每项带说明,一键套用走 `store.saveSettings`(串行队列 `queueSettingsSave` `store.ts:585-590` 已有)。
  - **上下文健康度**:缓存命中率(顶栏 hitRate 数据源)+ 最近请求 token 消耗展示。
- 后端:无需新端点;若做「推荐设置」静态定义放前端常量即可。

### 6.3 测试

前端:推荐设置应用逻辑纯函数(单测:合并 patch、仅改动勾选项);面板组件无渲染测试(项目无 @vue/test-utils),保持纯函数风格。

---

## 7. 6e 脚本导入导出

### 7.1 现状

ScriptTree 结构(`user_script_service.rs:1-7`)对齐酒馆 script.d.ts;`GET/PUT /api/scripts/tree?scope=global|character`(`api/user_scripts.rs:23-50,52-86`);`validate_trees`/`ensure_defaults`(`:210-253`)已含校验与默认补全,但无导出端点、无 preset 级(留扩展位)。

### 7.2 设计

- 后端新增 `GET /api/scripts/export?scope=&character_id=` → `{scope, trees}`(仿 GET tree,便于前端下载;`export_with` 过滤在导出时应用——仅导出 data/button 时剥离 content 等)。导入**复用既有 PUT `/api/scripts/tree`**(前端本地解析 + 校验 + id 重分配后全量 PUT,服务端 `validate_trees` 把关),不新增 import 端点 [建议,最小改动]。
- 前端 `api/scripts.ts` 加 `exportScriptTree(scope, characterId?)`(GET 全量);ScriptsModal 标题栏加「导出 / 导入」按钮:
  - 导出:Blob 下载 JSON(文件名 `脚本树-<scope>.json`),仿 `useAgentFlow.ts:169-185` 的 `exportFlowNow`。
  - 导入:隐藏 file input 读 .json(`useAgentFlow.ts:128-166` 的 `onFlowImport` 模式:JSON.parse → 校验 type ∈ {script,folder} → 重设 id 防撞车 → `enabled` 保持 → `saveScriptTree` 全量 PUT)。
- 导出支持「仅 data/button」勾选(映射 `export_with` 字段,`types.ts:243-246` 已有)。

### 7.3 测试

后端:`/api/scripts/export` 集成测试(global/character 两级、树结构 roundtrip)。前端:导入解析 + id 重分配纯函数单测(仿 useAgentFlow 既有测试风格)。

---

## 8. 6f swipe 变量同步

### 8.1 现状(从零新建)

后端 `MessageRecord`(`models/types.rs:140-147`)无多版本字段;`/api/chat/history`(`api/sessions.rs:205-291`)返回单版本 + `content_display`(显示层宏展开,不落库 `:282-285`);前端 `UiMessage`(`sseReducer.ts:44-53`)、消息操作区(`ChatWindow.vue:557-563,592-595`)、`resendMessage` 截断重生成管线(`store.ts:401-457`)均无 swipe 概念。酒馆参考:`ChatMessageSwiped`(`chat_message.d.ts:11-20`)、每页 swipe 独立变量表(`msg.variables[swipe_id]`)、`MESSAGE_SWIPED` 事件(`event.d.ts:288`)、ST `handleSwipeDeleted`(`handler.ts:944-958`)。

### 8.2 后端

- **消息模型**:`MessageRecord.extra` 扩承载:`swipes: Vec<String>`(版本列表,首元素 = 原始回复)+ `swipe_id: usize`(当前版本索引,0 基)。`GET /api/chat/history` 返回含 `swipes`(兼容:无则空数组)。
- **swipe 端点**:`POST /api/chat/messages/{id}/swipe`,body `{direction: 'left'|'right'}`(或 `{swipe_id}`)→ 越界钳制、更新 `extra.swipe_id`(不落库仅当前会话内存?——**落库**:`MessageRecord.extra` 更新经现有 `updateMessage` 路径,保证刷新/重进保留);返回 `{content, content_display, swipe_id}`。
- **变量同步**:消息变量按 swipe_id 每页独立——变量树里消息作用域键带 swipe 维度(对齐酒馆 `msg.variables[swipe_id]`);swipe 切换时前端重放该页变量(`replayMvuVariables` 复用,`store.ts:401-457` 已用)。后端 `ScopeVars` 消息作用域按 `(message_id, swipe_id)` 索引;swipe 删除时同步清理对应变量表(对齐 ST `handler.ts:944-958`)。
- **新版本生成**:「生成新 swipe 版本」= resend 变体:保存当前回复进 `extra.swipes` → 重新生成 → 新结果追加为最新版本(不替换)。引擎 `chat/send` 加 `swipe: true` 请求标志;实现沿用 `resend_message_id` 管线(`chat.rs`/`store.ts:401-457`),生成完成后写回 swipes。

### 8.3 前端

- 类型:`ChatMessage` 加 `swipes?: string[]`、`swipe_id?: number`;`UiMessage` 透传。
- assistant 消息操作区(`ChatWindow.vue:592-595`)加左右箭头(有 >1 版本时显示 + 当前 `swipe_id` 角标):点击 → `store.swipeMessage(id, direction)` → POST swipe → 本地更新 content/content_display + 重放该页变量。
- **「生成新版本」按钮**:调用 `store.resendMessage` 的 swipe 变体(保存旧版本 → 重新生成),生成中显示「swipe 生成中」状态。
- `MESSAGE_SWIPED` 事件名加入 6c 事件表(前端可监听)。

### 8.4 测试

后端:swipe 端点集成测试(切换/越界钳制/落库 roundtrip/每页变量隔离/生成新版本后 swipes 追加);历史 API 兼容(无 swipes 字段旧消息不破坏)。前端:swipe reducer/索引切换纯函数单测。

---

## 9. 6g TavernHelper 更多 API

> 桥接模式沿用阶段五音频先例:宿主 controller 注册表(`audioController.ts:35-42`)+ 沙箱 `sandboxScript` 注入(`characterScriptSandbox.ts:191-223`)+ postMessage op 白名单(`:428-462`)。后端 EvalBridge 侧走 Rust 闭包注入(`bridge.rs:75-144`)。

### 9.1 6g-1 生成类(静默生成)

- **契约**(`generate.d.ts:146,225-311`):`generate(config) → Promise<string>`,`stopGenerationById(id)`,`stopAllGeneration()`。
- **设计(受限落地 [建议])**:EvalBridge 加 `generate` handler → 经异步通道调引擎生成一段文本返回(不入聊天记录,对齐 `should_silence` 语义);`generation_id` 注册表支持 `stopGenerationById`。**限制**:上下文用当前会话当前配置、无流式返回、无 tools/json_schema 覆盖——完整 `GenerateConfig`(custom_api/tools/json_schema)标不支持返回错误,留扩展位。引擎接入点:`run_character_scripts`(`mod.rs:1801-1846`)构造 bridge 处挂 handler;生成走既有 connector 单发调用(复用引擎内部生成函数,不改 SSE 管线)。
- 前端沙箱:TavernHelper 加 `generate/stopGenerationById`(沙箱内 Promise;postMessage RPC → 宿主 → 后端)。
- 测试:桥单测(generate 返回文本/stop 命中)、集成(静默生成不写聊天记录)。

### 9.2 6g-2 导入类

- **契约**(`import_raw.d.ts:12-66`):`importRawCharacter(filename, content: Blob)`、`importRawChat`、`importRawPreset`、`importRawWorldbook`、`importRawTavernRegex`。
- **设计(映射现有端点)**:后端 EvalBridge 加 5 个 handler,映射到已有能力:`importRawCharacter` → `/api/characters/upload`(multipart 重组)、`importRawChat` → `/api/import/chat`(已存在 `import_export.rs:50-76`)、`importRawWorldbook` → `/api/world-books/upload`、`importRawPreset` → `/api/prompt-inject/import`(ST 预设,`api/settings.rs` 已有);`importRawTavernRegex` 无对应正则导入端点——**返回「暂不支持」**(留扩展位)。
- 沙箱 RPC:postMessage 传文本/ArrayBuffer,宿主端重组 Blob 调现有导入流程(Blob 无法跨窗口直接传,需宿主重组 [建议])。
- 测试:桥 handler 映射单测(合法/非法参数);character 导入集成复用既有上传用例。

### 9.3 6g-3 扩展管理

- **契约**(`extension.d.ts:2-104`):`isAdmin/getTavernHelperExtensionId/getExtensionType/isInstalledExtension/installExtension/uninstallExtension/reinstallExtension/updateExtension/getExtensionInstallationInfo`。
- **设计(只读视图落地)**:Kedai 无扩展安装基础设施(插件=工具 json/技能库)。EvalBridge 加:`isAdmin → true`(本机单用户)、`getTavernHelperExtensionId → 'kedai'`、`isInstalledExtension → false`(无外部扩展)、`getExtensionType → null`、`install/uninstall/reinstall/update/getExtensionInstallationInfo → 返回错误「Kedai 不支持扩展安装」`(对齐 `Scope::Extension` 作用域无数据源的事实,`bridge.rs:67,180-182`)。可选:`getExtensionList`(非酒馆 API)映射到「已加载工具插件/技能」只读清单 [建议]。
- 测试:桥单测(各 API 返回值/错误)。

---

## 10. 测试计划

| 位置 | 内容 | 命令 |
|---|---|---|
| `server-rs` lib 单测 | 宏 expand handler、swipe 索引/变量隔离纯逻辑、桥生成/导入/扩展管理 handler | `cargo test`(需先加载 vcvars64,见 MAINTENANCE.md:99-101) |
| `server-rs` 集成 | RENDER 条目注入、`/api/events/recent`、`/api/macros/expand`、`/api/scripts/export`、swipe 端点 + 历史兼容、静默生成不写记录 | 同上 |
| `web` 单测 | 宏展开请求封装、事件过滤/格式化、优化推荐设置合并、脚本树导入解析 + id 重分配、swipe 索引切换 | `npm test` |
| 回归 | 既有 393+34+11(后端)与 158(前端)不破坏;swipe/事件改动后跑全量 | 同上 |
| 构建 | 全量类型检查 + 打包 | `npm run build` |

## 11. 交付顺序与依赖

1. **6a RENDER**(小,阶段五遗留,纯后端)→ 2. **6e 脚本导入导出**(复用现有 tree 端点,独立)→ 3. **6b 宏调试**(独立)→ 4. **6c 事件监听**(后端缓冲独立;6f 的 MESSAGE_SWIPED 事件名依赖其事件表,可先行)→ 5. **6d 优化面板**(复用 prompt-preview,独立)→ 6. **6g TavernHelper 更多 API**(6g-2 依赖现有导入端点;6g-1 依赖引擎生成接口)→ 7. **6f swipe**(最大,依赖 6c 事件表命名与消息模型;最后做)。

每条线后端→前端→测试纵向推进,互不阻塞;RENDER/宏调试/脚本导入导出可最先交付。

## 12. 风险与边界

| 风险/边界 | 处理 |
|---|---|
| RENDER 语义差异 | Kedai 无独立显示渲染阶段,并入生成注入(system 首/尾);与酒馆「仅影响显示」语义的差异写入文档,前端显示渲染留扩展位 |
| 事件实时多播 | 本轮只做环形缓冲 + recent 轮询;SSE 多播订阅留扩展位(事件表结构兼容) |
| swipe 落库 | `extra` 扩字段向后兼容(旧消息无 swipes=单版本);不新增列,避免 DB 迁移 |
| swipe 生成新版本 | 复用 resend 管线加 `swipe` 标志;生成失败回滚(旧版本保留) |
| 生成类 API 受限 | generate 仅静默单发;custom_api/tools/json_schema 覆盖返回「不支持」,留扩展位 |
| 扩展管理无基础设施 | 只读视图 + 明确不支持错误;不伪造安装能力 |
| importRawTavernRegex 无对应端点 | 返回「暂不支持」,正则导入留扩展位 |
| 沙箱 Blob 跨窗口 | postMessage 传文本/ArrayBuffer,宿主端重组 Blob;受限尺寸(≤1MB,复用 `MAX_MESSAGE_BYTES` 先例) |
| 不引入 eval/新依赖 | 宏展开沿用 `expand_macros`;生成类走既有 connector;桥接沿用 postMessage RPC 白名单 |

---

## 13. 6a/6b 实施记录

### 13.1 6a RENDER 标签执行(已交付)

- **改动**:`server-rs/src/agents/engine/mod.rs` GENERATE 分类 match(现 `:886-927`)补两分支——`RenderBefore` 并入 `generate_before`、`RenderAfter` 并入 `generate_after`(与 `GenerateBefore/After` 同路);`_` 分支仅保留 `InitialVariables` 防御注释(该标签已由 `collect_generate_entries_with` 分流,不进 `generate_entries`)。消费点 `finalize_messages`(`:1125-1139`)无需改动:`generate_before` 拼 system 开头、`generate_after` 拼 system 末尾(计入 `protected_tail`)。
- **语义差异注记**(与酒馆原版差异,写入代码注释与本节):酒馆 `RENDER:BEFORE/AFTER` 仅影响显示渲染、不影响生成;Kedai 无独立显示渲染管道(世界书条目不进消息显示管道),故并入生成注入(system 首/尾)。前端显示渲染留扩展位。
- **测试**:`server-rs/tests/assistant.rs` 新增集成测试 `render_tag_injects_into_system_edges`(仿 `ejs_context_reads_world_quickreply_history_and_injects` 模式):内嵌 `[RENDER:BEFORE]`/`[RENDER:AFTER]` 两条 enabled 常驻条目,`[[floors]]` 回显断言 BEFORE 内容在 system 开头、AFTER 内容在 system 末尾、BEFORE 先于 AFTER、`[RENDER` 标签源码不泄漏。
- **回归**:既有 `parsing/assistant/mod.rs:462-463`(标签解析)与 `:525-559`(collect 拆分渲染)测试不破坏。

### 13.2 6b 宏调试(已交付)

- **后端**:新增 `server-rs/src/api/macros.rs`(`POST /api/macros/expand`):
  - body `{text, ctx?}`;`MacroExpandCtx` 全可选(serde default):`character_name/character_description/personality/scenario/user_name/user_input/history:[{role, content}]/vars:{key: value}`。
  - handler 组装 `MacroCtx`(模板照抄 `api/sessions.rs:270-281`;`assistant_vars: None`、`scopes: None`——纯扁平 vars 展开,不耦合引擎);`vars` 值经 `value_to_display` 序列化(字符串原样、数字/布尔走 JSON)。返回 `{expanded}`;空 text 返回空串;未知宏保持原样(既有 `expand_macros` 策略);变量写入({{setvar}}/{{addvar}})仅限单次请求,不跨请求持久化。
  - 路由:`api/mod.rs` 注册 `.route("/api/macros/expand", post(macros::expand))`(挂在 `/api/slash/commands` 之后)。
  - 测试:新增 `server-rs/tests/macros.rs`(`build_test_app` + POST json 集成):常见宏 + ctx 传参展开、`{{random}}`/`{{roll:1d6}}` 范围、未知宏原样、空 text、`history` 对 `{{firstMessage}}`/`{{lastMessage}}`/`{{lastUserMessage}}` 生效、`{{setvar}}` 不跨请求残留。
- **前端**:
  - `web/src/api/macros.ts`:`expandMacros(text, ctx?)`(对齐 scripts.ts 封装)+ 面板变量区解析纯函数 `parseVarsLines`(每行 `key::value` → map;空行/无 `::`/key 空忽略;值类型推断 true/false → boolean、纯整数 → number、其余字符串;仅按第一个 `::` 分割)。
  - `web/src/api/types.ts` 加 `MacroHistoryItem/MacroExpandCtx/MacroExpandResult`;`index.ts` 聚合导出。
  - `web/src/components/MacrosModal.vue`(新,仿 QuickRepliesModal 骨架):模板 textarea + 「展开」按钮(手动触发,避免击键防抖复杂度)+ 可选上下文区(角色名/用户名 + 变量文本区)+ 展开结果只读展示 + 「未知宏保留原样」提示 + 常用宏速查(复用 `usePromptInject.ts` 的 `MACRO_HINTS`,`join(' · ')` 展示)。
  - `store.ts`:`macrosOpen = ref(false)`(:110 附近)+ return 导出(仿 `scriptsOpen`);`App.vue` `<MacrosModal v-if="store.macrosOpen" />`;`Sidebar.vue` 脚本管理按钮后加「宏调试」入口按钮。
  - 测试:`web/src/api/macros.test.ts`(URL/方法/body 序列化/错误抛出,仿 scripts.test.ts 的 fetch mock + `resetApiTokenForTest`)+ `parseVarsLines` 纯函数单测(类型推断/忽略行/`::` 分割/空白去除)。
- **边界**(落实情况):ctx 无 `scopes/assistant_vars`——6b 用扁平 vars 最简展开;scopes/变量树维度留 6c 事件面板扩展位。面板手动「展开」按钮(非防抖自动),避免击键发请求。大 text/深递归沿用 `expand_macros` 既有单遍扫描与未知宏保留策略,无新增爆炸风险。
- **验证**:后端 `cargo test`(新增 `tests/macros.rs` 7 例 + `api/macros.rs` 1 例,既有 393+34+11 回归);前端 `npm test` 165 全过(基线 158 + 新增 7)+ `npm run build`。

### 13.3 6c 事件监听面板(已交付,前端采集简化)

- **设计决策(相对 plan6 简化)**:plan6 原设计为后端事件环形缓冲 + `GET /api/events/recent` + 前端轮询。探索确认 `send_event` 是自由函数(21 处调用点 + 2 处直发,`executor.rs` 需签名透传),后端缓冲侵入大——**改前端采集**:`store.ts` `onSseEvent` 入口一处埋点可采集 100% 事件(含本地合成事件,stop/interrupted 都流经此处),零后端改动。后端 recent API 留扩展位。
- **改动**:
  - `web/src/store.ts`:新增 `eventLog = ref<ApiEventLogEntry[]>([])`(cap 500 丢最旧,含 ts/session_id/event)+ `eventsOpen`;`onSseEvent` 函数体开头 push 采集。
  - `web/src/devTools.ts`(新,纯函数):`EVENT_TYPE_LABELS`(9 类中文标签:token→文本流、step→步骤、tool_call→工具调用、tool_authorization_required→工具待授权、tool_result→工具结果、vars→变量更新、interrupted→中断、error→错误、finish→完成)+ `eventTypeLabel`(未知回退原样)+ `filterEventLog`(类型/会话过滤)+ `truncatePayload`(长载荷截断)。
  - `web/src/components/DevToolsModal.vue`(新):类型过滤下拉 + 暂停/恢复实时采集(冻结快照)+ 清空 + 仅当前会话过滤 + 时间格式化 + 载荷 JSON 折叠展开。
  - 入口:`App.vue` 挂载 `<DevToolsModal v-if="store.eventsOpen" />`;`SettingsHub.vue` quickActions 加「事件监控」。
- **测试**:`web/src/devTools.test.ts`(10 例):9 类映射全覆盖/未知回退、类型/会话/组合过滤、截断限长 + 省略号。
- **边界**:事件日志仅前端采集——刷新页面丢失、仅当前会话期;后端 recent API(环形缓冲 + send_event 全链路透传)留扩展位。

### 13.4 6d 优化面板(已交付,纯前端)

- **改动**:
  - `web/src/contextStats.ts`(新,纯函数):`computeHitRate(u)`(命中率 = 缓存命中 token ÷ prompt token,钳制 100 内;无用量/prompt_tokens=0 → null;抽自 `ChatWindow.vue:355-360`,顶栏与面板共用)+ `RECOMMENDED_SETTINGS`(5 项:变量状态注入 system、开启安全 HTML 渲染、放行模式、工具循环轮次 32、上下文窗口 64K,每项带说明)+ `buildRecommendedPatch(selected)`(仅勾选键合并 patch)。
  - `web/src/components/ChatWindow.vue`:命中率计算改为 `computeHitRate(store.lastUsage)`(行为不变)。
  - `web/src/components/OptimizeModal.vue`(新):提示词检查(复用 `useAgentPromptEditor` 的 promptPreview/previewLoading/loadPromptPreview + 搬 `SettingsModal.vue:388-396` layers 渲染循环;每层 content 调 `api.countTokens` 显示 token 估算,懒加载 + 失败静默)+ 一键推荐设置(checkbox + 说明,「一键套用」合并勾选项 → `store.queueSettingsSave` 串行保存 + 反馈)+ 上下文健康度(命中率 + `store.lastUsage` 四类 token 展示)。
  - 入口:`store.ts` 加 `optimizeOpen`;`App.vue` 挂载 `<OptimizeModal v-if="store.optimizeOpen" />`;`SettingsHub.vue` quickActions 加「优化面板」。
- **测试**:`web/src/contextStats.test.ts`(9 例):命中率 null/0/100/四舍五入/钳制 + 推荐设置合并(勾选项仅含勾选键/空选/全选)。
- **边界**:推荐设置仅「一键套用」时应用勾选项(不自动改);走串行队列防交错;countTokens 层数有限、失败静默。

### 13.5 6e 脚本导入导出(已交付,前端为主简化)

- **设计决策(相对 plan6 简化)**:plan6 原设计后端加 `GET /api/scripts/export`。探索确认 `GET /api/scripts/tree` 已返回完整 trees、`PUT` 全量保存已存在——**导出 = 前端调 `getScriptTree` + 下载;导入 = 本地解析 + 重分配 id + 既有 `saveScriptTree` PUT**,后端零新增。
- **改动**:
  - `web/src/scriptTreeIO.ts`(新,纯函数):`parseScriptTreeImport(raw)`(顶层数组校验、script/folder 字段校验、**递归重分配 id**——仿 onFlowImport 恒分配新 id 避免与既有脚本撞车;非法抛中文错误)+ `stripByExportWith(tree)`(按酒馆 export_with 语义:data=false 清空 data、button=false 清空 button;仅 data/button 时剥离 content/info 保留结构)。
  - `web/src/components/ScriptsModal.vue`:标题栏 ✕ 前加「仅 data/button」勾选 + 导出/导入按钮组;导出 = `getScriptTree` → (勾选时 `forceExportWith` + `stripByExportWith`)→ `saveExportFile`(Tauri 保存框/浏览器下载,文件名 `脚本树-<scope>[-仅data-button].json`);导入 = 隐藏 file input → `JSON.parse` → `parseScriptTreeImport` → 替换列表 + `imported` 提示「点保存脚本写入」;保存按钮文案随导入态变化。
- **测试**:`web/src/scriptTreeIO.test.ts`(7 例):合法解析 + 递归 id 重分配(两次解析 id 均不同)、enabled 缺省 true、非法输入抛错(非数组/未知 type/缺字段)、export_with 剥离(缺省保留/data/button 剥离/文件夹递归)。
- **边界**:导入 id 冲突经递归重分配规避;仅 data/button 导出导入后脚本不可执行但结构保留(对齐酒馆 export_with 语义)。
- **验证**:三条线后端均零改动(cargo test 不重跑);前端 `npm test` 191 全过(165 + devTools 10 + contextStats 9 + scriptTreeIO 7)+ `npm run build` 通过。

### 13.6 6f/6g 实施记录(已交付)

#### 6f swipe 多版本 + 生成新版本

**swipe 数据模型(消除 plan6 原文歧义)**:
- `extra.swipes`:全版本数组,元素 `{swipe_id, content, ts}`,**含激活版本**;`extra.swipe_id`:激活索引(下标)。
- 首次生成不写 swipes/swipe_id(旧消息=单版本,前端隐藏切换 UI,向后兼容)。
- regenerate:swipes = [原历史(或旧 content)] + [新 content],`extra.swipe_id = len-1`,content 列存新文本。
- 切换:content 列更新为对应版本 + `extra.swipe_id = n`;GET /history 零改动(extra 原样透传)。

**regenerate 锚点语义(关键修正)**:
- **锚点 = 当前会话最后一条 assistant 消息**(独立于 resend_message_id 的 user 锚点;两锚点互斥,同时传 400)。
- **引擎收尾原地更新原消息行**(id 稳定、swipes 挂靠),**前端不清除该消息**——plan6 原文 6f-2 的「先 truncateMessages 删除该 assistant 消息」与 6f-1 的「更新原行」自相矛盾,实施时确认采用 6f-1(后端更新原消息)并同步修正前端:清空 content + streaming=true,流式 token 直接追加到该行(sseReducer token 事件追加最后一条 assistant 的既有行为恰好命中);仅当该消息后异常残留消息时才防御性 truncate。
- 旧内容并入 swipes 时与最后一条版本去重(相同文本不重复追加,避免 mock 确定性输出下重复 regenerate 无限膨胀)。

**改动(实际落点,plan6 行号有迁移)**:
- `server-rs/src/api/chat.rs`:`SendBody` 加 `regenerate_assistant_id`;send handler 消息写入改三路分支(regenerate 优先 → resend → 常规);`AgentRunRequest` 透传。
- `server-rs/src/agents/engine/mod.rs`:`AgentRunRequest` 加字段;收尾 extra 组装后分支,regenerate 走新方法 `upsert_regenerated_message`(读原行 extra → 合并 swipes → `update_message_full` 整行更新),首次生成走既有 `add_message`。
- `server-rs/src/services/session_service.rs`:新增 `update_message_full`(content + 整 extra 替换,不标记 edited,区别于编辑语义的 update_message)。
- `server-rs/src/api/sessions.rs` + `api/mod.rs`:新增 `POST /api/chat/messages/{id}/swipe`(Query session_id,body `{swipe_id}`):会话内校验 → 读 extra.swipes → 无/越界 400 → `update_message_content` + `merge_message_extra` → 返回 `{content, swipe_id, swipes_count}`。
- `web/src/api/chat.ts`:`ChatStreamPayload` 加 `regenerate_assistant_id?`;`web/src/api/sessions.ts` 加 `swipeMessage`。
- `web/src/sseReducer.ts`:导出纯函数 `swipeIndex(extra)` / `swipeCount(extra)`(单版本回退 -1 / 0)。
- `web/src/store.ts`:`regenerateMessage(id)`(锚点校验 + 防御性 truncate + 变量回滚复用 resendMessage 管线 → 清空内容进流式 → `startStream(userText, { regenerateAssistantId })`);`swipeMessage(id, swipeId)`(调 API → 更新本地 content/extra,清 content_display);`startStream` opts 扩展。
- `web/src/components/ChatWindow.vue`:assistant 操作区加「◀ n/N ▶」(N>1 才显示)+「生成新版本」按钮;`web/src/style.css` 加 `.sv-msg-action-label` 与 disabled 态。

**测试**:`server-rs/tests/swipe_regenerate.rs`(新增):regenerate 原地更新(id 不变/不新增行/swipes 追加/去重)、锚点校验(user 锚点 409/不存在 409/双锚点 400)、swipe 端点(切换 roundtrip/越界 400/单版本 400/history 反映切换);前端 `sseReducer.test.ts` 加 swipe 纯函数 3 例。

#### 6g TavernHelper 更多 API(只扩展后端 bridge)

**两套脚本系统分界**:ScriptTree 用户脚本 = server-rs 后端执行(`src/scripts/bridge.rs` + `runtime.rs`,rquickjs 0.12);角色卡状态栏脚本 = 前端 iframe 沙箱(`web/src/characterScriptSandbox.ts`)。6g 只扩展后端 bridge,前端沙箱不改。

**6g-1 生成类(静默单发)**:
- `engine/mod.rs` 新增 `generate_text(&self, messages, params, abort) -> Result<(String, TokenUsage), String>`(复刻 `reflector_integration.rs` generate_reflect_advice 模式:非流式 `connector.generate` + 拼 Token chunks + 累加 Usage;不入聊天记录、不推 SSE)。
- `scripts/bridge.rs`:`GenerateHandler` 类型 + `EvalBridge.generate: Option<Arc<GenerateHandler>>` + `with_generate()` + `generate_from_config(config_json)`(parse 子集 user_input/max_tokens/temperature;`custom_api`/`tools`/`json_schema` 返回「不支持」;无 user_input 报错)。
- `scripts/runtime.rs` `eval_with_bridge` 注册 `__kd_generate_native`(桥闭包捕获 EvalBridge clone,`generate_from_config` 内部 `Handle::current().block_on` 完成异步生成)。
- `engine/mod.rs` `run_character_scripts` 注入 `make_generate_handler()`(捕获 `self.connector` Arc clone,不引用引擎)。
- `stopGenerationById` 留扩展位(单发同步无注册表,脚本侧直接抛「不支持」)。

**6g-2 导入类(映射现有 service,绕过 HTTP)**:
- `scripts/bridge.rs`:`ImportHandler` 类型 + `EvalBridge.imports` + `with_imports()` + `import_raw(kind, filename, content, session_id)`。
- `runtime.rs` 注册 `__kd_import_native(kind, filename, content, session_id)`。
- `engine/mod.rs` `run_character_scripts` 注入 `make_import_handler()`(捕获各 service Arc clone,闭包内同步调用):
  - `character` → `CharacterService::upload(content.as_bytes(), filename)`;
  - `worldbook` → `WorldBookService::upload(content.as_bytes(), filename, None)`;
  - `preset` → `parse_st_preset(content)` → 合并进 `PromptInjectConfig.floors`(保留 simple 配置)→ `PromptInjectService::set`;
  - `chat` → session_id 空报「importRawChat 需指定会话」,否则 `SessionService::import_chat`(StMessage 数组);
  - `regex` → 返回「暂不支持」(无对应端点,留扩展位)。
- 脚本抛异常仅记日志,不中断主流程(沿用 3b-3 语义)。

**6g-3 扩展管理(只读视图,零后端逻辑)**:`build_script` 内加 `isAdmin → true`、`getTavernHelperExtensionId → 'kedai'`、`isInstalledExtension → false`、`getExtensionType → null`、`install/uninstall/reinstall/update/getExtensionInstallationInfo → 抛「Kedai 不支持扩展安装」`。

**测试**:`server-rs/tests/scripts_import.rs`(新增):character/chat/preset/worldbook 四类合法导入落库可见(角色列表/历史/楼层/世界书列表);缺 session_id 与 regex 不支持时主流程不中断、不写入消息。bridge 单测沿用既有 run_with 模式(bridge.rs 内嵌 tests,新增 generate_from_config 参数校验用例 —— 由集成链路覆盖主要路径)。

**风险与边界(实施确认)**:regenerate 锚点语义按上文修正;每页 swipe 独立变量表留扩展位(引擎 message 作用域未接线是事实,切换只改显示,生成新版本走既有 saveAssistantVars + replayMvuVariables 回滚管线);generate 子集限制(custom_api/tools/json_schema 明确报错,不伪造能力);扩展安装类一律报不支持;`PromptInjectConfig` 构造保留既有 simple 配置仅替换 floors(与 PUT /api/prompt-inject 的 replace 语义一致)。

**验证**:后端 `cargo test` 全过(基线 393+34+11 + 新增 swipe_regenerate 2 + scripts_import 3);前端 `npm test` 194 全过(基线 191 + swipe 3)+ `npm run build` 通过。不引入新依赖、不 eval、注释/日志简体中文、未提交 git。
