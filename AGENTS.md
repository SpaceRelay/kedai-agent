# AGENTS.md — Kedai 开发说明

> 本文件仅供开发 agent 阅读，用于代码维护、测试与构建；它不是产品运行时提示词，禁止注入聊天模型。
> 产品运行时主 Agent 提示词统一保存在 `DATA_DIR/AGENTS_RUNTIME.md`，由后端文件服务、Engine 与设置页共用同一路径。

## 项目范围

- Rust 后端：`server-rs/`
- Vue 前端：`web/`
- 桌面壳：`src-tauri/`
- 数据目录(运行时,不进仓库)：`DATA_DIR` 环境变量 > `%APPDATA%\com.kedai.app\data\`(已含用户数据时) > 项目根 `data\`；解析逻辑见 `server-rs/src/config.rs`。

## 实现约定

- 面向用户的提示、错误和代码注释使用简体中文。
- 修改核心行为先写失败测试，再做最小实现。
- 不泄露聊天正文、API Key 或本地真实数据。
- 不顺手实现未要求的安全/API 批次。**Git 提交按下方「Git 提交纪律」执行（2026-09-15 起由「一律不提交」改为「完成并验证后按体例提交」）。**

## 任务模式机制要点(2026-08-28 重构后)

- 后端任务引擎分两层:`server-rs/src/services/task_service/`(任务 CRUD/状态/取消/legacy 三段式执行/事件)与 `server-rs/src/services/task_engine/`(批次 4 六模式底座:ModeExecutor/TaskRunContext/事件桥;solo/multi/plan/team/custom 执行器,legacy 仍走 task_service 原路径);任务状态为枚举 `TaskStatus`/`TaskStepStatus`/`TaskSubtaskStatus`,运行模式为 `TaskRunMode`(`models/types.rs`,serde snake_case 字符串,落盘与 API 线格式不变)。六模式语义见 `docs/功能.md`。
- 数据流:DB 写入成功后 `TaskService` 经 broadcast 通道发射 `SseEvent::Task`,`GET /api/tasks/events` 转发为 SSE;前端 `web/src/stores/task.ts` 事件驱动精确刷新(无轮询;断线指数退避重连 + 5s 兜底轮询)。新增事件 kind 需前后端三处同步(枚举/发射点/store 刷新映射)。
- 提示词共享原语在 `server-rs/src/services/prompt_kit.rs`(世界书过滤/注入合成/占位符渲染/`untrusted_boundary`),引擎与任务双侧调用,勿在任一侧复制实现。
- 双模式提示词隔离由类型承载(`RoleplayPromptConfig`/`TaskPromptConfig`,settings.json 线格式不变);新增涉及双模式的设置字段时,继承/隔离规则先读 `docs/契约-协议与配置.md`。

## 验证命令

```powershell
cargo test --manifest-path server-rs/Cargo.toml -j 2
npm test -w web
npm run build -w web
cargo build --manifest-path server-rs/Cargo.toml
```

- **`cargo test` 必须带 `-j 2`**:本机默认并行度下 rustc 自身可能崩溃(`STATUS_STACK_BUFFER_OVERRUN`),报错却伪装成「依赖 rlib 缺失」,照它改依赖只会越改越偏;先降并发复跑(详见 `MAINTENANCE.md` §10 条目 29)。

## 构建提醒(后续模型必读)

- **改完任何前后端代码后,`dist\` 下的 exe 与项目根 `Kedai.exe` 不会自动更新。** 用 `target\debug\` 二进制验证通过后,交付/试用 exe 版前必须跑一次 `.\build.ps1`(默认双端同步:前端 + release + 便携版 + 构建指纹 sidecar;详见 MAINTENANCE.md §4)。只改了代码只跑过 debug,不等于 exe 版已更新。
- 快速迭代可用 `.\build.ps1 -TestOnly`(只产测试版),但记住便携版会因此未同步,交付前补一次完整 `.\build.ps1`。
- **本机环境**:裸 PowerShell/cmd/Git Bash 里可能没有 cargo 环境(MSVC toolchain 未注入)。构建/测试前先执行
  `call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"`,
  或在已加载 vcvars64 的 shell 里再调用 build.ps1 / cargo;Git Bash 中可用 `cmd //c "<bat 包装>"` 形式嵌套。
- Windows 上 curl 直接 `-d` 中文 JSON 会被编码破坏(invalid unicode code point);中文请求体一律写 UTF-8 文件后用 `--data-binary "@file"`。

## Git 提交纪律(2026-09-15 修订:此前为「不提交 Git」)

- **完成一批改动并验证通过后,按仓库体例提交。** 不再要求「一律不提交」——旧纪律导致改动长期滞留工作区,
  无法用 `git log` 追溯「哪次改动为哪批需求服务」,也让回滚失去粒度。
- **提交信息体例**:Conventional Commits 前缀 + 中文 scope 与描述,沿用既有 commit 风格。
  例:`refactor(架构治理): 三结合彻底落地——…`、`fix(可观测性): …`。
  正文写清三件事:**改了什么 / 为何这么改 / 验证证据(实际跑过的命令与结果)**。
- **粒度**:一个逻辑批次一个提交;互不相关的改动不混进同一提交。工作区同时存在多条线时,
  按主题分多次提交(必要时用 `git add <path>` 逐个指定),不要一把 `git add -A` 了事。
- **不提交**:构建产物与其 sidecar(`dist/`、`target/`、`Kedai.exe`、`*.lnk` 等,`.gitignore` 已覆盖)、
  用户数据(`data/`)、密钥(`.env`)、开发 agent 的会话目录(`.zcode/`)。
- **提交前的最低验证**:改了后端跑 `cargo test --manifest-path server-rs/Cargo.toml -j 2`;
  改了前端跑 `npm test -w web`;只改文档可跳过测试,但仍要跑 `node tools/check-arch.mjs` 与
  `node tools/count-tests.mjs --check` 确认门禁不漂移。
- **分支**:主干 `main` + 平台分支 `kedai-Win` / `kedai-Android`(模型见 `docs/契约-协议与配置.md`)。
  仓库当前**未配置远端**;有远端后推送前先确认当前分支,不要在平台分支上提交共享代码。
- **不改写历史**:不 `rebase`/`amend` 已推送的提交;`docs/archive/` 下的历史文档只读不改。
