# Kedai 维护指南(MAINTENANCE)

> 面向后续维护者的技术文档。涵盖架构、构建、启动、API 契约、数据库、日志与已知坑位。
> 版本:v0.3.0-B-beta(前端 Vue3 + 后端 Rust + Tauri 桌面壳) 最后更新:2026-09-16

---

## 0. 架构治理(三结合梯队架构)

**总纲**:借鉴「老中青三结合」组织原则,代码分三层——L1 老层·稳(Anchored Core:parsing/ 兼容解析、models 数据契约、测试套件、SillyTavern 兼容承诺)、L2 中层·干(Orchestration:agents/、services/、api/)、L3 青层·活(Frontier:tools/、skills/、沙箱、新能力)。四种机制:优势互补、传帮带晋升、梯队衔接、动态循环。**详见 `docs/契约-架构与数据.md`(结构性改动先读它再动代码)**。

关键纪律:
- **改 L1 契约**须先写失败测试、声明兼容影响、过评审;协议翻译集中在中层。
- **Mutex 纪律**:全项目锁中毒一律恢复(`.lock().unwrap_or_else(|e| e.into_inner())`),不 panic;**禁止持 std::sync::Mutex guard 跨 `.await`**(Clippy `await_holding_lock` 防护)。
- **DB 并发纪律**:api handler 的同步 DB 调用必须经 `db_call/db_read/db_write`(spawn_blocking),禁止 async 上下文直接持锁;细则见 §7「DB 并发纪律」。
- **渲染性能纪律(前端)**:消息渲染必须走 ChatMessageItem 的缓存 computed,禁止在 v-for 里直接调渲染方法。
- **样式分层纪律(前端,2026-09 D-5 起)**:`web/src/style.css` 只保留层① 设计变量(:root 令牌)与层② 全局基础层;新增组件样式一律 `<style scoped>` 或 Tailwind 工具类,禁止再写入 style.css;修改存量组件时顺手把该组件样式搬进 scoped(「改到谁拆谁」,文件头分层约定注释为准);新增样式不得引入 `!important`(存量 11 处见 style.css)。
- **跨端协议**(前后端 mvu)以 `docs/契约-协议与配置.md` 锁定双端一致,防行为漂移。
- **EJS 自研解释器冻结纪律**:`parsing/assistant/ejs/`(自研迷你 JS 引擎,约 4000 行)**只接受安全修复,不再扩展新能力**。任何新模板能力必须在 `scripts/runtime.rs` 的 rquickjs 沙箱侧实现(rquickjs 自带内存/中断/栈上限,见该文件 `set_memory_limit`/`set_interrupt_handler`)。理由:自研解释器缺引擎级沙箱限额,长期维护成本与风险高于复用;**已加固**(循环步数+墙钟预算、解析深度守卫,见 §10 踩坑记录),但债务不再增长。
- **门禁纪律(2026-09-13 起)**:`tools/check-all.ps1` 是唯一的本地 CI 入口,现已接三处触发——
  ① `build.ps1` 在构建前跑 `check-all -Quick`,失败即中止构建(`-SkipChecks` 仅限本地应急,
  **交付/试用前必须补跑一次完整 `check-all`**);② `tools/hooks/pre-push` 在推送前跑同一检查
  (装一次:`npm run hooks:install`;紧急可 `git push --no-verify`,同样须事后补跑);
  ③ `.github/workflows/ci.yml`(**纸面 CI,从未运行**):2026-09-13 落盘,但仓库
  **始终未配置 git 远端**(`git remote -v` 为空),故从未触发。**当前实际生效的闸门
  只有前两条(build.ps1 与 pre-push)**——不要依赖 CI 兜底。配置远端并推送后它会自动生效,
  届时本节应更新为「三处触发均实测生效」。
  变更 `tools/check-*.mjs` 的检查规则时,同步更新本节与本文件的检查项清单。
- **性能门禁(2026-09-14 起,可选)**:`tools/perf-baseline.mjs` 支持 p95 阈值判定
  (`--max-p95-factor`,默认 1.25),基线值存 `tools/perf-baseline.json`;
  经 `check-all.ps1 -Perf` 并入总门禁(**默认关闭**——需服务已在运行,
  避免把「没起服务」误报成门禁失败;服务不可用时 fail-closed 而非静默跳过)。
  判定口径 `p95 <= 基线 × 倍数` 且 `errors == 0`,`p95 < 5ms` 的端点跳过。
  基线是**本机 release 口径**,只作回归对比基准、不是跨机 SLA;换机器或数据量
  变化后须重采样。数据与用法见 `docs/功能-变更史.md` 的「2026-09-14 实测」小节。
- **代际归属与跨代依赖门禁(规则 I/J,2026-09-14 起)**:`tools/check-arch.mjs` 新增两条护栏,
  其唯一机器可读事实源(SSOT)是 **`tools/arch-layers.json`**——
  - **规则 I(代际归属完整性)**:`server-rs/src` 的每个顶层模块/根文件、`web/src` 的每个顶层目录/根文件
    **必须**在 `arch-layers.json` 登记代际(L1/L2/L3/entry)与职责,**未登记即 FAIL**。
    新增或改名顶层模块时先补登记再动代码——没有代际归属的模块无从按分层纪律评审,是「分层叙事的静默失效点」。
  - **规则 J(跨代依赖方向)**:按登记代际与允许方向校验实测 import 图。**新增未登记的越代边即 FAIL**;
    存量越代边必须在 `arch-layers.json` 的 `registeredEdges`(后端)/`frontendRegisteredEdges`(前端)登记,
    写明 `reason`(为何存在)与 `remediation`(收口路径)。登记表**只减不增**:还清一条删一条,
    **禁止为过检查而加条目**(那等同于关掉护栏)。
  - **前后端方向规则不同(务必区分)**:后端是**隔离模型**——L3(工具/沙箱)与 L2(编排)双向互斥,
    青层反向依赖骨干层会让「隔离」名存实亡;前端是**层次模型**——L3(展示)→ L2(状态编排)→ L1(纯契约)
    是严格向下的正常依赖(组件读 store、store 调 composable),故前端**额外允许 L3→L2**。
    配置分别见 `arch-layers.json` 的 `dependencyRules.backendAllowed` / `frontendAllowed`。
  - **修复纪律(2026-09-14 教训)**:规则 I 的前端分支曾误写入后端失败桶,而汇总在后端违规时立即 `exit(1)`,
    导致**前端规则 J 的结论永不打印**(护栏静默失效,且当时 26 项未登记使构建被阻断)。
    现已改为两个失败桶都打印后再统一退出。修改任何门禁脚本的失败聚合逻辑时,
    **必须验证两侧报告都能输出**,并确认退出码仍非 0。
- **依赖供应链纪律(2026-09-13 批次 1 起)**:三把闸已进 `check-all`——
  ① `deps: check-lock-sync`(双 Cargo.lock 漂移检查:src-tauri 内嵌 server-rs 时 cargo 会
  重新解析依赖树,同一后端源码可能编出不同版本依赖;**0 漂移为基线**,新增即 FAIL,
  例外登记在 `tools/lock-sync-baseline.json`);
  ② `cargo audit` 默认**硬门禁**(server-rs 锁历史 0 洞;仅 advisory DB 拉取失败时降级 WARN,
  `-LooseAudit` 可临时降档);
  ③ `npm audit --omit=dev` 警告档(历史 2 洞——sanitize-html 存储型 XSS / nanoid——已修复)。
- 新能力默认进 L3 隔离验证,成熟后按晋升通道(测试通过 + 不破坏协议 + 评审)升级。

### 新能力晋升状态(三结合梯队)

| 能力 | 当前代际 | 状态 |
|---|---|---|
| GENERATE/RENDER 内容注入([GENERATE:BEFORE/AFTER]、{idx}、REGEX) | L3 → 拟 L2 | 已在 engine 挂载,测试覆盖;协议文档见 docs/契约-协议与配置.md |
| @INJECT 精确消息插入(pos/target/regex) | L3 | messages/inject.rs 测试覆盖;协议文档见 docs/契约-协议与配置.md |
| @@ 装饰器解析(parse_decorators/EntryDecorators) | L1 | 已入 world_book.rs 兼容解析 |
| EJS 读取 API(getwi/getchar/injectPrompt 等 15 个) | L3 | ejs 层 10 个测试;injectPrompt 为兼容占位(engine 注入清单未接入) |
| **EJS 自研解释器本体** | **L3·冻结** | **仅接受安全修复,新能力一律走 rquickjs 沙箱**;已加固循环步数/墙钟预算 + 解析深度守卫(5 个新测试) |
| LAST_SEND/LAST_RECEIVE 统计变量 | L2 | engine 收尾写入会话宏,测试覆盖 |
| <#escape-ejs> 作用域转义 | L1 | ejs/mod.rs 占位符方案 + 测试 |
| PatchOp.reason / 转义还原 / delta 无效跳过 / Insert 并入 Replace | L1 | mvu 协议对齐,见 docs/契约-协议与配置.md |

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
└── docs/                       # 技术文档(活文档 + archive/ 历史归档;索引见 docs/README.md)
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
| 后端测试 | `cd server-rs && cargo test` | **1169 个测试(941 单测 + 228 集成,23 个集成文件)**,**需在 vcvars64 环境**;前端 `npm test -w web` **858 个(89 文件;静态计数;vitest 运行时为 876,差值 18 来自 `parser.contract.test.ts` 循环生成的 fixture 用例)**。数字由 `node tools/count-tests.mjs` 自动统计,勿手抄——`npm run count:tests` 查看当前值,`npm run check:tests` 校验文档是否漂移 |
| 全量检查(本地 CI) | `npm run check` | `tools/check-all.ps1`:fmt → clippy → cargo test → cargo audit(**硬门禁**)→ lock-sync(双锁漂移)→ contract → arch(C/D/E 分层)→ 类型 ratchet → npm audit(警告)→ vue-tsc(**硬门禁**)→ vitest → vite build。**已接入 build.ps1 与 pre-push hook**(CI 工作流为纸面、未运行,见 §0 门禁纪律)。
`check-arch` 规则自 2026-09-14 起含 C/D/E/G/H/I/J(代际归属与跨代方向以 `tools/arch-layers.json` 为 SSOT) |
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
| cargo-audit | 0.22.2 | `cargo install cargo-audit --locked` | `check-all` 的依赖审计阶段(**硬门禁**;未安装时该阶段提示跳过)。advisory DB 当前经 Gitee 镜像拉取 |

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
   `GET /api/health` 返回 `{ok, ts, version, build_id, build_time, data_dir, dependencies:{db}}`;设置中心底部常驻显示
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
> 这是本节唯一保留在此的红线说明——**其余正文已迁出**。

**正文位置(2026-09-16 迁出)**:

| 原小节 | 现位置 |
|---|---|
| 路由总表(全部端点请求/响应字段) | `docs/契约.md` §HTTP API |
| SSE 事件格式(chat/send 与任务事件) | `docs/契约.md` §SSE 线格式 |
| 约定(错误体 `{error:{code,...}}` 七值、状态码、ISO 8601) | `docs/契约.md` §错误体与状态码契约 |
| 线格式冻结清单与机检状态 | `docs/契约.md` §线格式冻结清单 / §机检状态总表 |

修改任一端点前**先读 `docs/契约.md`**,改完跑 `node tools/check-contract.mjs`(10 组 MAPPINGS;未登记的类型不校验)。

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

> **正文已迁出**(2026-09-16 文档整合):条目 1~32 的完整「现象 / 根因 / 做法」见
> `docs/经验.md`(分布在其 §一 环境与工具链、§二 构建与发布、§三 测量与实验、
> §五 并发与资源、§六 测试与门禁各组)。本节原位保留**编号与标题索引**。
>
> **编号纪律**:条目编号是全仓引用锚点——源码注释与文档按「`MAINTENANCE.md` §10 条目 N」
> 引用(现存量引用如条目 22、条目 29)。**编号不得重编、不得复用**;新增踩坑请追加到
> `docs/经验.md`,并回到本表追加一行索引(编号继续递增)。
>
> 注:源文档中**编号 11 出现两次**(一个关于便携版 `README.txt` 的 BOM,一个关于 mvu
> 来源澄清),系历史编号重复,照录保留,勿当笔误「修正」。

| 编号 | 标题 | 现位置 |
|---|---|---|
| 1 | cargo 必须 vcvars64 环境 | `docs/经验.md` |
| 2 | std::sync::Mutex 不可重入 | `docs/经验.md` |
| 3 | PowerShell 5.1 中文乱码 | `docs/经验.md` |
| 4 | PowerShell 里 `curl` 是别名 | `docs/经验.md` |
| 5 | axum 0.8 路由语法 | `docs/经验.md` |
| 6 | 端口残留 | `docs/经验.md` |
| 7 | tiktoken-rs 首次运行需联网下载 BPE 词表 | `docs/经验.md` |
| 8 | history 注入 | `docs/经验.md` |
| 9 | `.env` 修改即时性 | `docs/经验.md` |
| 10 | 日志编码 | `docs/经验.md` |
| 11 | 便携版 README.txt 保留 UTF-8 BOM(有意) | `docs/经验.md` |
| 11 | mvu 系统来源澄清(重要) | `docs/经验.md` |
| 12 | 导入 ST 预设的空楼层过滤 | `docs/经验.md` |
| 13 | 变量协议示例不要写进恒存在的 system 提示词 | `docs/经验.md` |
| 14 | 沙箱 realm 与酒馆不同:同卡脚本共享 window 需显式机制 | `docs/经验.md` |
| 15 | 悬浮窗脚本依赖的 jQuery 面比想象宽 | `docs/经验.md` |
| 16 | 改完必须重跑 `build.ps1` 才进 exe | `docs/经验.md` |
| 17 | 16GB 内存 + 11GB 页面文件下禁止全并行 debug 链接 | `docs/经验.md` |
| 18 | 世界书条目的 depth/role 在角色卡里常放在 `extensions` | `docs/经验.md` |
| 19 | 卡内 CSS 注释会吞掉紧随的声明 | `docs/经验.md` |
| 20 | 压缩 / 备份仓库报「系统找不到指定的路径」= jniLibs 里的 .so 符号链接悬空 | `docs/经验.md` |
| 21 | 压缩工具会「跟随」junction,把外置目录的体积一起装进压缩包 | `docs/经验.md` |
| 22 | `server-rs\target` 下新建的 exe 可能被安全软件拦截执行 | `docs/经验.md` |
| 23 | src-tauri 内嵌 server-rs 时,两份 Cargo.lock 会静默漂移 | `docs/经验.md` |
| 24 | API Key 解密失败曾在一次保存后被静默清空 | `docs/经验.md` |
| 25 | schema.rs 加列漏写 `ensure_*` 迁移只在老用户机器上炸 | `docs/经验.md` |
| 26 | 外置产物目录不能整删:会连带删掉同目录下的 Android 构建目录 | `docs/经验.md` |
| 27 | 构建/门禁报 `failed to remove file ... os error 5`,先查是不是 exe 还在跑 | `docs/经验.md` |
| 28 | 世界书 `depth` 是「插入深度」不是「关键词扫描窗口」,两者混用会让条目永不触发 | `docs/经验.md` |
| 29 | 见到「依赖 rlib 缺失」先降并发,不要改依赖 | `docs/经验.md` |
| 30 | `target\debug\deps\` 里只有 `.rmeta` 没有同名 `.rlib` 的孤立产物 | `docs/经验.md` |
| 31 | 360 安全卫士拦截 cargo 新生成的 build script 可执行文件 | `docs/经验.md` |
| 32 | 测试临时数据目录从不清理,会把 %TEMP% 撑爆 | `docs/经验.md` |

条目 32 末尾的「本批次后的实测残留」(约 30 个测试临时目录残留、`kedai-tool-test-*` 3 个为
已知有界残留)属**遗留登记**,见 `docs/遗留.md`。

---

## 11. 测试

```bash
cd server-rs && cargo test
```

- 单元测试(源文件内 `#[test]`/`#[tokio::test]`,**941 个**,以 `tools/count-tests.mjs` 为准):状态机迁移、planner(fast/deep/算式识别)、reflector(3 规则)、calculator(白名单解析)、censor(禁词同义替换)、token 编码映射与估算、工具注册表、世界书转换、世界书注入(`scan_depth` 扫描窗口:`scan_window_len` 优先/兜底/0/封顶、`scan_depth` 三来源解析、显式 scanDepth 覆盖 depth 兜底)、提示词注入(含禁词库)、mvu 变量系统(含 JSONPatch 转义/reason/delta 容错)、EJS 渲染器(含读取 API、escape-ejs 与**循环预算/解析深度守卫 5 例**)、角色卡解析、正则脚本、@INJECT 解析/应用、GENERATE 注入、运行时提示词内置默认回退、角色扮演默认提示词内置(from_config + load 空值回填)、结构化错误码(api/errors.rs)、任务编排工具契约(agentgo 逐项校验/子任务截断判失败/read subtask 多键命中/todo 跨 agent 可见/agentend interrupted)、任务工具策略(deny_dangerous 的 bash 例外不外溢:含 `bash2`/`mcp_x_bash` 精确匹配护栏)、任务白名单不豁免命令级高危硬门(custom_authorized + rm -rf/sudo 必须拒绝)、generate-raw 结构化预算下限与截断自愈(含显式值钳制)、**generate-raw 世界书窗口不认条目 depth(回归护栏)+ 吸血鬼卡真实形状格式条目必注入**、**密钥保留策略(批次 2:解密失败不覆盖磁盘密文 3 例)**、**pending_runs RAII 守卫(drop 与 panic 路径)**、**db writer 锁中毒回滚(未完成事务 ROLLBACK)**、**世界书概率门控不变式(常驻条目不参与概率 / 触发条目参与,2 例)**、**system 前缀不被概率扰动(构建侧锁定)**、**截断自愈预算四路合一(utils::retry 纯函数 8 例:翻倍/精确命中封顶/已封顶返回 None/饱和不溢出/零边界/带下限抬升小预算/仅 length 且轮次内触发/单轮限制;task_service 封顶回退 1 例见 [抽象收敛与残余修复-变更说明.md](docs/功能-变更史.md))**、**用量落库事务化(global_usage 写入失败回滚 1 例、会话与全局同进同退 1 例)**、**消息删除的 message 作用域变量清理(单条/截断/清空 3 例)**、mvu 变量系统(含 JSONPatch 转义/reason/delta 容错)、EJS 渲染器(含读取 API、escape-ejs 与**循环预算/解析深度守卫 5 例**)、角色卡解析、正则脚本、@INJECT 解析/应用、GENERATE 注入、运行时提示词内置默认回退、角色扮演默认提示词内置(from_config + load 空值回填)、结构化错误码(api/errors.rs)、任务编排工具契约(agentgo 逐项校验/子任务截断判失败/read subtask 多键命中/todo 跨 agent 可见/agentend interrupted)、任务工具策略(deny_dangerous 的 bash 例外不外溢:含 `bash2`/`mcp_x_bash` 精确匹配护栏)、任务白名单不豁免命令级高危硬门(custom_authorized + rm -rf/sudo 必须拒绝)、generate-raw 结构化预算下限与截断自愈(含显式值钳制)、**密钥保留策略(批次 2:解密失败不覆盖磁盘密文 3 例)**、**pending_runs RAII 守卫(drop 与 panic 路径)**、**db writer 锁中毒回滚(未完成事务 ROLLBACK)**、**世界书概率门控不变式(常驻条目不参与概率 / 触发条目参与,2 例)**、**system 前缀不被概率扰动(构建侧锁定)**、**截断自愈预算四路合一(utils::retry 纯函数 8 例:翻倍/精确命中封顶/已封顶返回 None/饱和不溢出/零边界/带下限抬升小预算/仅 length 且轮次内触发/单轮限制;task_service 封顶回退 1 例见 [抽象收敛与残余修复-变更说明.md](docs/功能-变更史.md))**、**用量落库事务化(global_usage 写入失败回滚 1 例、会话与全局同进同退 1 例)**、**消息删除的 message 作用域变量清理(单条/截断/清空 3 例)**
- API 集成测试(`tests/` 23 个文件,**228 个**,以 `tools/count-tests.mjs` 为准,mock 连接器 + 临时数据目录):api_integration、assistant、agent_flows、tasks、task_events(含 `task_solo_offers_bash_tool`:任务模式确实下发 bash)、generate_raw(自愈成功且 injected 保留 / 重发失败回退半截 / 自愈用尽返回末次文本 / max_tokens=0 400)、**schema_migration_meta(老库升级结构一致性,冻结基线见 `tests/fixtures/schema_baseline_v0_3_0_beta.sql`)**、prompt_inject、world_books、settings_connector、security、contracts_e2e、scripts_e2e、scripts_import、swipe_regenerate、undo、user_scripts、variables_scopes、db_concurrency、macros、repo_index、memory_embedding(P-5 记忆向量批量语义)——health、角色 CRUD(multipart 上传)、会话/消息/导入导出、设置与 token、agent plan、SSE 聊天流、任务引擎六模式、计算器工具 SSE、世界书/角色卡、提示词注入与酒馆预设导入、鉴权
  - **已知 flaky**:`settings_connector::mock_auto_switches_to_openai_on_save` 偶发因 Windows 文件占用失败
    (`settings.json 应已持久化: Os { code: 32 }`;另实测全量负载下的 `Os { code: 2 } NotFound` 变体),
    隔离重跑即通过——非代码缺陷:PUT 保存是 `db_call` 同步 await(`api/settings.rs:657-662`),返回 200 时
    文件必已落盘,失败属环境文件锁/时序竞争。CI 接入时给该断言加重试或改经 API 校验。
- 前端 `npm test -w web`(Vitest,**858 个 / 89 文件**,数字以 `tools/count-tests.mjs` 为准;vitest 实际输出为 **876**——差值 18 来自 `parser.contract.test.ts:68` 对 `mvu_patch_cases.json` 的 19 个 fixture 用例循环生成,静态计数把该 `it(` 计为 1):stores(**storeBridge 注册/降级/owner 诊断 14 例**)、**弹窗注册表单点派生三方一致(flag 无重复/label/组件已定义/MODAL_FLAGS 对应;registry ↔ uiPrefs 双向;App.vue v-for 派生且无硬编码残留,共 9 例)**、api client(含 ApiError 错误码分类)、**api stream(SSE 读循环跨 chunk 帧重组/CRLF/残留帧、非 JSON 错误体、上传成功失败与 401 重试,12 例)**、组件与 composables、CSS 清洗(含注释处理)与沙箱回归;类型门禁 `npm run typecheck -w web`(vue-tsc,**硬门禁**,存量 168 已于 2026-09-08 清偿归零,清偿记录见 docs/功能.md 附录 D);类型逃逸 ratchet `node tools/check-frontend-lint.mjs`(as never / as unknown as / 非空断言 / any,**只降不升**,基线见脚本内 BASELINE;2026-09-13 批次 5.2 后 as unknown as 71→70)
- 新增接口建议同步补集成测试;测试环境变量 `CONNECTOR=mock` 强制隔离

---

## 12. Roadmap(未实现)

> **正文已迁出**(2026-09-16 文档整合):未实现的方向条目见 `docs/展望.md` §三「平台与连接器」——
> RD-1 Oobabooga / KoboldAI 连接器适配、RD-2 知识库向量检索工具、RD-3 工具执行沙箱隔离。
> 已有明确改动方案(有验收断言)的未实现项见 `docs/计划.md`;已实测证伪、不要再提的方向见
> `docs/展望.md` §六。

原条目照录(便于旧引用可寻):

- Oobabooga / KoboldAI 连接器适配(世界书按 key 注入、Tauri 桌面化已完成)→ 展望 RD-1
- 工具执行沙箱隔离(角色卡脚本已有 iframe 沙箱,工具侧未做)→ 展望 RD-3
- 知识库向量检索工具 → 展望 RD-2
- (2026-08 已完成项移出:智能上下文压缩、自定义工具注册、缓存感知压缩管线、跨会话记忆蒸馏、
  技能渐进披露与子代理调度守卫——现见 `docs/功能.md`)
- (2026-09 已完成项移出:LLM 原生 function calling 全链路——下发 `tools`/`tool_choice` 并按
  index 聚合 `delta.tool_calls`,见 `connectors/openai_compatible/mod.rs`)

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

## 14. 新模块速览

> **正文已迁出**(2026-09-16 文档整合):各模块的维护入口、职责与设计要点见 `docs/功能.md`。
> 本节原位保留**模块清单索引**,便于旧引用可寻。

| 模块 | 代码入口 | 现位置 |
|---|---|---|
| 万花筒契约 DSL(contracts) | `server-rs/src/contracts/` + `services/kaleido_state_service.rs` | `docs/功能.md` §变量与契约引擎 |
| 任务工作台(task) | `services/task_service/` + `services/task_engine/` | `docs/功能.md` §六模式任务引擎 |
| 上下文压缩(compaction) | `agents/engine/compaction.rs` | `docs/功能.md` §上下文工程与压缩 |
| 缓存感知压缩管线 | `services/cache_diagnostics.rs` + `api/diagnostics.rs` | `docs/功能.md` §上下文工程与压缩 |
| 跨会话记忆蒸馏 | `services/memory_service.rs` + `api/memory.rs` | `docs/功能.md` §记忆 |
| 技能渐进披露与子代理守卫 | `services/skill_service.rs` + `tools/agent_tools_agent.rs` | `docs/功能.md` §技能与 MCP |
| 可观测性与错误面收口 | `api/request_id.rs` + `utils/logging.rs` + `models/llm_error.rs` | `docs/功能.md` §可观测性与错误面 |

契约类细节(线格式、双模式设置字段、DB schema 版本纪律)见 `docs/契约.md` 与
`docs/契约-架构与数据.md`。
