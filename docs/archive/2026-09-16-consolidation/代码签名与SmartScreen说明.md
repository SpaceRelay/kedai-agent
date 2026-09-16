# 代码签名与 SmartScreen 说明

> 定位:**活文档**。解释 Kedai Windows 产物为何没有 Authenticode 签名、用户首次运行遇到
> SmartScreen 拦截时如何安全继续、如何自行核对产物来路,以及将来若正式签名需要动哪些环节。
> 面向使用者与维护者,不涉及产品决策本身(是否购买证书)。
> 来源:残余任务 6.3(`docs/残余任务与后续计划-2026-09-13.md:202`);事实于 2026-09-13 逐项实测。

---

## 一、现状:哪些产物没有签名,为什么不签

### 1.1 产物签名状态

| 产物 | 路径 | Windows Authenticode | 说明 |
|---|---|---|---|
| 测试版后端 + 内嵌前端 | `dist\kedai-server.exe` | **无签名**(NotSigned) | release 单二进制,最终交付给用户 |
| 便携版桌面应用 | `dist\Kedai-portable\Kedai.exe` | **无签名**(NotSigned) | 首选交付形式(见 `README.md:86`、`README.md:89`) |
| 图形启动器 | 项目根 `Kedai.exe`(源码 `launcher/`) | **无签名**(NotSigned) | 由 `Kedai.lnk` 双击进入,自带过期/漂移检测(`MAINTENANCE.md:201-203`) |
| NSIS 安装包 | `src-tauri\target\release\bundle\nsis\*.exe` | **无签名** | 仅 `build.ps1 -Tauri` / `npm run build:desktop` 时产出;非首选交付形式(`README.md:98`、`build.ps1:12`) |
| Android APK | `D:\kedai-android\artifacts\Kedai-<版本>-<arch>-release.apk` | **有自签名**(APK 签名,非 Authenticode) | 详见第三节 3.3 |

上述 Windows 三份 exe 的「无签名」为实测结论(对当前工作区产物执行
`Get-AuthenticodeSignature` 返回 `NotSigned`)。它们**没有**任何受信任 CA 颁发的代码签名证书。

> 注:便携版目录内另有 `dist\Kedai-portable\README.txt`(由 `tools/build-portable.ps1:96-104`
> 生成),内容为启动与数据目录说明,不涉及签名。

### 1.2 为什么不签

- **代码签名证书是付费产品**,且需以真实主体实名申请、按年续费。是否购买属产品决策,
  当前未购买,故本轮只补文档(依据 `docs/残余任务与后续计划-2026-09-13.md:202`)。
- **自签名证书解决不了 SmartScreen**。SmartScreen 只认可受信任 CA 链上的证书;自签证书
  与「未签名」在用户侧表现一样都会告警,还会额外增加密钥保管负担,因此**不做自签的
  Authenticode**。
- **当前分发定位为内测/自用**,使用者在可核对的渠道拿到产物,接受「未知发布者」的一次性
  确认成本;配合第三节的指纹自查即可确认来路。

---

## 二、用户侧:首次运行遇到 SmartScreen 如何安全继续

### 2.1 什么时候会触发

从浏览器 / 网盘 / 聊天工具**下载**再解压的产物,ZIP 与解压出的 exe 会带上 Windows 的
「网络来源」标记(Mark of the Web / Zone.Identifier),双击时可能弹出:

> **Windows 已保护你的电脑**
> Windows Defender SmartScreen 阻止了无法识别的应用启动。运行此应用可能会导致你的电脑存在风险。

在项目目录内**本地构建**(`.\build.ps1`)的产物通常不带该标记,一般不会弹窗。

### 2.2 安全继续步骤(确认来源后)

1. **先核对来源**(本地构建可跳过;下载分发的 zip 请先做第三节 3.2 的哈希核对)。
2. 在蓝色弹窗中点 **「更多信息」** → 展开后出现 **「仍要运行」** → 点「仍要运行」。
3. 若整个 ZIP 被拦、或「仍要运行」不可点:**先解除 ZIP 的锁定再解压**——右键下载的
   `.zip` → **属性** → 勾选底部 **「解除锁定」** → 确定,然后重新解压并运行。已解压出的
   单个 exe 也可同样在「属性」里「解除锁定」。
4. 程序启动后,在应用**设置中心底部**核对「版本 · 构建时间 · 指纹前 8 位」是否与预期一致
   (`MAINTENANCE.md:198-200`)。

> **不要**为了省事全局关闭 Defender / SmartScreen,或对整个下载目录永久放行。只对**已核对
> 哈希的单个文件**放行;来源不明时不要以「仍要运行」强行启动。

---

## 三、可信性自查:确认产物来路

### 3.1 构建指纹 sidecar(`<exe>.build.json`)

`build.ps1` 与 `tools/build-portable.ps1` 会在每个 dist 产物旁写一份 sidecar
(`tools/Write-BuildStamp.ps1:34-50`):

```json
{"version":"<package.json 版本>","build_time":"<UTC 构建时间>","dist_hash":"<聚合 SHA256>"}
```

- `dist_hash` 的算法:对 `web\dist` 全部文件按「小写完整路径 + 单文件 SHA-256」排序后再做一次
  聚合 SHA-256(`tools/Write-BuildStamp.ps1:8-30`)。两台机器用同一份 `web\dist` 会得到同一个值。
- **测试版与便携版的 `dist_hash` 必须一致**(`build.ps1:403-410` 会在构建收尾断言,不一致打印
  红字「严重」并提示重跑)。两者是两条独立编译链,各自冻结编译那一刻的前端,指纹一致才说明同源。

查看方法(PowerShell):

```powershell
Get-Content .\dist\kedai-server.exe.build.json
Get-Content .\dist\Kedai-portable\Kedai.exe.build.json
```

本次核查(2026-09-13,版本 `0.3.0-A-beta`)两份 sidecar 的 `dist_hash` 均为
`eb9494373580…`(前 12 位),即两端同步;该值随每次构建变化,仅作示例。

**运行时复核**:二进制内嵌 `KEDAI_DIST_HASH` / `KEDAI_BUILD_TIME`,`GET /api/health` 返回
`build_id` / `build_time`(`MAINTENANCE.md:198-200`),可与 sidecar 相互印证。

### 3.2 直接核对 exe 哈希

若要确认拿到的 exe 与构建方公布的一致,对 exe 本身算 SHA-256:

```powershell
Get-FileHash .\dist\Kedai-portable\Kedai.exe -Algorithm SHA256
```

把结果与构建方/发布方公布的十六进制串逐字比对。**建议只从你信任的渠道取哈希值。**

### 3.3 Android APK 的签名与覆盖安装约束

APK 与 Windows exe 走不同的签名体系:APK 用 release keystore 自签,构建即签名
(`docs/残余任务与后续计划-2026-09-13.md:41-42`)。

**核验命令**(需本机 `D:\kedai-android\` 工具目录,仅用于核验,不参与运行):

```bat
cmd /c "D:\kedai-android\bin\apksign.bat verify D:\kedai-android\artifacts\<apk>"
```

**期望结果**(2026-09-13 实测 `Kedai-0.3.0-A-beta-arm64-release.apk`):

- `Verified using v2 scheme (APK Signature Scheme v2): true`
- `Signer #1 certificate DN: CN=Kedai, OU=Kedai, O=Kedai, L=Unknown, ST=Unknown, C=CN`
- `Signer #1 certificate SHA-256 digest: 19d79a89d87473d286b1ab4d5a45defceae70efe65fa2de0369c1de25f2f062f`

历史记录同值,见 `docs/android-port-plan.md:513` 与 `docs/0.3.0-A-beta-变更说明.md:79`。

**覆盖安装约束(重要)**:Android 要求**同包名 + 同签名密钥**才能覆盖升级。

- 用**同一密钥**(SHA-256 `19d79a89…`)签的新版 → 可直接覆盖安装,**保留应用数据**。
- 换了密钥 → 必须先卸载旧版才能安装,会**丢失应用数据**。

因此发布 APK 必须始终坚持用 `D:\kedai-android\keystores\kedai-release.jks`(别名 `kedai`),
口令配置在 `src-tauri\gen\android\keystore.properties`(已被 `src-tauri/gen/android/.gitignore`
忽略,不入库;详见 `docs/android-port-plan.md:509-513`)。**切勿重新生成密钥。**

### 3.4 指纹能证明什么、不能证明什么

**能**:证明某份产物由本仓库构建链产出、测试版与便携版同源同步、给你的哈希与公布值一致。

**不能**(诚实标注,勿据此宣称「防篡改」):

- sidecar(`.build.json`)**未与 exe 做密码学绑定**,可被一并替换,属一致性辅助而非信任根。
- `dist_hash` 只覆盖 **`web/dist` 前端资源**,**不是 exe 二进制自身的哈希**;
  exe 二进制哈希须用 3.2 的 `Get-FileHash` 单独核。
- 二进制内嵌指纹用 FNV-1a,与 sidecar 的 SHA-256 **算法不同、不可直接比对**——此「指纹链不闭环」
  已登记为残余风险 D2(`docs/威胁模型与残余风险.md:23`)。

真正防冒充需要 Authenticode 签名(第四节)。

---

## 四、后续:若要正式签名(只列要点,不实施)

1. **证书类型**
   - **OV 代码签名证书**:成本较低,但 SmartScreen 信誉需**时间与下载量积累**后才逐步消除告警,
     非即时生效。
   - **EV 代码签名证书**:首装即获 SmartScreen 信任,但成本更高,且私钥必须存于硬件令牌或云 HSM。
2. **私钥保护**:2023 年起主流 CA 要求 OV/EV 私钥存硬件令牌或云 HSM(如 Azure Trusted Signing、
   DigiCert KeyLocker),不能再以文件形式保存;需为签发环境与持有者建立流程。
3. **构建/发布改动点**(均为「新增签名步骤」,不改现有构建逻辑):
   - 每个 exe 产出后执行 `signtool sign /fd SHA256 /td SHA256 /tr <RFC3161 时间戳服务>`;时间戳
     **必须加**,否则证书过期后已签文件随之失效。
   - 接入 `build.ps1` 的三处产物(`dist\kedai-server.exe`、`tools/build-portable.ps1` 产出的
     `dist\Kedai-portable\Kedai.exe`、项目根 `Kedai.exe`)。
   - NSIS 安装包(可选交付形式)可在 Tauri 的 Windows signing 配置中声明证书,或构建后补签。
   - 若启用 `.github/workflows/ci.yml`(当前已落盘但**未激活**),需设计签名密钥在 CI 中的安全注入。
4. **预期效果与边界**:EV 可即时消除 SmartScreen 告警;OV 仍需信誉积累。签名无法覆盖
   「用户从不可信渠道取得被替换产物」的场景,来源核对(第三节)仍是必要环节。
5. **决策归属**:是否购买、买 OV 还是 EV,属产品决策,本文不代决。

---

## 附:相关文件

| 文件 | 关联点 |
|---|---|
| `build.ps1:19-21`、`build.ps1:281-282` | 构建指纹 sidecar 的写出 |
| `build.ps1:403-410` | 双端 `dist_hash` 一致性断言 |
| `tools/Write-BuildStamp.ps1:8-50` | `dist_hash` 算法与 sidecar 格式 |
| `tools/build-portable.ps1:89-104` | 便携版 sidecar 与 `README.txt` |
| `README.md:86`、`README.md:89`、`README.md:111` | 交付入口、复制分发、指纹说明 |
| `MAINTENANCE.md:196-203` | 两版同步机制(构建/运行时/启动三层) |
| `docs/android-port-plan.md:506-513` | APK 签名材料与证书 SHA-256 |
| `docs/威胁模型与残余风险.md:23` | 残余风险 D2(指纹链不闭环) |
| `docs/残余任务与后续计划-2026-09-13.md:202` | 本任务的来源项 6.3 |
