# Kedai 性能基线(优化前)

> 记录日期:2026-08-21;基线 commit:`5f9a55a`(优化前基线)。
> 环境:Windows 11,debug 构建(`cargo test` 产物),数据目录为本机真实 `data/`。

## 测试基线

| 项 | 结果 | 耗时 |
|---|---|---|
| `cargo test`(vcvars64 环境) | **643 绿**(538 单测 + 105 集成,0 失败) | 单测 5.88s,集成合计约 100s |
| `npm test -w web`(Vitest) | **295 绿**(33 文件) | 2.70s |
| `npm run build -w web` | 通过 | 4.17s |

## 前端 bundle 基线(vite build)

| chunk | 体积 | gzip |
|---|---|---|
| `index-*.js`(全部应用代码,13 个弹窗 eager) | 373.85 kB | 111.26 kB |
| `vendor-*.js` | 182.11 kB | 68.99 kB |
| `content-rendering-*.js`(markdown-it + sanitize-html) | 102.88 kB | 44.62 kB |
| `vue-vendor-*.js` | 78.35 kB | 31.09 kB |
| `index-*.css` | 70.44 kB | 13.53 kB |

## 接口延迟基线(`tools/perf-baseline.mjs`,20 并发 × 100 请求)

| 端点 | p50 | p95 | avg | rps | errors |
|---|---|---|---|---|---|
| `GET /api/characters` | 9.9ms | 20.8ms | 11.4ms | 1618 | 0 |
| `GET /api/chat/history?session_id=…` | 7.2ms | 10.1ms | 6.9ms | 2692 | 0 |

注意:本机数据量小(消息表近空),绝对延迟低;阶段 1 验收以**同等工作负载**对比 p95 为准,
并补充「流式生成进行中并发读」场景。

## 压测脚本用法

```bash
# 先启动服务(默认 127.0.0.1:3001),再:
node tools/perf-baseline.mjs            # 20 并发 × 100 请求/端点
node tools/perf-baseline.mjs -c 50 -n 200
```

脚本自动从 `/api/bootstrap` 取 token;自动发现首个有会话的角色做 history 压测,
无会话时跳过该端点。

## 阶段 1 改造后对比(2026-08-21,同机同脚本)

| 端点 | p50 | p95 | avg | rps | 基线 p95 |
|---|---|---|---|---|---|
| `GET /api/characters` | 13.6ms | 23.8ms | 14.6ms | 1276 | 20.8ms |
| `GET /api/chat/history` | 10.4ms | 15.4ms | 10.7ms | 1744 | 10.1ms |

**解读**:本机数据量极小(消息表近空),微基准下改造后延迟略升(r2d2 池借还 +
spawn_blocking 线程切换的固定开销,单请求约 +1~4ms),属于预期内的「固定成本换并发能力」。
本阶段真实收益在**竞争场景**:改造前所有读请求与写请求在单连接 `Mutex<Connection>` 上全局串行,
Agent 生成期间的长 SQL 会阻塞 tokio worker 并队头阻塞一切读请求;改造后读走 r2d2 只读连接池
(池大小 = CPU 核数)、写走单写连接、api 层 DB 访问统一 `spawn_blocking` 隔离,WAL 多读单写
能力真正生效。该场景已由集成测试 `tests/db_concurrency.rs` 覆盖(写进行中并发读全部 200、
写后读立即可见);微基准的 p95 ≥50% 下降目标在小数据集上不适用,待真实数据量上来后复测。

## Rust 编译环境备忘

`cargo` 命令需在 vcvars64 环境执行。已新增包装脚本:

```bash
cmd //c "C:\Users\LENOVO\Desktop\kedai\tools\cargo-vcvars.cmd cargo test"
cmd //c "C:\Users\LENOVO\Desktop\kedai\tools\cargo-vcvars.cmd cargo clippy -- -D warnings"
```

---

# 2026-09-14 实测与修复(第二轮性能治理)

> 方法:release 构建 + **真实角色卡**(`干物吸血鬼少女`,542KB,54 条内嵌世界书)
> + 隔离数据目录 + 真实上游(DeepSeek)。静态审计先行的上一轮不同,本轮**先实测再定优先级**。
> 本轮推翻/降级了上一轮静态审计的多项判断,详见下方「实测修正」。

## 一、端到端实测(debug 构建;token 与构建类型无关,可信)

| 档位 | 总耗时 | TTFT | prompt_tokens | 观察 |
|---|---|---|---|---|
| `fast` | 7,679 ms | 7,025 ms | 8,829 | 服务端本地预处理约 1 s,其余全在等上游 |
| `agent` | 68,064 ms | 30,868 ms | 40,943 | **零工具调用**;两次截断自愈 ~37 s;一次反思失败重生成 ~27 s |
| `solo`(任务) | 19,096 ms | — | 27,432 | 正常完成,4 次工具调用 |
| `plan`(任务) | **失控** | — | **累计 1,502,300** | 7 分钟不收敛,94 个 provider 轮次,需人工 stop |

`plan` 模式 prompt 逐步骤增长:`9,762 → 28,784 → 576,312 → 885,378`(**90 倍**)。

## 二、根因:两处缺陷叠加(已修复)

**缺陷 A —— token 计数器看不见工具参数。**
`services/token_service.rs` 的 `count_message_tokens` 只累加 `content`,
完全忽略 `tool_calls[].arguments`;而连接器会把 arguments 原样序列化进请求
(`connectors/openai_compatible/mod.rs` 的 `to_openai_messages`),它们真实占用 prompt。

**缺陷 B —— 裁剪器不裁工具参数。**
`agents/engine/messages/trim.rs` 的 `summarize_round` 只替换 tool 消息的 `content`
并清空 reasoning,**`tool_calls[].arguments` 原封不动**;而每轮都把完整调用塞回历史。

两者叠加使预算闸门**永远判定「未超预算」**→ 工具历史裁剪失效。实测日志佐证:
裁剪确实触发了 72 次却依然失控——因为它裁的是不占大头的东西。

**影响面超出任务模式**:`count_message_tokens` 同时被 `trim_to_context` 使用,
即**聊天主链路的上下文窗口裁剪也同期失效**(工具参数被低估),极端情况会超出模型窗口。

**缺失的防线**:`loop_broken` 熔断只存在于契约多步变量路径
(`contracts/multi_step.rs`,基于 `PatchOp` 哈希);**普通 agent 工具循环没有任何
重复调用检测**,唯一终止条件是 `max_tool_rounds`(实测配置为 64)。

## 三、实测定量(本地热路径,release 微基准)

| 项目 | 修复前 | 修复后 | 说明 |
|---|---|---|---|
| 世界书一轮匹配(33 条) | 9.439 ms | **0.062 ms** | 正则预编译(**152×**) |
| └ 其中纯正则编译 | 9.293 ms | ~0 | 进程级缓存,消除每轮重复编译 |
| token 计数摘要循环(12 轮) | 237 ms | **1.7 ms** | 内容缓存(**136×**),O(N²)→O(N) |
| `count_message_tokens`(30 条 × 2k) | 29.4 ms | 命中缓存 <1 ms | 同上 |

**前端(node 实测)**:

| 项目 | 耗时 |
|---|---|
| markdown-it 渲染 16.5k 字符 | **0.453 ms** |
| sanitize-html 处理 16.8k 字符 | **3.057 ms**(约 markdown 的 7 倍) |
| 模拟流式 60 帧全量重渲累计 | 18.1 ms |

**写路径(决定 P3-3 是否值得做)**:单条独立 INSERT 提交 **0.686 ms/条**,
同事务批量 **0.013 ms/条**(52×)。但一次任务约 12 条写入 ≈ **8 ms**,
相对 LLM 往返的秒级延迟不足以构成优化理由 → **事务化不做**(见下方清单)。

## 四、实测修正静态审计(避免为已证伪的假设浪费工时)

| 静态审计结论 | 实测裁定 |
|---|---|
| `prompt_kit` 6 次链式 `replace` | **撤销**。实测 0.042 ms/次,收益可忽略 |
| `settings_snapshot()` 整体 clone | **撤销**。clone 本身 0.007 ms;真正成本在工具定义的锁内 clone+sort(已改快照) |
| TaskBoard 全量 markdown 重算 | **降级**。markdown 极便宜(0.45 ms/16.5k);前端该管的是 sanitize 路径 |
| 世界书「重复 to_lowercase」与「正则编译」并列 | **拆分**。小写化外提仅省 0.35 ms;正则编译 9.3 ms 才是主项 |
| 任务写路径各抢一次写锁 | **不做**。实测 8 ms/任务,收益低于风险 |
| plan 模式每次 `agent_status` 重复发射 | **伪问题**。用 mock 裁决测试证明链路每类事件恰好一次;先前观察系探针脚本分帧所致 |

## 五、本轮落地的修复

| 项 | 改动 | 证据 |
|---|---|---|
| P0-1 工具参数计入 token 计数 | `token_service.rs` 计入 `tool_calls` 的 name/arguments;新增 `count_single_message_tokens` 统一口径 | 3 个单测 |
| P0-1 裁剪回收工具参数 | `trim.rs` 新增 `reclaim_round_arguments`,占位保持**合法 JSON**(严格后端会 400),id/name 保留以维持配对 | 4 个单测 |
| P0-1 收敛信号 | `trim_tool_history` 返回 `ToolHistoryTrimOutcome`,未达标记 warn(不再静默) | — |
| P0-2 重复调用熔断 | 新增 `utils/loop_guard.rs`(FNV 指纹 + 环形窗口,N=8/K=3),`run_tool_loop` 接入,中止理由经 step 事件透出 | 6 单测 + 1 端到端 |
| P1-1 世界书正则预编译 | `parsing/world_book.rs` 进程级正则缓存 + 小写化外提 | 2 单测 |
| P1-2 token 计数缓存 | `token_service.rs` 按 (编码,文本哈希,长度) 缓存 | 2 单测 |
| P2-1 推理感知自愈 | `executor.rs` 识别「推理耗尽预算」并一次给足预算(原为盲目翻倍) | 3 单测 |
| P2-2 性能门禁 | `perf-baseline.mjs` 加 p95 阈值 + `perf-baseline.json` 基线 + `check-all.ps1 -Perf`(默认关闭) | 退出码实测 1/0 |
| P3-1 工具定义快照 | `tools/registry.rs` 缓存 + 写路径失效(注册/注销) | 2 失单测 |
| P3-2 世界书条目缓存 | `world_book_service.rs` 按 character_id 缓存,5 条写路径全失效 | 2 单测 |

## 六、性能门禁用法

```bash
# 1) 启动服务(任一口径:start.ps1 / dist\kedai-server.exe / cargo run)
# 2) 跑门禁
node tools/perf-baseline.mjs                       # 仅测量(始终退出 0)
node tools/perf-baseline.mjs --max-p95-factor 1.25 # 带阈值判定
npm run check -- -Perf                             # 并入总门禁(默认不跑,需显式开启)
```

判定口径:`当前 p95 <= 基线 p95 × factor` 且 `errors == 0`;
`p95 < 5 ms` 的端点跳过(小数据集抖动)。
基线文件 `tools/perf-baseline.json` 记录的是**本机 release 构建**的实测值,
仅作回归对比基准,**不是跨机器的 SLA**;换机器或数据量变化后需重采样。

## 七、明确「不做」清单(已实测证伪)

| 项 | 实测 | 结论 |
|---|---|---|
| `prompt_kit` 链式 replace | 0.042 ms | 不做 |
| `settings_snapshot()` clone | 0.007 ms | 不做 |
| 任务写路径事务化 | 8 ms/任务 | 不做(收益低于语义风险) |
| TaskBoard 全量 markdown 重算 | 0.45 ms/16.5k | 降级(低优先) |
| 每请求 embedding HTTP | `embedding_enabled=false` 时不触发 | 暂不做,需单独立项 |
