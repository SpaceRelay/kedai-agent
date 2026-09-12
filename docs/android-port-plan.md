# Kedai Android 移植方案(可行性评估 + 实施计划)

> 状态:**已批准,待执行**(2026-09-11 起草)。
> 本文是 Android 移植的活文档:架构决策、cfg 门控清单、阶段进度在此维护;改代码时同步更新。
> 适用读者:开发 agent 与维护者。禁止注入聊天模型。

## 一、可行性结论

**可行,属「中等规模平台化重构」,不是打包换壳。** 后端与前端逻辑层耦合低、可大量复用;平台 API 层与 UI 层耦合高、必须重做。

### 有利条件

1. 后端已是 `bin + lib` 结构,且能以库形式同进程内嵌(`src-tauri/src/lib.rs:177` 用 `tauri::async_runtime::spawn` 托管 `kedai_server::run_server`),Android 可照搬;无 sidecar / `externalBin`(仓库本来就没有)。
2. 后端支持 `DATA_DIR` / `LOG_DIR` 环境变量覆盖(`server-rs/src/config.rs:114-123`),壳层注入即可让数据落到 app 私有目录。
3. 前端 API 全部走同源相对路径 `BASE = '/api'`(`web/src/api/client.ts:2`);流式用 `fetch + ReadableStream` 手工解析(`web/src/api/tasks.ts:104-147`),**非** `EventSource`/WebSocket,Android Chrome WebView 原生支持,契约定全不动。
4. 前端依赖极简(8 个运行依赖),无 CodeMirror/Monaco/图表/Worker/SharedArrayBuffer/Service Worker;`dist` 仅 1.2MB。
5. **工具授权框架已存在**,命令执行能力可直接挂接,无需另造审批体系:
   - `server-rs/src/tools/permissions.rs:1-60` — `AuthorizationMode`(Strict/Loose/Bypass)、`ToolRisk`;
   - `server-rs/src/tools/action_class.rs:13-68` — `ToolOp` / `PathZone` / `ToolAction` 分类;
   - 现有 oneshot 人工确认通道。

### 四个必须解决的硬点

| # | 问题 | 证据 | 性质 |
|---|---|---|---|
| 1 | `reqwest` 用 `native-tls`,Android 上会拉 `openssl-sys`,无系统 OpenSSL | `server-rs/Cargo.toml:22`、`src-tauri/Cargo.toml:30`;`Cargo.lock` 含 `openssl-sys 0.9.117` | **硬阻塞,改 `rustls-tls`** |
| 2 | API Key 存储是 Windows DPAPI,非 Windows **默认拒绝持久化** | `server-rs/src/services/secret_store.rs:23-37` | **硬阻塞,否则存不了密钥** |
| 3 | 前端 `min-width: 1100px` 固定三栏,零响应式/零触摸适配 | `web/src/style.css:277`;全仓无布局断点、无 touch 事件、无 `env(safe-area-inset)` | **成本大头(43 组件 / 3686 行 CSS)** |
| 4 | 桌面逻辑侵入启动主路径:`netstat`/`taskkill` 单实例、`%APPDATA%`、`CloseRequested` 退出确认 | `src-tauri/src/lib.rs:22-50, 58-60, 95-125` | 中等,cfg 拆分 |

### 仓库现状

全仓**零 Android 痕迹**:无 `src-tauri/gen/android/`、无 mobile schema、构建链为 PowerShell + NSIS + `.exe`。属绿地移植。

## 二、架构决策

### 决策 1:交付形态 = 全内嵌单机版

Rust 后端交叉编译进 APK,与 Tauri Android app 同进程;前端资源继续由内嵌 axum 在 `http://127.0.0.1:<port>/` 同源托管。手机可离线独立使用。代价:APK 约 25-40MB;后台长任务需前台服务保活。

### 决策 2:资源加载 = 保持 loopback HTTP 同源模型(方案 A)

**理由(为什么不做 assets 协议改造):** 后端只放行 loopback 来源——
- `server-rs/src/api/security.rs:141` 的 `valid_origin` 要求 scheme ∈ {http, https, tauri} 且 host 为 loopback 名;
- `server-rs/src/api/mod.rs:108-121` 的 CORS predicate 只允许 `127.0.0.1` / `localhost`。

走 loopback 则 `BASE='/api'`、同源 bootstrap、`credentials: 'same-origin'` 全部原样可用,**前后端零改动**。
备选方案 B(WebView 加载 `tauri.localhost` assets + 前端跨域访问 127.0.0.1)需同时改前端 BASE、后端 Origin 白名单与 CORS,且仍需明文放行,收益为负,**不采纳**。

代价:Android 侧需要 `INTERNET` 权限 + `network_security_config.xml` 放行 127.0.0.1 明文。

### 决策 3:UI 形态 = 响应式双形态

| 断点 | 形态 |
|---|---|
| `< 768px`(手机竖屏) | 移动信息架构:底部 Tab(对话/任务/角色库/设置)+ 侧栏抽屉 + 全屏 Sheet 弹窗 |
| `768–1100px`(平板/横屏) | 折叠式窄侧栏 + 双栏,保留桌面信息密度,按可用宽度弹性分配 |
| `≥ 1100px` | 沿用现有三栏布局,**桌面端观感零变化** |

### 决策 4:命令执行 = Operit 式分层执行器 + root 通道

按 `ROOT → Shizuku(DEBUGGER) → 内置 busybox(应用自身 UID) → 禁用` 逐级探测;默认只开到沙箱档,ROOT/Shizuku 由用户显式开启。

**参考实现:** `AAswordman/Operit`(LGPL-3.0)→ `core/tools/system/shell/ShellExecutorFactory` 分层思路、`libsu` + Shizuku + `lib*.so` 打包可执行文件的 trick。**只借鉴设计,不复制代码**(避免 LGPL 传染;如确需引用代码须单独评估许可)。

关键事实(源码实证,供实施参考):
- Operit 执行器优先级:`ROOT → ADMIN → DEBUGGER(Shizuku) → ACCESSIBILITY → STANDARD`;
- root 用 `com.github.topjohnwu.libsu:core:6.0.0`,并有 `su -c` exec 模式兼容 KernelSU;
- Shizuku 用 `dev.rikka.shizuku:api:13.1.5`,Manifest 声明 `moe.shizuku.manager.permission.API_V23`;
- 无 root 保底是应用自身 UID 的 `sh -c`;
- 自带 busybox/bash/proot 以 `lib*.so` 命名放入 `jniLibs/arm64-v8a/`,配 `packaging { jniLibs { useLegacyPackaging = true } }`,运行时从 `applicationInfo.nativeLibraryDir` 取可执行权限;
- 审批模型:工具级 `ALLOW/ASK/FORBID` 三态,默认 `ASK`,确认走悬浮窗。

**Kedai 的差异化:执行动作一律接入 Kedai 现有 `AuthorizationMode` 矩阵**,不引入第二套审批语义。

## 三、分阶段实施计划

### 阶段 0:工具链 + 可行性 spike(3–5 天)

**这是决定后续全部工作是否成立的门槛。** 交付物:真机/模拟器上跑通的 Tauri Android 壳 + 内嵌后端交叉编译通过 + loopback 页面成功加载。

1. 安装工具链(本机安装位置见 §五):
   - JDK 17(Temurin),`Android cmdline-tools` + `platform-tools` + `platforms;android-34` + `build-tools;34.0.0` + `NDK r27`;
   - 环境变量 `JAVA_HOME` / `ANDROID_HOME` / `ANDROID_NDK_HOME`;
   - `rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android`。
2. `npx tauri android init` 生成 `src-tauri/gen/android/`,**提交入库**(为 native 定制留位)。
3. **Spike 1(最大不确定项)**:把 `kedai-server` 作为 path 依赖编到 android target,逐项验证 C 依赖交叉编译:`rusqlite bundled`、`sqlite-vec`、`rquickjs-sys`、`tiktoken-rs`。**此步不过则架构要改。**
4. **Spike 2**:确认 Tauri Android WebView 能 `navigate` 到 `http://127.0.0.1:<port>/` 并渲染 `web/dist`,且 capability 的 `remote.urls`(`capabilities/default.json:6-8`)在此环境生效。
5. **Spike 3**:验证 Android Keystore 经 JNI 的读写最小闭环(为阶段 1 第 2 项探路)。
6. 产出 spike 结论报告,确认或调整阶段 1–2 方案。

### 阶段 1:后端平台解耦(1–2 周)

1. **TLS 切换(必修)**:`server-rs/Cargo.toml:22` 与 `src-tauri/Cargo.toml:30` 的 reqwest 改 `rustls-tls`,移除 `native-tls`;确认 `Cargo.lock` 不再含 `openssl-sys` / `openssl-probe`。注意 Tauri 自身可能传递 native-tls,冲突时按平台 cfg 指定 feature。
2. **密钥存储(必修)**:`secret_store.rs` 引入 `SecretStore` trait;`DpapiStore`(`cfg(windows)`,即现状)与 `AndroidKeystoreStore`(`cfg(target_os="android")`)双实现。Android 侧经 JNI 调 Kotlin 的 Android Keystore AES-GCM(密钥不可导出,密文落 app 私有文件),密文沿用现有 `v1:` 前缀以兼容读取。**收紧降级**:`KEDAI_ALLOW_INSECURE_PLAINTEXT_SECRETS` 仅 debug 构建生效,release 拒绝明文落盘。
3. **数据目录**:`config.rs:73-81` 的 `canonical_user_data_dir` 加 Android 分支(不依赖 `APPDATA`);主路径仍由壳注入 `DATA_DIR`/`LOG_DIR`(照 `src-tauri/src/lib.rs:71-72`),缺省时给 app 私有目录默认值而非回退 `current_dir`。`config.rs:47-67` 的 `project_root()`(靠 `current_exe` 找 `web`/`server-rs`)降级为开发态逻辑。
4. **Windows 专属代码 cfg 复核**:`utils/fs_atomic.rs:32-40`(已有非 Windows rename 回退,确认即可)、`services/runtime_prompt_service.rs:173-199`(`ReplaceFileW`)、`mcp/process.rs:122-141`(Windows 测试)、`api/repo_index.rs:28-57`(开发态 current_dir 索引)→ 加 `cfg(windows)` / `cfg(not(target_os="android"))` 门控,Android 走 no-op 或明确报错。
5. **MCP 与子进程**:`mcp/process.rs` 的 `tokio::process::Command` 在 Android 门控;Android 侧改由阶段 3 执行器承接(第一版可先返回「未支持」)。
6. **tiktoken 词表**:`services/token_service.rs:60-70` 依赖运行期联网下载,移动端不可靠;改为 vendored 词表随包,或明确降级为估算计数并在设置页标注。
7. **日志**:`LOG_DIR` 由壳注入;补 Android logcat 双写便于真机排查。
8. **回归保障**:所有改动以 cfg 门控,**Windows 行为逐字节不变**;跑 `cargo test` + `cargo clippy`;新增 android target 的 `cargo check`。

### 阶段 2:Tauri 壳 Android 化 + 原生配置(1 周)

1. **入口点**:`lib.rs` 加 `#[tauri::mobile_entry_point] fn main`;`main.rs` / `portable_main.rs`(带 `windows_subsystem`)cfg 门控为 desktop-only。
2. **`setup` 平台拆分**(`lib.rs:57-88`):Android 用 `app.path().app_data_dir()`(映射 filesDir),删 `APPDATA` 回退;`find_project_data_dir` / `migrate_data_if_needed`(便携版迁移)门控 desktop;`terminate_other_port_owners`(`netstat`/`taskkill`)与 `start_and_wait_ready` 的端口接管(`:152-173`)门控 desktop,Android 直接 bind、失败即明确报错。
3. **退出流程**:`CloseRequested` 确认框(`:95-125`)在 Android 无对应事件;改处理返回键(首次返回弹应用内确认,再次退出)。
4. **资源加载**:`show_main_window`(`:224-236`)在 Android 同样 navigate 到 `http://127.0.0.1:<port>/`;保留 `SplashScreen.vue` 遮首帧白屏。
5. **原生配置(`gen/android` 内)**:`AndroidManifest.xml` 加 `INTERNET`;新增 `network_security_config.xml` 放行 `127.0.0.1`/`localhost` 明文;`configChanges` 避免旋转重启 Activity;`jniLibs { useLegacyPackaging = true }`(阶段 3 需执行 native 可执行文件);`abiFilters` 至少 `arm64-v8a`,开发期加 `x86_64`。
6. **图标**:`npx tauri icon src-tauri/icons/icon.png` 生成 mipmap 全套(现 512×512 源足够);`tauri.conf.json` 的 `nsis` / `bundle.windows` 段保留(按平台生效)。
7. **capability**:补 mobile capability,保留 `core:default` + `dialog:allow-save` + `fs:allow-write-text-file`;`remote.urls` 保持 `127.0.0.1`(loopback IPC 必需)。
8. **壳层日志**:Android 无 `eprintln` 查看渠道,启动错误改走 logcat + 应用内错误页。

### 阶段 3:Android 命令执行层(2–3 周)

1. **抽象与分层**:server-rs 新增 `exec` 模块,`ShellExecutor` trait + `ShellExecutorFactory`,按 `ROOT → Shizuku(DEBUGGER) → STANDARD` 探测;`STANDARD` 保底用内置静态 `busybox`(`libbusybox.so` 放入 `jniLibs/arm64-v8a/`)。
2. **桥接**:进程派生、`su` 弹窗、Shizuku binder 均为 Android API,执行须在 Android 侧完成。新增最小 Kotlin 模块,经 Tauri 命令/JNI 把 `exec(command, cwd, env) -> {stdout, stderr, exitCode}` 暴露给 Rust;输出截断。
3. **接入现有授权框架(关键)**:
   - `tools/action_class.rs` 的 `ToolOp` 增加 `Exec` 变体(危险度最高),`classify` 增加 `exec` / `terminal` 分支;
   - `tools/permissions.rs` 三档矩阵纳入 `Exec`:Strict/Loose 一律需授权;Bypass 下「root 提权 / 系统路径」仍需授权(与现「系统路径写删始终需授权」硬底线一致);
   - 复用现有 oneshot 确认通道实现 ASK;前端确认卡展示**完整命令原文 + 执行器等级 + 风险标签**;
   - 危险命令静态拦截(`rm -rf /`、`mkfs`、`dd of=/dev/block`、`format` 等)**只作兜底告警,不作为安全边界**。
4. **等级可见与可关闭**:UI 显示当前执行器等级(ROOT / Shizuku / 沙箱);root 命令逐条确认、通道可一键关闭;设置页保留审计日志。
5. **合规**:root 能力不适合 Google Play;依赖 Shizuku 的应用审核严格,通常侧载。默认只开沙箱档,ROOT/Shizuku 由用户显式开启,产品说明写清。
6. **与 MCP 的关系**:Android 上 MCP 也走同一执行器;`npx`/`uvx` 类服务器需额外打包 Node 运行时,第一版不做。`exec` 工具本身已覆盖「跑命令」需求。

### 阶段 4:前端移动适配(2–3 周,成本最大)

**A. 移动信息架构(`<768px`)**:底部 Tab(对话/任务/角色库/设置);`Sidebar`(260/320px,`style.css:277,283,300`)改抽屉;`AgentPanel`(340px 抽屉,`:309-320`)改全屏 Sheet;`.sv-modal-mask`(居中 fixed,`:1085,1874`)改全屏 sheet + 安全区内边距;音频浮窗(`:2503,2533`)改底部播放条避开手势条;右缘 AGENT 竖条(`:325`)移除并入 Tab。

**B. 触摸交互替换**:消息操作 hover 显示(`style.css:639`)改常显 + 长按菜单;角色卡右键菜单(`Sidebar.vue:310,409-419`)改长按;HTML5 拖放排序(`PromptManager.vue:438`、`AgentFlowSection.vue:99`、`PromptInjectSection.vue:151`)改触摸拖拽或强化已有 ↑↓ 兜底;`target=_blank`(`render.ts:608`)改系统浏览器/应用内;12+ 处 `alert/confirm` 统一为应用内弹窗;slash 联想键盘导航(`ChatInput.vue:117-144`)改触摸点选。

**C. 视口与软键盘**:`index.html` viewport 加 `viewport-fit=cover`;补 `env(safe-area-inset-*)`;`height:100%`(`style.css:158,276`)改 `100dvh`;用 `visualViewport` 保证软键盘不遮挡输入框;`touch-action` 抑制双击缩放;触摸目标 ≥ 44px;range 滑块(`:1358`)加大命中区。

**D. 文件导出**:`web/src/exportFile.ts:39-49` 的 `plugin-dialog.save` + `plugin-fs.writeTextFile(path)` 在 Android 返回 SAF `content://` URI,路径语义不同 → 改为写入 app 数据目录后经分享 Intent 导出;移除 Android 上不可用的 File System Access / `a.download` 分支(已有 `isTauri` 结构可挂)。

**E. 平板/横屏(`768–1100px`)**:三栏降 `min-width` 并按宽度弹性分配,侧栏可折叠。

**F. 明确复用边界**:全部 `api/*`、stores、SSE 解析、markdown-it/sanitize-html、Pinia、CSS 设计令牌、本地字体**均不改**;前端工作量集中在布局与交互层。

### 阶段 5:验证、性能与发布(1–2 周)

- **设备矩阵**:minSdk 26(Android 8.0)起;arm64 真机 + x86_64 模拟器;覆盖 Android 10/12/14。
- **功能回归**:流式聊天(SSE 长连接)、任务引擎、角色卡脚本(rquickjs 沙箱)、世界书、记忆/向量检索(sqlite-vec)、角色卡导入(35MB 上限)、导出、音频播放、设置持久化(Keystore)、后台/锁屏行为。
- **移动专项**:进程被回收后重启恢复;长任务前台服务保活;低内存 profiling;电量与发热。
- **构建与签名**:`tauri android build` 出 APK/AAB + release keystore;把 `build.ps1` / `bump-version.ps1` 中 Windows 专属部分抽成跨平台 node 脚本,**桌面打包流程保持不变**。
- **分发**:含 root/Shizuku 能力建议自签 APK 侧载;上架需另做「无 root 能力」Play 变体。

## 四、风险与缓解

| 风险 | 等级 | 缓解 |
|---|---|---|
| `rusqlite`/`sqlite-vec`/`rquickjs` 交叉编译不通 | **高** | 阶段 0 Spike 1 最先验证;NDK r27 + `cc`/`cargo-ndk` 显式配置;rquickjs 不通则评估替换 JS 引擎 |
| UI 重做超期(最大不确定项) | **高** | 先跑通主路径移动壳(聊天/角色/设置),次要弹窗逐个迁移;沿用现有 pinia 布尔开关切页,不引入 router |
| root/Shizuku 的合规与安全 | **高** | 默认关闭、逐条确认、审计日志、不进 Play |
| loopback navigate 被拒或明文被拦 | 中 | `network_security_config` + capability remote;不通则退方案 B |
| Android Keystore JNI 复杂度 | 中 | 阶段 0 Spike 3 先做最小闭环;保留 debug 明文开关 |
| 后台被杀导致长任务中断 | 中 | 前台服务 + 任务状态落库可续跑 |
| TLS 切换引发证书行为差异 | 中 | rustls + webpki-roots,实测各 LLM 供应商 |
| 桌面端回归 | 中 | 全部改动 cfg 门控 + 现有测试 + 双目标构建校验 |

## 五、本机环境与阶段 0 执行结果(2026-09-11)

### 阶段 0 执行结果(完成)

**已落地的基础设施:**

| 项 | 位置 / 结果 |
|---|---|
| Git 分支 | `kedaiAndroid`(工作区 `D:\kedai` 已切至该分支) |
| Android 工作区 | `D:\kedai-android\`(sdk / jdk / gradle-home / libclang / avd / keystores / artifacts / bin) |
| JDK 17 | Temurin 17.0.20.1 → `D:\kedai-android\jdk\temurin-17`(**不可用系统 JDK 26**) |
| Android SDK | `D:\kedai-android\sdk`:`platforms;android-34`+`android-36`、`build-tools;34.0.0`+`36.0.0`、`platform-tools`、`emulator`、`system-images;android-34;google_apis;x86_64` |
| NDK | r27 `27.0.12077973` → `D:\kedai-android\sdk\ndk\27.0.12077973` |
| libclang | 18.1.1(PyPI wheel 解压)→ `D:\kedai-android\libclang\libclang.dll`(**NDK 不含 libclang.dll,bindgen 必需**) |
| AVD | `kedai_test`(Pixel 6 / Android 14 / x86_64)→ `D:\kedai-android\avd` |
| Rust targets | aarch64 / armv7 / i686 / x86_64-linux-android 四个均已安装 |
| 构建脚本 | `D:\kedai-android\bin\`:`android-env.bat`(共享环境)、`ata.bat`(tauri android)、`acargo.bat`(交叉编译)、`wcargo.bat`(Windows 校验) |
| Gradle 缓存 | `D:\kedai-android\gradle-home`(避开 C: 盘,已确认 C: 仅剩 7.4GB) |

**Spike 1(交叉编译)— 通过 ✅**

- `kedai-server` 成功产出 Android ARM64 ELF(22MB,`interpreter /system/bin/linker64`,仅依赖 libc/libm/libdl)。
- `src-tauri` 壳同样可交叉编译通过,**无 openssl-sys 泄漏**。
- Windows 侧 `cargo check` 无回归(仍走 `tokio-native-tls`/`hyper-tls`)。
- APK 全链路打通:产出 `app-universal-release-unsigned.apk`(4 ABI),以及 jniLibs 下 4 个 `libkedai_desktop_lib.so`。

**过程中踩到并已解决的四个坑(重要,后续勿重犯):**

1. **`openssl-sys` 硬阻塞(已按计划解决)**。`reqwest` 的 `native-tls` 在 Android 上要求系统 OpenSSL。已改为按目标平台分离:
   - `server-rs/Cargo.toml` 与 `src-tauri/Cargo.toml` 的 reqwest 改为 `cfg(not(target_os="android"))` → `native-tls`,`cfg(target_os="android")` → `rustls-tls`。
   - 桌面行为逐字节不变(仍走 schannel),已用 `cargo check` 验证。

2. **`rquickjs-sys` 无 Android 预生成绑定**。0.12.2 与 0.13.0 均不含 `bindings/aarch64-linux-android.rs`,必须启用 `bindgen` feature(需 libclang)。
   - 上游 build.rs 在 `target == host` 时复用内置绑定,故桌面构建不真正调用 libclang,行为不变。
   - 已改为 `cfg(target_os="android")` 才启用 `bindgen`。
   - **NDK 自带 clang 但不含 `libclang.dll`**,须单独准备并设 `LIBCLANG_PATH`。

3. **bindgen 的 clang 参数在 Windows 上被吃反斜杠**(最隐蔽)。clang 把参数里的 `\` 当转义符,`--sysroot=D:\a\b` 会变成 `D:ab`,导致 `stdbool.h`/`stdio.h` not found。
   - 解决:不用 `--sysroot`,改为**逐架构显式 `-isystem`**,且路径**全部用正斜杠**:
     `-isystem <ndk>/sysroot/usr/include/<arch> -isystem <ndk>/sysroot/usr/include -isystem <ndk>/lib/clang/18/include`
     经 `BINDGEN_EXTRA_CLANG_ARGS_<target>` 传入。

4. **Tauri 生成的 Gradle `BuildTask.kt` 有 bug**。模板把可执行文件设为 `C:\Program Files\nodejs\node` 并把 `"tauri"` 当第一个参数传给 node,node 会把它当**脚本文件**解析而失败("A problem occurred starting process 'command '...\node.bat''")。
   - 已改为:`node.exe` + `node_modules/@tauri-apps/cli/tauri.js` 绝对路径(从 `rootDirRel` 逐级向上查找,适配 workspaces 布局),参数直接跟 `android android-studio-script`。
   - 该文件位于 `src-tauri/gen/android/buildSrc/.../BuildTask.kt`,**重新 `tauri android init` 会被覆盖,需重打补丁**。

**另外两个环境坑:**

- **Gradle/Maven 直连超时**。`services.gradle.org` 与 `google()`/`mavenCentral()` 在本网络不可达。
  - gradle wrapper 已切腾讯镜像;`build.gradle.kts` 已加阿里云镜像(镜像在前、官方兜底)。
  - **注意**:`src-tauri/gen/android/` 下这些文件重新 init 会被覆盖。
- **`.bat` 脚本必须纯 ASCII + CRLF**。cmd.exe 在本机 GBK 代码页下会误解析 UTF-8 中文注释(吞首字节),导致脚本整体崩坏。

**Spike 2(上机运行 + loopback 加载)— 通过 ✅(决定性结果)**

在 Android 14 / x86_64 模拟器(`kedai_test`)上安装 debug APK 并启动,实测结果:

| 验证项 | 结果 |
|---|---|
| 应用进程存活 | ✅ `com.kedai.app` 常驻,无崩溃 |
| 内嵌后端启动 | ✅ 日志 `[信息] DATA_DIR=/data/user/0/com.kedai.app/data` |
| 服务监听 | ✅ `Kedai 已启动 → http://127.0.0.1:3001/`,4 个路由(health/静态/API)就绪 |
| 数据目录落位 | ✅ `/data/user/0/com.kedai.app/data` 下生成 `kedai.db` + `-wal` + `-shm`、`characters/`、`avatars/`、`agent_flows.json` |
| WebView 加载 loopback | ✅ 页面 URL = `http://127.0.0.1:3001/`,完整 Kedai UI 渲染成功(截图见 `D:\kedai-android\artifacts\kedai1.png`) |
| Tauri IPC 可用 | ✅ CDP 实测 `window.__TAURI_INTERNALS__` 存在,`invoke`/`postMessage` 均为 function |
| Vue 应用挂载 | ✅ `#app` 有子节点,界面数据(侧栏角色「系统助手」)来自后端 |
| **同源 API 链路**(架构决策 2 的核心假设) | ✅ CDP 实测 `GET /api/bootstrap` → **200 + token**,`GET /api/characters` → **200 且返回 1 条** |

**结论:决策 2(保持 loopback HTTP 同源模型)成立,无需改为 assets 协议。前端 `BASE='/api'`、同源 bootstrap、bearer token 链路在 Android 上原样工作。**

关于启动日志里的 `Cannot redefine property: postMessage / __TAURI_PATTERN__ / metadata` 等报错:这是 Tauri IPC 初始化脚本在 loopback 页面被**重复注入**产生的无害噪音(桌面端导航到 HTTP 页面时同样存在),不影响功能——`invoke`/`postMessage` 实测可用。

**Spike 2 暴露的新问题(Phase 4 的量化依据):**

- CDP 实测 WebView 视口 `innerWidth = 1100`,`devicePixelRatio = 2.625`,`.sv-frame-col` 的 `min-width: 1100px` 恰好等于视口宽度 → **布局没有溢出,但也没有任何移动适配空间**:页面正以 1100px 桌面布局硬塞进不存在的宽度里,字体与控件按桌面尺寸渲染。这正是计划里 UI 必须重做的实测证据(截图可见三栏被压缩、右侧输入区被裁切)。
- `tauri android build --target x86_64` 只编一个 ABI;若要限制 ABI 集合,用 CLI 的 `--target` 逐个构建(gradle.properties 里的 `abiList`/`targetList` 对 tauri CLI 构建**未生效**,实测仍产出 4 ABI 的 125MB universal APK)。Phase 5 需据此设计分包策略。
- release 构建下 `usesCleartextTraffic=false`(`app/build.gradle.kts:20`),而 loopback HTTP 需要明文放行。debug 构建默认 `true` 故本次验证通过;**release 版必须补 `network_security_config.xml` 只放行 `127.0.0.1`**,否则正式包会白屏(Phase 2 待办)。
- 桌面专属逻辑的 cfg 门控已按计划完成第一批(`mobile_entry_point`、`app_data_dir` 回退、便携版迁移、`netstat`/`taskkill` 端口清理、`CloseRequested` 退出确认均仅 desktop 编译),桌面侧待回归验证。

**阶段 0 未完成项:** Spike 3(Android Keystore JNI)尚未做。

---

## 五之二、阶段 4(前端移动适配)执行结果(2026-09-11 完成)

**问题起点:** 首次上机截图显示,1100px 桌面三栏被硬塞进 411px 视口 —— 顶栏被状态栏压住、
右侧 Agent 面板与输入区被裁切、设置弹窗内容溢出到视口外。原「布局没溢出」的判断只说明
`min-width` 恰好等于视口宽,并不代表可用。

### 已落地的改造

**1. 信息架构(三级断点)**

| 断点 | 形态 |
|---|---|
| ≥1100px | 沿用原三栏,**计算样式实测与改动前逐项一致** |
| 768–1099px | 三栏收窄(侧栏 260→220、Agent 340→300),解除 1100px 硬最小宽度,不再横向裁切 |
| <768px | 三栏 → **底部导航 5 格(角色库/模式/AGENT/音频/设置)+ 左右覆盖式抽屉 + 全屏 Sheet** |

**2. 触摸交互**

- **长按替代右键**:角色卡的「编辑提示词/删除」菜单原仅 `@contextmenu` 触发,触屏完全不可达。
  新增 500ms 长按(位移 >10px 视为滚动则取消),并吞掉长按后紧随的 click 避免误选角色。
- **Teleport 修正 fixed 定位**:移动端侧栏带 `transform` 抽屉动画,而 `transform` 祖先会成为
  `position:fixed` 后代的包含块 —— 导致菜单定位偏移、遮罩只盖住侧栏、且被 `overflow` 裁切。
  将角色菜单与提示词弹窗 `Teleport to="body"` 解决。
- hover 显示的消息操作按钮在触摸设备改为常显;触摸目标 ≥44px;滑块拇指放大到 26px。

**3. 视口与安全区**

- viewport 补 `viewport-fit=cover`;`height:100dvh`;
- 16px 输入字号(低于此值 Android WebView 聚焦时会自动放大页面);
- **移除 `MainActivity` 的 `enableEdgeToEdge()`**:该调用让内容延伸到状态栏之下,而
  `env(safe-area-inset-top)` 依赖 display cutout,无刘海设备返回 0 → 顶栏被状态栏压住。
  改回系统默认 `fitsSystemWindows` 行为后正常,刘海屏仍需 CSS inset 兜底。

**4. 系统栏配色**

模板只给了 Material 默认 purple/teal 色板,状态栏/导航栏渲染为紫色,与应用淡粉纸色体系冲突。
`colors.xml` / `themes.xml`(含 `values-night`)改为与 web 设计令牌同色,并把图标设为深色
(`windowLightStatusBar`)。**注意 Kedai 无深色主题,故夜间主题也必须显式保持浅色**,否则
系统深色模式下系统栏变黑而 Web 内容仍是浅粉,出现割裂。

**5. 模态框统一全屏化**

桌面各模态有 480/640/720/760/960/1080px 等固定宽度,窄屏必然溢出。用一条
`.sv-modal-mask .sv-modal…` 组合选择器统一拉满(不删各自定义,只在窄屏覆盖),
并用属性前缀保证特异性压过 `.sv-modal.lg`。设置面板内部 180px 导航 + 内容双栏
改为「两行导航条(功能域 / 分区)+ 全宽内容」。

### 上机验证结果

| 验证项 | 结果 |
|---|---|
| 主界面 | ✅ 全宽聊天区 + 输入栏 + 底部导航,顶栏不再被状态栏压住 |
| 侧栏抽屉 | ✅ CDP 实测 `x` 从 -320 滑到 0,遮罩出现(z-index 51),关闭键可见 |
| 设置弹窗 | ✅ 全屏 Sheet `412×842` 完全覆盖视口、`maxWidth: none`、头部 sticky |
| 底部导航 | ✅ 5 格,当前项高亮 |
| **桌面零回归** | ✅ CDP 把视口临时切到 1280/1440 后比对计算样式:侧栏回到 `static`/260px/`transform:none`、`min-width` 回到 1100px、工具栏 `flex-wrap: wrap`、底导航/遮罩/关闭键全部 `display:none` |
| 前端测试 | ✅ 65 文件 / **649 tests passed**,`vue-tsc` 无错 |
| Windows 编译 | ✅ `cargo check`(server-rs 与 src-tauri)通过 |

### 新增的调试基建

- **外置 dist 热更新(重要)**:debug 构建下,若应用私有目录存在 `kedai-dist/index.html`,
  壳经 `KEDAI_WEB_DIST` 把静态资源改为磁盘读取。改 UI 的迭代从「重编 Rust + 打 APK 约 4 分钟」
  降到 **`bash D:/kedai-android/bin/deploy-ui.sh` 约 20 秒**。release 不含此分支。
  - **路径必须与 Rust 侧推导一致**:`app_data_dir()` 在 Android 是 `Context.getDataDir()`
    = `/data/user/0/<pkg>`(**不是** `files/`),代码取 `data_dir.parent()/kedai-dist`。
    故目标为 `/data/user/0/<pkg>/kedai-dist`;早期误推到 `files/kedai-dist` 导致不生效。
  - 写入方式:`tar` 打包 → `adb push` 到 `/data/local/tmp` → `run-as <pkg> tar xzf`。
    直接 `adb push` 到私有目录会失败;`/sdcard/Android/data/<pkg>/` 则因 Android 11+
    scoped storage 拒绝应用读取(owner 为 shell,**chmod 777 也无效**)。
  - `adb` 是原生 Windows 程序,脚本中须用 `cygpath -w` 转换路径;`tar` 则相反,
    带盘符的绝对路径会被误判为「远程主机」。
- CDP 诊断脚本(`D:\kedai-android\bin\`):`cdp-eval.js`(页面状态)、`cdp-api-check.js`
  (同源 bootstrap/API)、`cdp-layout.js`、`cdp-style-diff.js`(手机 vs 桌面计算样式对比)、
  `cdp-modal-check.js`、`cdp-drawer-check.js`。

### 本阶段的额外踩坑

- **`.bat` 脚本必须纯 ASCII**:`wcargo.bat` 与 `ata.bat` 都因 UTF-8 中文注释在 GBK 代码页下
  被误解析而整体崩坏(吞首字节导致命令拼接错误)。已全部改为英文注释。
- **XML 注释不得含连续两个连字符**:`colors.xml` 里写 `--sv-paper` 这类 CSS 变量名会让
  AAPT 报「注释中不允许出现字符串 --」而构建失败。
- **`java`/`cargo` 残留进程会耗尽内存**:15.2GB 物理内存 + 16.7GB 页面文件在
  「模拟器 + gradle daemon + 多路 rustc」并发下被打满,导致 rustc 以
  `STATUS_STACK_BUFFER_OVERRUN` 崩溃并写出损坏的 `.rmeta`(表现为
  `found invalid metadata files for crate ...`)。处置:停 gradle daemon、清 `deps/`、
  限制 `CARGO_BUILD_JOBS`。**该报错是资源问题,不是代码问题,勿误判为逻辑 bug。**
- **debug APK 体积**:未 strip 的 Rust debug `.so` 达 386MB(单 ABI APK 约 394MB)。
  已尝试在 gradle 中 `keepDebugSymbols.clear()`,但 `.so` 由 Rust 侧直接产出,
  gradle 未能剥离;正式优化留待阶段 5(用 release 构建或 NDK strip)。

---

**回归验证(已做):** `cargo test --lib`(server-rs,Windows) **715 passed / 0 failed** —— cfg 门控改动未破坏桌面行为。

**已完成的原生配置补丁(`src-tauri/gen/android/` 内,重新 `android init` 会被覆盖,需重打):**

1. `app/src/main/res/xml/network_security_config.xml`(新增)—— 只对 `127.0.0.1`/`localhost`/`::1` 放行明文,其余强制 HTTPS。**解决 release 构建 `usesCleartextTraffic=false` 导致 loopback 白屏的问题,且不用全局开门。**
2. `app/src/main/AndroidManifest.xml` —— `<application>` 增加 `android:networkSecurityConfig="@xml/network_security_config"`。
3. `buildSrc/.../BuildTask.kt` —— 修正 node 调用(见上"踩坑 4")。
4. `gradle/wrapper/gradle-wrapper.properties` —— gradle 发行包切腾讯镜像。
5. `build.gradle.kts` —— 加阿里云 Maven 镜像。
6. `gradle.properties` —— 追加 `abiList`/`targetList`(**注意:对 tauri CLI 构建不生效,仅 gradle 直接构建时有用**)。

---

## 五之三、阶段 1(Android Keystore 密钥存储)执行结果(2026-09-11 完成)

**问题:** `secret_store` 在非 Windows 平台默认**拒绝**持久化非空凭据。而 `RuntimeSettings::save()`
对两个 Key 字段无条件调用 `protect()` 并把错误向上传播 —— 结果是**只要在手机上填了 API Key,
整个设置保存就失败**(不只是 Key 存不下),手机端实际不可用。

### 实现

**Kotlin 侧** `src-tauri/gen/android/app/src/main/java/com/kedai/app/KeystoreBridge.kt`
- `AndroidKeyStore` 中生成 AES-256-GCM 密钥(不可导出,`setUserAuthenticationRequired(false)`
  以便后台任务也能解密);
- 密文格式 `[1 字节 IV 长度][IV][密文+标签]`,返回 base64。**存 IV 长度而非写死 12**,
  避免不同实现下 IV 长度变化导致解密失败。

**Rust 侧** `server-rs/src/services/keystore_android.rs`
- 两个 JNI 约束必须遵守,否则必然踩坑:
  1. **类加载器**:在 `AttachCurrentThread` 附加的原生线程上 `FindClass` 走的是系统类加载器,
     找不到应用类。必须在 `JNI_OnLoad` 期间(此时类加载器可见应用类)`FindClass` 一次并缓存
     为 `GlobalRef`,后续线程复用。
  2. **VM 指针**:Rust 没有 JavaVM,只能由 JVM 在 `System.loadLibrary` 时经 `JNI_OnLoad` 传入。
     该函数定义在 **cdylib 根**(`src-tauri/src/lib.rs`),依赖库内部定义可能被链接器丢弃;
     已核查 tao/wry/tauri 全栈**无**其它 `JNI_OnLoad`(否则会符号冲突)。
- 调用 Kotlin 只走两个字符串进出的静态方法,不在 Rust 里逐个拼 `javax.crypto` 的 JNI 调用。

**`secret_store.rs`** 改为三平台分支:Windows 走 DPAPI、Android 走 Keystore、
其它平台维持原拒绝语义。三者共用 `enc:v1:` 前缀(都是「本机可解、外拷不可解」,
存储格式对上层无差异,避免迁移逻辑分叉)。`protect` 的前缀分支按平台 `cfg` 分离。

### 上机验证(实测)

| 验证项 | 结果 |
|---|---|
| 写入 API Key | ✅ `PUT /api/settings` 返回 200(**此前必失败**) |
| 落盘为密文 | ✅ `settings.json` 中 `openai_api_key` = `enc:v1:...`,长度 87,**不含明文探针** |
| **重启后解密** | ✅ 杀进程重启,`has_api_key: true`、`api_key_masked: "****7890"`(与探针尾号一致) |
| 桌面无回归 | ✅ `cargo test --lib services::secret_store` 7 passed(DPAPI 往返/前缀/兼容语义全绿) |

> 注:`GET /api/settings` **有意不回显明文 Key**,只给 `has_api_key` 与 `api_key_masked`。
> 因此验证以 `has_api_key` 为准 —— 该字段来自内存中的明文,为 true 即证明密文被成功解密。
> 不要用 `openai_api_key` 字段判断(它恒为空)。
>
> 另注:`PUT` 对空字符串是「不修改」语义(防前端误清),故清理测试 Key 需直接改文件。

---

## 五之四、阶段 2(Android 壳收尾)执行结果(2026-09-11 完成)

### 返回键处理

**问题:** 壳的初始 URL 是 `about:blank`,导航到 `http://127.0.0.1:<port>/` 后 WebView 历史有两条记录;
Tauri 内建的返回处理会 `goBack()` → 退回 `about:blank` → **整页白屏且无法恢复**。

**方案:** 前端注册 `onBackButtonPress`(注册后壳不再自行处理),按 Android 习惯分级:

1. 有打开的弹窗 → 关最上层那个;
2. 有打开的抽屉 → 收起;
3. 都没有 → 退出应用。

**退出命令的两个坑(都实测过):**
- `plugin:app|exit`(壳内 AppPlugin 的命令)**不可用** —— Rust 侧没有对应权限声明,
  调用被 ACL 拒绝,报 `app.exit not allowed. Command not found`;
- `getCurrentWindow().close()` 在 Android 上**不结束 Activity** —— 调用后进程仍在且 Promise 悬挂。
- 结论:自己加 `#[tauri::command] fn exit_app` + `invoke_handler`,自定义命令不受插件 ACL 约束。
  注意该命令**不能加 `#[cfg(mobile)]`** —— `generate_handler!` 在桌面端会因找不到宏而编译失败;
  桌面端无 UI 入口调用它,两端都注册不影响行为。

### 系统栏与主题

见「五之二」第 4 节(颜色/主题)与第 3 节(移除 `enableEdgeToEdge`)。

**应用图标**

原 APK 内含 Tauri 默认图标(黄蓝占位图,与 Kedai 视觉无关)。已用仓库自身的粉色图标生成全套:
`icon.png`(512)→ 放大到 1024(`D:\kedai-android\artifacts\app-icon-1024.png`)→
`tauri icon` 生成 5 组密度 + 自适应图标(anydpi-v26)。

> **`tauri icon` 的输出路径坑**:它不认 `--output <res 目录>`,而是按「平台名」在输出目录下
> 建子目录(`<output>/android/mipmap-*`、`<output>/ios/...`)并顺带写一堆 Windows/iOS 资源。
> 正确做法是输出到临时目录后,把 `android/` 下的 mipmap 与 values 搬进
> `gen/android/app/src/main/res/`,再删掉多余平台目录。否则 APK 里用的仍是旧图标
> (资源合并不报错,只是静默不生效)。
>
> 自适应图标背景色已改为 Kedai 纸色 `#FBEFF1`(见 `values/ic_launcher_background.xml`)。

---

## 五之五、正式包(R8 混淆)暴露的问题与修复(2026-09-11 完成)

**这是只在 release 包出现、debug 包完全正常的问题**,若只测 debug 就会漏到用户手上。

**症状:** release 签名包保存 API Key 报
`500「设置保存失败: 调用 KeystoreBridge::encrypt 失败: Java exception was thrown」`,
而 debug 包同一操作正常。

**根因:** release 启用 R8 混淆,它**重命名**了 `com.kedai.app.KeystoreBridge` 及其方法;
而 Rust 侧是按**字符串名**反射调用的(`FindClass("com/kedai/app/KeystoreBridge")` +
`CallStaticMethod("encrypt")`),改名后自然找不到。

**为什么 Tauri 自动生成的 `proguard-wry.pro` 挡不住:** 它保留的是
`-keep class com.kedai.app.* { native <methods>; }`,
即「**Java 声明、native 实现**」的方法(供 Rust 侧被 Java 调用的那批);
而 `KeystoreBridge` 是**反向**的 —— Kotlin 实现、被 native 调用。方向不同,规则不覆盖。

**修复:** 两处双保险(见 `app/proguard-rules.pro` 与 `KeystoreBridge.kt`):

```proguard
# app/proguard-rules.pro —— 整类保留
-keep class com.kedai.app.KeystoreBridge { *; }
```
```kotlin
// KeystoreBridge.kt —— 类与成员加 @Keep(androidx 注解,R8 识别)
@Keep object KeystoreBridge {
    @JvmStatic @Keep fun encrypt(plain: String): String { ... }
    @JvmStatic @Keep fun decrypt(encoded: String): String { ... }
}
```

> **关键踩坑:逐成员写签名不可靠。** 最初的规则写成
> `-keep class ...KeystoreBridge { public static java.lang.String encrypt(java.lang.String); }`,
> 结果**类名保留了、方法名仍被混淆** —— 在 dex 里搜得到 `KeystoreBridge` 但搜不到
> `encrypt`/`decrypt`(Kotlin `object` + `@JvmStatic` 的实际描述符与手写规则未逐字匹配)。
> 必须用整类保留 `{ *; }`,并**用 dex 字节搜索验证方法名**,不能只看类名。

**修复后实测(release 签名包):** `PUT /api/settings` → **200**,
`has_api_key: true`、`api_key_masked: "****7890"`;
dex 内 `KeystoreBridge` / `encrypt` / `decrypt` 三者均在。

> 教训:**凡是被 Rust 通过 JNI 按名调用的 Kotlin 类/方法,都必须加 keep 规则**
> (整类保留,并验方法名)。后续阶段 3 的命令执行层(见计划)会有同类桥接,需同样处理。
> 验证方式:release 包必须实测关键路径,不能只验 debug。
>
> 注:release 包 `android:debuggable=false`,无法用 `run-as` 读写其私有目录,
> 排查需依赖应用内 API(前端经 CDP 调用)或 logcat。

---

## 五之六、正式包发布信息(2026-09-11)

| 项 | 值 |
|---|---|
| 版本 | 0.2.0 |
| 包名 | `com.kedai.app` |
| minSdk / targetSdk | 24 / 36(编译 36) |
| 签名 | 自签 release(keystore 与口令见下) |
| 产物 | `D:\kedai-android\artifacts\Kedai-0.2.0-arm64-release.apk`(约 33MB,arm64 真机)<br>`Kedai-0.2.0-x86_64-release.apk`(约 36MB,模拟器) |

**签名材料(自行保管,勿提交仓库):**
- keystore:`D:\kedai-android\keystores\kedai-release.jks`(别名 `kedai`,有效期 10000 天)
- 口令:`D:\kedai\src-tauri\gen\android\keystore.properties`
  (已被 `gen/android/.gitignore` 忽略,不会入库)
- 证书 SHA-256:`19d79a89d87473d286b1ab4d5a45defceae70efe65fa2de0369c1de25f2f062f`

**构建命令:**
```bash
# 正式包(单 ABI,arm64 真机)
D:\kedai-android\bin\ata.bat android build --apk --target aarch64
# 模拟器用
D:\kedai-android\bin\ata.bat android build --apk --target x86_64
# UI 快速迭代(仅 debug 包,约 20 秒;无需重编 Rust)
bash D:\kedai-android\bin\deploy-ui.sh
```

> **注意 `--target` 取值**:是 `aarch64` / `x86_64` / `armv7` / `i686`(Rust 三元组风格),
> **不是** `arm64` —— 传 `arm64` 会直接报 invalid value。
>
> `gradle.properties` 里的 `abiList`/`targetList` 对 tauri CLI 构建**不生效**;
> 要控制 ABI 只能靠 CLI 的 `--target`(可传多个),或直接调用 gradle 构建。

---

## 六、本机环境安装前基线记录

### 原环境现状

| 项 | 状态 |
|---|---|
| Rust | `stable-x86_64-pc-windows-msvc`,安装前无 android target |
| MSVC 工具链 | 已装(BuildTools 2022),构建前需 `vcvars64.bat` 注入(见 `AGENTS.md`) |
| Node / npm | 可用;`D:\kedai\node_modules` 已含 `@tauri-apps/cli` |
| JDK | 仅 `D:\Java\jdk-26.0.1`,**AGP 不支持 26,已另装 17** |
| Android SDK / NDK | 安装前完全未安装 |
| platform-tools | 已装(winget),adb 1.0.41 |
| **磁盘** | **C: 仅剩 7.4GB(97% 占用)← 关键约束,所有内容落 D: ** |

### 安装规划(全部落 D: 盘,避免挤爆 C:)

**工作区根目录:`D:\kedai-android\`**(已建,2026-09-11);Git 分支:`kedaiAndroid`(已建,工作区 `D:\kedai` 已切至该分支)。

```
D:\kedai-android\
├─ sdk\              Android SDK 根(ANDROID_HOME)
│  └─ cmdline-tools\latest\
├─ jdk\              JDK 17(解压版,JAVA_HOME 指向此)
├─ gradle-home\      GRADLE_USER_HOME(默认在 C: 会吃数 GB)
├─ keystores\        release 签名密钥(.jks / keystore.properties,不入库)
├─ artifacts\        APK/AAB 产物
└─ downloads\        安装包暂存
```

1. **JDK 17** → 解压 Temurin 17 到 `D:\kedai-android\jdk\`;`JAVA_HOME` 指向 17,**不要用现有 JDK 26**(AGP 不支持)。
2. **Android SDK** → `D:\kedai-android\sdk`:
   - 解压 `commandlinetools-win-*_latest.zip` 到 `sdk\cmdline-tools\latest`;
   - `sdkmanager "platform-tools" "platforms;android-34" "build-tools;34.0.0" "ndk;27.0.12077973"`;
   - `ANDROID_HOME=D:\kedai-android\sdk`,`ANDROID_NDK_HOME=D:\kedai-android\sdk\ndk\27.0.12077973`。
3. **Gradle 缓存迁离 C:**:`GRADLE_USER_HOME=D:\kedai-android\gradle-home`。
4. **Rust Android targets**:`rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android`。
5. **Cargo 目标目录**:Android 产物落在 `server-rs/target` 与 `src-tauri/target`(已在 D:),无需迁移。

### 待确认的前置条件(阶段 0 已解决,保留供追溯)

1. ~~是否允许安装约 10-15GB 的 SDK/NDK 到 D:~~ **已确认**:工作区为 `D:\kedai-android`。
2. **是否有 root 真机可测?** ROOT / Shizuku 通道在模拟器上无法验证,只能验证沙箱档。**仍待确认。**
3. **是否接受 MVP(阶段 0–2+4,约 5–7 周)不含 root/Shizuku/MCP,仅沙箱 shell,执行层作为独立阶段 3 追加?**

## 七、cfg 门控清单(实施时逐项维护)

改动必须是 **cfg 门控式**(Windows 行为逐字节不变),以下是当前已识别的门控点:

| 文件:行 | 现状 | Android 处理 |
|---|---|---|
| `server-rs/src/services/secret_store.rs:23-37` | DPAPI / 拒绝持久化 | 新增 Keystore 实现 |
| `server-rs/src/config.rs:47-67` | `current_exe` 向上找项目根 | 降级为开发态 |
| `server-rs/src/config.rs:73-81` | `%APPDATA%` | 加 Android 分支 |
| `server-rs/src/utils/fs_atomic.rs:32-40` | `ReplaceFileW` / rename 回退 | 确认回退可用 |
| `server-rs/src/services/runtime_prompt_service.rs:173-199` | `ReplaceFileW` | 门控 |
| `server-rs/src/mcp/process.rs:13,29-42` | spawn 子进程 | 门控,转执行器 |
| `server-rs/src/api/repo_index.rs:28-57` | `current_dir` 索引 | 门控 |
| `server-rs/src/services/token_service.rs:60-70` | 联网下载词表 | vendored 或降级 |
| `src-tauri/src/lib.rs:22-50` | `netstat`/`taskkill` | desktop-only |
| `src-tauri/src/lib.rs:58-60` | `%APPDATA%` 回退 | 删,用 `app_data_dir()` |
| `src-tauri/src/lib.rs:95-125` | `CloseRequested` 退出确认 | 改返回键 |
| `src-tauri/src/lib.rs:152-173` | 端口接管/杀进程 | desktop-only |
| `src-tauri/src/lib.rs:286-299` | `find_project_data_dir` | desktop-only |
| `src-tauri/src/main.rs` / `portable_main.rs` | `windows_subsystem` | desktop-only |
| `src-tauri/Cargo.toml:11-18` | 两个 `[[bin]]` | desktop-only |
| `server-rs/Cargo.toml:51-56` | `windows-sys` | 已仅 windows 目标 |
| `server-rs/Cargo.toml:22` / `src-tauri/Cargo.toml:30` | `native-tls` | 改 `rustls-tls` |
| `web/src/exportFile.ts:39-49` | dialog+fs 路径写 | SAF/分享 Intent |
| `web/index.html:5` | viewport 无 `viewport-fit` | 补 `cover` |
| `web/src/style.css:277` | `min-width:1100px` | 加移动断点 |

## 八、新增文档与测试约定

- 本文(`docs/android-port-plan.md`)为 Android 移植活文档,阶段推进时同步更新进度与门控清单。
- 阶段 1/2 每个 cfg 改动补最小单测;阶段 5 建立移动端功能回归清单(可脚本化)。
- 桌面与 Android 双目标构建纳入 `tools/check-all.ps1` 校验链。
- 遵循 `AGENTS.md`:面向用户提示与注释用简体中文;改核心行为先写失败测试;**不提交 Git**。
