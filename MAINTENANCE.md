# Kedai 维护指南(MAINTENANCE)

> 面向后续维护者的技术文档。涵盖架构、构建、启动、API 契约、数据库、日志与已知坑位。
> 版本:v0.2.1(前端 Vue3 + 后端 Rust + Tauri 桌面壳) 最后更新:2026-09-12

---

## 0. 架构治理(三结合梯队架构)

**总纲**:借鉴「老中青三结合」组织原则,代码分三层——L1 老层·稳(Anchored Core:parsing/ 兼容解析、models 数据契约、测试套件、SillyTavern 兼容承诺)、L2 中层·干(Orchestration:agents/、services/、api/)、L3 青层·活(Frontier:tools/、skills/、沙箱、新能力)。四种机制:优势互补、传帮带晋升、梯队衔接、动态循环。**详见 `docs/ARCHITECTURE-3H.md`(结构性改动先读它再动代码)**。

关键纪律:
- **改 L1 契约**须先写失败测试、声明兼容影响、过评审;协议翻译集中在中层。
- **Mutex 纪律**:全项目锁中毒一律恢复(`.lock().unwrap_or_else(|e| e.into_inner())`),不 panic;**禁止持 std::sync::Mutex guard 跨 `.await`**(Clippy `await_holding_lock` 防护)。
- **DB 并发纪律**:api handler 的同步 DB 调用必须经 `db_call/db_read/db_write`(spawn_blocking),禁止 async 上下文直接持锁;细则见 §7「DB 并发纪律」。
- **渲染性能纪律(前端)**:消息渲染必须走 ChatMessageItem 的缓存 computed,禁止在 v-for 里直接调渲染方法。
- **样式分层纪律(前端,2026-09 D-5 起)**:`web/src/style.css` 只保留层① 设计变量(:root 令牌)与层② 全局基础层;新增组件样式一律 `<style scoped>` 或 Tailwind 工具类,禁止再写入 style.css;修改存量组件时顺手把该组件样式搬进 scoped(「改到谁拆谁」,文件头分层约定注释为准);新增样式不得引入 `!important`(存量 11 处见 style.css)。
- **跨端协议**(前后端 mvu)以 `docs/mvu-protocol.md` 锁定双端一致,防行为漂移。
- 新能力默认进 L3 隔离验证,成熟后按晋升通道(测试通过 + 不破坏协议 + 评审)升级。

### 新能力晋升状态(三结合梯队)

| 能力 | 当前代际 | 状态 |
|---|---|---|
| GENERATE/RENDER 内容注入([GENERATE:BEFORE/AFTER]、{idx}、REGEX) | L3 → 拟 L2 | 已在 engine 挂载,测试覆盖;协议文档见 docs/generate-render-protocol.md |
| @INJECT 精确消息插入(pos/target/regex) | L3 | messages/inject.rs 测试覆盖;协议文档见 docs/inject-protocol.md |
| @@ 装饰器解析(parse_decorators/EntryDecorators) | L1 | 已入 world_book.rs 兼容解析 |
| EJS 读取 API(getwi/getchar/injectPrompt 等 15 个) | L3 | ejs 层 10 个测试;injectPrompt 为兼容占位(engine 注入清单未接入) |
| LAST_SEND/LAST_RECEIVE 统计变量 | L2 | engine 收尾写入会话宏,测试覆盖 |
| <#escape-ejs> 作用域转义 | L1 | ejs/mod.rs 占位符方案 + 测试 |
| PatchOp.reason / 转义还原 / delta 无效跳过 / Insert 并入 Replace | L1 | mvu 协议对齐,见 docs/mvu-protocol.md |

---

## 1. 项目概览与架构

**定位**:本地运行的 AI 角色扮演 / 文学创作客户端,SillyTavern 生态原生兼容,内置 Agent 引擎(计划 → 执行 → 反思)。

```
kedai/
├── server-rs/                  # 后端:Rust + axum + tokio + rusqlite(核心,可单独交付)
│   ├── src/
│   │   ├── main.rs             # 命令行入口(健康自检 + run_server)
│   │   ├── lib.rs              # 库入口(run_server 供 main/Tauri 复用,build_test_app)
│   │   ├── config.rs           # 配置加载(环境变量 + .env,支持 DATA_DIR/LOG_DIR 注入)
│   │   ├── api/                # 路由层:mod.rs(组装/CORS/SPA 回退)+ routes/ 五域(chat/settings/agent/content/misc)
│   │   │   │                   #   + static_files.rs(静态文档/SPA 回退)+ util.rs(WithStatus/db_err)
│   │   │   │                   #   + errors.rs(结构化错误码 ErrorCode/err_with_code)
│   │   ├── agents/
│   │   │   ├── engine/         # 目录模块(拆分自原 engine.rs):mod.rs(主流程)+ run_loop/run_finish/run_scripts
│   │   │   │                   #   + messages/ 目录(build/inject/trim)+ worldbook/executor/mvu/compaction/reflector_integration
│   │   │   ├── state_machine.rs# 8 状态 + 迁移表(幂等迁移)
│   │   │   ├── planner.rs      # fast/deep 计划 + 算式识别
│   │   │   └── reflector.rs    # 质量反思(空/截断/未答疑问 3 规则)
│   │   ├── connectors/         # LLM 后端适配(openai_compatible/ 目录模块 + mock)
│   │   ├── tools/              # 工具系统(registry / calculator / memory / agent_tools)
│   │   ├── models/             # db/ 目录(schema 建表/backfill 迁移)/ types.rs(契约类型)
│   │   ├── parsing/            # character_card.rs + assistant/(ejs/ 目录模块:mvu 变量渲染)
│   │   ├── services/           # character / session / agent_session / token 服务
│   │   └── utils/logging.rs    # tracing 日志:PinoFormat JSON + non-blocking 双 channel(按天归档)
│   └── tests/                  # API 集成测试(api_integration/assistant/agent_flows/prompt_inject/settings_connector/security/world_books)
├── web/                        # 前端:Vue 3 + Vite + Tailwind v4 + Pinia
│   └── src/
│       ├── api/                # 目录模块(拆分自 api.ts):client/types/characters/sessions/worldbooks/... + index 聚合
│       ├── stores/             # Pinia 七子 store(character/chat/genSettings/modelConn/resources/task/uiPrefs)
│       ├── store.ts            # 全局状态门面(facade,聚合七子 store 保持原引用路径)
│       ├── sseReducer.ts       # SSE 事件纯函数(拆分自 store)
│       ├── mvu/                # 前端 mvu 变量系统(parser/variables/host/mvuStore)
│       └── components/         # Sidebar / ChatWindow / ChatInput / AgentDock / SettingsModal
│           └── settings/       # 设置弹窗十一 section(Api/Connection/GenParams/PromptInject/Agent/AgentFlow/PresetImportExport/DataManagement/Ui/Mcp/Embedding)
├── src-tauri/                  # Tauri 2 桌面壳:窗口加载 http://127.0.0.1:3001,进程内复用 run_server
│   ├── src/lib.rs              # 数据目录注入(%APPDATA%\com.kedai.app)+ 服务自检/启动
│   └── tauri.conf.json         # 窗口 1280×800、NSIS 打包、图标
├── tools/make-icons.ps1        # 品牌图标生成脚本(圆角 + 透明背景,零依赖)
├── start.ps1                   # 智能启动:默认测试版(浏览器模式),-Portable 启动便携版;过期/漂移自动双端重建
├── build.ps1                   # 一键构建(默认双端同步:前端 + Rust release + 便携版;-TestOnly 仅测试版;-Tauri 追加 NSIS)
├── Kedai.exe / Kedai.lnk       # 图形启动器(双击正式入口;源码在 launcher/,由 build.ps1 幂等维护)
├── logs/                       # 运行日志(自动清理 3 天前;桌面场景在 %APPDATA%\com.kedai.app\logs)
├── data/                       # SQLite + 角色卡原图 + avatars(勿删)
└── docs/                       # 技术文档(如 kedai-agent-coordination.md)
```

### 请求数据流

```
浏览器 → /api/xxx → api/mod.rs 路由 → services → SQLite(rusqlite)
                                        └→ connectors(LLM)→ SSE 流回推
```

### Agent 引擎状态机

`idle → planning → executing ⇄ tool_call → reflecting → finished`,可 `interrupted`/`error`。
中断通过 `watch::channel<bool>` 中止标志贯穿全链路。

---

## 2. 快速命令速查

| 操作 | 命令 | 说明 |
|---|---|---|
| 一键启动(桌面) | 双击 `Kedai.lnk` 或 `.\start.ps1 -Portable` | lnk 指向项目根 `Kedai.exe` 图形启动器(过期/漂移自动询问重建);`.\start.ps1` 默认启动测试版浏览器模式 |
| 桌面安装包 | `.\build.ps1 -Tauri` | 双端同步之外追加 NSIS 安装程序(需 `@tauri-apps/cli`);安装后从开始菜单启动 |
| 一键构建 | `.\build.ps1` | **默认双端同步产出**:前端 web/dist + Rust release(测试版)+ 便携版;`-TestOnly` 仅测试版快速通道 `-Dev` debug 构建 `-NoWeb` 仅 Rust `-Tauri` 追加 NSIS 打包 |
| 兼容别名 | `npm run build:all` / `npm run build:rs` | 均等价 `.\build.ps1`(双端同步);`npm run build:test` 等价 `.\build.ps1 -TestOnly` |
| 统一改版本号 | `npm run version:bump -- x.y.z` | 7 处版本号一次改全(2 个 package.json、3 个 Cargo.toml、tauri.conf.json、本文档版本行);支持 `-DryRun` 预览 |
| 后端测试 | `cd server-rs && cargo test` | 825 个测试(654 单测 + 171 集成,2026-09-08 实测),**需在 vcvars64 环境**;前端 `npm test -w web` **663 个(67 文件,2026-09-11 实测)**;后端 lib 单测 **740 个(2026-09-12 实测)** |
| 全量检查(本地 CI) | `npm run check` | `tools/check-all.ps1`:fmt → clippy → cargo test → vue-tsc(**硬门禁**,2026-09-08 起)→ vitest → vite build |
| 开发模式 | `cd server-rs && cargo run` + `npm run dev -w web` | 后端 3001 / 前端 5173(代理到 3001) |
| 前端构建 | `npm run build -w web` | 产出 web/dist(编译进 exe 用) |

> **⚠️ 关键**:本机 cargo 编译必须先加载 VS 环境,否则报 `link.exe not found`:
> ```
> cmd /c "call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" && cd server-rs && cargo build --release"
> ```
> `build.ps1` 内部已做此处理,直接 `.\build.ps1` 最省事。

---

## 3. 环境依赖(本机已配置,新机器需重装)

| 依赖 | 版本 | 安装方式 | 备注 |
|---|---|---|---|
| Rust 工具链 | 1.97.1 stable | `rustup`(本机经清华镜像安装) | rustup/cargo/rustc 在 `~/.cargo/bin`(可能不在 PATH,用全路径或加 PATH) |
| VS2022 Build Tools | 17.14(C++ 工作负载) | winget | 提供 MSVC 链接器 `link.exe` 与 cl.exe(必需) |
| Node.js | ≥ 18(验证 24) | 已有 | 仅前端构建需要;发布 exe 运行时**不需要** Node |

### 原生依赖说明(Phase 3 起)

| 依赖 | 形式 | 备注 |
|---|---|---|
| `sqlite-vec` 0.1.9 | C 源码静态编译(`cc` + `cl.exe`) | 记忆库向量检索的 `vec0` 虚拟表扩展;走 `sqlite3_auto_extension` 全局注册(`models/db/mod.rs::register_sqlite_vec`),产物仍为**单 exe**,便携版无需附带 dll。仅需已有 MSVC 工具链,不引入 cmake/clang/nasm。 |

### crates 国内镜像(已配置 `~/.cargo/config.toml`)

```toml
[source.crates-io]
replace-with = "ustc"
[source.ustc]
registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/"
[net]
git-fetch-with-cli = true
```

若官方源可用,可改回 `crates-io` 直连。

---

## 4. 构建与发布

### 单二进制原理

`kedai-server.exe` 通过 `include_dir!` **内嵌** `web/dist`(编译期快照,见 `server-rs/src/api/static_files.rs`)。
运行时默认只服务内嵌版本;仅显式设置环境变量 `KEDAI_WEB_DIST` 指向磁盘目录时才用磁盘版覆盖
(开发调试用,见 `server-rs/src/config.rs`)。

因此**每次改前端后必须重新构建 exe** 才会带上新界面;`server-rs/build.rs` 的
`rerun-if-changed=../web/dist` 保证 cargo 感知前端变化,构建脚本另有 mtime 兜底检测。

### 构建流程

```powershell
.\build.ps1          # 默认双端同步:前端 + cargo build --release + 便携版
.\build.ps1 -TestOnly  # 快速迭代:只产出测试版(结尾会警告便携版未同步)
```

> **本机环境提示(2026-08 批次构建时验证)**:裸 shell 里没有 cargo——MSVC toolchain 需先
> `call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"`,
> 再在同一 shell 里执行 build.ps1(脚本内部直接调 cargo/npm,继承环境)。
> Git Bash 里用 `cmd //c "<包装.bat>"` 嵌套调用。另外:构建只跑 debug 不代表 exe 版已更新,
> 交付前必须跑 `.\build.ps1`(详见 AGENTS.md「构建提醒」)。

产物:`dist\kedai-server.exe`(测试版)+ `dist\Kedai-portable\Kedai.exe`(便携版),各自约 20~30MB。
双端全量构建约 5~10 分钟(便携版含 Tauri 全量编译);快速迭代用 `-TestOnly` 或 `-Dev`。

### 两版同步机制(构建指纹)

测试版与便携版是两条独立编译链,各自把编译那一刻的 `web/dist` 冻结进二进制——分开构建必然漂移。
根治手段是「默认双端同步 + 指纹可查 + 启动对齐」三层:

1. **构建层**:`build.ps1` 默认一次产出两端;每个 dist 产物旁边写 `<exe>.build.json`
   (`{version, build_time, dist_hash}`,算法见 `tools/Write-BuildStamp.ps1`)。
2. **运行时层**:`server-rs/build.rs` 把 `KEDAI_DIST_HASH`/`KEDAI_BUILD_TIME` 编进二进制,
   `GET /api/health` 返回 `{ok, ts, version, build_id, build_time}`;设置中心底部常驻显示
   「版本 · 构建时间 · 指纹前 8 位」,两端各开一次对比即可肉眼确认同步。
3. **启动层**:`start.ps1` 与图形启动器(`Kedai.exe`,源码 `launcher/`)启动前比对两端
   sidecar 的 `dist_hash`,不一致自动执行 `build.ps1` 双端重建;`Kedai.lnk` 指向图形启动器,
   双击自带过期/漂移检测(直接双击 dist 里的裸便携版 exe 没有这层保障)。

### 部署方式

把整个 `kedai/` 目录(或仅 `exe + data + logs`)拷到目标机即可。exe 通过向上遍历定位项目根(找 `web` 目录),因此 **exe 需放在 `server-rs/target/release/` 原路径,或保证其上级存在 `web` 目录**。若想单独分发,保持目录结构即可。

---

## 5. 启动与配置

### 启动器(start.ps1,推荐)

统一 PowerShell 启动脚本(旧 C# `kedai.exe` / `启动Kedai.bat` 已废弃删除):
- 测试版(默认 `.\start.ps1`):启动 `dist\kedai-server.exe` 并自动打开浏览器;`-NoBrowser` 不开浏览器,`-Build` 强制先执行 `build.ps1`。
- 正式版(`.\start.ps1 -Portable`):启动 `dist\Kedai-portable\Kedai.exe` 便携版桌面应用(数据存 `%APPDATA%\com.kedai.app`);双击入口是项目根 `Kedai.lnk`(指向 `Kedai.exe` 图形启动器,自带过期/漂移询问重建)。
- 自动重建:启动前检测产品源码(web/src、server-rs/src、src-tauri/src 等)是否比可执行产物新,过期则自动执行 `build.ps1`(默认双端同步);`-NoRebuild` 跳过。
- **同步纪律**:测试版与便携版是两条独立编译链的产物,`build.ps1` 默认双端同步产出;只有 `-TestOnly`/`-Dev` 快速通道会只刷新测试版。两端 sidecar 指纹(`<exe>.build.json`)不一致时,启动脚本与图形启动器都会自动触发双端重建,无需人工记忆。
- 两版共用端口 3001 与同一数据目录,勿同时运行。

### 配置(.env,复制自 `.env.example`)

| 变量 | 默认 | 说明 |
|---|---|---|
| HOST / PORT | 127.0.0.1 / 3001 | 监听地址(3001 是前端代理硬契约) |
| DATA_DIR | ./data | SQLite + 角色卡 + avatars |
| CONNECTOR | openai-compatible | `openai-compatible` \| `mock`(未知回退 mock) |
| OPENAI_BASE_URL | https://api.openai.com/v1 | 兼容 Oobabooga/vLLM/Ollama 等 |
| OPENAI_API_KEY | (空) | 无 Key 时自动 mock 演示 |
| OPENAI_MODEL | gpt-4o-mini | 默认模型(可在设置中运行时切换) |
| DEFAULT_TEMPERATURE / DEFAULT_MAX_TOKENS | 0.8 / 1024 | 生成默认参数 |
| LOG_LEVEL | info | debug/info/warn/error |

> `.env` 加载:`dotenvy` 从当前工作目录向上搜索。启动器/直接运行 exe 时 cwd 为项目根,配置正常。

---

## 6. API 契约(维护红线)

> 前端 `api.ts` 与后端 `types.rs` 的字段必须保持逐一对应;改任何响应结构需两端同步,否则前端渲染异常。

### 路由总表

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | /api/health | `{ok, ts}` |
| POST | /api/chat/send | SSE 流(见下) |
| POST | /api/chat/stop | `{session_id}` → `{ok:true}` |
| GET | /api/chat/sessions?character_id= | `{sessions:[...]}` |
| POST | /api/chat/sessions | `{character_id, title?}` → 201 session |
| DELETE | /api/chat/sessions/{id} | 204 |
| GET | /api/chat/history?session_id= | `{messages:[...]}`(id 自增数字) |
| PUT/DELETE | /api/chat/messages/{id}?session_id= | 编辑(extra 覆写为 `{edited:true}`)/ 删除(204) |
| POST | /api/chat/clear | `{session_id}` → `{ok:true}`(清消息保留会话) |
| GET | /api/characters | `{characters:[...]}`(列表**不含** data_raw) |
| POST | /api/characters/upload | multipart 字段 `file`,20MB,→ 201 完整记录 |
| GET/PUT/DELETE | /api/characters/{id} | 详情(含 data_raw)/ 更新 / 删除(204,级联) |
| POST | /api/settings/connect | `{ok, message, models}` |
| GET | /api/settings/models | `{models:[...]}` |
| GET | /api/settings/info | `{connector, model, models, availableConnectors}` |
| GET/PUT | /api/settings/model | 获取 `{model}` / 切换 `{model}` → `{ok, model, changed}` |
| POST | /api/token/count | `{messages, model?}` → `{total, model}`(每条+4,末尾+2) |
| GET | /api/export/chat?session_id= | `{session_id, messages:[StMessage]}` |
| POST | /api/import/chat | `{session_id, messages}` → `{ok, imported}`(先清空) |
| POST | /api/agent/plan | `{message, agent_mode?, session_id?}` → `{plan, summary, tools, history?}` |
| POST | /api/agent/execute | 固定桩(引擎自动编排,不真正执行) |
| POST | /api/agent/interrupt | 等价 /api/chat/stop |
| GET | /api/avatars/{file} | 头像静态文件(数据目录 avatars) |

### SSE 事件格式(chat/send)

```
data: {"type":"step","step":"计划中…","detail":"..."}\n\n
data: {"type":"token","text":"片"}\n\n
data: {"type":"tool_call","name":"calculator","input":{...}}\n\n
data: {"type":"tool_result","name":"calculator","output":{...}}\n\n
data: {"type":"interrupted"}\n\n
data: {"type":"finish","usage":{prompt_tokens,completion_tokens,total_tokens,context_tokens},"content":"..."}\n\n
```

- 格式:`data: {json}` + 空行,`type` 编码在 JSON 内,无 `event:` 字段
- 事件类型:`token` `step` `tool_call` `tool_result` `interrupted` `finish`
- `finish.usage.context_tokens` 为单独计算的上下文总 token
- 客户端断开时自动中断生成(engine 检测 mpsc send 失败)

### 约定

- 错误响应统一 `{"error":"消息"}`;已接入结构化错误码的端点附带 `"code"`(如 `{"error":"会话不存在","code":"NOT_FOUND"}`,码表见 `server-rs/src/api/errors.rs`:VALIDATION/UNAUTHORIZED/NOT_FOUND/CONFLICT/DB/UPSTREAM/INTERNAL,前端按 code 分类提示)
- 400 缺参/校验失败,404 资源不存在,409 会话生成中,204 删除成功,201 创建成功
- 全部时间字段为 ISO 8601 字符串

---

## 7. 数据库(data/kedai.db)

rusqlite(bundled,零原生依赖),**WAL 模式 + foreign_keys ON**。共 28 张表(定义见 `server-rs/src/models/db/schema.rs`),核心表:

| 表 | 关键字段/约束 |
|---|---|
| characters | id PK, name, chara_name, description, file_path, avatar_path, data_raw(JSON 字符串), created_at |
| sessions | id PK, character_id **FK→characters ON DELETE CASCADE**, title, created_at, updated_at |
| messages | id INTEGER PK AUTOINCREMENT, session_id FK→sessions CASCADE, role CHECK(user/assistant/system), content, extra(JSON), created_at;索引(session_id, id) |
| agent_sessions | id PK, session_id **UNIQUE** FK→sessions CASCADE, state, plan/steps(JSON), step_index, agent_mode, started_at, updated_at |
| tool_calls | id PK, agent_session_id FK→agent_sessions CASCADE, name, input/output(JSON), duration_ms, created_at;索引(agent_session_id) |
| world_books | 独立世界书:原始 JSON(data_raw)、绑定角色、启用状态、条目统计 |
| skills / agent_subtasks | 技能库 / Agent 子任务 |
| session_vars / session_assistant_vars | 会话宏变量 / 酒馆助手变量树(stat_data) |
| session_usage / global_usage | 会话级 / 全局 token 用量统计 |
| tasks / task_subtasks / task_usage / task_llm_calls / task_messages | 任务模式:任务/子任务/token 用量/LLM 调用追踪/阶段消息 |
| memory_entries | 跨会话记忆蒸馏条目(按角色维度,含 selected/pinned) |
| scope_variables / backfill_meta | 7 作用域变量 / 增量回填游标 |
| contract_changelog / kaleido_state / kaleido_changelog | 契约变更历史 / kaleido 状态与历史 |
| llm_requests / session_compactions / undo_snapshots / user_scripts / quick_replies | LLM 请求缓存诊断 / 压缩记录 / 撤销快照 / 用户脚本 / 快速回复 |

> 迁移逻辑见 `server-rs/src/migration/`(backup/conflict/ddl/merge/mod 目录模块),表结构定义见 `server-rs/src/models/db/`(mod/schema/backfill)。

### 维护注意

- **数据兼容**:表结构与原 Node 版完全一致,旧 `kedai.db` 可直接使用(已验证)
- **备份**:复制 `data/` 目录即可(SQLite WAL 模式建议先停服再拷,或同时拷 `-wal`/`-shm`)
- **⭐ 代码约定**:`Db::write()` 返回 `MutexGuard<Connection>`,**必须在其作用域结束后再调用其他取写锁方法**,否则 std::sync::Mutex 重入死锁(曾踩坑,详见 §10);只读查询用 `Db::read()`(连接池,无此约束)

### DB 并发纪律(2026-08 改造)

- **两类句柄**:`Db::read()` 返回只读连接池句柄(`PooledRead`),**SELECT 专用**,可并发多连接;`Db::write()` 返回全局唯一写连接的 `MutexGuard`,一切写 SQL 与「读+写同事务」的混合场景走它。
- **handler 必须过阻塞池**:api handler 禁止在 async 上下文直接执行同步 DB 调用(会卡住 tokio worker);一律经 `AppState::db_call`(任意 services 同步方法)/ `db_read`(只读闭包)/ `db_write`(写闭包),三者内部均为 `spawn_blocking`。
- **db_write 非重入**:闭包执行期间已持有唯一写连接,**闭包内不得再调用会重新获取写锁的 services 写方法**(Mutex 非重入 → 自死锁);需要走 services 写方法时用 `db_call`(连接由服务内部按需获取)。
- **失败出口**:阻塞任务 JoinError / 连接池错误 / 服务内 String 错误统一由 api 层 `db_err` 转 500 + `code: "DB"`;锁中毒一律恢复(`.lock().unwrap_or_else(|e| e.into_inner())`),不 panic。

---

## 8. 日志

- 位置:`logs/kedai-YYYY-MM-DD.log`,按天归档(自研 DailyFileWriter,命名与旧 logger 时代一致)
- 格式:pino 风格 JSON 行:`{"level":"info","time":"...","msg":"...",...自定义字段}`(2026-09 D-4 起键序非字母序:level/time/msg 在前;离线比对脚本注意)
- 实现:tracing + tracing-subscriber(PinoFormat)+ non-blocking 双 channel(stdout/文件),热路径无锁;`RUST_LOG` 环境变量覆盖级别(默认取 LOG_LEVEL/info,白名单压制第三方库 debug 噪音)
- 双写:控制台 + 文件;启动时清理 3 天前的旧日志
- 新增日志字段约定(见 utils/logging.rs 头部):字符串走 record_str 绝不回解析;复合 JSON 值经 `%JsonField(v)` 透传
- 排查问题先看 `logs/` 当天文件

---

## 9. 常见维护操作

| 场景 | 操作 |
|---|---|
| 换 LLM 后端 | 改 `.env` 的 OPENAI_BASE_URL / API_KEY,重启 |
| 换默认模型 | 改 OPENAI_MODEL;或运行中在设置界面切换(持久化于内存,重启失效) |
| 改默认系统提示词 | 改 `data/settings.json` 的 `agent_system_prompt`(优先级最高,保存即生效);运行中的服务需重启才读取;代码内置默认在 `server-rs/src/services/settings_service/params.rs`(default_roleplay_agent_prompt,新装/空值回填用);显式清空时另由引擎兜底模板兜底:`server-rs/src/agents/engine/messages/build.rs`(build_llm_messages_with_position),均需重编译 |
| 演示模式 | CONNECTOR=mock 或清空 API Key,启动即演示 |
| 端口被占 | `netstat -ano \| findstr ":3001"` → `taskkill /f /pid <pid>`;注意可能残留 Node 旧服务 |
| 前端改了没生效 | 需重新 `.\build.ps1`(exe 内嵌的是编译期快照;开发期则靠 web/dist 磁盘优先) |
| 数据迁移/备份 | 停服后复制 `data/` |
| 磁盘瘦身 | 见下方「空间清理与数据布局」:可安全删除 `server-rs/target`、`node_modules`、`sandbox`、`dist`、`logs` 等可再生目录 |
| 跑测试 | vcvars64 环境 + `cargo test` |
| 深链设置 | 浏览器访问 `http://127.0.0.1:3001/#settings` 直接开设置 |

### 空间清理与数据布局(磁盘瘦身)

> 项目主要占用来自**构建缓存**(可安全删除、随构建自动重建);用户数据集中在 `data/`,**切勿删除**。

| 目录 | 典型大小 | 可否删 | 恢复方式 |
|---|---|---|---|
| `server-rs/target/` | 数 GB~11GB | ✅ | `cargo build/test`(vcvars64 环境,全量重编) |
| `src-tauri/target/` | ~2.3GB | ✅ | `build.ps1 -Tauri` 重打桌面包 |
| `node_modules/`(根) | ~112MB | ✅ | `npm install`(根目录,npm workspace 自动装 `web/`) |
| `sandbox/` | ~65MB | ✅ | 需参考插件源码时从 GitHub 重新下载 |
| `dist/` | ~28MB | ✅ | `build.ps1 -Tauri` 重建便携版 |
| `data-backup-before-v3fix/` | ~20MB | ✅ | 一次性旧备份,删后不可恢复 |
| `launcher/target/` | ~10MB | ✅ | 编译启动器时重建 |
| `logs/` | ~1MB | ✅ | 运行日志,启动时自动重建(桌面场景在 `%APPDATA%\com.kedai.app\logs`) |
| **`data/`** | — | ❌ | **用户数据**:`kedai.db`(SQLite)、`characters/`(角色卡原图+JSON)、`avatars/`、`settings.json`、`prompt_floors.json`、`agent_flows.json`、`AGENTS_RUNTIME.md` |
| `web/dist/` | — | ❌(建议保留) | 前端产物,exe 编译期内嵌;删除后仅影响「磁盘优先」调试路径,内嵌版本仍可用 |
| `.env` / `src/` / 文档 | — | ❌ | 源码与配置 |

- **清理命令**(PowerShell,应用停止时执行):
  ```powershell
  Remove-Item server-rs\target, src-tauri\target, launcher\target, node_modules, sandbox, dist, logs, data-backup-before-v3fix -Recurse -Force
  npm install   # 恢复依赖(如不删除 node_modules 可跳过)
  ```
- **实测记录(2026-08-12)**:删除上述全部目录回收约 **13.5GB**(server-rs/target 11.3G + src-tauri/target 2.3G + node_modules 112M + sandbox 65M + dist 28M + data-backup 20M + launcher/target 10M + logs 1.2M)。`data/`(角色卡、数据库、设置)与 `web/dist` 未动,应用数据完整。
- **注意**:删除 `server-rs/target` 后,首次 `cargo build/test` 需全量重编(集成测试编译约 2~5 分钟量级);删除 `node_modules` 后必须 `npm install` 才能构建/开发前端。

---

## 10. 已知问题与注意事项(踩坑记录)

1. **cargo 必须 vcvars64 环境**:新终端直接 `cargo build` 会报 `link.exe not found`。用 `build.ps1` 或先 `call vcvars64.bat`。
2. **std::sync::Mutex 不可重入**:所有 `services` 层 DB 操作持锁期间禁止再调用同类方法(会死锁)。目前代码已按"先释放锁再查"处理,新增代码须遵守。
3. **PowerShell 5.1 中文乱码**:`start.ps1` 需 UTF-8 with BOM;`Get-Content` 读中文文件建议 `-Encoding UTF8`。`Write-Content` 默认 UTF-16,写 JSON 给 curl 会带 BOM 导致解析失败(冒烟曾踩坑)。
4. **PowerShell 里 `curl` 是别名**:实际是 `Invoke-WebRequest`,用 `curl.exe` 才是真 curl。
5. **axum 0.8 路由语法**:路径参数用 `{id}` 而非 `:id`;multipart 由 `axum::extract::Multipart` 提供(multer 在 one-shot 测试流下会挂起,故 upload 用了手动字节解析)。
6. **端口残留**:后台/被 kill 的旧实例会短暂占用 exe 句柄,重新编译前确保无 `kedai-server.exe` 进程(`Get-Process kedai-server`)。
7. **tiktoken-rs 首次运行需联网下载 BPE 词表**(与 js-tiktoken 一致);token 计数按模型映射 o200k/cl100k/p50k,未知模型回退 cl100k。
8. **history 注入**:engine 构造 LLM 消息时,`system` 历史消息被跳过(与 Node 版一致,因历史不带 extra),`memory` 消息的注入逻辑在 Node 版存在但当前调用路径不传入 extra,保持行为一致。
9. **`.env` 修改即时性**:配置在启动时读取,改后需重启。
10. **日志编码**:Windows 控制台显示日志中文可能乱码(GDK/UTF-8 混排),不影响落盘内容,可用编辑器以 UTF-8 打开 log 查看。
11. **便携版 README.txt 保留 UTF-8 BOM(有意)**:`dist\Kedai-portable\README.txt` 带 EF BB BF 头——Windows 记事本对无 BOM 的 UTF-8 中文按 ANSI 猜解会乱码,BOM 是兼容性刻意选择,不要在构建脚本里「修复」掉它(2026-09 D-8 注明)。
11. **mvu 系统来源澄清(重要)**:本项目的 mvu 变量系统位于 `web/src/mvu/`(前端)+ `server-rs/src/parsing/assistant/`(后端),是 **MagVarUpdate**(原作者 MagicalAstrogy,`github.com/MagicalAstrogy/MagVarUpdate`,MIT)的独立兼容实现。曾有一份外部 AI 分析以某 SillyTavern 扩展的 `bundle.js`(含 `generation_id` 随机 UUID、`遵循<must>指令`、`兼容假流式`、`额外模型解析配置`、`ordered_prompts` 数组)为依据得出「缓存命中率极低 5 因素」结论——**这些代码在本项目全部不存在**,该分析不适用于 Kedai。Kedai 真实的缓存注意点:变量树经 `{{format_message_variable}}`/`{{getvar::stat_data…}}`/EJS 宏展开进 system 提示词,变量更新会使 system 前缀变化、前缀缓存失效;可用设置 `mvu_vars_position=user_tail` 把变量块挪到最新用户消息尾部以提升命中率(见 API 契约节)。
12. **导入 ST 预设的空楼层过滤**:导入酒馆预设后,纯 `{{addvar}}` 累积宏的楼层展开为空字符串(宏自身输出空),engine 组装消息时已跳过空楼层/空注入(`agents/engine/messages/build.rs` build_llm_messages_with_position),不会产生空 user/assistant 消息;但这类楼层没有可注入的实质内容,若希望「累积宏 + 末尾 getvar 输出」的拼接语义生效,需把实际内容放在带 `{{getvar}}` 输出的楼层里(如「可待精华增强楼层」的写法)。
13. **变量协议示例不要写进恒存在的 system 提示词**:`<UpdateVariable>` 输出协议示例只应由 `make_state_block` 在角色卡有变量树时注入。若把示例硬编码进默认/自定义 system 提示词,无变量树的普通卡也会看到协议:mock `[[floors]]` 回显 system 时示例会被 `parse_update_variable` 解析成真实补丁、空树建树并推送 vars 事件(曾有集成测试全红);真实场景下模型也可能模仿示例输出补丁、误激活变量系统。注意 `apply_mvu_patches` 允许空树建树(用户/模型显式输出补丁是合法语义,`pure_mvu_*` 测试依赖此行为),隔离靠「不暴露协议」而非「禁止空树应用」。
14. **沙箱 realm 与酒馆不同:同卡脚本共享 window 需显式机制**(2026-09 修复记录,commit f2c10d1)。Kedai 每脚本一个不透明源 iframe,同卡多个 tavern_helper 脚本**互不可见 window 全局**——ST 生态脚本「th-A 挂 window.WuWaShared → th-B 读」的写法在这里会失效,直接裸读 `top`/`parent` 还抛 SecurityError(WuWa Solaris-3 卡崩溃根因)。两条通道(按需二选一,勿混):① **同卡同 realm**:卡级脚本已合并进单 iframe,靠逐 `<script>` 注入共享 window(web/src/cardScriptHost.ts + boot-script.ts 多段形态),同段组内的脚本可互读全局;② **跨 realm 共享桥**:不同沙箱(消息级脚本、不同时机起的卡级组)之间靠 shared-globals 快照桥(`web/src/sandbox/shared-globals.ts`),只同步合法键 + JSON 可序列化 ≤256KB 的 window 自有属性,函数/DOM 引用不共享(见 docs/known-limitations.md L5)。另:**运行期注入 `<style>` 会触发 dom-rpc 的声明级清洗 + 容器作用域化**——容器必须带 `data-kd-scope`(渲染块自带,卡级容器由 cardScriptHost 设 `cardScopeId`),否则样式段退回纯白名单仍被剥,界面裸渲染。
15. **悬浮窗脚本依赖的 jQuery 面比想象宽**(2026-09 修复记录,commit 280e1cf)。WuWa 卡三个悬浮窗暴露了三类缺口,新增卡脚本报「悬浮球不显示/拖不动/切卡残留」时按此排查:① **jQuery UI `.draggable()` 沙箱没有**——th-5 无 `typeof` 守卫会直接 TypeError 中断整段脚本(连悬浮球都不挂);现在宿主侧 `web/src/sandbox/draggable.ts` 提供最小实现(handle/cancel/containment/distance + start/drag/stop 回调),能力边界见 known-limitations L6。② **`$('head')` 与 `.css({...})`**:`queryScoped` 只特判 body/html 时,`$('head').append('<style>…')` 零匹配静默丢失(面板失去 position:fixed/display:none → 落进消息流);`.css({k:v})` 对象形式若被当 getter 吞掉,`$('<div>').css({position:'fixed'})` 全部丢失;created 元素 `.attr('id',…)` 同理。③ **注入的 DOM 无人回收**:cleanup 原本只销毁 iframe/监听器/订阅,脚本 `$(window).on('unload')` 钩子在沙箱内静默失效 → 切卡后幽灵悬浮窗。现由 `data-kd-injected` 标记 + `data-kd-overlay-root` 覆层根统一清理;**capture 登记的监听器必须带 capture 摘除**(`removeEventListener` 的 capture 不匹配会摘不掉,拖拽中切卡残留 document 监听)。
16. **改完必须重跑 `build.ps1` 才进 exe**(2026-09 实测八项修复复核)。`cargo test`/`npm run build` 只更新 `server-rs/target/` 与 `web/dist/`;`dist\kedai-server.exe`、`dist\Kedai-portable\Kedai.exe` 与项目根 `Kedai.exe` 都不会自动更新,交付前必须跑一次完整 `.\build.ps1`(前端 + release + 便携版 + 指纹 sidecar)。
17. **16GB 内存 + 11GB 页面文件下禁止全并行 debug 链接**(2026-09 实测踩坑)。`cargo test`/`cargo build`(debug)全并行链接 `libkedai_server-*.rlib`(带 debuginfo 约 665MB)会触发 `os error 1455 页面文件太小`,rlib 被写坏后**后续所有编译持续报 E0786「invalid metadata files」**,表现为莫名其妙的全量失败。规避:`.shakedown/krun.bat` 已封装 `vcvars64 + CARGO_BUILD_JOBS=4 + debuginfo=0`;遇到 E0786 先删坏 rlib(`rm server-rs/target/debug/deps/libkedai_server-*.rlib`)再重建。**注意 release 构建(build.ps1)不受影响**(LTO + strip,产物小),但构建期间不要与前端 vitest/cargo test 并发跑,内存争用会让链接器崩(0xc0000409)。
18. **世界书条目的 depth/role 在角色卡里常放在 `extensions`**(2026-09 第四轮实测踩坑)。顶层 `depth` 缺省 4;而真实卡(赛马娘 95 条、爱托邦 109 条、吸血鬼 54 条)**全部**把 `depth` 写在 `extensions`,顶层没有。解析必须两侧都读,否则作者设的 1/2/5 一律退化到 4:depth=1/2 被放宽 → 关键词在窗口外仍命中、污染提示词;depth=5 被收窄 → 漏注入。`probability`/`use_probability` 早有 extensions 回退,`role` 还须兼容 ST 数字(`0/1/2` → system/user/assistant)。改这条解析时对照 `parsing/world_book.rs` 的 `read_role`/depth 段与 `probability` 段保持同一写法;测试要覆盖「顶层优先 / 仅 extensions / 两者都缺省」三态(第四轮修复记录见此文 §10 之后的 `docs/实测四轮修复-变更说明.md` F1)。
19. **卡内 CSS 注释会吞掉紧随的声明**(2026-09 第四轮实测踩坑)。`/* 说明 */\n background-image: …` 的属性名会被声明切分当成 `/* 说明 */ background-image`,不匹配属性白名单 → **整条声明被丢弃**。赛马娘卡因此丢了 `body` 的纸纹背景、`.select-item` 的 `gap`、`.description-box` 的 `padding`。修法是切分前先剥注释(`cssSanitize.ts` 的 `stripCssComments`,引号感知以免误伤 `url("https://a/*.png")`);**必须在 `splitCssBlocks` 之前剥**——注释里含 `{}` 会先破坏顶层块切分。改 CSS 清洗管线时注意 `sanitizeScopedCss`(规则体)与 `sanitizeCssDeclarations`(规则体 + style 属性)两个入口都要经过剥注释。
20. **压缩 / 备份仓库报「系统找不到指定的路径」= jniLibs 里的 .so 符号链接悬空**(2026-09 实测踩坑)。`tauri android build` 为省空间不在 `gen\android\app\src\main\jniLibs\<abi>\` 存真文件,而是**建符号链接**指向 `src-tauri\target\<triple>\release\libkedai_desktop_lib.so`;构建收尾又会删除 `src-tauri\target` 回收磁盘 → 链接悬空。此后用资源管理器 / 7-Zip / HaoZip 压缩或备份仓库,工具跟随不到目标即报 `...libkedai_desktop_lib.so: 系统找不到指定的路径` 并中断,产物不完整。**判定陷阱**:悬空链接上 `Test-Path` 仍返回 `True`(PS 5.1 不穿透解析),必须比对 `(Get-Item -Force).Target` 是否存在;只看 `Test-Path` 会漏掉。现已自动化:`tools\Write-BuildStamp.ps1` 的 `Clear-KedaiDanglingJniLibs` 在删除 target 后清理悬空链接(`build.ps1` 的 `-Tauri` 分支与 `tools\build-portable.ps1` 收尾均已接入,幂等,只删 SymbolicLink 不碰真实文件)。这些链接与 .so 本就是派生文件(`gen/android/app/.gitignore` 已忽略),下次 Android 构建自动重建。
21. **压缩工具会「跟随」junction,把外置目录的体积一起装进压缩包**(2026-09 实测确认:HaoZip 对 junction 是跟随而非跳过)。若把构建产物用 junction 外置(如 `src-tauri\gen\android\app\build` → `D:\kedai-build\...`,1.4GB),则压缩 `D:\kedai` 得到的包会明显大于目录本身——实测 218MB 的目录压出 417MB 包。需要「压缩包 ≙ 目录可见体积」时,压缩前应删除或排除 junction 目标,或改用 7-Zip 的 `-xr!` 排除;另注意**悬空 junction 会被静默跳过、不报错**,与悬空 symlink(条目 20 的报错)行为不同。

---

## 11. 测试

```bash
cd server-rs && cargo test
```

- 单元测试(源文件内 `#[test]`/`#[tokio::test]`,740 个,2026-09-12 实测):状态机迁移、planner(fast/deep/算式识别)、reflector(3 规则)、calculator(白名单解析)、censor(禁词同义替换)、token 编码映射与估算、工具注册表、世界书转换、世界书注入(含 `extensions.depth` 扫描窗口)、提示词注入(含禁词库)、mvu 变量系统(含 JSONPatch 转义/reason/delta 容错)、EJS 渲染器(含读取 API 与 escape-ejs)、角色卡解析、正则脚本、@INJECT 解析/应用、GENERATE 注入、运行时提示词内置默认回退、角色扮演默认提示词内置(from_config + load 空值回填)、结构化错误码(api/errors.rs)、任务编排工具契约(agentgo 逐项校验/子任务截断判失败/read subtask 多键命中/todo 跨 agent 可见/agentend interrupted)
- API 集成测试(`tests/` 18 个文件,171 个,mock 连接器 + 临时数据目录):api_integration、assistant、agent_flows、tasks、task_events、prompt_inject、world_books、settings_connector、security、contracts_e2e、scripts_e2e、scripts_import、swipe_regenerate、undo、user_scripts、variables_scopes、db_concurrency、macros——health、角色 CRUD(multipart 上传)、会话/消息/导入导出、设置与 token、agent plan、SSE 聊天流、任务引擎六模式、计算器工具 SSE、世界书/角色卡、提示词注入与酒馆预设导入、鉴权
- 前端 `npm test -w web`(Vitest,686 个 / 68 文件,2026-09-12 实测):stores、api client(含 ApiError 错误码分类)、组件与 composables、CSS 清洗(含注释处理)与沙箱回归;类型门禁 `npm run typecheck -w web`(vue-tsc,**硬门禁**,存量 168 已于 2026-09-08 清偿归零,清偿记录见 docs/优化实施方案-2026-09.md 附录 D)
- 新增接口建议同步补集成测试;测试环境变量 `CONNECTOR=mock` 强制隔离

---

## 12. Roadmap(来自 README,未实现)

- Oobabooga / KoboldAI 连接器适配(世界书按 key 注入、Tauri 桌面化已完成)
- LLM 原生 function calling 全链路(当前工具为规则触发 + 部分 function calling)
- 工具执行沙箱隔离(角色卡脚本已有 iframe 沙箱,工具侧未做)
- 知识库向量检索工具
- (2026-08 已完成项移出:智能上下文压缩、自定义工具注册、缓存感知压缩管线、跨会话记忆蒸馏、技能渐进披露与子代理调度守卫——见第 14 章)

---

## 13. 相关文件对照

| 关注点 | 文件 |
|---|---|
| 启动 | `start.ps1`(统一启动器;Rust 启动器工程见 `launcher/`) |
| 构建 | `build.ps1` |
| 配置示例 | `.env.example` |
| API 文档 | `API.md`(与代码基本一致;`/api/settings/info` 实际含 `models` 字段) |
| 用户指南 | `README.md` |
| 原 Node 后端(参照) | 已归档删除(历史版本存于代码历史,不在仓库内) |

---

## 14. 新模块速览(2026-08 补记)

> 以下模块晚于本文档上次整理(2026-08-12)落地,此处补记维护入口;详细设计见 `docs/`。

### 万花筒契约 DSL(contracts)

- 位置:`server-rs/src/contracts/`(op / field / due_fields / observe / changelog / invariant / multi_step / render / state / validation / registry / extract / meta)。
- 职责:变量系统唯一事实源——字段定义、更新策略、护栏、不变量、置信度门控、熔断指纹;HTTP 出口 `/api/variable/update|state|changelog`。
- 配套服务:`services/kaleido_state_service.rs`(注意其中 FNV-64 为 SHA-256 占位,尚未兑现)、`services/variable_apply.rs`(统一写入出口)。
- 前端:`components/ContractsModal.vue` + `contracts/contractDiff.ts`。
- 预留未实现:M10+(achievements/ejs/runBoundary 等 Option 字段)、M13/M15/M16(plot/dice/memory 写者),见 `contracts/mod.rs`。

### 任务工作台(task)

- 位置:`services/task_service/`(目录模块:mod/db/cancel/events/executor/parse/prompt,legacy 三段式 planning → running(逐步派子智能体)→ done)+ `services/task_engine/`(六模式底座:solo/multi/plan/team/custom,详见 [docs/任务引擎六模式.md](docs/任务引擎六模式.md))+ `api/` 任务路由 + `web/src/components/TaskBoard.vue` + `api/tasks.ts`。

### 上下文压缩(compaction)

- 位置:`agents/engine/compaction.rs`(可逆投影 + LLM 摘要;原文消息永不删除,摘要存 `session_compactions` 表,删摘要行即恢复完整历史)。
- 触发:manual(`/api/chat/compact`)或 auto(历史 token 超阈值);设置项 `compaction_mode` / `compaction_threshold`。
- 2026-08 起配合「缓存感知压缩管线」升级(usage 缓存落库、四级水位、摘要槽增量式),设计见 `docs/learn-harness-2026-08.md`。

### 缓存感知压缩管线(2026-08 新增)

- 位置:`services/cache_diagnostics.rs`(命中率/费用/水位汇总)+ `api/diagnostics.rs`(`GET /api/diagnostics/cache`)+ `connectors/openai_compatible/`(目录模块:mod/retry/sse_parser/tests;usage 5 元组解析,DeepSeek `prompt_cache_hit_tokens` 优先、OpenAI `cached_tokens` 回退)。
- 落库:`llm_requests` 表新增 usage 列(幂等迁移 `ensure_llm_requests_usage_columns`);轻量 usage 行恒落库,与请求快照开关解耦。
- 前端:`components/CacheHealthPanel.vue` + `cacheHealth.ts`(「优化」弹窗内,缓存健康面板)。
- 压缩升级:摘要槽独立(system → 摘要槽 → 记忆槽 → 历史,`messages/inject.rs` 的 `insert_summary_slot` / `insert_memory_slot`、`messages/trim.rs` 的 `protected_head_len`);摘要改追加式增量(旧段字节冻结);LLM 摘要前先 snip 超长陈旧工具结果(`compaction.rs` 的 `snip_tuples` / `should_snip`,错误特征保留、尾部 2 条原文保留);`compaction_keep_recent`(默认 4)与 `compaction_snip_bytes`(默认 8192)可配置。

### 跨会话记忆蒸馏(2026-08 新增)

- 位置:`services/memory_service.rs`(distill_session 以闭包注入 LLM,mock 可测)+ `api/memory.rs`(distill/search/prune/list/create/update/delete 七端点)+ `tools/memory.rs`(agent 主动写记忆落同表)。
- 表:`memory_entries`(character_id 维度、kind CHECK distilled|tool|manual、usage_count、last_usage、selected、pinned、索引)。
- 注入:精选排序 usage_count DESC → last_usage DESC → id DESC,注入摘要槽之后的记忆槽;使用后在 step_loop 成功路径 touch。
- 前端:`components/MemoryPanel.vue` + `memoryPanel.ts` + `composables/useMemoryPanel.ts`。

### 技能渐进披露与子代理守卫(2026-08 新增)

- 位置:`services/skill_service.rs`(manifest 固定格式清单,按 name 字节序稳定)+ skills 表新增 `allowed_tools` / `run_as_subagent` / `model` 三列。
- 预载仅 name+description(设置 `skill_progressive_disclosure`,默认开;旧技能正文从未预载,关闭即回退零注入)。
- 子代理:`tools/agent_tools_agent.rs` 深度守卫(`subagent_max_depth` 默认 2)、并发守卫(`subagent_max_concurrency` 默认 6)、结果截断(`subagent_result_max_chars` 默认 2000,截断附尾注)。
- 工具治理:`tools/registry.rs` 错误文案带「下一步怎么做」指引;定义顺序按 name 稳定(有跨构建序列化一致性测试,保前缀缓存)。
