# Kedai 任务模式修复与优化 — 交接计划(Kimi 续做)

> 本文档是给后续 AI(或人)接手用的完整交接手册。项目根目录 `C:\Users\LENOVO\Desktop\kedai`。
> 语言规范:回复/注释用简体中文;代码、路径、API 名保留英文。
> 验证命令(项目 AGENTS.md 约定,每个工作包完成后全量跑):

```powershell
cargo test --manifest-path server-rs/Cargo.toml
npm test -w web
npm run build -w web
cargo build --manifest-path server-rs/Cargo.toml
```

---

## 0. 一句话背景

试跑任务「写一首关于秋天的短诗」发现任务模式跑偏成言情小说、2/4 步骤空输出、侧栏状态停滞、token 计数恒 0。根因与修复分为 7 个工作包(WP1~WP7)。**WP1 已全部完成且测试绿;WP2 做了一半(connector 层已完成、执行循环未收尾,当前 `cargo build` 有 4 个编译错误);WP3~WP7 未开始。**

---

## 1. 根因链(已实证,修复方向据此)

1. `server-rs/src/services/settings_service.rs` 的 `for_mode(Task)` 在无 task 覆盖层时整体回退角色扮演扁平值(含 `agent_system_prompt` 人设词)。
2. `server-rs/src/services/task_service.rs` 把该人设词拼进执行器/汇总器 system 提示词,且旧 `render_agent_prompt` 不展开 `{{char}}/{{personality}}/{{scenario}}`,宏原文直发模型。
3. `generate_text` 只收 `Token` 块,丢弃 `Usage/Reasoning`,connector 层拿不到 `finish_reason`(旧 sse_parser 只认 `tool_calls`),空输出无从诊断、只能同参数傻重试一次。
4. 任务 LLM 调用无任何 usage 落库;前端统计块只在聊天 SSE 链路有数据源;侧栏任务列表仅在终态刷新。

---

## 2. 当前进度快照(重要:交接时以此为准)

### 已完成且测试通过(绿灯)

| 工作包 | 内容 | 验证状态 |
|---|---|---|
| WP1 | 任务模式提示词解耦 + 宏安全展开 | ✅ `cargo test --lib harness_settings` 绿;`cargo test --test tasks` 5 passed |
| WP2(connector 层) | `LlmStreamChunk::Finish` + `reasoning_tokens` 全链路;`stream_options.include_usage` | ✅ `cargo test --lib connectors::` 14 passed |

### 进行中(未收尾,当前编译失败)

| 位置 | 状态 |
|---|---|
| WP2(执行循环) | `generate_text` 已返回 `TaskGenOutput`、`generate_step_with`/`summarize_task_with` 已建;**但 `run_task_background` 仍是旧 `String` 处理逻辑 → 4 个编译错误** |

**当前 `cargo build` 的 4 个错误**(全部在 `server-rs/src/services/task_service.rs`,因为返回类型已改但调用方未改):

```
error[E0599]: no method named `trim` found for struct `TaskGenOutput`  (×2)
error[E0277]: the size for values of type `str` cannot be known at compilation time  (×2)
```

错误源:`run_task_background`(当前 827~940 行)里 `deps.generate_step(...).await` 的 `Ok(text)` 分支仍在 `text.trim()` 和 `set_subtask_status(..., Some(&text), ...)`,但 `text` 现在是 `TaskGenOutput`,不是 `String`。

### 未开始

WP3「部分完成」终态、WP4 usage 落库与展示、WP5 侧栏实时刷新、WP6 启动日志/桌面双启动、WP7 前端文案与测试补齐。

---

## 3. 【立即做】WP2 收尾:让 `run_task_background` 重新编译通过并落地分级重试

文件:`server-rs/src/services/task_service.rs`

### 3.1 已建好的类型与函数(不要重复造,直接调用)

```rust
// 已定义(约在文件顶部常量区之后)
pub(crate) struct TaskGenOutput {
    pub text: String,
    pub finish_reason: Option<String>,   // stop/length/content_filter 等;上游未下发为 None
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub reasoning_tokens: i64,          // completion_tokens_details.reasoning_tokens;0=无观测
    pub reasoning_chars: usize,         // reasoning_content 流字符数(与上互补)
}

// 已有签名:
async fn generate_step(&self, task, step, cancel) -> Result<TaskGenOutput, String>;       // 读任务模式默认 max_tokens/temperature
async fn generate_step_with(&self, task, step, max_tokens, temperature, cancel) -> Result<TaskGenOutput, String>;
async fn summarize_task(&self, task, plan, cancel) -> Result<TaskGenOutput, String>;
async fn summarize_task_with(&self, task, plan, max_tokens, temperature, cancel) -> Result<TaskGenOutput, String>;
```

### 3.2 分级重试规则(空输出时)

- 空内容(`text.trim().is_empty()`)且 `finish_reason == Some("length")` → **max_tokens 翻倍**(上限 `RETRY_MAX_TOKENS_CAP = 65536`)重试一次;
- 空内容且非 length → **temperature = 0.7** 重试一次(沿用 `mvu.rs` 先例);
- 重试前 `tokio::time::sleep(EMPTY_RETRY_BACKOFF)`(已定义常量 500ms);
- 仍空 → 步骤标 error,文案带原因:`format!("子任务返回空内容(finish_reason={})", reason)`。

> 设计意图:重试走 `*_with` 变体,只改单个参数、复用一个 `cancel`,不重建 system 提示词。这是为 WP4 usage 落库铺路——`TaskGenOutput` 里的 token 字段在 WP4 会被写入 `task_usage` 表,所以**不要**把返回值简化回 `String`。

### 3.3 建议的重构:抽两个内部辅助函数

在 `run_task_background` 之上加两个自由函数(与 `finalize_run` 同层),逻辑清晰且可单测:

```rust
/// 生成步骤:空输出按 finish_reason 分级重试一次(仍空返回带原因的 Err)。
async fn generate_step_retry(
    deps: &TaskService,
    task: &TaskRecord,
    step: &TaskStep,
    cancel: &watch::Receiver<bool>,
) -> Result<TaskGenOutput, String> {
    let first = deps.generate_step(task, step, cancel).await?;
    if !first.text.trim().is_empty() {
        return Ok(first);
    }
    tokio::time::sleep(EMPTY_RETRY_BACKOFF).await;
    if *cancel.borrow() {
        return Err("任务已停止".into());
    }
    let reason = first.finish_reason.as_deref().unwrap_or("");
    let retried = if reason == "length" {
        let settings = deps.task_settings();
        let doubled = (settings.default_max_tokens * 2).min(RETRY_MAX_TOKENS_CAP);
        deps.generate_step_with(task, step, doubled, settings.default_temperature, cancel).await
    } else {
        deps.generate_step_with(task, step, settings_here.default_max_tokens, 0.7, cancel).await
    };
    match retried {
        Ok(o) if !o.text.trim().is_empty() => Ok(o),
        Ok(o) => Err(format!("子任务返回空内容(finish_reason={})", o.finish_reason.as_deref().unwrap_or("未知"))),
        Err(e) => Err(e),
    }
}

/// 汇总:同样的分级重试。
async fn summarize_task_retry(
    deps: &TaskService, task: &TaskRecord, plan: &[TaskStep], cancel: &watch::Receiver<bool>,
) -> Result<TaskGenOutput, String> { /* 同上结构 */ }
```

> 注意:`generate_step_with` 内部 `let settings = self.task_settings();` 只用于取 `default_top_p` 与 `agent_system_prompt` 等;`max_tokens/temperature` 是入参,别在辅助函数里重复读错 settings。重试分支里读一次 `deps.task_settings()` 存成局部变量即可。

### 3.4 改写 `run_task_background` 的步骤执行段(约 876~911 行)

把旧 `gen` 匹配改成:

```rust
let gen = generate_step_retry(&deps, &task, step, &cancel).await;
match gen {
    Ok(out) => {
        final_plan[i].status = "done".to_string();
        final_plan[i].result = out.text.clone();
        deps.set_subtask_status(&subtask_id, "done", Some(&out.text), None);
        // WP4 在这里追加 usage 落库:phase="step", step_index=i, out.*_tokens
    }
    Err(e) => {
        if *cancel.borrow() {
            finalize_run(&deps, &task_id, token, true, None);
            return;
        }
        final_plan[i].status = "error".to_string();
        final_plan[i].result = e.clone();
        deps.set_subtask_status(&subtask_id, "error", None, Some(&e));
    }
}
```

### 3.5 改写汇总段(约 918~938 行)

```rust
match summarize_task_retry(&deps, &task, &final_plan, &cancel).await {
    Ok(out) => {
        if deps.is_current_run(&task_id, token) {
            // WP3 会改成:含 error 步骤时 set_result(..., "partial"),否则 "done"
            let _ = deps.set_result(&task_id, &out.text);
        }
        // WP4 在这里追加 usage 落库:phase="summary"
    }
    Err(e) => finalize_run(&deps, &task_id, token, *cancel.borrow(), Some(&e)),
}
```

### 3.6 收尾验证

```powershell
cargo build --manifest-path server-rs/Cargo.toml   # 必须先 0 error
cargo test --manifest-path server-rs/Cargo.toml --test tasks
```

现有 `tests/tasks.rs` 的 `task_run_to_done` 已用 `[[reply:...]]` 钩子覆盖成功路径,改完必须仍 5 passed。

---

## 4. WP2 剩余:空输出重试的集成测试

文件:`server-rs/tests/tasks.rs`(mock 钩子 `[[empty]]` 只回 Usage 无 Token,已存在)

新增用例(参考现有 `task_run_to_done` 写法):

```rust
#[tokio::test]
async fn task_step_empty_output_retries_then_errors() {
    let app = test_app();
    // 规划器返回 1 步,该步 goal 用 [[empty]] 触发空输出
    let title = r#"[[reply:[{"name":"空步骤","goal":"[[empty]]"}] ]]"#;
    let id = create_task(app, title).await;
    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (st, detail) = wait_terminal(app, &id).await;
    // 空输出重试一次仍空 → 步骤 error,任务最终 partial(见 WP3)或按当前逻辑走 error
    // 断言:该步骤 status=="error",error 文案包含 "空内容"
}
```

> 注意:`[[empty]]` 是 mock 连接器按「最后一条 user 消息是否含 `[[empty]]`」匹配(见 `server-rs/src/connectors/mock.rs`),而 `[[reply:...]]` 在首个 `]]` 截断——两者放同一 goal 会冲突。因此上面的 title 用 `[[reply:...]]` 让规划器返回计划、goal 内用 `[[empty]]` 触发执行器空输出,二者不冲突(mock 对规划器消息与执行器消息是分别匹配的)。

---

## 5. WP3「部分完成」终态(前后端)

### 后端
- `run_task_background` 汇总成功时,若 `final_plan.iter().any(|s| s.status == "error")` → 新状态 `"partial"`,否则 `"done"`。
- `set_result(&self, id, result)` 加 status 参数(现为 `set_result(id, result)` 无条件 `status='done'`,见 `task_service.rs` 约 315 行)。
- `tasks.status` 是 `TEXT` 无 CHECK 约束(`server-rs/src/models/db/schema.rs` 88 行起),**无需 DB 迁移**。

### 前端
- `web/src/api/types.ts` 约 606 行:`TaskStatus` 加 `'partial'`。
- `web/src/taskStatus.ts`:`taskStatusLabel` 加 `case 'partial': return '部分完成';`;`taskStatusClass` 加 `case 'partial': return 'active';`(黄色语义)。

### 测试
- 后端 `tests/tasks.rs` 用 `[[fail:消息]]` 钩子(mock 已支持)造部分步骤失败,断言终态 `partial`。
- 前端新增 `web/src/taskStatus.test.ts` 覆盖映射(写法参考 `web/src/sseReducer.test.ts` 纯函数风格)。

---

## 6. WP4 任务模式 usage 落库与展示

### 6.1 建表(`server-rs/src/models/db/schema.rs`,与 tasks 表同区,幂等 `CREATE TABLE IF NOT EXISTS`)

```sql
CREATE TABLE IF NOT EXISTS task_usage (
  id               TEXT PRIMARY KEY,
  task_id          TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  phase            TEXT NOT NULL,             -- plan | step | summary
  step_index       INTEGER,                   -- 仅 phase='step' 有意义
  model            TEXT NOT NULL,
  prompt_tokens    INTEGER NOT NULL DEFAULT 0,
  completion_tokens INTEGER NOT NULL DEFAULT 0,
  reasoning_tokens INTEGER NOT NULL DEFAULT 0,
  created_at       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_task_usage_task ON task_usage(task_id);
```

> 不复用 `llm_requests` 表:其 `session_id` 外键指向 `sessions(id)`,任务 id 不在其中会被外键挡(已实证,任务子任务因此才另建 `task_subtasks` 表)。

### 6.2 落库(`task_service.rs`)
- 新增 `fn record_usage(&self, task_id, phase, step_index: Option<usize>, out: &TaskGenOutput)` 写库。
- `run_task_background` 三处调用:规划后(phase="plan")、每步成功(phase="step",step_index=i)、汇总成功(phase="summary")。
- model 从 `task_settings()` 或 `connector.model()` 取(注意:启动日志里 env model 是 `gpt-4o-mini`,实际生效 `deepseek-v4-flash`,见 WP6;这里应取**合并 settings 后的有效 model**)。

### 6.3 API
- `GET /api/tasks/{id}` 的 `TaskDetail` 增字段 `usage_total { prompt_tokens, completion_tokens, reasoning_tokens }`(按 task_id SUM 聚合)。
- 新增 `GET /api/tasks/usage-total` 返回全部任务累计(供侧栏「全局累计」)。
- 路由注册在 `server-rs/src/api/routes/misc.rs`(42 行附近已有任务路由注释)。

### 6.4 前端
- `web/src/api/types.ts` 的 `TaskDetail` 加 `usage_total`;`web/src/api/tasks.ts` 加 `getTaskUsageTotal`。
- `web/src/components/TaskBoard.vue` 状态行旁显示「累计 token X」。
- `web/src/components/Sidebar.vue`:任务模式(`appMode==='task'`)下统计块改绑——本会话累计→当前任务累计、全局累计→全部任务累计、上下文 TOKEN 格隐藏、命中率格维持 null 隐藏。聊天统计来自 `stores/chat.ts` 的 SSE usage,任务模式无此数据源,需在 `stores/task.ts` 里加 `currentTaskUsage`/`globalTaskUsage` 两个 ref 并由详情轮询带出。

### 6.5 测试
- 后端 `tests/tasks.rs`:任务跑完后断言 `task_usage` 有 ≥3 行、`detail["usage_total"]` 聚合正确。
- 前端 `web/src/stores/storeFacade.test.ts` 更新门面键断言(新增字段)。

---

## 7. WP5 侧栏任务状态实时刷新

文件:`web/src/stores/task.ts`(约 142~158 行 `startTaskPolling`)

现状:轮询每 1s 只 `loadTaskDetail(id)`,只有终态才 `loadTasks()`(刷新列表),故执行全程侧栏列表停在「待执行」。

改法:轮询 tick 内,当 `taskDetailSig`(约 46~52 行,任务状态+计划状态+子任务状态签名)与上一签名不同时,**除发事件外同时 `loadTasks()`**。签名机制已存在,改动最小且不多发请求。

Vitest 新增 `web/src/stores/task.test.ts`:`vi.stubGlobal('localStorage', 内存桩)`(见 `storeFacade.test.ts:19-27`)+ fetch spy(见 `api/client.test.ts`,注意先 `resetApiTokenForTest()`)+ `vi.useFakeTimers()`,断言 pending→running 时列表被刷新。

---

## 8. WP6 启动日志有效模型 + 桌面双启动竞态

### 8.1 启动日志 model 失真
- `server-rs/src/lib.rs` 86~99 行:"Kedai server starting" 的 `model` 字段记的是 `config.openai_model`(env 默认 `gpt-4o-mini`),但实际生效是合并 settings 后的模型。
- 改法:`AppState::new` 之后,在 "Kedai 已启动" 行补 `model`=合并 settings 后的有效模型(`state.settings` 里的 model,或 `state.config` 被 settings 覆盖后的值)。

### 8.2 桌面双启动竞态
文件:`src-tauri/src/lib.rs`
- 现象:同一秒两次 "Kedai server starting",第二次 `os error 10048` 端口绑定失败;`health_ok → bind` 是 TOCTOU;且 `tauri-start-error.log`(8/16 旧文件)未被更新——疑似失败路径没走到 `write_start_error`。
- 改法:
  1. 桌面启动 ready 后删除陈旧 `tauri-start-error.log`;
  2. 核实 `write_start_error` 调用路径(25 行附近);
  3. 竞态:`run_server` 绑定失败且 `health_ok` 为真 → 记 info 日志「检测到已有 Kedai 实例,直接复用」并照常导航显示窗口(不再 exit 1);不健康则维持现报错路径。

---

## 9. WP7 前端文案与测试补齐

- `web/src/components/AgentPanel.vue` 约 189~192 行:流程进度由「X/Y」改为——出错步骤 >0 时显示「完成 X/Y · N 步出错」(新增 `planErrorCount` computed)。
- `web/src/sseReducer.test.ts` 补 `task` 事件用例:断言无状态变更(仅事件监控消费)。
- 检查 `TaskBoard.vue`/`AgentPanel.vue`/`Sidebar.vue` 对 `partial` 的渲染(Sidebar 用 `taskStatus.ts` 自动生效;AgentPanel 阶段行自动生效)。

---

## 10. 实施顺序与最终验收

顺序:先完成 §3(WP2 收尾,恢复编译)→ §4 测试 → WP3 → WP4 → WP5 → WP6 → WP7。

每个 WP 完成跑一次 §0 的四条验证命令。

最终人工验收(重新构建后重跑「写一首关于秋天的短诗」):
1. 产出切题(诗 + 标题);
2. 空输出步骤的日志含 `finish_reason` 与 `reasoning_tokens`;
3. 侧栏状态实时跳动;
4. token 计数有数;
5. 部分失败时状态为「部分完成」。

---

## 11. 坑与注意事项(务必读)

1. **不提交 Git**(项目 AGENTS.md 约定);不改动用户 `%APPDATA%\com.kedai.app\data` 下的 settings.json/数据库(代码升级自动生效,`task_usage` 表由幂等 schema 创建)。
2. **Windows 环境**:默认 shell 是 PowerShell;`cargo` 命令在 `server-rs` 目录或 `--manifest-path` 指定;读中文文件用 UTF-8。
3. **测试 hook 陷阱**:
   - mock `[[reply:...]]` 在首个 `]]` 截断(所以含 `[[floors]]`/`[[empty]]` 的 goal 要用 `\u005b\u005b...\u005d\u005d` 转义,见 WP1 已加的回归测试写法);
   - 数组闭合 `]` 与标记闭合 `]]` 之间须留空格(见 `task_run_to_done` 注释);
   - `build_test_app` 用进程级 `OnceLock` 共享 app,同一测试进程内多个测试共享 DB 状态,测试要幂等/自清理。
4. **枚举 churn**:`LlmStreamChunk` 已新增 `Finish { reason: String }` 和 `Usage { ..., reasoning_tokens }` 两个字段,所有穷尽 match 已补 `..` 或显式 `Finish` 臂(executor.rs / reflector_integration.rs / run_scripts.rs / mod.rs)。**新增变体或字段时必须同步这些消费点**,否则编译失败。
5. **`stream_options.include_usage`** 已加到 OpenAI 兼容请求体;若个别上游实测 400,再加 `KEDAI_DISABLE_STREAM_USAGE=1` 开关(本期先不加)。
6. **`for_mode(Task)` 语义已变**:`agent_system_prompt` 在 task 覆盖层 `None` 时返回 `default_task_agent_prompt()`(内置任务向词,定义于 settings_service.rs),`Some("")` 返回空串(不注入)。设置页 task 模式的 Agent 提示词框现在会显示该内置默认词(可编辑/可清空),属预期行为。
