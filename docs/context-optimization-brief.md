# Kedai 上下文机制优化实施简报

> ⚠️ **已归档,仅供溯源**:本文为 2026-08-25 的交接简报,其中任务模式「REST + 前端 1 秒轮询、不走 SSE」的描述**已被 WP4/WP5 取代**——现行实现为 SSE 事件驱动(`GET /api/tasks/events`,断线 5s 兜底轮询),见 [任务引擎六模式.md](任务引擎六模式.md) 与 `web/src/stores/task.ts`。文中行号与测试计数为当时实测值,不随代码更新。
>
> **本文件用途**:交给 Kimi(网页版)作为完整实施依据。文件自包含——包含项目背景、现状核实(带 `文件:行号` 证据)、痛点清单、外部借鉴机制、分阶段任务卡、约束与验收标准。执行时无需再做全面调查,按阶段 0→4 顺序施工即可。
>
> 生成日期:2026-08-25。基线:`cargo test` 219 绿(175 单测 + 44 集成),`vitest` 62 绿。

---

## 0. 如何使用本文件

1. 按 **阶段 0 → 1 → 2 → 3 → 4** 顺序执行;阶段 3 的每个机制级任务卡相互独立,任一可跳过或单独回退,不影响其他卡。
2. 每个任务卡包含:目标、改动点(文件+函数级)、默认参数、测试要求、验收标准。**每完成一个任务卡就跑该卡要求的测试**,不要攒到最后。
3. 所有代码注释、面向用户的文本用**简体中文**;标识符/API 名保留英文。
4. 遇到与本简报描述不符的代码(行号漂移、函数已重构),以实际代码为准,先理解再改,不要强行套用。

---

## 1. 项目速览

- **项目根**:`C:\Users\LENOVO\Desktop\kedai`
- **技术栈**:Rust(axum)后端 `server-rs/`(crate 名 `kedai-server`,lib 供 Tauri 复用)+ Vue3 前端 `web/`(构建产物内嵌进服务端)+ Tauri 2 桌面壳 `src-tauri/`(加载 `http://127.0.0.1:3001`)。
- **业务**:角色扮演/对话型 agent,含世界书(worldbook)、EJS 模板、mvu 变量系统(酒馆助手 MagVarUpdate 兼容)、反思(reflector)、任务工作台(task mode)、agentgo 子智能体。
- **启动/构建**:`.\start.ps1`(桌面优先,回退浏览器);`.\build.ps1`(`-Tauri` 追加桌面打包)。**改前端后必须重建**:`npm run build -w web`。
- **测试**:`cd server-rs && cargo test`(基线 219);`npm test -w web`(基线 62)。
- **硬约束**:端口 3001 是默认契约;`data/` 勿删;PowerShell 脚本必须带 UTF-8 BOM;**反思路径的任何修改必须保持有界**(历史教训:回退条件恒不成立导致 1ms 间隔 reflect 洪流,详见 §2.4)。

### 关键文件地图

| 路径 | 职责 |
|---|---|
| `server-rs/src/agents/engine/mod.rs` | AgentEngine 编排入口:规划→上下文收集→消息构建→步骤循环→落库 |
| `server-rs/src/agents/engine/messages/build.rs` | 6 层位置消息拼装、with_step_prompt |
| `server-rs/src/agents/engine/messages/inject.rs` | 摘要槽/记忆槽/反思建议注入 |
| `server-rs/src/agents/engine/messages/trim.rs` | trim_to_context 按 token 预算裁剪 |
| `server-rs/src/agents/engine/compaction.rs` | snip 零成本裁剪档 + LLM 增量摘要 |
| `server-rs/src/agents/engine/run_loop.rs` | 步骤循环、反思重试硬上限 |
| `server-rs/src/agents/engine/executor.rs` | run_tool_loop function calling 循环 |
| `server-rs/src/agents/engine/worldbook.rs` | 世界书触发/注入、mvu 状态块 |
| `server-rs/src/agents/engine/mvu.rs` | 变量补丁应用、两步生成 |
| `server-rs/src/agents/engine/reflector_integration.rs` | LLM 反思判定、反思建议生成 |
| `server-rs/src/services/task_service.rs` | **任务模式全部逻辑(本次优化主战场)** |
| `server-rs/src/services/session_service.rs` | 会话/消息持久化(SQLite) |
| `server-rs/src/services/settings_service.rs` | 设置项与 AppMode 覆盖层 |
| `server-rs/src/services/token_service.rs` | tiktoken-rs 计数(降级字符估算) |
| `server-rs/src/tools/agent_tools_agent.rs` | agentgo 子智能体工具 |
| `server-rs/src/models/db/schema.rs` | SQLite 表结构 |
| `web/src/stores/task.ts` | 任务前端 store(1 秒轮询) |
| `web/src/sseReducer.ts` | SSE 事件归并 |

---

## 2. 上下文机制现状(已逐行核实)

### 2.1 角色扮演链路(成熟,非本次重点)

**消息拼装 = 6 层位置模型**(`messages/build.rs:25-39` 注释 + `build_llm_messages_with_position` build.rs:90):

- 位置5:system 主提示词 + `AGENTS_RUNTIME.md` 前置(mod.rs:1569-1587)
- 位置4:提示词注入(简单合成/复杂楼层,build.rs:215-251)
- 位置3:角色卡 + 世界书常驻(角色卡字段包 `<UNTRUSTED_PROMPT_SOURCE>` 边界)
- 位置2:历史(经 `project_history` 投影)
- 位置1:世界书激发条目,钉在**最新 user 消息尾部**(缓存友好,build.rs:274-354)
- 位置0:反思建议 + 预设尾部提示词,同样钉在最新 user 尾部

**多级裁剪管线**:
1. `trim_to_context`(trim.rs:14-65):超 `max_context_tokens` 从最旧丢弃;受保护头部 = 首条 system + 摘要槽 + 记忆槽(`protected_head_len` trim.rs:70-80);极端情况截断 system 但保头保尾。
2. **snip 零成本裁剪**(compaction.rs:144-186):历史达窗口 0.6(`SNIP_THRESHOLD` compaction.rs:18)时,超 `compaction_snip_bytes`(默认 8192)的陈旧消息替换占位符;尾部 2 条与含 error/exception/failed/panic 的不裁。
3. **LLM 增量摘要**(compaction.rs:111-140):历史达窗口 0.8(`compaction_threshold` 默认 0.8,settings_service.rs:231-234)触发;旧摘要字节冻结只摘新段;原文永不删除,摘要存 `session_compactions` 表,可逆。

**token 计数**:`services/token_service.rs:1-96`,tiktoken-rs(cl100k/o200k),词表加载失败降级为「英文 4 字符/中文 1 字符」估算;`count_message_tokens` 每条 +4 开销。预算钳制 `MAX_CONTEXT_TOKENS = 1_048_576`(api/chat.rs:183-184)。

### 2.2 任务模式(**完全无界,本次优化主战场**)

实现独立于 AgentEngine,在 `services/task_service.rs`,走 REST + 前端 1 秒轮询(`web/src/stores/task.ts:142-158`),不走 SSE。

生命周期:`create`(199)→ `run`(277,tokio::spawn 后台)→ `run_task_background`(730):

1. **规划 `plan_task`**(536-562):system = 固定规划词「拆 2~5 步…严格只输出 JSON 数组」+ 世界书常驻全量拼接;user = 任务标题;`max_tokens=1024, temperature=0.3`。**不带历史、不带提示词注入;JSON 解析失败不重试,直接 Err**(557-560)。
2. **逐步执行 `generate_step`**(566-619):system = 固定执行者词 + 角色人设(`persona_style` 677-694)+ 世界书常驻全量 + 提示词注入 + `agent_system_prompt` 渲染(`render_agent_prompt` 161-195);user = 该步 goal。空内容重试一次(780-790)。
3. **汇总 `summarize_task`**(623-671):user = 「用户目标 + 各步骤结果」**全量拼接**(655-658),无任何截断。

**无界证据(逐行核实)**:
- `world_context`(114-131):过滤 `enabled && constant` 后 `texts.join("\n\n")` 全量拼接,无预算。
- `summarize_task` 循环拼接 `s.result` 全文(655-658),无预算。
- 全链路**无 token 计数、无 trim、无压缩、无 snip**——角色扮演链路的多级管线在任务模式完全缺位。
- 护栏仅有:`TASK_LLM_TOTAL_TIMEOUT = 300s` 单次调用看门狗(task_service.rs:30, 494-510);取消经 watch channel + run token(76-82, 421-455)。

**上下文隔离设计(保留,不改)**:任务模式无对话历史(`inject_text` 注释 134 明确「楼层 before/after/depth 语义不适用」)、无 mvu、无反思;设置经 `for_mode(AppMode::Task)` 覆盖层,`agent_system_prompt` 不回退 roleplay 值防污染(settings_service.rs:325-, 344-348)。这个「隔离但无界」的形态正是优化对象:**保持隔离,补上预算**。

> 更正(2026-09):原文称「任务模式无工具循环」已过时。批次 4 起 solo / multi / team 主 agent / plan 续跑 / custom 步骤均有工具循环;任务模式工具集由 `task_tool_policy`(all / deny_dangerous / allowlist)决定,且恒不等待授权(名单外直接拒绝,见 docs/任务引擎六模式.md)。仅 legacy 执行步无工具。

### 2.3 持久化与加载

全部 SQLite(`models/db/schema.rs`):`messages`(22)、`session_compactions`(175)、`llm_requests`(184)、`tasks`/`task_subtasks`(89/101)、`session_assistant_vars`(120)、`scope_variables`(128)等。

**痛点**:`SessionService::get_messages`(session_service.rs:187-200)`SELECT … WHERE session_id ORDER BY id ASC` **无 LIMIT,每轮 run 全量读出全历史**;且同一轮被读两次(`maybe_compact` mod.rs:1033-1044 一次,`collect_context` mod.rs:1134-1147 一次),每次还对全历史逐条 `count_tokens` 求和(mod.rs:1048-1055 与 1162-1170)。压缩只影响模型可见投影,DB 原文不动;EJS 渲染与世界书扫描仍读完整历史。`llm_requests` 快照每 run 结束裁剪保留 50 条(mod.rs:927-929)。

### 2.4 反思机制(已有界,勿破坏)

- 机械规则(reflector.rs:36-111)+ LLM 判定(`reflect_with_llm` reflector_integration.rs:13-71;带工具版 `reflect_with_tools` 78-196,最多 3 轮)。
- 失败放弃时自动生成 ≤200 token 建议(`REFLECT_ADVICE_MAX_TOKENS` reflector_integration.rs:6;`generate_reflect_advice` 204-277),由 `inject_reflect_advice`(inject.rs:16-91)插入位置0。
- **有界化三处(历史 bug 修复,回归测试已固化,改动时必须保持)**:
  - `retreat_to_generating_step`(build.rs:367-381):回退锚点必须 `action=="direct" && generates!=Some(false)`;build.rs:669/734 有针对旧无限循环的回归测试。
  - `reflect_retries` 独立硬上限(run_loop.rs:48-54, 273):deep/agent/custom=3;放弃条件 `retry_action == Some("stop") || reflect_retries >= max_attempts`。
  - 回退时 `rctx.llm_messages.truncate(base_len)`(run_loop.rs:366)丢弃中间消息。

### 2.5 mvu 变量系统

- `<UpdateVariable>` JSONPatch → `apply_mvu_patches`(mvu.rs:7-53)→ 落库 → SSE `Vars` 事件。
- 状态进上下文:变量树非空时注入「[当前状态]+[状态更新协议]」状态块(`make_state_block_with_contract` worldbook.rs:219-251,整树 `to_json`),位置由 `mvu_vars_position`(system / user_tail,默认 system,mod.rs:1402-1431);有契约时按 dueFields 裁剪只暴露到期字段。
- 两步生成:正文后追加 `generate_mvu_status`(mvu.rs:245-457),输入仅精简状态+正文(不含历史),独立温度 `mvu_temperature` 默认 0.3。

### 2.6 世界书与 EJS

- 触发(`collect_world_text_grouped_with` worldbook.rs:56-166):constant 恒注入;非 constant 按 keys/副关键词/正则命中,扫描窗口 = 最近 `depth` 条**用户消息**(缺省 4,0=全部);`use_probability` 随机;@@if/unless/var/set 装饰器。**排序 position→order→id 保证字节稳定(缓存友好)**。
- EJS:自研解析器 `parsing/assistant/ejs/`,递归求值深度上限 `MAX_EVAL_TEMPLATE_DEPTH = 16`(ejs/exec.rs:26)。

### 2.7 痛点清单汇总(带证据)

| # | 痛点 | 证据 | 严重度 |
|---|---|---|---|
| P0-1 | 任务模式 summarize 全量拼接步骤结果,无界 | task_service.rs:655-658 | 高 |
| P0-2 | 任务模式 world_context 全量拼接常驻条目,无界 | task_service.rs:114-131 | 高 |
| P0-3 | plan_task JSON 解析失败不重试 | task_service.rs:557-560 | 中 |
| P0-4 | 任务模式全链路无 token 计数/预算/裁剪 | task_service.rs 全文 | 高 |
| P1-1 | 历史全量加载无 LIMIT,每轮 DB 读两次、token 计数两遍 | session_service.rs:187-200;mod.rs:1033,1134,1048-1055,1162-1170 | 中 |
| P1-2 | trim_to_context token/char 单位混用:`(budget*0.8) as usize` 把 token 预算直接当字符数 | trim.rs:39-41 | 中 |
| P1-3 | 工具循环内不再裁剪;tool 输出 `e.output.to_string()` 整体回填无截断,32 轮 push 可单轮突破 max_context_tokens | executor.rs:338-344, 488-494;trim 只在构建期跑一次 mod.rs:1637-1646 | 高 |
| P1-4 | 步骤级消息视图每步全量 clone | run_loop.rs:445;build.rs:391/396 | 低 |
| P1-5 | 反思建议同 run 内只增不减,可叠加多条 | inject.rs:16-91 | 低 |
| P1-6 | reflect_with_tools 工具结果无截断,revise_passage 全文回填 | reflector_integration.rs:177-192 | 低 |
| P1-7 | session_compactions 旧行永久保留,无 GC | compaction.rs:312-359 测试固化可回溯语义 | 低 |
| P1-8 | 硬编码常量分散:snip 0.6、压缩 0.8、system 截断 80%、反思 200 token/300 字符、任务 300s、agentgo 5 个、llm_requests 50 条 | compaction.rs:18;trim.rs:39;reflector_integration.rs:6,275;task_service.rs:30;agent_tools_agent.rs:58;mod.rs:927 | 低 |

---

## 3. 外部机制借鉴(已调研,直接引用即可)

### 3.1 本机其他 agent 部署的机制

**dsh(DeepSeek,`@deepseek-ai/dsh`)**:
- **压缩前先免模型剪枝**:触发 LLM 摘要前先跑 toolResultPruner(超 8192 字符的工具结果改写为头 4096 + 省略标记 + 尾 1024),重新计量后压力已解除则**跳过摘要**。「Below-pressure step checks never prune」。
- **摘要调用复用 KV 缓存**:摘要请求**原样回放会话自己的系统提示+消息**,把压缩指令作为最后一条 user 消息追加——命中 provider 暖缓存而非使其失效;摘要必须比原文短否则拒绝,重试仍不达标则抛错。
- **replay-safe 改写**:原始事件保留在 append-only 日志,改写是新增的 `surfaceOp: replace` 记录;幂等(二次扫描不产生新改写)。

**opencode dcp 插件(`@tarquinen/opencode-dcp`)**:
- **发送前占位符替换,历史不可变**:挂在 `messages.transform` 钩子,每次发请求前把被裁内容替换为占位符(「[Output removed to save context]」),会话存储从不修改——可逆、可重算、审计友好。
- **dedup 签名去重**:签名 = `tool名::排序键后的JSON参数`;同签名只保留最近一次输出,旧的标记裁剪;重算时机绑在 compress 运行时,缓存只断一次。
- **purge-errors**:出错工具调用超过 N 轮(默认 4)后剪掉大输入,**保留错误消息本身**。
- **compress 作为模型可调用工具**:主模型自己写摘要(零额外 API 费用),按 `m0001` 短别名引用区间;嵌套摘要防稀释。实测缓存命中率 ≈85%(带 DCP)vs 90%(不带)——用少量缓存换 token 节省是合算交易。

**Reasonix**:四级水位 `soft 0.5(只提醒)/ snip 0.6(先剪工具结果再考虑摘要)/ compact 0.8 / force 0.9`;`cold_resume_prune` = **重开已越过 provider 缓存窗口的会话时才剪陈旧工具结果**(此时缓存已过期,裁剪零损失);`keep = ["errors"]` 压缩保留错误。

**ZCode context-compressor**:水位判定用 rollout 最后一条 `response.usage` **精确值**(非估算);`--slim` 输入侧瘦身不动主会话;冷却守卫(距最后请求 <6h 禁止产物替换活跃会话输入);SessionStart hook 无事时**零输出**保前缀逐字节稳定。

**Codex CLI**:极简指令覆盖而非追加(系统提示最短化);skills 黑名单;结构化状态进 sqlite、对话流水进 jsonl。

### 3.2 业界 2025-2026 上下文工程要点

1. **前缀绝对稳定 + append-only**(Manus:「KV-cache 命中率是生产级 agent 最重要的单一指标」;DeepSeek context caching 前缀完全匹配才命中):system/角色卡/常驻世界书不写任何每轮变化的内容(时间戳、token 计数);动态状态注入到消息尾部;JSON 序列化必须确定性(key 顺序不稳会静默击穿缓存)。
2. **tool result clearing 是最安全的压缩形式**(Anthropic):发送前清理旧工具结果/调用,按前缀顺序清理以保缓存——与 kedai 现有 snip 同构,可放心扩展。
3. **可恢复压缩**(Manus):压缩不丢「找回入口」——占位符应带找回线索(条目 id / 消息 id / 变量路径)。
4. **两级水位**(MemGPT):~70% 发「记忆压力」信号让模型自主抢救信息进 core memory;~100% 逐出 ~50% 折进递归摘要。
5. **Context rot**(Chroma 2025,18 模型复测):性能随输入变长普遍退化;~300 token 聚焦上下文大幅优于 ~113k 全量历史。**工程结论:宁可触发压缩也不撑满窗口;「少而准」优先于「摆位置」**。
6. **SillyTavern World Info 预算语义**(与本项目场景最接近的成熟实现):token budget 支持 Context% 或固定值;预算耗尽即停止激活;优先级 **常驻 > 高 order > 直接命中 > 递归命中**;@Depth 定点插入对抗近因衰减。
7. **子智能体蒸馏**(Anthropic 多智能体研究系统):子 agent 跑独立上下文,可烧几万 token 但只回 1-2k token 蒸馏摘要;任务计划外置存储再注入指针。注意成本:multi-agent ≈ 15× token,只在高价值任务启用。
8. **TOON(Token-Oriented Object Notation)**:同构对象数组用「表头声明 + CSV 行」渲染,相对格式化 JSON 省 ~42.6% token 且结构校验更强;深嵌套/非均匀数据仍用 compact JSON。
9. **遮蔽而非移除**(Manus):动态增删工具定义会破坏缓存并使历史引用失效——工具常驻前缀,用状态机控制可用性;失败调用与错误留在上下文里让模型自我修正。

---

## 4. 实施任务卡

### 阶段 0:准备(无代码)

- [ ] 通读本简报;`cd server-rs && cargo test` 确认 219 绿基线;`npm test -w web` 确认 62 绿基线。

### 阶段 1(P0):任务模式上下文有界化

> 主战场:`server-rs/src/services/task_service.rs` + `server-rs/src/services/settings_service.rs`。原则:**保持任务模式的上下文隔离设计,只补预算与降级链**。

#### 任务卡 1.1:任务模式 token 预算设置项

- **改动**:`settings_service.rs` 的 AppMode::Task 覆盖层新增:
  - `task_context_budget`:u32,默认 `32768`(任务模式单次 LLM 调用的上下文 token 总预算)
  - `task_step_result_max_chars`:usize,默认 `8000`(单步结果进入汇总前的字符上限)
  - `task_worldbook_budget`:u32,默认 `8192`(世界书常驻条目注入预算,token)
- **验收**:设置可读写、有默认值、不回退 roleplay 同名项(延续防污染语义 344-348)。

#### 任务卡 1.2:world_context 预算化填充

- **改动**:`world_context`(task_service.rs:114-131)增加预算参数;常驻条目按现有 `(position, order, id)` 排序后**贪心填入**,用 `TokenService` 计数;预算耗尽停止,尾部追加 `\n(另有 N 条设定因预算省略)`。
- **测试**:超预算时条数截断且尾注正确;预算充足时输出与原逻辑逐字节一致(黄金测试);空世界书仍返回空串。

#### 任务卡 1.3:summarize_task 防膨胀降级链

- **改动**:`summarize_task`(623-671)拼接步骤结果前:
  1. 单步结果超 `task_step_result_max_chars` → 头 70% + `\n[中间省略]\n` + 尾 30% 截断(头尾保留式,dsh pruner 思路);
  2. 拼接后 user 整体超 `task_context_budget` 的 user 份额(建议 system 占 1/3、user 占 2/3,用 TokenService 实测)→ **滚动两两合并**:相邻步骤结果两两调一次廉价摘要(复用现有 compaction 的摘要调用,低温、max_tokens 1024)合并为一段,直到落入预算;摘要失败则退化回更激进截断,**绝不允许超预算发送**。
- **测试**:小结果不触发任何降级(输出与现状一致);大结果先截断;超大结果触发合并;合并失败降级截断;预算硬顶有断言。

#### 任务卡 1.4:plan_task 解析重试

- **改动**:`plan_task`(536-562)`parse_plan` 失败时,把错误信息作为 user 追加重试(「上次输出不是合法 JSON: {err}。请重新只输出 JSON 数组」),最多重试 2 次;仍失败才返回 Err。
- **测试**:mock 连接器第一次返回垃圾、第二次返回合法 JSON → 成功;三次都垃圾 → Err 且恰好转 3 次。

#### 任务卡 1.5:任务模式统一预算守卫

- **改动**:plan/step/summarize 三处组装完 messages 后、发送前统一过一道 `enforce_task_budget(&mut messages, budget, token_service)`:超预算时先截 system 尾部注入段(注入可再省),再截 user 中段,保头保尾;记录 warn 日志。
- **验收**:三个入口任一构造超预算输入,实际发出请求的 token 数 ≤ budget。

### 阶段 2(P1):角色扮演链路痛点修复

#### 任务卡 2.1:历史加载去重 + 增量 token 计数

- **改动**:
  - `AgentEngine::run` 每轮只调用一次 `get_messages`,结果共享给 `maybe_compact` 与 `collect_context`(消除 mod.rs:1033 与 1134 的重复查询)。
  - 消息 token 计数按消息 id 缓存(HashMap 或 messages 表加 `token_count` 列,二选一,取改动小的);每轮只对**新增消息**计数,全量总和增量维护(消除 1048-1055 与 1162-1170 的双重逐条计数)。
- **验收**:行为不变(既有 219 测试全绿);加一个计数调用次数的 mock 断言测试,证明每轮 DB 读 1 次、每条历史消息只计数 1 次。

#### 任务卡 2.2:trim 单位统一

- **改动**:`trim_to_context`(trim.rs:39)`(budget*0.8) as usize` 的 token→字符直接转换,改为通过 `TokenService` 估算(逐段二分或按模型 chars/token 比率换算后留 10% 余量再实测微调)。`protected_tail` 参数语义保持字符数不变,但换算点加注释说明单位边界。
- **测试**:既有 trim 黄金测试全绿;新增「截断后实测 token ≤ budget」断言。

#### 任务卡 2.3:工具循环内再裁剪 + 工具输出截断

- **改动**:
  - `run_tool_loop`(executor.rs:292-)每轮 push assistant/tool 消息后:总量超 `max_context_tokens` 即重跑 snip 占位符替换 + trim(复用既有函数,不新写逻辑)。
  - 新增设置 `tool_output_max_chars`(默认 16000):tool 输出回填前头尾截断(头 70%/尾 30%,中部插 `[输出过长,中间 N 字符已省略]`)。
  - 保底:即使裁剪后仍超,循环退出时返回明确错误而非裸超发。
- **测试**:构造 32 轮大输出工具循环,断言任意一轮发出请求 ≤ 预算;截断标记存在;既有工具循环测试全绿。

#### 任务卡 2.4:反思建议去重 + 反思工具结果截断

- **改动**:`inject_reflect_advice`(inject.rs:16-91)同 run 内新建议**替换**旧建议(按注入槽位识别,而非追加);`reflect_with_tools`(reflector_integration.rs:177-192)工具结果回填前截断(复用 2.3 的截断函数)。
- **测试**:连续 3 次反思失败后上下文内只有 1 条建议;反思重试硬上限既有测试(run_loop.rs 相关)必须保持全绿。

#### 任务卡 2.5:步骤视图去全量克隆

- **改动**:`run_loop.rs:445` 的 `rctx.llm_messages.clone()` 与 `with_step_prompt`(build.rs:391/396)的 `to_vec()`,改为借用 + 尾部临时 push/truncate 复原,或索引视图;保持行为不变。
- **验收**:既有测试全绿;无新分配热点(clippy 无新告警)。

#### 任务卡 2.6:session_compactions GC

- **改动**:每次写入新压缩行后,删除该 session 超出最近 20 行的旧行;**保留「可回溯」语义的折中**:被删行的摘要内容已冻结进最新行(增量摘要设计保证),注释说明。
- **测试**:写 25 次压缩后表内仅 20 行,且最新行摘要完整。

#### 任务卡 2.7:常量收编(低优先,可最后做)

- **改动**:P1-8 列出的硬编码常量收进 settings(或集中在 `constants.rs` 并加中文注释说明取值依据);有设置项的保留默认一致。

### 阶段 3(机制级,每卡独立可灰度)

> 每卡都是独立增量,做完单独验收;出问题单独回退,不影响阶段 1/2 成果。

#### 任务卡 3.1:压缩前先免模型剪枝(借鉴 dsh)

- **改动**:`maybe_compact`(mod.rs:1014)触发 LLM 摘要前,先执行 snip/剪枝并用 TokenService 重计量;若压力已低于 `compaction_threshold`,**跳过本次 LLM 摘要**并记日志。
- **验收**:构造刚好越阈值的会话,剪枝后回落 → 不发生摘要调用(mock 连接器断言);既有压缩测试全绿。

#### 任务卡 3.2:摘要调用回放前缀(借鉴 dsh)

- **改动**:compaction 的摘要请求组装改为:原样带上会话自己的 system 前缀 + 待摘要消息,压缩指令作为**最后一条 user 消息**追加;摘要比原文短才接受,否则重试(≤2)再降级截断。
- **验收**:摘要请求的消息前缀与同会话常规请求的前缀逐字节一致(测试断言),保证命中 provider 缓存。

#### 任务卡 3.3:发送前投影层增强(借鉴 dcp)

- **改动**:`project_history`(compaction.rs:31-52)扩展两种策略(DB 原文永不动):
  1. **dedup 签名去重**:同 `tool名::规范化参数JSON(键排序、剔null)` 签名的工具结果只保留最近一次,旧的替换占位符;
  2. **purge-errors 老化**:出错工具调用超过 4 轮后剪掉大输入、保留错误消息。
  - 占位符带找回线索:「[已折叠:消息 id=xxx,可用历史查看找回]」(可恢复压缩)。
- **验收**:三种策略各有单测;DB 原文不变有断言;既有投影测试全绿。

#### 任务卡 3.4:四级水位 + 前端可观测性

- **改动**:水位语义对齐 0.5(仅提示)/ 0.6(snip)/ 0.8(摘要)/ 0.9(强制摘要);`finish` 事件(已含 `context_tokens`,mod.rs:1647-1650)增加 `watermark` 档位与 usage 的缓存命中字段(若 provider 返回 `prompt_cache_hit_tokens` 则透传);前端状态栏/Sidebar 显示当前水位与缓存命中率。
- **验收**:0.5 档不产生任何裁剪动作(缓存友好);前端显示随轮次更新;改前端后 `npm run build -w web`。

#### 任务卡 3.5:世界书 token 预算 + 优先级队列(借鉴 SillyTavern)

- **改动**:新增 `worldbook_token_budget`(默认 0 = 不限,保持现状兼容);开启时:命中条目按优先级 **常驻 > order 降序 > 直接命中 > 递归命中** 排队,预算耗尽即停并记尾注;**排序仍须确定性**(同优先级内 position→order→id),保证渲染逐字节稳定。
- **验收**:预算内全量注入与现状一致;超预算按优先级截断;两次相同输入渲染逐字节一致(缓存友好断言)。

#### 任务卡 3.6:agentgo 子智能体蒸馏(借鉴 Anthropic)

- **改动**:`agent_tools_agent.rs` 子任务结果返回主流前,可选经一次蒸馏摘要调用(新增 `subagent_distill` 开关,默认开;蒸馏预算 `subagent_distill_max_tokens` 默认 1536);蒸馏失败回退现有 `subagent_result_max_chars` 截断。
- **验收**:大结果回主流 ≤ 蒸馏预算;失败回退路径有测试;既有并发/深度守卫测试全绿。

#### 任务卡 3.7:mvu 状态块 TOON 化试点

- **改动**:`make_state_block_with_contract`(worldbook.rs:219-251)渲染变量树时,若变量树顶层是同构对象数组,用「表头声明 + CSV 行」表格形渲染(TOON 风格);新增 `mvu_state_format`(json / toon,默认 json 保持兼容);深嵌套/非均匀数据自动回退 compact JSON。
- **验收**:toon 模式 token 数 ≤ json 模式(测试断言);`apply_mvu_patches` 往返一致;契约 dueFields 裁剪在两种格式下等价。

#### 任务卡 3.8:前缀稳定审计(纯核查,可能零代码)

- **改动**:grep 排查 system 前缀拼装路径上是否存在每轮变化内容(时间戳、token 计数、随机数);检查所有进上下文的 JSON 序列化是否确定性(`BTreeMap` / 固定 struct 字段序);发现则在位置 1/0(消息尾部)注入或改用确定性结构。
- **验收**:产出审计结论(中文注释或 docs 追加);若改动,两次相同输入的 system 前缀逐字节一致断言。

---

## 5. 全局约束(红线)

1. **测试基线**:任一阶段结束时 `cargo test` 全绿(基线 219,新增测试另计);动前端则 `vitest` 62 绿 + `npm run build -w web` 重建内嵌 dist。
2. **反思路径必须保持有界**:回退锚点条件、`reflect_retries` 硬上限、`truncate(base_len)` 三处机制不可削弱;相关回归测试(build.rs:669/734 附近、run_loop.rs)必须全绿。
3. **DB 原文不可变**:所有裁剪/投影只影响「发给模型的视图」;`messages` 表原文永不修改、永不删除(唯一例外:任务卡 2.6 的压缩行 GC)。
4. **缓存友好**:世界书/状态块/注入的排序与序列化必须确定性;每轮变化的内容只进消息尾部(位置 0/1),不进 system 前缀。
5. **端口 3001 契约不变;`data/` 不删;PowerShell 脚本 UTF-8 BOM;代码注释与面向用户文本用简体中文。**
6. 任务模式的**上下文隔离设计保留**:不引入对话历史/mvu/反思/工具循环,只加预算与降级。

## 6. 总验收清单

- [ ] 阶段 1:任务模式三次 LLM 调用全部有 token 硬顶;plan 失败可重试;大步骤结果不撑爆汇总。
- [ ] 阶段 2:每轮 DB 读 1 次、token 增量计数;工具循环 32 轮不突破预算;反思建议不叠加;压缩行有 GC。
- [ ] 阶段 3:可跳过摘要时跳过;摘要请求吃前缀缓存;投影层 dedup/purge 生效且 DB 不动;前端可见水位;世界书/子智能体/状态块各有预算或格式开关。
- [ ] 全部既有测试 + 新增测试绿;`.\start.ps1` 冒烟启动成功(浏览器模式 health 200)。

