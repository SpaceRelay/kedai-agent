# 借鉴本机四套 harness 与业界最新实践的落地计划(2026-08)

> 学习对象:本机 Codex CLI(`~/.codex/`)、ZCode(`~/.zcode/`)、opencode(`~/.config/opencode/`)、Reasonix(`~/AppData/Roaming/reasonix/`)四套 harness 的配置与工具设计,
> 以及 2025-2026 业界公开实践(Anthropic / OpenAI / DeepSeek 官方工程文档,Claude Code / Codex CLI / OpenCode / Gemini CLI / Aider)。
> 本文档是分析与落地计划,按 `learn-deepseek-harness.md` 同款格式组织;落地遵循项目纪律「先写失败测试,再做最小实现」,每批次一个本地 git 提交。

## 结论概览

kedai 的压缩(可逆投影)、工具注册表、技能库、子智能体、任务工作台都有基础;
真正的差距集中在三处:**提示词前缀缓存没有系统性维护**(DeepSeek 命中价约 1/16,长对话 RP 场景收益极大)、
**跨会话记忆蒸馏缺位**(长对话人设漂移的核心痛点)、**技能/子代理元数据未渐进披露**(预载成本与调度能力不足)。
以下按落地批次排序。

---

## 落地项 1:缓存感知压缩管线(批次 1,最高优先级)

### 为什么

kedai 主接 DeepSeek,其磁盘上下文缓存全自动、命中价约为全价的 1/16,命中率完全取决于消息数组的组织纪律:
前缀逐字节一致才命中。当前 `compaction.rs` 的摘要由消息构建层拼进 system——每次新摘要都改写 system,等于整段前缀全部 miss;
且没有任何缓存命中率观测,用户无从知道压缩与注入策略是否在「烧缓存」。

### 可借鉴的机制

- **ZCode/Reasonix 四级水位管线**(`~/.zcode/context-compressor/`):soft 0.5 提示 / snip 0.6 零成本裁陈旧工具结果 / compact 0.8 付费摘要 / force 0.9 强制;
  `keep=["errors"]`、尾部 recentKeep 条原文保留;**摘要产物绝不改写活跃会话前缀**。
- **cache-check 前缀诊断**(`~/.zcode/context-compressor/cache-check.mjs`):纯追加性校验、system+tools 锚点逐字节稳定、命中率骤降分级
  (「前缀漂移=严重」vs「LRU 淘汰=自愈」)。
- **DeepSeek 官方缓存指引**:system 与工具定义放最前且字节稳定;历史只 append 不改写;不在历史中间插入内容;
  用 `usage.prompt_cache_hit_tokens / prompt_cache_miss_tokens` 观测。
- **Anthropic compaction 经验**(Claude Code v2 踩坑):摘要「先保 recall 再保 precision」——角色扮演丢设定即灾难,宁可摘要冗长;
  最轻量的一档是「擦除陈旧工具结果」而非全文摘要。

### kedai 落地计划

1. connector(`openai_compatible.rs`)解析并透传缓存 usage 字段,`llm_request_log` 落库每轮命中/未命中 token(迁移脚本)。
2. 新增 `GET /api/diagnostics/cache` 统计端点:近 N 轮命中率、按可配置单价估算费用与节省、四级水位报告(level + 距下一档 gap)。
3. `messages.rs` 前缀稳定化:组装分层固定为「system+工具定义(静态)→ 摘要槽(半静态)→ 尾部历史(只追加)」;
   worldbook/@INJECT 注入位置与排序稳定;补「同会话两次构建消息数组公共前缀逐字节一致」回归测试。
4. 摘要槽改造:摘要移出 system 内嵌、改为独立消息槽位;**追加式增量摘要**——旧摘要文本冻结,新压缩段的摘要追加其后,
   使每次压缩只 miss 尾部而不是全量。本项风险最高,先写失败测试再动拼装逻辑。
5. 阶梯压缩:LLM 摘要前加零成本 snip 档(陈旧工具结果压成占位符、错误保留、尾部原文保留);`KEEP_RECENT_MESSAGES` 可配置化。
6. 前端 OptimizeModal/设置页加「缓存健康」面板(命中率、趋势、节省金额、当前水位档,借鉴 Reasonix status_bar_items)。

### 风险与测试

- 中~高:批次 4 动消息拼装核心路径,必须以「前缀一致性回归测试」护航;不改变 worldbook/变量注入语义,只固定顺序。
- 缓存观测是纯增量(新列+新端点+新面板),风险低,先行。

---

## 落地项 2:跨会话记忆蒸馏(批次 2)

### 为什么

角色扮演长对话的痛点是跨会话人设漂移。Anthropic 实测「结构化笔记/记忆」可把任务成功率从 ~36% 提到 ~54%;
Codex CLI 的记忆子系统就是「rollout 会话 → 两阶段蒸馏」:stage1 自动生成 raw_memory+summary,phase2 按 selected 精选注入。

### 可借鉴的机制

- **Codex `memories_1.sqlite`** 表结构:`stage1_outputs(thread_id PK, raw_memory, rollout_summary, usage_count, last_usage, selected_for_phase2)`+`jobs` 队列。
- **Reasonix 长期记忆**:记忆索引与正文分离,每次修改留修订快照。
- **注入位置纪律**:记忆注入放在尾部动态层之前的固定槽位,避免破坏前缀分层(与落地项 1 配合)。

### kedai 落地计划

1. 新表 `memory_distillation`(session 维度、raw_memory、summary、usage_count、last_usage、selected),会话结束或手动触发蒸馏,复用现有 connector 与 compaction 同取向的摘要提示词。
2. 注入策略:按 usage_count/last_usage 衰减排序精选,注入固定槽位。
3. 与 `tools/memory.rs` 打通:agent 主动写入进同一张表、同一衰减策略。
4. 前端记忆库管理面板(查看/编辑/删除/手动蒸馏)。

### 风险与测试

- 低~中:纯增量(新表+新服务+新面板);蒸馏走 LLM 的部分复用 mock connector 测试;注意不泄露聊天正文到日志。

---

## 落地项 3:技能渐进披露 + 子智能体调度(批次 3)

### 为什么

kedai 技能库目前是整体预载;Anthropic Agent Skills 的测算:每个工具/技能定义常驻 500 token,12 个就白带 6000 token/轮。
渐进披露(SKILL.md 三级:预载仅 name+description 约 20-50 token → 正文按需 → 资源按需)能把常驻成本压到趋近于零。
Reasonix 的 `runAs: subagent` 把「技能=子代理=工具白名单+模型路由」统一为单文件声明;其编排参数
(`max_subagent_depth=2`、`max_subagent_concurrency=6`、per-skill model/effort)是成熟默认值。

### 可借鉴的机制

- **Anthropic Agent Skills**:SKILL.md frontmatter(`name`+`description`)预载,正文 <500 行按需读取;description 写清「何时用」。
- **Codex 技能白名单**:`[skills] config = [{name, enabled}]` 默认全关、按需开。
- **Reasonix 子代理参数**:深度/并发上限、per-技能模型路由;**子代理只回 1000-2000 token 摘要**(Anthropic 多智能体系统:主代理仅占 20% token)。
- **Anthropic《Writing effective tools》**:错误信息要「可操作」(直接告诉模型下一步怎么做);合并必然连续的工具;工具输出硬截断(已有 64KB,可再按 token 细化)。

### kedai 落地计划

1. 技能格式升级:frontmatter(`name`/`description`/可选 `allowed-tools`/`runAs: subagent`/`model`),预载只注入 name+description;技能白名单表默认关、显式开。
2. 子代理调度:`max_subagent_depth`/`max_subagent_concurrency` 可配置;子代理结果截断为摘要回传;支持 per-技能模型路由。
3. 工具治理:错误文案可操作化;注册表顺序固定(增删不改既有定义顺序,保缓存)。

### 风险与测试

- 中:技能加载路径改动需保旧行为兼容(无 frontmatter 的旧技能按现状整体加载);子代理截断要有明确标记,不静默丢信息。

---

## 方向参考(本轮不做,登记备查)

- **Plan Mode + 权限三档**(Claude Code / Codex):规划模式只读、产出经确认再执行——与 kedai 任务工作台(planning→running)可结合,暂缓。
- **evals 回归**(pass^k + LLM-as-judge + mini evals):RP 无单测可跑,LLM-as-judge 是唯一判分器;待 prompt/压缩策略稳定后建。
- **会话分层持久化**(Reasonix:turn ckpt + event-index + content_digest + recovery):kedai 已有 SQLite 落库,可后续加 digest 校验与按轮恢复。
- **工具结果 post-execute 管道**(deepseek-harness 借鉴点 3): censor/mvu 剥离迁入统一管道,回归面大,单独批次。
- **限流代理 + 仪表盘**(opencode rate-limit-proxy):kedai 为本地单用户场景,暂无必要。
- **消息队列 steer/drop 策略**(Reasonix bot):对 RP 连发消息有参考价值,暂缓。

## 来源

- 本机:`~/.zcode/context-compressor/`(estimate/snip/compress/cache-check)、`~/.codex/config.toml + memories_1.sqlite 结构`、
  `~/AppData/Roaming/reasonix/config.toml`(四级水位/子代理参数/计费感知)、`~/.config/opencode/`(thinking-rules、dcp)。
- 官方:DeepSeek KV Cache 指南(api-docs.deepseek.com/guides/kv_cache)、Anthropic prompt caching 文档与
  《effective-context-engineering-for-ai-agents》《equipping-agents-for-real-world-with-agent-skills》
  《writing-tools-for-agents》《built-multi-agent-research-system》、OpenAI prompt caching / function calling 指南、
  Claude Code sub-agents/hooks 文档、Codex CLI 仓库、Gemini CLI agent design、Aider repomap。
