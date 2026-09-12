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
- 不顺手实现未要求的安全/API 批次，不提交 Git。

## 任务模式机制要点(2026-08-28 重构后)

- 后端任务引擎分两层:`server-rs/src/services/task_service/`(任务 CRUD/状态/取消/legacy 三段式执行/事件)与 `server-rs/src/services/task_engine/`(批次 4 六模式底座:ModeExecutor/TaskRunContext/事件桥;solo/multi/plan/team/custom 执行器,legacy 仍走 task_service 原路径);任务状态为枚举 `TaskStatus`/`TaskStepStatus`/`TaskSubtaskStatus`,运行模式为 `TaskRunMode`(`models/types.rs`,serde snake_case 字符串,落盘与 API 线格式不变)。六模式语义见 `docs/任务引擎六模式.md`。
- 数据流:DB 写入成功后 `TaskService` 经 broadcast 通道发射 `SseEvent::Task`,`GET /api/tasks/events` 转发为 SSE;前端 `web/src/stores/task.ts` 事件驱动精确刷新(无轮询;断线指数退避重连 + 5s 兜底轮询)。新增事件 kind 需前后端三处同步(枚举/发射点/store 刷新映射)。
- 提示词共享原语在 `server-rs/src/services/prompt_kit.rs`(世界书过滤/注入合成/占位符渲染/`untrusted_boundary`),引擎与任务双侧调用,勿在任一侧复制实现。
- 双模式提示词隔离由类型承载(`RoleplayPromptConfig`/`TaskPromptConfig`,settings.json 线格式不变);新增涉及双模式的设置字段时,继承/隔离规则先读 `docs/模式提示词边界.md`。

## 验证命令

```powershell
cargo test --manifest-path server-rs/Cargo.toml
npm test -w web
npm run build -w web
cargo build --manifest-path server-rs/Cargo.toml
```

## 构建提醒(后续模型必读)

- **改完任何前后端代码后,`dist\` 下的 exe 与项目根 `Kedai.exe` 不会自动更新。** 用 `target\debug\` 二进制验证通过后,交付/试用 exe 版前必须跑一次 `.\build.ps1`(默认双端同步:前端 + release + 便携版 + 构建指纹 sidecar;详见 MAINTENANCE.md §4)。只改了代码只跑过 debug,不等于 exe 版已更新。
- 快速迭代可用 `.\build.ps1 -TestOnly`(只产测试版),但记住便携版会因此未同步,交付前补一次完整 `.\build.ps1`。
- **本机环境**:裸 PowerShell/cmd/Git Bash 里可能没有 cargo 环境(MSVC toolchain 未注入)。构建/测试前先执行
  `call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"`,
  或在已加载 vcvars64 的 shell 里再调用 build.ps1 / cargo;Git Bash 中可用 `cmd //c "<bat 包装>"` 形式嵌套。
- Windows 上 curl 直接 `-d` 中文 JSON 会被编码破坏(invalid unicode code point);中文请求体一律写 UTF-8 文件后用 `--data-binary "@file"`。
