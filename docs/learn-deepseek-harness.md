# 借鉴 deepseek-harness 的落地建议

> 学习对象:`deepseek-ai/deepseek-harness`(TypeScript pnpm monorepo,「一切皆插件」,基于 vendored Cordis)。
> 源码已下载到 `C:\Users\LENOVO\Desktop\deepseek-harness-master\`(zip 在桌面,看完可删)。
> 本文档只做分析与落地建议,**未修改任何行为代码**。

## 结论概览

对照后 kedai 并不落后:harness 的工具注册表、权限门槛、插件加载、slash 命令,kedai 都已自己实现;
世界书/角色卡/mvu 变量/提示词楼层更是 kedai 独有。真正值得借鉴、且不破坏「文学创作/角色扮演」主题的是下面 3 项,按价值排序。

---

## 借鉴点 1:智能上下文压缩(最高优先级)

### 为什么

kedai 的 `max_context_tokens` 目前只是「历史超出后按时间裁剪」——见 `web/src/contextStats.ts:54` 的推荐设置说明,
以及后端 `server-rs/src/services/settings_service.rs` 里的对应字段。README Roadmap 里「智能上下文压缩(摘要/滑动窗口)」
仍是未完成项。这是唯一一个 kedai 明确留白、而 harness 有成熟参考的点。

### harness 可借鉴的机制(`packages/compaction/`)

- **token-pressure 触发**:不是等溢出才砍,而是按 token 压力预判,提前压缩。
- **可插拔摘要 provider**:压缩动作收敛到一个服务接口,后端可换。
- **compaction-tool-result-pruner**:无模型(不调 LLM)的工具结果裁剪,把旧工具结果压成「摘要 + 定位符」。
  与 kedai 现有的 `MAX_TOOL_OUTPUT_BYTES`(64KB 截断,`server-rs/src/tools/registry.rs:15`)正好拼成两级:
  先裁旧工具结果,再裁对话历史。

### kedai 落地计划

1. 新建 `server-rs/src/services/compaction_service.rs`(或 `compaction/` 目录模块):
   - 输入:会话消息数组 + 当前 token 预算(`token_service.rs` 已有计数能力)。
   - 触发:预估 token 超过 `max_context_tokens * 阈值`(如 0.8)时自动触发,而非溢出后。
   - 摘要 provider:先做「朴素 provider」(取最近 N 条 + 对更早历史做一次 LLM 摘要,LLM 复用现有 connector)。
2. 工具结果裁剪:在 `tools/registry.rs` 的 `run_tool` 结果落库/回填处,对「历史工具结果」做无模型压缩,
   保留最近几次完整结果 + 更早的摘要占位,与现有 64KB 截断互补。
3. 前端:`web/src/contextStats.ts` 增加压缩触发提示(复用现有 OptimizeModal 面板)。
4. 测试:仿 `token_service` 现有单测,补「触发阈值」「摘要 provider 可换」「工具结果裁剪不破坏最近结果」三组。

### 风险与测试

- 低:纯后端,不碰角色卡/世界书/变量注入;改的是「模型看到的上下文」,需用 snapshot/集成测试确认摘要后回复质量不回退。
- 注意:与提示词缓存冲突——压缩会截断历史、导致 DeepSeek/Anthropic 前缀缓存失效。触发阈值要设得足够保守,
  并在压缩前评估缓存命中率(项目已有 cache 相关工具链,`AGENTS.md` 的上下文压缩章节可参考)。

---

## 借鉴点 2:工具输出卡片化渲染(render intent)

### 为什么

harness 让每个工具自带 `presentCall`/`presentResult`,前端按 `card` 类型渲染,不用 switch 工具名。
kedai 现在 `web/src/components/AgentDock.vue:94-108` 是统一 `JSON.stringify` 直出,工具变多(read/search/write/memory)后难以阅读。

### harness 可借鉴的机制(`packages/core/tools` 的 "Tool-owned UI presentation")

card 类型:`generic` / `terminal` / `diff` / `search` / `read` / `web`,每个工具返回纯函数的 `presentCall`/`presentResult`,
UI 只按 card 渲染,不感知工具名;未声明则回退 generic。

### kedai 落地计划

1. 后端 `server-rs/src/models/types.rs` 的 `ToolDefinition` 增加可选 `render` 字段(如 `render_kind: Option<String>`),
   或直接在工具注册时带上展示元数据;SSE 的 `tool_call`/`tool_result` 事件透传该字段。
2. 前端 `web/src/sseReducer.ts` 保留该字段,`AgentDock.vue` 改成:按 `render_kind` 分派渲染,未知/缺失回退现有 JSON 直出。
3. 首期只给 1~2 个内置工具(如 `calculator` → `generic` 卡片带结果高亮、`search` → `search` 卡片)打样,其余走兜底。

### 风险与测试

- 低~中:前端改动为主,后端只加可选字段(向后兼容,不破坏已有工具)。补 `sseReducer` 单测 + 组件渲染测试。

---

## 借鉴点 3:工具执行 post-execute 后处理钩子(架构级,建议最后做)

### 为什么

harness 的管线是 `pre-execute → guard → execute → post-execute → finalize`;`post-execute` 可「替换结果 / 附加上下文」。
kedai 现在这些「执行后处理」散落在不同模块:censor 禁词兜底(`server-rs/src/tools/censor.rs`)、
`<UpdateVariable>`/`<JSONPatch>` 输出协议剥离(`server-rs/src/parsing/assistant/patch.rs`)、变量状态注入。

### kedai 落地计划

1. 在 `tools/registry.rs` 的 `execute`/`run_tool` 内,把「权限裁决 → 执行 → 结果后处理」拆成可注册的阶段钩子,
   保持现有 `execute` 语义不变(向后兼容)。
2. 把 censor、mvu 剥离、变量注入逐步迁入 post-execute 钩子,统一成「结果管道」。
3. 测试:现有 219 个后端用例必须全绿,确认迁移零行为变化。

### 风险与测试

- 高:触及工具执行核心路径,回归面大。建议等借鉴点 1、2 稳定后再做,单独 PR,先写失败测试再迁移。

---

## 方向参考(暂不落地)

- **append-only session log +「model-visible ⟺ logged」不变量**:保证模型看到的每个输入都能从会话日志回溯,
  对提示词缓存、调试、SillyTavern 导入导出有利。但 kedai 已有 SQLite 落库,改造大,属长期方向。
- **并行工具执行(isConcurrencySafe + 并行池)**:kedai 工具串行,read/search 等只读可并行。
  但角色扮演场景工具调用少,收益有限,暂缓。

---

## 当前状态与下一步

- 未修改 kedai 任何代码。
- harness 源码在 `C:\Users\LENOVO\Desktop\deepseek-harness-master\`,zip 包 `deepseek-harness.zip` 可自行删除。
- 建议顺序:先落地借鉴点 1(补齐 Roadmap 空白、价值最高),再 2,最后评估是否做 3。
