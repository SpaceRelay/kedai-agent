# Kedai — 文学创作 / 角色扮演交互工具

前后端分离的 AI 角色扮演/文学创作客户端,**SillyTavern 生态原生兼容**,内置 **Agent 引擎**(计划 → 执行 → 反思),输出质量远超一次性生成。

## 核心特性

- 🎭 **SillyTavern 兼容**
  - 角色卡:支持 V2/V3 规范(`.png` tEXt 内嵌 或 独立 `.json`),未知字段无损保留
  - 世界书(World Info):支持独立世界书 JSON(ST 导出格式)与角色卡内嵌 `character_book`,按关键词 **或正则(`regex`+`use_regex`)** 命中注入
  - 正则脚本(`regex_scripts`):AI 输出中的占位符/变量标记可替换为显示 HTML(如状态栏卡片),前端可开关
  - 对话:消息数组与 SillyTavern 完全一致,互相导入/导出无障碍
  - 后端:OpenAI 兼容接口(`/v1/chat/completions` 流式,兼容 Oobabooga --api / vLLM / LM Studio / Ollama 等)
- 🧩 **酒馆助手插件兼容**(SillyTavern-Assistant)
  - **EJS 模板渲染**:世界书条目中的 `<% %>` / `<%= %>` / `<%_ _%>` 标签按当前变量状态渲染(内建 `getvar/setvar/addvar`、`Math`、数组/字符串方法、if/for/箭头函数等 JS 子集),「分阶段人设」类条目开箱即用
  - **变量系统**:会话级 `stat_data` 变量树(SQLite 持久化),`[InitVar]` 条目初始化,支持 YAML/JSON/`_.set()` 三种初始格式
  - **输出协议**:模型回复中的 `<UpdateVariable><JSONPatch>` 块自动剥离并应用(`replace/delta/insert/remove/move`),同时兼容 MagVarUpdate 的 `_.set(...)` 格式;更新后的变量树经 SSE `vars` 事件推送前端,消息快照随 `extra.mvu` 落库
  - **状态注入**:`{{format_message_variable::path}}` 宏与 `<StatusPlaceHolderImpl/>` 占位符展开为当前变量状态(含 `{{getvar/setvar/addvar}}` 树路径);注入位置可在设置中切换(默认 `system`,可选「最新用户消息尾部」——变量更新不再使整个 system 前缀缓存失效,DeepSeek/Anthropic/OpenAI 等提示词缓存命中率显著提升)
  - **内嵌插件识别**:打开「插件」窗口可查看当前角色卡内嵌插件(自动检测,含特性明细:初始变量/EJS 模板/变量系统/状态注入/输出协议),kedai 内置实现无需安装
- 🤖 **Agent 引擎**
  - 状态机驱动:`planning → executing ⇄ tool_call → reflecting → finished`,全程可观测、可中断
  - 两种模式:`fast`(单步直接生成)、`deep`(计划 → 执行 → 反思,质量更高)
  - 推理链通过 SSE 流式推送到前端,实时展示思考过程
- 🔧 **工具系统**
  - 标准化 Tool 接口(OpenAI Function Calling 格式)
  - 内置:`calculator`(白名单解析,不使用 eval)、`memory_read/write`(会话长期记忆)
- ⚡ **流式体验**:token 逐字渲染 + Agent 步骤事件,首 Token 低延迟
- 🔐 **隐私**:数据仅存本地(SQLite + 文件),API Key 只存服务端环境变量

## 技术栈

| 层 | 选型 |
|---|---|
| 桌面壳 | **Tauri 2**(窗口加载本地服务,NSIS 安装包,`src-tauri/`) |
| 前端 | Vue 3 + Vite + Tailwind CSS v4 + Pinia(极简黑白至上主义) |
| 后端 | **Rust + axum + tokio** |
| 数据库 | SQLite(rusqlite,bundled 零原生依赖) |
| 流式 | SSE(Server-Sent Events) |
| Token 计数 | tiktoken-rs(按模型自动选分词器) |
| 测试 | cargo test(单测 + API 集成)+ Vitest(前端单测,`npm test -w web`) |

> 原 Node.js + TypeScript + Fastify 后端已重写为 Rust(见 `server-rs/`),原代码归档于 `_archive/node-server/`;原 C# 启动器被 Tauri 桌面壳取代,归档于 `_archive/csharp-launcher/`。

## 快速启动

> 维护指南见 [MAINTENANCE.md](MAINTENANCE.md)(架构、构建、API 契约、数据库与踩坑记录)。

要求:
- 前端:Node.js ≥ 18(npm)
- 后端:**Rust 工具链**(`cargo`,`rustup`)
  - 未安装:`winget install Rustlang.Rustup`

### 开发模式(前后端分离)

```powershell
# 1. 按 package-lock.json 确定性安装前端依赖
npm ci

# 2. 后端编译(debug)
cd server-rs
cargo build

# 3. 两个终端分别启动
#    终端 A(后端):cargo run -p kedai-server   # 监听 127.0.0.1:3001
#    终端 B(前端):npm run dev -w web          # 监听 http://localhost:5173
```

> 开发模式无需先构建前端:Rust 服务启动时自动探测 `web/dist`,缺失则回退内嵌产物。

### Windows 便携版(正式交付)

```powershell
# 构建前端、Tauri release,并组装独立便携目录
npm run build:portable
# 正式入口:dist\Kedai-portable\Kedai.exe
```

把整个 `dist\Kedai-portable\` 目录复制给用户即可;运行时不需要 Node.js、Rust 或项目源码。系统需要 Microsoft Edge WebView2 Runtime(Windows 10/11 通常已内置)。

桌面壳与 Axum 后端运行在同一进程。主窗口初始隐藏,后端 `/api/health` 就绪后才导航并显示,避免启动阶段白屏;关闭窗口即退出,不残留后端进程。数据与日志统一保存到:

- 数据:`%APPDATA%\com.kedai.app\data`
- 日志:`%APPDATA%\com.kedai.app\logs`

若从项目目录运行桌面产物,首次启动会在目标数据目录尚未使用时,自动把项目 `data` 复制过去。迁移幂等且保守:不删除源数据、不覆盖已有桌面数据、跳过 SQLite `-wal`/`-shm`,并修正数据库中的旧头像绝对路径。便携目录本身不携带用户数据。

仍需安装包时可执行 `npm run build:desktop`;NSIS 不是本项目的首选交付形式。

### 浏览器模式(仅开发/调试)

```powershell
# 一键构建:前端 web/dist + Rust release 单文件
.\build.ps1
# 启动服务并自动打开浏览器
.\start.ps1              # 始终使用浏览器开发/调试模式
.\start.ps1 -NoBrowser   # 加 -NoBrowser 不自动开浏览器
```

> 也可直接双击 `server-rs\target\release\kedai-server.exe` 启动;服务已在运行时再启动会自检并复用,不重复开端口。

### 配置(可选)

复制 `.env.example` 为 `.env` 并填写 `OPENAI_API_KEY` 与 `OPENAI_BASE_URL`。
无 Key 也能跑:将 `CONNECTOR` 设为 `mock` 使用演示模式(未配置 Key 时自动进入 mock)。

> 💡 也可直接在应用内「设置」中编辑 API 地址 / Key / 模型,以及温度、Top-P、最大生成长度、最大上下文窗口;
> 保存后写入 `data/settings.json` 并立即生效(优先级高于 `.env`,Key 仅存本地服务端、不回显明文)。Windows 使用当前用户 DPAPI 加密。非 Windows 在没有系统凭据后端时默认拒绝持久化非空 Key;只有明确接受明文风险并设置 `KEDAI_ALLOW_INSECURE_PLAINTEXT_SECRETS=1` 才允许写入 `plain:v1:` 值,更推荐通过环境变量提供 Key。

### 试跑演示

1. 打开应用,点左侧 📤 上传角色卡(`.png` 或 `.json`)
2. 选中角色,输入消息发送(Fast 模式)
3. 切到 **Deep** 模式,输入「帮我算 12*34」→ 观察底部 Agent 抽屉的工具调用与推理链
4. 生成中点「■ 停止」可中断;抽屉可手动收起/展开

## 目录结构

```
kedai/
├── src-tauri/              # 桌面壳(Tauri 2):窗口 + NSIS 安装包 + 进程内复用 Rust 服务
├── server-rs/              # 后端(Rust + axum)
│   ├── src/
│   │   ├── api/            # RESTful + SSE 路由(app_state/router/chat/characters/sessions/...)
│   │   ├── agents/         # 状态机 / 规划器 / 执行器 / 反思器 / engine/(目录模块)
│   │   ├── connectors/     # 后端适配器(openai-compatible / mock)
│   │   ├── tools/          # 工具系统(registry / calculator / memory / agent_tools)
│   │   ├── models/         # 类型 + SQLite 表结构
│   │   ├── parsing/        # 角色卡 V2 解析 + assistant/ejs/(mvu 变量渲染)
│   │   ├── services/       # 角色/会话/Agent会话/Token 服务
│   │   └── utils/          # 结构化 JSON 日志(按天归档)
│   └── tests/              # API 集成测试
├── web/                    # 前端(Vue 3 + Pinia + Tailwind v4)
│   └── src/
│       ├── api/            # REST + SSE 流式客户端(按域拆分,index 聚合)
│       ├── store.ts        # Pinia 全局状态
│       ├── sseReducer.ts   # SSE 事件纯函数
│       └── components/     # Sidebar / ChatWindow / ChatInput / AgentDock / SettingsModal
├── tools/make-icons.ps1    # 品牌图标生成(圆角 + 透明背景)
├── launcher/               # 旧项目目录启动器源码(兼容保留,非正式入口)
├── dist/Kedai-portable/    # 正式 Windows 便携目录(npm run build:portable 生成)
├── start.ps1               # 浏览器开发/调试启动器
├── build.ps1               # 后端/前端与可选 NSIS 构建
├── tools/build-portable.ps1# 便携目录构建脚本
├── logs/                   # 运行日志(按天归档 kedai-YYYY-MM-DD.log,自动清理 3 天前)
├── data/                   # 运行时生成:SQLite + 角色卡原图 + avatars
└── _archive/               # 历史物归档(node-server / csharp-launcher)
```

## 日志与维护

- 运行日志双写:控制台 + `logs/kedai-YYYY-MM-DD.log`(按天归档;桌面应用场景在 `%APPDATA%\com.kedai.app\logs\`)。
- 自动清理:启动时删除 3 天前的过期日志。
- 数据库:`data/kedai.db`(WAL 模式,10 张表;桌面场景在 `%APPDATA%\com.kedai.app\data\`)。

## API 概览

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/health` | 健康检查 |
| POST | `/api/chat/send` | 发送消息 → Agent 全流程 → **SSE 流** |
| POST | `/api/chat/stop` | 中断生成 |
| GET | `/api/chat/history` | 会话消息历史 |
| GET/POST/DELETE | `/api/chat/sessions` | 会话管理(不带 character_id 时返回全部会话,联表角色名与消息数) |
| PUT/DELETE | `/api/chat/messages/:id` | 编辑/删除消息 |
| POST | `/api/chat/clear` | 清空会话消息 |
| GET | `/api/characters` | 角色卡列表 |
| POST | `/api/characters/upload` | 上传角色卡(.png/.json,V2 解析) |
| GET/PUT/DELETE | `/api/characters/:id` | 角色卡详情/更新/删除 |
| POST | `/api/settings/connect` | 测试后端连接 |
| GET | `/api/settings/models` | 可用模型列表 |
| GET | `/api/settings/info` | 连接器信息 |
| GET/PUT | `/api/settings/model` | 获取/切换当前模型 |
| POST | `/api/token/count` | 消息数组 Token 计数 |
| GET | `/api/export/chat` | 导出(SillyTavern 兼容) |
| POST | `/api/import/chat` | 导入(SillyTavern 兼容) |
| GET | `/api/world-books` | 世界书列表(含条目数/绑定角色) |
| POST | `/api/world-books/upload` | 上传独立世界书(JSON,可绑定角色) |
| PUT/DELETE | `/api/world-books/{id}` | 更新(启用/绑定/改名)/删除世界书 |
| GET | `/api/world-books/{id}/entries` | 世界书条目预览 |
| POST | `/api/agent/plan` | 预览行动计划(不执行) |
| POST | `/api/agent/execute` | 手动执行步骤 |
| POST | `/api/agent/interrupt` | 中断 Agent |

### SSE 事件格式

```text
data: {"type":"step","step":"计划中…","detail":"快速模式:直接生成回复"}

data: {"type":"token","text":"（"}

data: {"type":"tool_call","name":"calculator","input":{"expression":"12*34"}}

data: {"type":"tool_result","name":"calculator","output":{"result":408}}

data: {"type":"finish","usage":{"prompt_tokens":166,"completion_tokens":35,"total_tokens":201,"context_tokens":490},"content":"…"}
```

事件类型:`token`(文本片段)、`step`(Agent 步骤)、`tool_call`/`tool_result`(工具调用)、`interrupted`(中断)、`finish`(结束,含 usage)。

## 世界书(World Info)

- **来源**:两种——独立上传(SillyTavern 导出格式,顶层 `entries` 对象/数组)与角色卡内嵌 `character_book.entries`(兼容 V2 顶层与 V3 `data.character_book` 两种布局)。
- **注入规则**:常驻条目(`constant=true`,启用的)始终注入;非常驻条目按 `keys` 对最近 6 条用户消息做大小写不敏感子串匹配,或按条目 `regex`+`use_regex` 做正则匹配(优先于 keys),命中才注入;`enabled=false` / 非常驻且无 key 无 regex 的条目跳过。
- **自动转换**:上传(独立世界书与角色卡内嵌)时自动规范化酒馆变体——关键词兼容 `keys/key/keywords/keyword` 字段名与逗号分隔字符串、`constant` 兼容字符串/数字形态并支持缺失时自动判定(无关键词无正则 → 常驻)、`role` 缺失即「自动」(按 常驻→system、激发→user 分配)。上传响应附带转换统计 `conversion`。
- **自动分配机制自检**:世界书窗口「检查自动分配机制」按钮(或 `GET /api/world-books/auto-assign-check`)可一键验证条目解析、属性自动分配、转换链路是否可用。
- **管理**:侧边栏「世界书」按钮打开管理界面——上传、查看条目(含正则标记)、绑定角色(空=全局)、启用/停用、删除。
- **持久化**:`data/kedai.db` 的 `world_books` 表,原始 JSON 无损保留(`data_raw`)。

## 提示词注入

- **简单模式**:字数 / 转述 / 对话 / 视角 四项,启用项按可调顺序合成一条注入提示词拼入系统提示词末尾,对所有会话生效(支持酒馆宏)。
- **禁词库**(简单模式):自由编辑「禁词 → 替换词」映射表。输出含禁词时,**所有模式**(fast/deep/agent/custom)先注入自省提示词要求换用更得体表达;deep/agent/custom 模式另由引擎在生成收尾调用 `censor_text` 工具做同义替换兜底(替换后同时作用于落库与前端显示),fast 模式无工具、仅靠提示词预防。替换词留空 = 直接删除该词。
- **复杂模式(楼层)**:仿 SillyTavern Prompt Manager 的楼层系统——角色/位置/深度/拖拽排序,可导入酒馆预设(`examples/presets/` 示范)。
- **管理**:设置 → 提示词注入,或「提示词管理」弹窗;配置持久化到 `data/prompt_floors.json`。

## 消息 HTML 渲染

- 角色卡 `extensions.regex_scripts`(或 V3 `data.extensions.regex_scripts`/顶层)中的脚本,可用于把 AI 输出的占位符/变量标记替换为显示 HTML(如「状态栏」卡片)。
- 安全性:先在原文上匹配脚本、再对 AI 输出整体做 HTML 转义,最后仅把脚本命中的片段还原为脚本自带内容(作者可信),避免注入;`<script>` 一律剔除。后续版本改为「脚本受控执行」:提取 `<script>` 源码后由沙箱 iframe 执行(隔离宿主 window/document,遮蔽网络与存储),并受角色卡 JS 授权门禁约束。
- **开关**:聊天窗口顶栏「HTML」开关按钮,或「设置 → 界面 → 消息 HTML 渲染」。顶栏开关常驻显示(无论角色卡是否带脚本),状态按角色卡记忆,未设置的角色卡回退全局默认;默认关闭,开启后仅对 AI 回复生效(无脚本的角色卡开关不产生渲染效果)。
- 脚本 HTML 清洗:自动剥除 markdown 代码围栏(` ```html `)、提取 `<head>` 内 `<style>` 前置并只取 `<body>` 内容。

## 聊天记录管理

- 侧边栏「记录」按钮打开「聊天记录」面板:列出全部会话(联表角色名、消息数、最近活动时间),支持:
  - **选择切换**:打开任一会话(跨角色自动切换)
  - **新建**:为当前角色新建会话
  - **删除**:删除会话及其全部消息
  - **导出**:单个会话导出为 SillyTavern 兼容 JSON
  - **导入**:导入 JSON 替换指定会话内容

## 角色开场白

- 角色卡 `first_mes` 自动识别并在「编辑提示词」弹窗中提供「开场白(First Message)」编辑框(右键角色 → 编辑提示词)。
- 开场白随角色卡保存在 `data_raw` 中,新建会话时可作为角色的第一句话;留空表示无开场白。

## 测试

```bash
cd server-rs
cargo test          # 单元测试 + API 集成测试(219 个)
cd web
npm test            # Vitest 前端测试(62 个)
```

覆盖:角色卡解析(V2/V3/未知字段/PNG tEXt/无效输入)、世界书解析(ST 导出/角色卡内嵌/正则条目/条目过滤)、世界书 API 集成(CRUD/绑定/预览)、世界书注入逻辑(常驻/关键词/正则/禁用)、正则脚本解析与占位符替换、Token 计数、状态机迁移、规划器/反思器、计算器工具、API 集成(CRUD/SSE/导入导出/Agent plan)、mvu 变量系统、EJS 渲染器、提示词注入、Agent 流程库。

## Roadmap

- [x] 世界书(World Info)按 key 注入
- [x] Tauri 桌面化(安装包 + 窗口 + 品牌图标)
- [ ] Oobabooga / KoboldAI 连接器适配
- [ ] 智能上下文压缩(摘要/滑动窗口)
- [ ] LLM 原生 function calling 全链路
- [ ] 自定义工具注册(OpenAI 格式 JSON 配置)
- [ ] 工具执行沙箱隔离
- [ ] 知识库向量检索工具
- [ ] SettingsModal.vue(58KB)拆分

## 示例模板(Skill 与工具插件)

`examples/` 提供两套扩展体系的示范模板与使用说明:

- **示范 Skill**:`examples/skills/写作风格指南.json` —— agent 可 `read(type=skill)` 加载的写作规范,演示推荐结构(适用范围 → 规则 → 示例 → 使用方式)。
- **示范工具插件**:`examples/plugins/tools/score_eval.json`(评分判定,三元 + 对象返回)与 `clamp.json`(数值限幅,嵌套 Math 调用)。
- **说明文档**:`examples/README.md` —— 导入/启用命令、脚本语法边界与常见陷阱。

> 工具插件脚本为白名单求值器(不使用 eval),语法能力有限,编写前请阅读 `examples/README.md` 的「脚本语法边界」。

- **示范预设**:`examples/presets/可待精华增强楼层.json` —— 提炼自酒馆「可待」预设的可选增强楼层(长思考/实时总结/伏笔/大纲/禁库清单/逻辑加强/NSFW,默认全部关闭),在「设置 → 提示词注入 → 导入酒馆预设」中一键导入按需开启;创作总纲/基础文风/模块化剧情等核心准则已内置在默认系统提示词中,无需导入。

## 致谢

- **mvu 变量系统**(`web/src/mvu/` 与后端 `parsing/assistant/`)为 **MagVarUpdate** 的兼容实现:协议约定(`stat_data`/`display_data` 变量树、`_.set('path', old, new);//原因` 输出格式、`[InitVar]` 世界书条目初始化)与全局脚本 API(兼容 `Mvu`/`waitGlobalInitialized`/`errorCatched`/`$`)均源自该扩展。原作者:**[MagicalAstrogy](https://github.com/MagicalAstrogy/MagVarUpdate)**([MagVarUpdate](https://github.com/MagicalAstrogy/MagVarUpdate),MIT License)。Kedai 为独立实现,不包含原版代码、不依赖其运行时,仅保持协议与命名兼容。
- 酒馆助手(SillyTavern-Assistant)兼容层的 `<UpdateVariable><JSONPatch>` 输出协议参考 SillyTavern 社区插件生态的公开约定。

## 许可与说明

本地个人创作工具,数据不上传任何第三方(用户自定义远程后端除外)。
