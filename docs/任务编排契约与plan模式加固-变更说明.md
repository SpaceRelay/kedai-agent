# 任务编排契约与 plan 模式加固 — 变更说明(2026-09-12)

> 语言规范沿用项目约定:回复/注释简体中文,代码/路径/API 名保留英文。
> 依据:一轮针对任务模式(重点 plan)的**实测审计**——五条结论均已定位到确切行号,
> 本轮严格按清单落地,不扩大范围。
> 前序记录:[实测四轮修复-变更说明.md](实测四轮修复-变更说明.md)、
> [任务模式重构-变更说明.md](任务模式重构-变更说明.md)。

---

## 0. 审计结论(5 项)

| 编号 | 结论 | 影响 |
|---|---|---|
| A1/A2 | `agentgo` 循环内整批 `return Err`,合法项已成孤儿;纯空白 `" "` 当合法 | 静默丢任务 / 派发不可观测 |
| B | 子任务结果**只要正文非空就 `set_done`**,不看 `finish_reason` | 「自然完成 / 截断 / 拒绝执行」三态同判 done,**假成功主通道** |
| C | `read(type=subtask)` 只查 `task_id`,按子任务名必失败 | 模型只知名字时无法取回结果 |
| D/E | `todo.tool_calls` 恒 `[]`(虚拟 session 无行);`subtasks` 用精确 session | 诱导审计误判「无调用」;子 agent 看不到兄弟子任务 |
| F | `agentend` 对已 ended 任务同样 `ended:true` | 无法证明「本次中断成功」 |

---

## 1. 修复总览

| 编号 | 修复 | 落点 |
|---|---|---|
| A1/A2 | 先全量校验、再创建/派发;非法项进 `rejected`,合法项照常 `create`+`spawn`;`name`/`instruction` 取 **trim 后**值;仅「tasks 与 rejected 都为空」或「合法项为 0 且存在 rejected」才 `Err`,文案带逐项原因 | `tools/agent_tools_agent.rs` `register_agentgo`(26 起,校验/派发循环约 89) |
| B | 新增 `AgentSubtaskService::set_failed`;写回判定抽为纯函数 `classify_subtask_result`——`finish_reason=length` 且正文非空 → 记 `error`(保留截断正文到 `result`);空内容 `error` 带 `finish_reason` | `services/agent_subtask_service.rs:220`;`tools/agent_tools_agent.rs` 写回分支约 427、纯函数 491 |
| C | `read_subtask` 改为接收 `ctx`;三键命中:精确 `id` → 精确 `name`(trim)→ 大小写不敏感子串;命中唯一时 JSON 同时含 `id` 与 `name` + `matched_by`;多命中返回候选清单;无命中报错列可用候选 | `tools/agent_tools_read.rs` `read_subtask:182`、`ambiguous_json:235`、`subtask_json:250` |
| D | 新增单一出处 helper `task_session_prefix`(`task:{id}` 折叠,非 task/空 id → `None`)+ 单测 | `tools/agent_tools_shared.rs:23` |
| E | `todo.subtasks` 改走 `subtask_candidates`(前缀列举,跨 `:main:`/`:sub:` 可见);`tool_calls` 补 `tool_calls_available`(有行 `true` / 无行 `false` + `tool_calls_note`) | `tools/agent_tools_agent.rs` `register_todo:704` |
| F | `agentend` 每项 `end()` **之前**取记录,返回补 `prior_status` 与 `interrupted = existed && status∈{pending,running}`;`ended` 保持原值 | `tools/agent_tools_agent.rs` `register_agentend:645` |
| G | `PLANNER_PROMPT` 追加「契约先行」三条(交付物+可判是判据 / 权威版本唯一 / 字段名唯一来源);`EXECUTOR_PROMPT` 追加两条(写明取代对象、审计只回填结论) | `services/task_service/prompt.rs:16,19` |
| H | `agentgo` 描述追加「写指令时按此模板」(契约/角色/单一交付物/判据/输出/长度/引用;缺 END 视为截断) | `tools/agent_tools_agent.rs:30` |

---

## 2. 行为变更与兼容性

- **`agentgo` 返回结构**:保留 `tasks` 键(兼容既有调用/测试),新增 `rejected` 数组
  (`index` 为入参 0-based 下标)。混合批从「整批 Err」变为「部分派发 + 逐项拒绝」;
  全废批由整批 Err 变为「Err + 逐项原因」,语义更可自纠。
- **子任务截断**:`status=done` → `status=error`,但**正文不丢**(`result` 保留截断内容,
  `error` 写入 `finish_reason=length` 定性)。这是本轮唯一的语义反转,针对假成功主通道。
  无 `finish_reason` 的纯生成回退路径(`run_subtask_plain` 与 `finish_reason=None`)
  行为不变,不误伤。
- **`read(type=subtask)`**:响应新增 `matched_by`(另有 `id`+`name` 双向别名);
  多命中由「随便挑一条」改为返回候选清单(不报错),单条不中报错带候选。
- **`todo`**:`subtasks` 可见范围扩大(同任务全虚拟 session);`tool_calls_available`
  与 `tool_calls_note` 为**新增字段**,旧客户端忽略即可。
- **`agentend`**:`prior_status`/`interrupted` 为新增字段;`ended`/`existed` 语义不变。
- **提示词**:仅追加文本,JSON 输出契约与元素格式不变;`EXECUTOR_PROMPT` 仍含「任务执行者」、
  `PLANNER_PROMPT` 仍含「任务规划器」,mock 条件钩子(`[[empty_if:]]`/`[[reply_if:]]`)不受影响。

---

## 3. 测试验证

| 项目 | 结果 |
|---|---|
| 后端 lib 单测 | **737 passed**(基线 726;新增 11 例) |
| 后端集成 `--test tasks` | **50 passed**(基线 49;新增截断端到端 1 例) |
| clippy --lib | 无新增告警(仅 7 条既有告警,集中于 settings_service/permissions) |

新增用例覆盖:agentgo 混合批/纯空白/全废;read 三键命中 + 无命中候选 + 多命中清单;
`task_session_prefix` 三形态 + 非 task + `"task:"` 边界;todo 派生 session 跨 agent 可见 +
`tool_calls_available==false`;agentend `interrupted` 真伪两态;`classify_subtask_result`
纯函数三态;`set_failed` 落库保留正文;提示词契约硬约束;以及
`task_multi_mode_subagent_truncation_marks_error`(mock `[[finish:length]]` 端到端:
子任务 `error` + `result` 保留半截正文)。截断端到端用例已成功构造并跑通,无需降级为纯函数单测。

---

## 4. 残留(设计边界,非本轮缺陷)

- **「拒绝执行」态仍无法自动识别**:模型以自然语言拒答时,`finish_reason` 为 `stop`、
  正文非空,与「自然完成」无法在线区分。本轮只解决了可机器判定的 `length` 截断;
  拒答识别需模型自述标记或二次审计,未纳入范围。
- **plan 规划阶段无法落盘 `contract.json`**:plan 模式规划阶段按设计只有只读侦察工具
  (零副作用纪律),写文件能力不在白名单内。契约纪律改由 **提示词 + 计划本身**承载
  (PLANNER_PROMPT 契约先行三条),不落磁盘文件。
- **`tool_calls` 在任务模式仍是显式不可用而非真实日志**:任务模式虚拟 session 无
  `agent_sessions` 行,`todo.tool_calls` 恒空。本轮以 `tool_calls_available:false` +
  note 显式标注消除误判,但未新建调用日志存储——真实调用证据仍走
  `read(type=subtask)` / 任务详情 / `GET /api/tasks/{id}/calls`。
- **子任务的 END 收尾未做机械校验**:模板要求「缺 END 即视为截断」,但当前仅作为指令
  约束下发给模型,未在执行侧强制解析 END 标记(与 `length` 判据互补;若后续实测需要,
  可在 `classify_subtask_result` 增加「要求 END 的指令 + 末尾缺 END → Truncated」分支)。
