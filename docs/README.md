# docs/ 索引

> 本目录分两类:**协议锁定类**是活文档,改代码时必须同步;**过程交接类**是历史快照,仅供溯源,不再更新。

## 协议锁定类(活文档)

| 文档 | 内容 |
|---|---|
| [ARCHITECTURE-3H.md](ARCHITECTURE-3H.md) | 「三结合」梯队架构总纲 |
| [mvu-protocol.md](mvu-protocol.md) | mvu 变量系统协议(UpdateVariable / MagVarUpdate 双格式) |
| [generate-render-protocol.md](generate-render-protocol.md) | 生成与渲染协议(双端一致性约束) |
| [inject-protocol.md](inject-protocol.md) | 提示词注入协议 |
| [模式提示词边界.md](模式提示词边界.md) | 角色扮演/任务双模式提示词的继承与隔离规则 |
| [授权模式.md](授权模式.md) | 三档授权模式(严格/宽松/放行)判定矩阵、任务工具策略与授权生命周期 |
| [任务引擎六模式.md](任务引擎六模式.md) | 六模式语义对照 |
| [任务模式重构-变更说明.md](任务模式重构-变更说明.md) | 任务模式重构的对外变更契约 |
| [任务模式实跑修复-变更说明.md](任务模式实跑修复-变更说明.md) | 2026-09-10 六模式实跑修复 F1~F8 的变更契约(含 F2 默认行为变更) |
| [实测八项修复-变更说明.md](实测八项修复-变更说明.md) | 2026-09-10 实测八项问题(任务对话呈现/team 收敛/呼吸灯条/agent 记录/状态栏/装饰条/首楼事件/关闭确认)的变更契约 |
| [实测三轮修复-变更说明.md](实测三轮修复-变更说明.md) | 2026-09-11 第三轮实测修复(赛马娘卡首楼交互 U1/U2、team 协作 T1~T7)的变更契约 |
| [实测四轮修复-变更说明.md](实测四轮修复-变更说明.md) | 2026-09-12 第四轮实测修复(世界书 depth 解析、CSS 注释吞声明、JS 授权提示、剪贴板失败语义 F1~F5)的变更契约 |
| [任务编排契约与plan模式加固-变更说明.md](任务编排契约与plan模式加固-变更说明.md) | 2026-09-12 任务编排工具契约与 plan 提示词加固(agentgo 部分派发、子任务截断不再假成功、read(type=subtask) 多键命中、todo 跨 agent 可见、agentend 可判)的变更契约 |
| [0.2.1-关于与提示词同源-变更说明.md](0.2.1-关于与提示词同源-变更说明.md) | 2026-09-12 0.2.1:关于/教程设置分区、任务 Agent 提示词丰富、Win 端提示词纳入代码默认(两端同源)、mock 工具白名单忠实化 |
| [角色扮演默认提示词内置-变更说明.md](角色扮演默认提示词内置-变更说明.md) | 2026-09-12 角色扮演默认提示词纳入代码默认(default_roleplay_agent_prompt + from_config/load 空值回填),修复 Android 首装提示词为空 |
| [perf-baseline.md](perf-baseline.md) | API 性能基线方法与记录 |
| [known-limitations.md](known-limitations.md) | 已知能力缺口清单(有意裁剪 vs 待办,防误判为 bug) |
| [contract-drift-2026-09-09.md](contract-drift-2026-09-09.md) | 前后端契约漂移登记(后端已实现、前端未接的字段与端点) |
| [优化实施方案-2026-09.md](优化实施方案-2026-09.md) | 架构详解 + 22 项优化方案 + 实施路线图(2026-09-07 起执行) |
| [android-port-plan.md](android-port-plan.md) | Android 移植方案:可行性评估、架构决策、分阶段计划、cfg 门控清单、风险表(2026-09-11 起执行) |

## 过程交接类(已归档,仅供溯源)

| 文档 | 说明 |
|---|---|
| [TASK-MODE-FIX-PLAN.md](TASK-MODE-FIX-PLAN.md) | 任务模式修复计划(「Kimi 续做」交接) |
| [任务模式重构-实施计划-v2.md](任务模式重构-实施计划-v2.md) | 重构实施计划 v2 |
| [plan2-variable-scopes.md](plan2-variable-scopes.md) | 变量作用域批次计划 |
| [plan4-slash-quick-replies.md](plan4-slash-quick-replies.md) | slash 命令与快速回复批次计划 |
| [plan5-audio-render-panels.md](plan5-audio-render-panels.md) | 音频与渲染面板批次计划 |
| [plan6-ecosystem-devtools.md](plan6-ecosystem-devtools.md) | 生态与开发者工具批次计划 |
| [learn-harness-2026-08.md](learn-harness-2026-08.md) / [learn-deepseek-harness.md](learn-deepseek-harness.md) | harness 学习笔记 |
| [kedai-agent-coordination.md](kedai-agent-coordination.md) | 多 agent 协作约定 |
| [context-optimization-brief.md](context-optimization-brief.md) | 上下文优化简报(**已归档**:其中「1 秒轮询/不走 SSE」已被 WP4/WP5 取代) |
| [archive/任务模式修复-变更说明.md](archive/任务模式修复-变更说明.md) | 上一轮任务模式修复的变更记录(**已归档**:被 [任务模式重构-变更说明.md](任务模式重构-变更说明.md) 取代) |
