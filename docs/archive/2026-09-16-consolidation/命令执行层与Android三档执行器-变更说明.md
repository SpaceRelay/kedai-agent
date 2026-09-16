# 命令执行层与 Android 三档执行器 — 变更说明(2026-09-13)

> 对外变更契约。本次新增「命令执行(bash 工具)」与「Android 执行层(ROOT/Shizuku/沙箱)」,
> 并补完 7 个前端组件测试。**不改既有 API 线格式**(仅新增 exec 端点)、**不改既有 DB schema**
> (仅新增 `exec_audit` 表)。

## 一、命令执行(bash 工具)

### 新增工具
- 名称 `bash`,参数 `command`(必填)/ `cwd` / `timeout_ms`。
- 平台默认 shell:Windows 用 `cmd /c`,其他平台用 `sh -c`。
- **不加入任何只读白名单**(READONLY_SCOUT / SUBAGENT / REFLECT),不自动下发给
  规划侦察轮、子 agent、反思步骤。
- **任务模式默认集合**(后续修订,2026-09-13):风险恒 `Dangerous`,但
  `task_engine::tool_policy` 的 `deny_dangerous` 档按**工具名**对 `bash` 开例外下发——
  用户要求任务模式具备命令执行能力。下发工具不等于放行危险命令:`destructive`/`admin`
  命令仍由命令级硬门在任何自动放行之前拒绝(任务模式无 UI 通道 → 直接拒绝),
  详情见 [授权模式.md](授权模式.md) 三·五与四节。

### 四道安全闸
| 闸 | 落点 | 行为 |
|---|---|---|
| 总开关 | `settings.exec_enabled`(默认 **false**) | 关闭时一律拒绝 + 留审计 |
| 工具级 | `permissions::default_risk` = `Dangerous` | 走既有三档 + 「始终需授权」清单 |
| **命令级** | `tools/command_risk.rs` | `destructive`/`admin` 命令**逐条确认,不受任何授权豁免** |
| 执行级 | `services/exec/` | 强制超时(60s 默认/300s 上限)+ 强杀 + stdin null + 输出截断 32K |

命令分级四级:`safe`(只读)/ `sensitive`(写入)/ `destructive`(删除、格式化、覆写设备)/
`admin`(提权、服务、网络、包管理、Android `pm`/`am`)。多命令串联逐段判定取最高危。

### ★ 实测发现并修复的越权缺口(重要)
**冒烟测试实测复现**:修复前 `explicitly_allowed`(会话/角色级工具授权)判定在命令级硬门**之前**,
导致用户一旦授权过 `bash`,`rm -rf /tmp/xxx` 会被**直接执行**(实测确认文件被创建)。

**根因**:`bash` 是单一工具名,工具级授权粒度是「工具」而非「这次跑的命令」——
授权 bash 后同一工具既能跑 `ls` 也能跑 `rm -rf`,工具级授权无法承担命令级区分责任。

**修复**:把命令级高危硬门提到判定链**最前**(未注册检查之后、白名单/显式授权之前),
使 `destructive`/`admin` 命令在任何授权模式下都需逐条确认。
**回归测试**:`explicit_tool_grant_does_not_bypass_destructive_command_gate` +
`explicit_tool_grant_still_applies_to_safe_commands`(后者确保修复没有过度拦截:已授权后
`safe` 命令仍不再重复问)。**修复后实测**:已授权 bash 状态下 `rm -rf` 弹确认且未执行,
`echo` 正常执行。

### 审计
新增表 `exec_audit`(幂等迁移 + 基线 DDL 双份),字段:
`id, ts, source(chat|task|android), task_id, session_id, command, shell, tier, risk, decision,
exit_code, stdout_summary, stderr_summary`。
**每次尝试都落一行,含被拒绝的**——「有人试图跑 rm -rf」同样要留痕。
输出摘要截断 2000 字符,避免审计表被长输出灌爆。

### 新增 API
| 端点 | 说明 |
|---|---|
| `GET /api/exec/tier` | 当前执行器等级 + 中文标签 |
| `GET /api/exec/audit?limit&source&risk` | 审计列表(时间倒序,limit 上限 500) |
| `DELETE /api/exec/audit` | 清空审计 |

## 二、Exec 抽象层(`server-rs/src/services/exec/`)

- `mod.rs`:`ShellTier{Root,Shizuku,Sandbox,Disabled}`、`ExecRequest`/`ExecResult`、
  `execute(req, allowed_tiers)`(等级放行校验)、`detect_tier()`、超时夹取与输出截断。
- `desktop.rs`:`tokio::process` 直派,`kill_on_drop` + 超时显式强杀。
- `android.rs`(`cfg(target_os="android")`):经 JNI 调 Kotlin `ShellExecutorBridge`,
  入参 `command␟cwd␟timeoutMs`,返回 `exitCode␟stdout␟stderr`(沿用 `\u{1f}` 约定)。
- `audit.rs`:审计写入/查询/清空。

**等级放行**:探测到的等级必须被用户显式放行(`exec_allow_*`),否则拒绝并提示——
这是「等级可见 + 可关闭」的执行侧落点(避免探测到 ROOT 就悄悄以高权限执行)。

## 三、Android 执行层(阶段 3)

### 新增 Kotlin `ShellExecutorBridge.kt`
- 三档探测 `detectTier()`:ROOT(`su -c id` 3s 超时探测)→ Shizuku(binder ping + version)→ 沙箱。
- `exec(payload)`:ROOT 用 `su -c`,Shizuku 用 `Shizuku.newProcess("sh","-c",cmd)`,沙箱用 `sh -c`。
- **Shizuku 经反射访问**(`Class.forName("rikka.shizuku.Shizuku")`):不引入编译期依赖,
  未安装/未授权时静默降级为沙箱档,不因 `NoClassDefFoundError` 崩溃进程。
- `requestShizukuPermission()`:经动态代理实现回调接口,触发系统授权弹窗。
- `refreshTier()`:前端「刷新」按钮用。

### 构建接入
- `AndroidManifest.xml`:加 `moe.shizuku.manager.permission.API_V23` + `<queries>` 声明
  (Android 11+ 包可见性,用于探测 Shizuku 是否安装)。
- `proguard-rules.pro`:整类 keep 新桥类(与 KeystoreBridge 同一踩坑教训:
  逐成员签名不可靠,必须 `{ *; }`)。
- `KedaiNative.appContextOrNull()`:新增只读上下文访问器,供兄弟桥类做探测
  (探测路径不应因上下文缺失抛异常)。
- **gradle 依赖无需新增**(Shizuku 走反射),从根上规避了 Maven 拉取风险。

### 已验证 / 未验证(如实标注)
- ✅ **Rust 侧 Android 交叉编译通过**:`acargo check --target aarch64-linux-android --lib`
  (含 `jni`/`rquickjs-sys` 等原生依赖)。
- ✅ **Kotlin 编译通过**:`gradlew :app:compileUniversalDebugKotlin`(零警告零错误)。
- ❌ **ROOT / Shizuku 运行时未验证**:需 root 真机与 ADB 激活的 Shizuku 服务,
  模拟器无法验证(方案文档已明示)。交付为「实现完成 + 待真机验证」。
- 沙箱档逻辑可在模拟器验证(用系统 `sh -c`,无需额外二进制)。

## 四、前端

- 新增 `web/src/components/settings/AndroidExecSection.vue`:总开关、Android 三档放行、
  当前等级展示与刷新、Shizuku 授权请求、审计列表(来源/风险过滤 + 清空)。
  挂在 `AgentSettingsSection` 的「授权管理」之后;**Android 专属档位按 `isAndroidTauri` 条件显示**,
  桌面/浏览器只显示总开关与审计。
- 新增 `web/src/api/exec.ts`(tier / audit 列表 / 清空)。
- `genSettings` store 新增 4 个字段与 `requestShizukuPermission` action
  (经 Tauri 事件 `kedai://shizuku-request`,与 openExternal/shareFile 同一模式)。
- 契约登记:`tools/check-contract.mjs` 新增 `ExecAuditEntry` 映射(现共 10 组)。

## 五、组件测试补完(阶段 A)

补 7 个此前无测试的组件:`Sidebar`(5)、`ChatInput`(4)、`ChatWindow`(3)、`SettingsHub`(4)、
`ScriptsModal`(4)、`ContractsModal`(4)、`AudioPlayer`(4)。
`vitest.config.ts` 关闭 `transformAssetUrls`(模板内 `/logo.png` 等公共资源在测试环境无 dev server
可解析,报 `file:///logo.png` 非法路径;测试从不断言资源 URL)。

## 六、测试统计(2026-09-13 实测)

| 项 | 数量 |
|---|---|
| 后端 | **962** 个(阶段 B/C 新增 39:命令分级 12、执行器 10、审计 6、权限矩阵 7、迁移 1、bash 3) |
| 前端 | **733** 个(阶段 A 新增 32、阶段 E 新增 8) |
| 契约映射 | 10 组 |

## 七、本轮未做

- 不做 proot / busybox 打包(沙箱档用系统 `sh -c` 已够;busybox 作为可选增强,未引入二进制)。
- 不做 Android 上的 Node 运行时(`npx`/`uvx` 类 MCP 服务器)。
- 不做无障碍(Accessibility)执行档。
- `check-all.ps1` 未加 Android 编译校验 stage(避免拖慢日常门禁;可按需手动跑 `acargo`)。
