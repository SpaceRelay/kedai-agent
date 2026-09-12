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

> 新增缺口时按同一模板登记:现状行为(带文件:行号)/ 判定(有意裁剪 or 待办)/
> 补齐路径 / 生态兼容考量。已补齐的项从本文删除并在对应章节留一行迁移注记。
