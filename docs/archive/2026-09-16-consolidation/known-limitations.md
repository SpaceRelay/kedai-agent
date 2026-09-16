# 已知能力缺口清单(有意裁剪 vs 待办)

> 目的:把「看起来没做完」的地方标注清楚——哪些是**有意裁剪**(兼容子集,不要当 bug 修),
> 哪些是**待办**(有明确补齐路径)。改代码前先来对一遍,避免误判。
> 机制背景见 [优化实施方案-2026-09.md](优化实施方案-2026-09.md) 第一篇对应章节(下文逐项标注)。

## L1 世界书 sticky / cooldown 不参与匹配(有意裁剪)

- **现状行为**:条目的 `sticky`(激发后持续轮数)与 `cooldown`(激发后冷却轮数)在转换层
  已解析但固定归零(`server-rs/src/agents/engine/worldbook.rs:267-268`、`292-293`),
  匹配算法(同文件 56-133)不消费这两个字段,也没有「递归激活」(已触发条目的注入文本
  不再回头触发别的条目)。
- **判定**:**有意裁剪**。Kedai 对齐 SillyTavern 世界书行为的一个子集:关键词/正则触发、
  概率门控、constant/triggered 两组注入位置都已实现;sticky/cooldown 依赖跨轮状态台账,
  与 Kedai「每次生成都从消息池重新装配」的无状态管线相冲突。
- **若要补齐**:需在会话维度维护「条目激发台账」(哪条何时触发、剩余 sticky 轮数),
  落库表 + collect_context 装配时读取;递归激活则要在注入文本上再跑一轮匹配并防环路。
- **生态兼容考量**:ST 卡若重度依赖 sticky/cooldown(如进度条式世界书),在 Kedai 中表现
  为「条目只按关键词当下触发」——语义更弱但不错乱,属可接受降级。

## L2 契约不变量仅落地 mutex(待办)

- **现状行为**:契约(Contract)可声明三类跨字段不变量,`check_invariants()`
  (`server-rs/src/contracts/op.rs:214` 起)只判定 **mutex**(同组路径同时有值 ≥2 即违反);
  **range / require_if 两个变体恒判为满足**(匹配分支为空操作,注释已留痕)。
- **判定**:**待办**。代码注释指明依赖「白名单谓词引擎(§17.8 CES)」这一后续里程碑;
  当前恒通过意味着声明了 range/require_if 的契约卡不会得到校验,也不会报错。
- **若要补齐**:实现谓词白名单解析与求值(比较/区间/存在性),在 op.rs 的两个空分支接线;
  注意谓词引擎本身是另一项独立设计,不要在此顺手发明语法。
- **生态兼容考量**:契约(Kaleido)是 Kedai 自研体系,无 ST 生态包袱;待办期间前端不应
  向用户暴露「不变量已生效」的措辞。

## L3 MCP 孙进程不保证回收(v1 保守语义,待评估)

- **现状行为**:MCP 服务器子进程以 `kill_on_drop(true)` 托管、显式 kill 后 wait 回收僵尸
  (`server-rs/src/mcp/process.rs:1-10`);但 kill 只作用于**直接子进程**——若服务器命令是
  `cmd /c xxx` / `sh -c xxx` 这类包装,真正干活的孙进程不保证被杀掉。
- **判定**:**v1 有意保守 + 待评估**。文件头注释已声明「如需进程组级清理另起批次」。
- **若要补齐**:Windows 上用 Job Object 把子进程树编入作业对象(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE),
  或 spawn 时包一层 `taskkill /T`;跨平台方案需分别处理 Unix 进程组。
- **生态兼容考量**:多数 MCP 服务器(npx/uvx 直起)不是包装形态,影响面有限;但用户配置了
  批处理包装时,禁用服务器后孙进程可能残留占端口。

## L4 世界书激发条目 role=system 钳制为 user(有意裁剪)
  `role=system` 也会被钳制为 `user`(`server-rs/src/agents/engine/worldbook.rs:29`,
  注释「role=system 在尾部钳制为 user」);只有**常驻**条目(位置 3)的 system 才真的
  拼入 system 文本。
- **判定**:**有意裁剪**。尾部消息流里夹 system 角色在多数 OpenAI 兼容端点上语义不明
  (有的端点直接拒绝中段 system),钳制为 user 是兼容性最优解。
- **生态兼容考量**:ST 生态里激发条目写 system 的卡较少;这些卡在 Kedai 中表现为
  「注入仍然出现,只是角色是 user」,内容不丢。

## L5 跨 realm 共享全局桥只覆盖 JSON 子集(有意裁剪)

- **现状行为**:沙箱「每脚本一个不透明源 iframe」下,同角色不同沙箱 realm 之间无法直读
  window;共享桥(commit f2c10d1,`web/src/sandbox/shared-globals.ts` + `boot-script.ts`
  内联镜像 `__kdShared*`)只同步**键为合法标识符且非黑名单、值 JSON 可序列化且 ≤256KB**
  的 window 自有属性快照:卡级 realm diff 上报(shared-publish)→ 宿主按角色浅合并持久化
  → 广播给同角色订阅沙箱(shared-update)。
- **判定**:**有意裁剪**。函数/DOM 引用/循环结构/超大对象不跨 realm 共享(JSON.stringify
  失败或超限即静默跳过),黑名单防覆写 window/document/location/fetch 等通信基建与 `__kd`
  内部前缀。
- **补齐路径**:需要真函数共享的脚本应改写为数据形态(存配置让对端按数据重建),或等
  卡级合并单 realm(同段组共享 window)覆盖该场景后迁移到卡级脚本里。
- **生态兼容考量**:ST 真实环境同页同 realm,脚本间可挂任意函数全局;Kedai 跨沙箱只保
  证纯数据互通。多数生态脚本(状态栏数据、剧情数据库、消息级降级读)用的正是纯 JSON
  对象(如 `window.WuWaShared = {STORY_MAP,…}`),在此子集内可工作。

---

## L6 沙箱 draggable 只实现定位与钳制(有意裁剪)

- **现状行为**:宿主侧 `web/src/sandbox/draggable.ts` 复刻 jQuery UI `.draggable()`
  的最小语义:支持 `handle`/`cancel`/`containment:'window'`/`distance`/`cursor` 与
  `start`/`drag`/`stop` 回调(`event.ui.position` 经 jq-event 回发)。**未实现**
  `helper`(克隆/代理元素)、`axis`、`grid`、`snap`/`snapMode`、`revert`、`stack`、
  `zIndex` 管理,也不支持在 `drag` 回调里改写 `ui.position` 反写回宿主(回调是只读通知;
  WuWa th-5 的手写边界钳制已由宿主按 `containment` 等价实现)。
- **判定**:**有意裁剪**。生态悬浮窗脚本的拖拽诉求集中在「能拖、不跑出视口、位置能存」,
  复杂选项在角色卡场景几乎不用;完整移植 jQuery UI 的收益与维护成本不成比例。
- **补齐路径**:需要 `helper`/`axis` 等的卡可在此文件按同一模式扩展(pointer 事件已在
  宿主侧,document 级跟踪与 cleanup 登记基建已就绪)。
- **生态兼容考量**:th-3/th-4 有 `typeof .draggable === 'function'` 守卫,缺失时静默跳过;
  th-5 无守卫——本实现补上后不再 TypeError 中断,是三窗能显示的前提。

## L7 记忆向量检索:精确 KNN,无 ANN 索引(有意裁剪)

- **现状行为**:记忆语义召回已落地(`server-rs/src/services/embedding_service.rs` +
  `memory_service.rs::select_recall_hybrid`),向量存 `vec0` 虚拟表,按
  「向量×0.7 + Jaccard×0.3」混合打分;但**未建 ANN 索引**,vec0 走精确 KNN。
- **判定**:**有意裁剪**。记忆量级为每角色数百至数千条(受 `memory_max_entries` 约束),
  精确 KNN 在 5k×512 维下约 1–15 ms,ANN 的召回率损失与索引一致性维护成本不划算。
- **补齐路径**:若单角色记忆量突破数万条,可启用 `sqlite-vec` 的分区/量化能力;
  接口层 `select_recall_hybrid` 无需改动。
- **生态兼容考量**:换 embedding 模型导致维度变化时,`vec0` 表维度固定无法原地迁移,
  服务端只记录 `dim_mismatch` 并**保留旧数据**,需用户在设置页点「重建向量索引」
  (清表 + 全量回填)——刻意不自动重建,避免用户未预期地消耗 API 额度。

---

## L8 授权:自定义风险级无入口、外部工具区域判定受限(待办)

- **现状行为**(`server-rs/src/tools/permissions.rs`、`tools/action_class.rs`):
  1. `PermissionConfig.classifications`(自定义风险级)字段存在,但全仓无写入通路
     (无 API、无 UI),插件与 MCP 工具恒按危险级处理(`default_risk` 的未知分支)。
  2. 插件 / MCP 工具的路径区域靠扫描参数中的路径样式(盘符 / UNC / 绝对路径 / `..`)判定:
     无路径参数的调用按「非系统路径」处理;带路径参数则一律按系统路径写/删拦截
     (无法区分只读与写入,故偏严)。
- **判定**:**待办**。让用户自定义风险级会扩大安全面(把危险工具降级即放开自动执行),
  需要独立的产品与安全设计,不在授权改造批次内。
- **补齐路径**:新增 `PUT /api/agent/tool-permissions/classifications` 与设置页入口;
  区域判定若需更精确,可要求插件/MCP 在工具定义中声明参数语义。
- **生态兼容考量**:三档授权模式的硬底线(系统路径写/删恒需授权)不因风险级自定义而改变。

## L9 `deep` 模式主生成不启用工具(有意裁剪)

- **现状行为**(`server-rs/src/api/chat.rs`):角色扮演四档中只有 `agent`(与 `custom`)
  向模型下发工具定义;`deep` 主生成无工具。2026-09 授权改造后,`deep` 的**反思轮**已放开
  `read`(可查世界书/资料佐证判定),但主生成仍不调用工具。
- **判定**:**有意裁剪**。给 deep 主生成引入只读工具循环会改变其语义(从「一次生成 + 反思」
  变为多轮工具交互)并显著增加 token 成本,需独立评估。
- **补齐路径**:若确有需求,可为 deep 增加近似 agent 的受限只读工具循环(白名单
  `tool_sets::READONLY_SCOUT`),沿用 `ToolGate` 闸门。
- **生态兼容考量**:deep 用户当前的 prompt 与成本预期是「无工具」,变更需在前端明确标注。

---

## L10 角色扮演 Agent 记录只恢复工具调用,不恢复推理链(有意裁剪)
- **现状行为**(`server-rs/src/api/agent.rs` 的 `session_trace` + `web/src/stores/chat.ts`
  的 `restoreAgentTrace`):切会话/重启后,右侧 Agent 面板可恢复 `agent_sessions` 的
  state/plan/step_index 与 `tool_calls` 列表,但 `AgentActivity.chain`(推理链步骤明细)
  只存在于前端 SSE 内存,无持久层,恢复后为空。
- **判定**:**有意裁剪**。推理链是流式过程的逐条快照(`step` 事件),量级大、复用价值低;
  工具调用与最终产出才是复盘所需。持久化整条 chain 会显著增大写入量而收益有限。
- **补齐路径**:若需回看推理过程,可在 `agent_sessions.steps` 或独立表记录 `step` 事件
  (现有 `SseEvent::Step` 已是结构化载荷,落库即可);前端恢复时按 `at` 升序回填 chain。
- **生态兼容考量**:恢复的 `toolCalls` 一律置 `status='done'`(历史记录不可能处于 running),
  与实时 SSE 累积的形态区分,避免面板显示永不结束的「执行中」。

---

---

## L11 沙箱 iframe 保留 `allow-modals`,`postMessage` 无法校验 origin(有意裁剪 + 平台限制)

- **现状行为**(`web/src/sandbox/iframe-lifecycle.ts` 的 `sandboxAttributes()` 与 `onMessage`):
  - iframe `sandbox="allow-scripts allow-modals"`(无 `allow-same-origin`,不透明源)。
  - 消息身份校验**仅**为「高熵 nonce + 固定 channel」;不校验 `event.origin` / `event.source`。
- **判定**:
  - `origin` / `source` 校验属**平台限制不可用**(2026-09-12 实测评估):
    不透明源 iframe 的 origin 恒为 `"null"`(见 `render.ts` 注释),无判别力;
    而 WebView2 会把跨源 iframe 的 `event.source` 包装成另一个对象,与
    `iframe.contentWindow` 不相等——按「不等即拒」会误杀全部正常消息
    (`scriptRunner.test.ts`「接受 WebView2 source wrapper」已锁定该契约)。
    故 nonce(128 位随机、仅存于执行闭包、经 URL fragment 交付、不落 DOM/存储)
    是当前唯一可靠身份凭据。
  - `allow-modals` 属**有意保留**:角色卡作者脚本用 `alert`/`confirm` 做协议确认与提示
    (赛马娘卡 `submitCreation` 经剪贴板桥回包后 `alert` 弹窗,`sandbox/uma-creation.test.ts`
    锁定该链路)。移除会让这类卡静默失效,故保留,前提是「沙箱内弹窗不触及宿主 DOM/存储」。
- **安全边界**:外层仍有站点 CSP、`/sandbox.html` 下发、RPC op 白名单、innerHTML 清洗。
  当前未发现可实际利用的逃逸路径,故不构成现实漏洞。
- **加固路径**(若未来平台支持):改用 `MessageChannel` 端口传递(端口对象天然只有双方持有,
  取代「广播 channel + nonce」模型),可彻底摆脱 origin/source 判别与 `'*'` targetOrigin。

---

## L12 角色卡脚本:前端沙箱有授权台账,后端执行路径未打通(**已修复**,2026-09-14)

- **现状行为**(2026-09-13 复核后修正描述——**并非「完全没有授权机制」**):
  - **前端**路径已有授权:`web/src/scriptAuthorization.ts` 的
    `LocalScriptAuthorizationStore` 按「角色 id + 脚本内容 SHA-256」记录授权
    (localStorage,key `kedai.character-script-authorizations.v1`),
    `stores/character.ts` 暴露 `currentScriptAuthorized` / `scriptAuthorizations`,
    设置页 `components/settings/UiSection.vue` 可查看与撤销。前端沙箱脚本受此约束。
  - **后端**路径无授权门槛:`agents/engine/mod.rs:641` 在消息生成完成后调用
    `run_character_scripts`,串行执行角色卡 `extensions.tavern_helper` 里的启用脚本;
    脚本经 TavernHelper 兼容桥(`scripts/bridge.rs`)可写 global/character/preset/script
    作用域(`write_scope:265`)、导入数据(`import_raw:244`)、发起生成(`generate_from_config:198`),
    写回结果由 `take_others` **落库**。此路径不查前端的授权台账(后端无从读取浏览器 localStorage),
    即**同一张卡的脚本,前端要授权、后端自动跑**。
- **判定与处置**:**已修**(2026-09-14,采用下方「方案 1 复用前端授权」的变体):
  后端新增 `script_authorizations` 表 + `ScriptAuthorizationService` 授权门,
  `run_character_scripts` 把 **global(用户自有,可信,不走门禁)与 character(受门禁)拆分**,
  character 部分 **fail-closed**——台账无「与当前脚本内容一致的哈希」即不执行并告警。
  **哈希唯一来源是后端**:`GET /api/script-authorizations` 返回实时计算的 `current_hash`,
  前端授权时原样回传(避免前后端各算一遍导致算法漂移永不匹配)。
  端点:GET 查询 / PUT 授权 / DELETE 撤销 / GET list。e2e 测试 4 条
  (`tests/scripts_e2e.rs`:未授权不执行、授权后执行、撤销后不执行、陈旧哈希 409)。
  **这是有意的兼容性收紧**(旧行为自动执行),已按兼容性变更登记。
- **补齐路径**(2026-09-14 已选定方案 2,见 `docs/授权模式.md` 的「后端脚本授权门」节):
  1. **复用前端授权**(**已选定**):后端执行的**角色脚本树正是角色卡的
     `data_raw.extensions.tavern_helper`**(`services/user_script_service.rs:4`),
     与前端 `cardScripts.ts` 读的是**同一份数据**——故后端可在同一数据源上算出与前端**一致**的
     规范化哈希,据此对账。方案:新增 `script_authorizations` 表 + `GET/PUT/DELETE
     /api/script-authorizations?characterId=` + 前后端共享哈希 fixture 对拍;
     `run_character_scripts` 把 **global(用户自有,视为可信)与 character(受门禁)拆分**,
     character 部分查台账,**默认拒绝**并发出可观测事件。
  2. **后端独立开关**(备选,改动更小):新增设置项经
     `ToolPermissionManager::decide_with_policy`(`tools/permissions.rs:165`)裁决。
- **生态兼容考量**:ST 生态大量卡依赖脚本自动写变量(状态栏、好感度),
  默认开启授权会让这类卡静默失效。**本方案的 fail-closed 收紧属有意的安全变更**,
  须在变更说明中作为**兼容性变更**登记,并在 UI 给出清晰的重新授权引导。
- **本轮已完成的相关加固**(E.2,与授权正交):脚本单次执行加 5 秒墙钟硬超时
  (`agents/engine/run_scripts.rs`,协作式中断拦不住阻塞在 native 闭包者)、
  单轮脚本数量上限 32(防「脚本海」耗尽资源)。

## L19 插件工具可重名覆盖内置工具(**已修复**,2026-09-14)

- **现状行为**:插件加载走 `plugins/mod.rs` 的 `load_file` → `registry.rs` 的
  `register_external`,后者用 `g.tools.insert(name, ...)` **无条件写入**,
  **不校验是否与内置工具重名**(`registry.rs:123` 的 `is_builtin` 已存在但注册路径未调用)。
  插件 `name` 仅校验非空(`plugins/mod.rs:68-73`)。
  后果:一个名为 `bash` / `write` 的插件工具会**覆盖内置工具的执行器**,
  且其 `origin` 变为 `Plugin`——三档授权的路径可信度判定随之改变(外部工具参数被视为不可信),
  但**执行体已被替换**。`data/plugins/tools/*.json` 无总开关,启动即加载目录下全部文件。
- **判定与处置**:**已修**(2026-09-14)。三处加固:
  ① 注册前置校验内置名冲突并**拒绝**(`api/plugins.rs::register_plugins`,冲突计入 errors 而非覆盖);
  ② 解析期名称格式校验(`plugins/mod.rs::validate_plugin_name`):`^[a-z][a-z0-9_]*$`、长度 ≤48、
  **拒绝保留前缀** `mcp_`/`agent`(防命名空间伪造——比重名更隐蔽);
  ③ 回归测试 `plugin_name_validation_rejects_reserved_and_invalid_names`(12 条断言)。
  插件「目录即加载」的设计保持不变(注册表 + 默认 Dangerous 仍是隔离手段),
  但**不得劫持已有工具**这条不变量已落地。

## L20 rquickjs 沙箱无栈上限(**已修复**,2026-09-14;**实测修正了原判断**)

- **现状行为**:`scripts/runtime.rs` 设置了内存上限(64MB,`set_memory_limit`,行 55/94)
  与协作式中断(1s,`set_interrupt_handler`,行 58/97),但**未设置栈上限**
  (全仓无 `set_max_stack_size`)。深递归脚本可致栈溢出——rquickjs 虽为独立 Runtime,
  但宿主进程与之同栈空间,存在崩溃宿主的风险。
- **判定与处置**:**已修**(2026-09-14),且**实测修正了本条最初的判断**:
  1. **原判断(未显式设置 = 拿到库默认值)是错的**:实测 rquickjs **不会**自动施加安全默认值,
     未调用 `set_max_stack_size` 时深递归以 `STATUS_STACK_OVERFLOW` **直接杀进程**
     ——这是比「缺一个上限」更严重的真实漏洞(可被单张卡挂掉整个后端);
  2. **该上限不是「越大越安全」,而是越大越危险**:实测(Windows,单档隔离)——
     | 栈上限 | 深度 30 | 深度 100 | 深度 300 |
     |---|---|---|---|
     | 未显式设置 | **崩进程** | **崩进程** | **崩进程** |
     | 256KB | JS 异常 | JS 异常 | JS 异常 |
     | **512KB(采用值)** | **正常** | JS 异常 | JS 异常 |
     | 1MB+ | 正常 | **崩进程** | **崩进程** |
  3. 原因是 QuickJS 以**原生栈指针**为基线记账:上限越大允许的 JS 帧越多,
     原生栈越可能在 QuickJS 检查触发前先溢出——那时是进程被杀,没有可捕获的 JS 异常。
- **已落地**:`EvalOptions.stack_limit`(缺省 `DEFAULT_STACK_LIMIT = 512KB`,
  `scripts/runtime.rs`),两处执行入口(`eval_js`/`eval_with_bridge`)均显式设置;
  回归测试 3 条(深递归返 Error 不崩进程、默认上限允许常规递归、显式上限生效)。
- **教训**:资源上限的正确取值靠**实测**确定边界,不能凭「留宽裕些更好」的直觉放大。

## L21 数据库 schema 升级链无事务包裹(**已修复**,2026-09-15;与 L18 一并解决)

- **原现状行为**:`models/db/mod.rs:62` 建表批 + `:65-91` 的 9 个 `migration::ensure_*`
  顺序固定执行,失败即 `Err` 退出(`api/app_state.rs:113-116`)——但**整链无事务包裹**
  (`:76-79`)。中途失败会留下**部分升级态**(部分 `ALTER TABLE ADD COLUMN` 已生效)。
  因每条 `ensure_*` 自身幂等(探测列是否存在再补),重跑可收敛,故不是数据损坏,
  但「失败后库处于何种状态」不可预测,且无版本号可判(见 L18)。
- **判定与处置**:**已修**(2026-09-15),与 L18 合做一次落地。`Db::open` 现在用
  `BEGIN IMMEDIATE … COMMIT` 包住**建表批 + 9 个 `ensure_*`**,任一步失败统一 `ROLLBACK`
  并返回 `Err`;连接**绝不以打开事务的状态返回**(双层闭包写法保证)。
  高代价回填(`ensure_memory_entries_fts_backfill` / `backfill_scope_variables`)留在
  COMMIT 之后,避免长写事务膨胀 WAL(二者靠 `backfill_meta` 幂等)。
- **验收证据**:`tests/schema_migration_meta.rs:273` 的
  `failed_ensure_rolls_back_whole_upgrade_chain_and_keeps_version`——构造令 `ensure_*`
  失败的库(把 `skills` 换成同名视图,`ALTER TABLE … ADD COLUMN` 在视图上必然报错),
  断言:返回 `Err`、`user_version` 仍是旧值、建表批新建表被撤销、失败前已成功的补列被撤销、
  注入对象原样保留、失败后另起连接可**立刻** `BEGIN IMMEDIATE`(证明未停在事务里)。
  变异测试确认:去掉 `BEGIN/COMMIT` 或用例前置提前 `COMMIT` 均使该用例变红。
- **补齐路径**:已完成,见 `docs/数据库版本与降级行为.md`(§一/§三 已按新现实改写)。

## L13 聊天流逐 token 全量重渲染(**已修复**,2026-09-13 核对表 G.1)

- **原现状行为**:后端逐 token 发 `SseEvent::Token`,前端每 token 就地追加
  (`web/src/sseReducer.ts:155-164`),触发 `ChatMessageItem.vue:132` 的 `html` computed
  对**整条增长中的消息**重跑 markdown 渲染 + `v-html` 替换——长回复呈 O(n²)。
  对比:任务侧的流式 delta 经 `DeltaBatcher`(200ms/80 字攒批,
  `server-rs/src/services/task_engine/sink.rs:45`)已做攒批,聊天侧没有对应节流。
- **判定与处置**:**已修**(2026-09-13),但**机制描述于 2026-09-14 修正**(原表述与代码不符,见下)。
  实际落地的是**按帧节流**:内容立即累积(不丢数据),以 rAF(≤16ms)为节拍把累计文本写入
  `paintText`(`ChatMessageItem.vue` 的 `schedulePaint`/`paint`,非旧文所述 `renderTick`),
  `html` computed 只依赖 `paintText` 而不直接依赖 `content`——这是关键,否则 Vue 的依赖追踪
  会让节流失效。**每次刷新仍是对「整段累计文本」跑一次 markdown**,并非「只对已完成块渲染」:
  成本为 O(当前长度) × 帧数,较原「逐 token 全量重渲」的 O(n²)(token 粒度)是实质改善,
  但仍是O(n)每帧而非增量块渲染。
  证据见 `docs/三结合落实核对表.md` G.1 与 `ChatMessageItem.test.ts:137-162`(rAF 节流语义锁定)。
- **保留本条的理由**:作为「性能债曾经存在」的溯源记录,不代表当前仍有该问题。
- **2026-09-14 实测补充**:单次 markdown 渲染本身很便宜(`markdown-it` 渲染 16.5k 字符
  仅 **0.453 ms**,node 实测),故本条残余的「每帧全长重渲」并非当前瓶颈;
  前端真正较重的路径是 `render_html=true` 时的 `sanitize-html`(**3.057 ms** / 16.8k 字符,
  约 markdown 的 7 倍)。详见 `docs/perf-baseline.md` 的「2026-09-14 实测」小节。

## L14 核心渲染组件无测试(**已补**,2026-09-13 核对表 G.2)

- **原现状行为**:44 个 `.vue` 组件中 24 个无测试,含最核心的
  `web/src/components/ChatMessageItem.vue`(markdown/状态栏/swipe/资源卡全在此);
  `ChatWindow.test.ts` 仅 3 个用例(空态存在/不同/输入栏存在)。
- **判定与处置**:**已补**(2026-09-13)。`ChatMessageItem.test.ts` 已落地,
  以 `stores/task.test.ts` 为写法范例。证据见 `docs/三结合落实核对表.md` G.2。
- **仍有空洞的部分**:组件层覆盖面仍不完整(如 `SettingsModal`、`TaskModeSelect`、
  `QuickRepliesModal` 等 14 个组件无测试),该部分归入
  `docs/架构分析-2026-09-14.md §4.10` 的测试空洞清单继续跟踪。

## L15 前端契约文案硬编码分散(**已收敛**,2026-09-13 核对表 G.4)

- **原现状行为**:任务模式与消息 kind 的中文文案手写在
  `web/src/components/TaskBoard.vue:32-39`(`MODE_LABELS`)、`:124-129`
  (`MESSAGE_KIND_LABELS`);弹窗开关在 `App.vue:98`(`MODAL_FLAGS`)、`uiPrefs`、模板三处
  分别罗列。**新增一个任务模式或事件 kind 需改 4 处**。
- **判定与处置**:**已收敛**(2026-09-13)。文案已迁 `web/src/api/labels.ts` 并以
  `satisfies Record<TaskRunMode, string>` 做穷尽校验(漏配即 typecheck 报错);
  弹窗开关已由 `web/src/modals.ts` 单点注册表派生(`App.vue` 与 check-arch 白名单同源)。
  证据见 `docs/三结合落实核对表.md` G.4。

## L16 MCP 服务器死亡后工具不注销,且无热重连(待办)

- **现状行为**:MCP 客户端仅在**启动时**装配(`server-rs/src/mcp/mod.rs:1-5`,
  改设置需重启生效、无热重连);服务器进程中途退出时,其工具**不从 `ToolRegistry` 注销**,
  注册表内这些工具会持续报错直到进程重启。另 `client.rs:252` 的 `read_line` 对服务器
  单行输出无长度上限。
- **判定**:**待办**(韧性)。
- **补齐路径**:EOF 时从注册表注销该服务器全部工具并标记禁用;`read_line` 加最大行长;
  孙进程回收见 L3(Windows Job Object)。

## L17 Windows 产物无 Authenticode 签名,首装触发 SmartScreen 告警(有意裁剪)

- **现状行为**:`dist\kedai-server.exe`、`dist\Kedai-portable\Kedai.exe` 与项目根
  `Kedai.exe` 均无 Authenticode 签名(实测 `Get-AuthenticodeSignature` → `NotSigned`);NSIS 安装包同。
  从网络渠道下载分发时,Windows SmartScreen 弹「Windows 已保护你的电脑」拦截启动。
- **判定**:**有意裁剪**(是否购买代码签名证书属产品决策,当前未购买;自签证书对 SmartScreen 无效)。
- **用户指引**:操作步骤与产物来路(构建指纹/哈希/APK 签名)自查见
  [代码签名与SmartScreen说明.md](代码签名与SmartScreen说明.md);转正式签名的证书选型与构建改动要点见该文档第四节。
- **生态兼容考量**:正式签名需 OV/EV 证书 + 硬件令牌或云 HSM 保管私钥;OV 仍需 SmartScreen 信誉积累,非即时消除告警。

## L18 数据库无 schema 版本号,降级无官方路径(**已修复**,2026-09-15)

- **原现状行为**:`kedai.db` 的 `PRAGMA user_version` 恒为 0,无迁移台账(`backfill_meta` 只存
  FTS 回填标记与 message 回填游标)。升级靠启动时 `CREATE TABLE IF NOT EXISTS` + `ensure_*`
  幂等补列/补表/补索引(`server-rs/src/models/db/mod.rs`),schema 至今只增不减。
  因此旧版 exe 打开新版库**不会报错、静默运行**;降级只能靠用户升级前的手工备份,
  没有 down 迁移,升级前也不自动备份。
- **判定与处置**:**已修**(2026-09-15)。`models/db/schema.rs` 新增
  `pub const SCHEMA_VERSION: i32 = 1`(0 留给「从未标记」的旧库,与 SQLite 默认值天然区分);
  `Db::open` 在建表批**之前**读 `PRAGMA user_version`,库版本高于应用 → 返回 `Err`
  (文案含两个版本号与处置建议),全部升级成功后才**最后**写 `user_version`。
- **验收证据**:`tests/schema_migration_meta.rs` 的 7 个测试对应
  [数据库版本与降级行为.md](数据库版本与降级行为.md) §4.3 的 7 条断言——
  全新库版本号正确、旧库升级后版本 + 结构指纹一致、**伪造更高版本被拒绝且在建表批前返回**、
  两次 open 幂等、`ensure_*` 失败整体回滚且版本未抬、合并产物版本对齐、`ddl.rs` 与
  `schema.rs` 文本 normalize 后一致。变异测试(把版本号提前写、放宽降级门)均使对应用例变红。
- **兼容性变更登记**:「库比代码新时拒绝启动」是**有意**的变更(把静默继续改为启动点显式失败,
  文案可直接读出两个版本);副作用是测试版/便携版共用库且一端为旧 exe 时旧端打不开
  ——属期望的更早暴露。详见 `docs/工程韧性修复-变更说明.md` §3.1。
- **仍有意不做**:down 迁移(回滚仍依赖用户备份,本轮只把「库比代码新」从静默变显式)。

---

> 新增缺口时按同一模板登记:现状行为(带文件:行号)/ 判定(有意裁剪 or 待办)/
> 补齐路径 / 生态兼容考量。已补齐的项从本文删除并在对应章节留一行迁移注记。
>
> **2026-09-13 已封堵项**(从缺口转为已修,留此索引供溯源):
> - EJS 自研解释器的循环无上限与解析无深度守卫(原可被单张角色卡触发挂死/栈溢出):
>   已在 `parsing/assistant/ejs/exec.rs` 加迭代步数(20 万)+ 墙钟(2 秒)预算、
>   在 `parser.rs` 加三处递归深度守卫(上限 64),并冻结该引擎不再扩展新能力
>   (新模板能力一律走 `scripts/runtime.rs` 的 rquickjs 沙箱)。见 `MAINTENANCE.md §0`。

## L22 同类工具不同参数的空转不被熔断(有意裁剪,2026-09-14 实测发现)

- **现状行为**:`utils/loop_guard.rs` 的重复调用熔断按「工具名 + **完整参数**」指纹判定
  (窗口 8 轮内同一指纹 ≥3 次)。因此它能抓住「同一工具同一参数反复调用」这类真死循环,
  但**抓不住「同一工具、参数每次略变」的空转**——例如模型连续 54 轮调用 `bash`,
  每次命令都不同(`ls` → `cat a` → `cat b` → …),指纹各不相同,熔断不触发,
  只能靠 `max_tool_rounds`(默认 32,可配至 200)兜底。
- **2026-09-14 实测数据**:一次 solo 任务在该形态下跑满 54 轮、耗时 300 秒、
  累计 prompt 137 万 token(单轮受裁剪约束稳定在 4k–13k,**上下文已收敛**,
  与 L23 的裁剪失效是两回事),最终正常完成。
- **判定**:**有意裁剪**。收窄到「同工具名 >N 次即熔断」会误杀合法工作
  (真实任务确实可能需要连续执行几十条不同命令)。要可靠区分「空转」与
  「有进展的多步工作」需要判断**任务状态是否推进**(如结果是否变化、目标是否更近),
  这属于语义级判定,误杀代价高于收益。
- **若要补齐**:可考虑「同工具连续调用 >N 轮 且 该工具输出连续 K 次无实质变化」的双条件;
  或对 `bash`/`read` 这类只读工具引入「重复读同一目标」的语义去重。
  接口层 `LoopGuard` 已支持自定义窗口/阈值(`LoopGuard::new(window, threshold)`),
  扩展无需改动熔断算法本体。
- **务实的缓解手段**:调低 `max_tool_rounds`(设置页可配,默认 32)是当前最直接的
  成本上限控制;perf 门禁(`tools/bench/task-probe.mjs` 的「上下文收敛性」一栏)
  会提示 prompt 增长是否异常。

## L23 工具参数曾不参与 token 计数与裁剪(**已修复**,2026-09-14)

- **原现状行为**(两处缺陷叠加):
  1. `services/token_service.rs` 的 `count_message_tokens` 只累加 `content`,
     **不计数** `tool_calls[].arguments`;而连接器会把 arguments 原样序列化进请求
     (`connectors/openai_compatible/mod.rs` 的 `to_openai_messages`)——它们真实占用 prompt。
  2. `agents/engine/messages/trim.rs` 的 `summarize_round` 只摘要 tool 消息的 `content`
     与 reasoning,**不动 `tool_calls[].arguments`**;而每轮都把完整调用塞回历史。
  叠加后果:预算闸门永远判定「未超预算」→ 工具历史裁剪实质失效。
- **实测影响**(真实角色卡 + 真实上游):plan 模式任务 prompt 逐步骤
  `9,762 → 28,784 → 576,312 → 885,378`(**90 倍**),7 分钟不收敛、需人工 stop,
  累计 150 万 prompt token。裁剪确实触发了 72 次却依然失控——因为它裁的不是大头。
  **影响面超出任务模式**:`count_message_tokens` 同时被 `trim_to_context` 使用,
  即聊天主链路的上下文窗口裁剪也同期失效。
- **判定与处置**:**已修**(2026-09-14)。① 计数计入 `tool_calls` 的 name/arguments
  (新增 `count_single_message_tokens` 统一口径,`trim_to_context` 同步改用);
  ② `trim.rs` 新增 `reclaim_round_arguments`,把旧轮参数替换为**合法 JSON**占位
  (`{"_trimmed":true,"chars":N}`,严格后端会对 arguments 做 JSON 解析,非法即 400),
  保留 `id`/`name` 以维持 assistant↔tool 配对;③ `trim_tool_history` 返回
  `ToolHistoryTrimOutcome` 并上报「未能收敛」,由调用方记 warn(不再静默)。
- **修复后实测**:同一 plan 任务 **48 秒正常完成**,累计 prompt **54,376**
  (较修复前 **↓96.4%**);单轮 prompt 稳定在 4k–13k,**不再无界增长**。
  证据见 `docs/perf-baseline.md` 的「2026-09-14 实测」小节与 `trim.rs` 的 4 条单测。

## L24 无 metrics / OTLP 导出,无 `/metrics` 端点(有意不做,2026-09-15)

- **现状行为**:可观测性只做到「结构化日志 + requestId + 访问日志 + health 依赖探测」
  (见 `docs/工程韧性修复-变更说明.md` §2.2)。**没有** metrics 计数/直方图、没有
  Prometheus/OpenTelemetry 导出、没有 `/metrics` 端点。
- **判定**:**有意不做**(2026-09-15)。本应用是**本地单用户**形态,没有集中采集端与
  跨实例聚合需求;引入 metrics 栈(新依赖 + 指标口径设计 + 生命周期管理)的收益
  需独立评估,不应挂在「可观测性最小骨架」批次里顺手加。
- **补齐路径**:若将来出现「需要计时序指标做容量规划」的真实需求,优先考虑
  从既有结构化日志派生(访问日志已含 `status`/`duration_ms`,可离线聚合),
  再评估是否值得引入 exporter crate。**登记此项是为避免后续误判为遗漏**。

## L25 无 down 迁移(有意不做,2026-09-15)

- **现状行为**:L18 修复后新增了 `SCHEMA_VERSION` 与「库比代码新则拒绝启动」,但
  **只做单向**:没有降级脚本、没有升级前自动备份。回滚仍依赖用户手工备份。
- **判定**:**有意不做**(2026-09-15)。本轮目标是把「库比代码新」从**静默**变**显式**
  (启动即失败并给出两个版本号),而非提供回滚能力。down 迁移的成本随每次
  schema 演进累积(schema 至今只增不减,写降级脚本意味着长期维护两条路径)。
- **补齐路径**:若出现「必须让新版库退回旧版 exe 运行」的真实场景,优先做
  **升级前自动备份**(成本低、覆盖所有降级路径),而非逐版本 down 迁移。

## L26 事件流无持久化与回放(有意不做,2026-09-15)

- **现状行为**:任务事件经 `broadcast` 通道转发为 SSE,接收端滞后(`Lagged`)时
  **丢弃积压事件**——本轮只把该情况从 `debug` 提为 `warn` 并记录 `skipped` 条数
  (L21 同批次的 4.2),**没有**改为事件持久化 / 断线续传。
- **判定**:**有意不做**(2026-09-15),属**设计取舍**而非缺口。前端已有补偿设计:
  `web/src/stores/task.ts` 的 settle 全量刷新 + 5s 兜底轮询——事件是「提示刷新」的信号,
  不是唯一事实源,**丢了也能靠 REST 收敛**。事件持久化会引入事件表、保留策略、
  回放游标三套新机制,与本应用的规模不匹配。
- **补齐路径**:若将来出现「必须逐条审计任务事件」的需求,应先评估是否直接查
  `task_messages` / `task_llm_calls` 落库记录(它们已是持久事实),而不是新建事件流存储。

## L27 413(请求体过大)未统一为 JSON 错误体(有意保留例外,2026-09-15)

- **现状行为**:批次 1 把畸形 JSON(400)、方法不允许(405)、未知路径(404)统一为
  `{error, code}` JSON,**唯独 413 仍是 axum 内建的 `text/plain`**。
- **判定**:**有意保留例外**(2026-09-15)。根因:`DefaultBodyLimit`(35MB 层)在
  **提取器被调用前**就由 tower 中间件拒绝,产生 `LengthLimitError`,自研的
  `JsonBody<T>` 的 `FromRequest` 根本不执行——要统一 413 形状必须自定义 tower 层
  拦截 body `Frame` 错误流,改动面覆盖**全站请求体**。
- **权衡**:收益低(体积超限本就罕见,且前端上传前已做体积校验,用户几乎撞不到),
  成本高(动全站 body 流,回归面大)。故登记为已知例外。
- **补齐路径**:若将来 413 真的成为用户可见问题,可在 `DefaultBodyLimit` 之外包一层
  自定义 `Layer`,把 `LengthLimitError` 映射为 `{error, code}` + 413。原因写在
  `server-rs/src/api/json_body.rs` 头部注释。
