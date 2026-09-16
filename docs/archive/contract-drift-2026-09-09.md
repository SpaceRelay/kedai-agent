# 契约漂移登记(2026-09-09)〔已归档〕

> **归档说明(2026-09-13)**:本表登记的全部漂移项 **#1-#12 均已修复**(#1-#4、#9-#11
> 在授权改造批次修复;#5-#8、#12 在后续批次修复,经 2026-09-13 复核确认:
> `web/src/api/memory.ts` 已补 `searchMemories`/`prune`/`MemoryEntry.pinned`/`DistillResult.skipped`)。
> 本文件由协议锁定类活文档降级为**历史快照**,只供溯源,不再更新;当前漂移检查由
> `tools/check-contract.mjs` 在每次 `npm run check` / 构建时自动执行。
> 唯一仍属实的历史遗留项:`POST /api/agent/execute` 为 501 桩(见 `docs/known-limitations.md`)。

> **更新(2026-09 授权改造批次)**:本表 #1-#4、#9-#11 已修复——前端 `RuntimeSettings` /
> `RuntimeSettingsPatch` 补齐 `tool_history_keep_rounds`、`tool_history_budget_tokens`、
> `memory_inject_char_budget`、`memory_max_entries`,并在生成参数区加控件;
> `SkillRecord.allowed_tools` / `run_as_subagent` / `model` 已补类型与技能库「高级」编辑入口;
> `TokenUsage.prompt_cache_miss_tokens` 已补。`web/src/api/skills.ts` 的 patch 类型同步补齐。
> 另新增授权相关字段 `authorization_mode` / `tool_authorization_timeout_secs` /
> `task_tool_policy` / `task_tool_allowlist`(见 [授权模式.md](../授权模式.md))。

> 本文档登记**审计发现但本轮未修改**的前后端契约差异,供后续批次处理。
> 原因:这些字段属于 2026-09-09 工作区未提交的在途改动(实测 `find -newermt` 确认),
> 按「不覆盖用户正在进行的工作」原则仅登记不动代码。
>
> 验证基线:`cargo test` 836 通过、`npm test -w web` 627 通过(65 文件)、
> `npm run typecheck -w web` 与 `npm run build -w web` 均通过。

## 一、后端已实现、前端类型/API 未接

| # | 字段/端点 | 后端位置 | 前端缺口 | 影响 |
|---|---|---|---|---|
| 1 | `memory_inject_char_budget` | `server-rs/src/api/settings.rs:109,176,408` | `web/src/api/types.ts` 的 `RuntimeSettings`/`RuntimeSettingsPatch` 无此字段 | 记忆注入字符预算无法在 UI 配置 |
| 2 | `memory_max_entries` | `settings.rs:112,177,414` | 同上 | 记忆容量上限无法配置 |
| 3 | `tool_history_keep_rounds` | `settings.rs:140,186,475` | 同上 | 工具历史保留轮数无法配置 |
| 4 | `tool_history_budget_tokens` | `settings.rs:143,187,483` | 同上 | 工具历史 token 预算无法配置 |
| 5 | `GET /api/memory/search` | `api/routes/settings.rs:36`、`api/memory.rs:132` | `web/src/api/memory.ts` 无 `searchMemories` | 记忆检索能力前端不可用 |
| 6 | `POST /api/memory/prune` | `routes/settings.rs:37`、`api/memory.rs:162` | `memory.ts` 无 `prune` | 记忆精简能力前端不可用 |
| 7 | `MemoryEntry.pinned` | `services/memory_service.rs:55-56,71` | `memory.ts:10-25` 的 `MemoryEntry` 无该字段;`updateMemory` patch 只允许 `{content?, selected?}` | 「常驻置顶」记忆前端不可见、不可写 |
| 8 | `DistillResult.skipped` | `api/memory.rs:103`、`memory_service.rs:84` | `memory.ts:28-32` 的 `DistillResult` 无该字段 | 蒸馏跳过数前端不显示 |
| 9 | `SkillRecord.allowed_tools` | `models/types.rs:481`、`api/skills.rs:25-37` | `web/src/api/types.ts:563-570` 缺字段;`api/skills.ts:18-23` patch 不含 | 技能工具白名单前端不可配置 |
| 10 | `SkillRecord.run_as_subagent` | `models/types.rs:483` | 同上 | 技能派子智能体开关前端不可配置 |
| 11 | `SkillRecord.model` | `models/types.rs:485` | 同上 | 技能模型覆盖前端不可配置 |
| 12 | `TokenUsage.prompt_cache_miss_tokens` | `models/types.rs:256-259` | `web/src/api/types.ts:136-143` 缺该字段 | 类型不完整(不影响现有命中率计算,因 `prompt_tokens = hit + miss`) |
| 13 | `GET /api/repo-index` | `api/routes/settings.rs:50`、`api/repo_index.rs` | 前端 `web/src/api/repoIndex.ts` 已接(同批新增) | 无缺口,仅记录 |

## 二、后端有路由、前端零调用(历史遗留,非缺陷)

| 端点 | 位置 | 说明 |
|---|---|---|
| `POST /api/agent/execute` | `api/agent.rs:127-132` | 501 历史桩,`API.md` 已标注 |
| `POST /api/agent/interrupt` | `routes/agent.rs:13` | 前端改用 `/api/chat/stop` |
| `GET/PUT/PATCH /api/variables`、`/api/variable/update|state|changelog` | `routes/chat.rs:44-47`、`routes/content.rs:50-52` | 前端变量走 SSE `vars` 事件与消息级端点 |
| `GET /api/memory/search`、`POST /api/memory/prune` | 见上表 #5/#6 | 同属在途改动 |
| `getToolPermissions` / `authorizeTool` / `revokeTool` / `agentPlan` | `web/src/api/agent.ts` | 完整 API 封装,暂无调用方;同文件 `resolveToolAuthorization` 在用,故保留 |

## 三、其他已核实但未修改项

| 项 | 位置 | 说明 |
|---|---|---|
| `mvu_temperature` / `mvu_model` | `settings_service/mod.rs:92,96` | 可落盘但无 API 通路,`mvu_model` 注释自称「P2 仅预留」;`mvu_temperature` 恒为 `None`(用内置 0.3) |
| `SimplePromptConfig.banned_words` | `prompt_inject_service.rs:154-157` | 旧格式兼容字段,已迁移到 `banned_prompt`,字段仅保留解析 |
| `PlannerStep.action = "tool"` | `models/types.rs:211` | 已在源码注释中修正为 `direct | reflect`(本轮已改注释) |
| `web/src/plugin.ts` 插件框架 | 本轮已删除 | 全仓零 import,`main.ts` 从不调 `loadExternalPlugins` |
| `web/src/components/toolRender.ts` | 本轮已删除 | 唯一引用是自身测试;`AgentPanel.vue` 用内联实现 |
| `server-rs/src/bin/kedai-data-merge.rs` | `server-rs/src/bin/` | Cargo 未声明 `[[bin]]`,按约定可编译但 `build.ps1` 不产出,属孤儿 bin |

## 四、处置建议(优先级)

1. **P0**:上表 #1-#4(4 个设置字段)、#7(`pinned`)、#9-#11(技能 3 字段)——后端能力已上线但 UI 完全不可见,属功能缺失。
2. **P1**:#5/#6(记忆 search/prune)、#8(`skipped`)——补齐 `web/src/api/memory.ts` 封装。
3. **P2**:#12(`prompt_cache_miss_tokens` 类型补齐)、`mvu_temperature`/`mvu_model` 是否接 API 需产品决策。
