# Kedai 维护指南(MAINTENANCE)

> 面向后续维护者的技术文档。涵盖架构、构建、启动、API 契约、数据库、日志与已知坑位。
> 版本:v0.2.0(前端 Vue3 + 后端 Rust + Tauri 桌面壳) 最后更新:2026-08-12

---

## 0. 架构治理(三结合梯队架构)

**总纲**:借鉴「老中青三结合」组织原则,代码分三层——L1 老层·稳(Anchored Core:parsing/ 兼容解析、models 数据契约、测试套件、SillyTavern 兼容承诺)、L2 中层·干(Orchestration:agents/、services/、api/)、L3 青层·活(Frontier:tools/、skills/、沙箱、新能力)。四种机制:优势互补、传帮带晋升、梯队衔接、动态循环。**详见 `docs/ARCHITECTURE-3H.md`(结构性改动先读它再动代码)**。

关键纪律:
- **改 L1 契约**须先写失败测试、声明兼容影响、过评审;协议翻译集中在中层。
- **Mutex 纪律**:全项目锁中毒一律恢复(`.lock().unwrap_or_else(|e| e.into_inner())`),不 panic;**禁止持 std::sync::Mutex guard 跨 `.await`**(Clippy `await_holding_lock` 防护)。
- **跨端协议**(前后端 mvu)以 `docs/mvu-protocol.md` 锁定双端一致,防行为漂移。
- 新能力默认进 L3 隔离验证,成熟后按晋升通道(测试通过 + 不破坏协议 + 评审)升级。

### 新能力晋升状态(三结合梯队)

| 能力 | 当前代际 | 状态 |
|---|---|---|
| GENERATE/RENDER 内容注入([GENERATE:BEFORE/AFTER]、{idx}、REGEX) | L3 → 拟 L2 | 已在 engine 挂载,测试覆盖;协议文档待补 |
| @INJECT 精确消息插入(pos/target/regex) | L3 | messages.rs 9 个测试;待沉淀协议文档 |
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
│   │   ├── api/                # 路由层(mod.rs 路由组装/头像/静态/SPA 回退,CORS 仅放行本机)
│   │   ├── agents/
│   │   │   ├── engine/         # 目录模块(拆分自原 engine.rs):mod.rs(主流程)+ messages/worldbook/executor/mvu/reflector_integration
│   │   │   ├── state_machine.rs# 8 状态 + 迁移表(幂等迁移)
│   │   │   ├── planner.rs      # fast/deep 计划 + 算式识别
│   │   │   └── reflector.rs    # 质量反思(空/截断/未答疑问 3 规则)
│   │   ├── connectors/         # LLM 后端适配(openai_compatible / mock)
│   │   ├── tools/              # 工具系统(registry / calculator / memory / agent_tools)
│   │   ├── models/             # db.rs(SQLite 建表)/ types.rs(契约类型)
│   │   ├── parsing/            # character_card.rs + assistant/(ejs/ 目录模块:mvu 变量渲染)
│   │   ├── services/           # character / session / agent_session / token 服务
│   │   └── utils/logger.rs     # pino 风格结构化 JSON 日志(按天归档)
│   └── tests/                  # API 集成测试(api_integration/assistant/agent_flows/prompt_inject/settings_connector/security/world_books)
├── web/                        # 前端:Vue 3 + Vite + Tailwind v4 + Pinia
│   └── src/
│       ├── api/                # 目录模块(拆分自 api.ts):client/types/characters/sessions/worldbooks/... + index 聚合
│       ├── store.ts            # Pinia 全局状态(setup store,单一)
│       ├── sseReducer.ts       # SSE 事件纯函数(拆分自 store)
│       ├── mvu/                # 前端 mvu 变量系统(parser/variables/host/mvuStore)
│       └── components/         # Sidebar / ChatWindow / ChatInput / AgentDock / SettingsModal
├── src-tauri/                  # Tauri 2 桌面壳:窗口加载 http://127.0.0.1:3001,进程内复用 run_server
│   ├── src/lib.rs              # 数据目录注入(%APPDATA%\com.kedai.app)+ 服务自检/启动
│   └── tauri.conf.json         # 窗口 1280×800、NSIS 打包、图标
├── tools/make-icons.ps1        # 品牌图标生成脚本(圆角 + 透明背景,零依赖)
├── start.ps1                   # 智能启动:优先 Tauri 桌面应用,回退浏览器模式
├── build.ps1                   # 一键构建(前端 + Rust release;-Tauri 追加桌面打包)
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
| 一键启动(桌面) | `.\start.ps1` | 优先启动 Tauri 桌面应用(带 logo 窗口);未构建桌面产物时回退浏览器模式 |
| 桌面安装包 | `.\build.ps1 -Tauri` | 前端 + Rust release + NSIS 安装程序(需 `@tauri-apps/cli`);安装后从开始菜单启动 |
| 一键构建 | `.\build.ps1` | 前端 web/dist + Rust release;`-Dev` 仅前端 `-NoWeb` 仅 Rust `-Tauri` 追加桌面打包 |
| 后端测试 | `cd server-rs && cargo test` | 约 241 个测试(约 175 单测 + 66 集成),**需在 vcvars64 环境** |
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

`kedai-server.exe` 通过 `include_dir!` **内嵌** `web/dist`(编译期快照)。启动时:
1. 若磁盘存在 `web/dist` → 优先读盘(便于开发调试)
2. 否则回退内嵌版本

因此**每次改前端后必须重新构建 exe** 才会带上新界面。

### 构建流程

```powershell
.\build.ps1          # = npm run build -w web  +  cargo build --release
```

产物:`server-rs\target\release\kedai-server.exe`(约 20~30MB,单文件,含 LTO 优化,编译约 2~5 分钟)。

### 部署方式

把整个 `kedai/` 目录(或仅 `exe + data + logs`)拷到目标机即可。exe 通过向上遍历定位项目根(找 `web` 目录),因此 **exe 需放在 `server-rs/target/release/` 原路径,或保证其上级存在 `web` 目录**。若想单独分发,保持目录结构即可。

---

## 5. 启动与配置

### 启动器(kedai.exe,推荐)

C# 编译的本地 exe(源码 `launcher.cs`,Windows 自带 .NET Framework 编译器):
1. 检测 `http://127.0.0.1:3001/api/health` → 已在运行则直接开浏览器退出
2. 产物缺失 → 自动调用 `build.ps1`(首次编译 2~5 分钟)
3. 启动 `server-rs\target\release\kedai-server.exe`(未配置 Key 时注入 CONNECTOR=mock)
4. 轮询就绪(60s 超时)→ 自动打开浏览器 → 按 **Q** 关闭服务
5. `--smoke`:启动→就绪→打印 `SMOKE_OK`→关闭(自动化验证用)

> 重新编译启动器(改源码后):
> ```
> "C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe" /nologo /codepage:65001 /out:kedai.exe launcher.cs
> ```
> PowerShell 版 `start.ps1 / 启动Kedai.bat` 为备用,逻辑相同。

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

- 错误响应统一 `{"error":"消息"}`
- 400 缺参/校验失败,404 资源不存在,409 会话生成中,204 删除成功,201 创建成功
- 全部时间字段为 ISO 8601 字符串

---

## 7. 数据库(data/kedai.db)

rusqlite(bundled,零原生依赖),**WAL 模式 + foreign_keys ON**。5 张表:

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

> 共 12 张表;迁移逻辑见 `server-rs/src/migration.rs`,表结构定义见 `server-rs/src/models/db.rs`。

### 维护注意

- **数据兼容**:表结构与原 Node 版完全一致,旧 `kedai.db` 可直接使用(已验证)
- **备份**:复制 `data/` 目录即可(SQLite WAL 模式建议先停服再拷,或同时拷 `-wal`/`-shm`)
- **⭐ 代码约定**:`services/*.rs` 中 `self.db.conn()` 返回 `MutexGuard`,**必须在其作用域结束后再调用 `self.get()/self.touch()` 等其他取锁方法**,否则 std::sync::Mutex 重入死锁(曾踩坑,详见 §10)

---

## 8. 日志

- 位置:`logs/kedai-YYYY-MM-DD.log`,按天归档
- 格式:pino 风格 JSON 行:`{"level":"info","time":"...","msg":"...","app":"..."}`
- 双写:控制台 + 文件;启动时清理 3 天前的旧日志
- 排查问题先看 `logs/` 当天文件

---

## 9. 常见维护操作

| 场景 | 操作 |
|---|---|
| 换 LLM 后端 | 改 `.env` 的 OPENAI_BASE_URL / API_KEY,重启 |
| 换默认模型 | 改 OPENAI_MODEL;或运行中在设置界面切换(持久化于内存,重启失效) |
| 改默认系统提示词 | 改 `data/settings.json` 的 `agent_system_prompt`(优先级最高,保存即生效);运行中的服务需重启才读取;代码内置兜底模板在 `server-rs/src/agents/engine/messages.rs`(build_llm_messages_with_position),需重编译 |
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
11. **mvu 系统来源澄清(重要)**:本项目的 mvu 变量系统位于 `web/src/mvu/`(前端)+ `server-rs/src/parsing/assistant/`(后端),是 **MagVarUpdate**(原作者 MagicalAstrogy,`github.com/MagicalAstrogy/MagVarUpdate`,MIT)的独立兼容实现。曾有一份外部 AI 分析以某 SillyTavern 扩展的 `bundle.js`(含 `generation_id` 随机 UUID、`遵循<must>指令`、`兼容假流式`、`额外模型解析配置`、`ordered_prompts` 数组)为依据得出「缓存命中率极低 5 因素」结论——**这些代码在本项目全部不存在**,该分析不适用于 Kedai。Kedai 真实的缓存注意点:变量树经 `{{format_message_variable}}`/`{{getvar::stat_data…}}`/EJS 宏展开进 system 提示词,变量更新会使 system 前缀变化、前缀缓存失效;可用设置 `mvu_vars_position=user_tail` 把变量块挪到最新用户消息尾部以提升命中率(见 API 契约节)。
12. **导入 ST 预设的空楼层过滤**:导入酒馆预设后,纯 `{{addvar}}` 累积宏的楼层展开为空字符串(宏自身输出空),engine 组装消息时已跳过空楼层/空注入(`agents/engine/messages.rs` build_llm_messages_with_position),不会产生空 user/assistant 消息;但这类楼层没有可注入的实质内容,若希望「累积宏 + 末尾 getvar 输出」的拼接语义生效,需把实际内容放在带 `{{getvar}}` 输出的楼层里(如「可待精华增强楼层」的写法)。
13. **变量协议示例不要写进恒存在的 system 提示词**:`<UpdateVariable>` 输出协议示例只应由 `make_state_block` 在角色卡有变量树时注入。若把示例硬编码进默认/自定义 system 提示词,无变量树的普通卡也会看到协议:mock `[[floors]]` 回显 system 时示例会被 `parse_update_variable` 解析成真实补丁、空树建树并推送 vars 事件(曾有集成测试全红);真实场景下模型也可能模仿示例输出补丁、误激活变量系统。注意 `apply_mvu_patches` 允许空树建树(用户/模型显式输出补丁是合法语义,`pure_mvu_*` 测试依赖此行为),隔离靠「不暴露协议」而非「禁止空树应用」。

---

## 11. 测试

```bash
cd server-rs && cargo test
```

- 单元测试(源文件内 `#[test]`/`#[tokio::test]`,约 320 个):状态机迁移、planner(fast/deep/算式识别)、reflector(3 规则)、calculator(白名单解析)、censor(禁词同义替换)、token 编码映射与估算、工具注册表、世界书转换、世界书注入、提示词注入(含禁词库)、mvu 变量系统(含 JSONPatch 转义/reason/delta 容错)、EJS 渲染器(含读取 API 与 escape-ejs)、角色卡解析、正则脚本、@INJECT 解析/应用、GENERATE 注入
- API 集成测试(`tests/` 7 个文件,约 66 个,mock 连接器 + 临时数据目录):api_integration、assistant、agent_flows、prompt_inject、world_books、settings_connector、security——health、角色 CRUD(multipart 上传)、会话/消息/导入导出、设置与 token、agent plan、SSE 聊天流、计算器工具 SSE、世界书/角色卡、提示词注入与酒馆预设导入、鉴权
- 新增接口建议同步补集成测试;测试环境变量 `CONNECTOR=mock` 强制隔离

---

## 12. Roadmap(来自 README,未实现)

- Oobabooga / KoboldAI 连接器适配(世界书按 key 注入、Tauri 桌面化已完成)
- 智能上下文压缩(摘要/滑动窗口)
- LLM 原生 function calling 全链路(当前工具为规则触发 + 部分 function calling)
- 自定义工具注册(OpenAI 格式 JSON 配置)
- 工具执行沙箱隔离(角色卡脚本已有 iframe 沙箱,工具侧未做)
- 知识库向量检索工具

---

## 13. 相关文件对照

| 关注点 | 文件 |
|---|---|
| 启动 | `start.ps1` / `启动Kedai.bat` |
| 构建 | `build.ps1` |
| 配置示例 | `.env.example` |
| API 文档 | `API.md`(与代码基本一致;`/api/settings/info` 实际含 `models` 字段) |
| 用户指南 | `README.md` |
| 原 Node 后端(参照) | 已归档删除(历史版本存于代码历史,不在仓库内) |
