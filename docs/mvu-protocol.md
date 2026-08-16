# mvu 变量协议（前后端双端一致性契约）

> 本文档是 kedai mvu 变量系统**前后端语义对齐的权威规范**，是「三结合梯队架构」中
> 老层兼容契约（L1）与跨端传帮带经验文本（见 `docs/ARCHITECTURE-3H.md`）。
> 双端实现：前端 `web/src/mvu/`（展示/交互层），后端 `server-rs/src/parsing/assistant/`
> （权威协议 + 持久化 + 注入 LLM）。语言不同不可合并代码，靠本文档锁定行为一致。
> 前端 `web/src/mvu/parser.test.ts` 与 `variables.test.ts` 的测试用例是**跨端验收基线**。

---

## 1. 协议概览

- **变量树**：根即 `stat_data`（会话级持久化，SQLite `session_assistant_vars` 表）。前端另有
  `display_data` 展示树（`stat_data` 镜像，仅前端交互用，不参与后端落库/注入）。
- **输出协议**：模型回复中 `<UpdateVariable>…</UpdateVariable>` 块被剥离并应用为变量补丁。
- **初始化**：世界书条目注释含 `[InitVar]` / `[InitialVariables]` 时，条目内容作为初始变量树。

## 2. UpdateVariable 块解析

- 正则：`<UpdateVariable\b[^>]*>([\s\S]*?)</UpdateVariable\s*>`，大小写不敏感、容忍属性。
- 块内格式优先级：**① `<JSONPatch>` 数组（JSONPatch 协议）→ ② `_.set(...)` 语句序列
  （MagVarUpdate 兼容）**。JSONPatch 整块解析失败时回落到 `_.set` 解析。
- 同一文本可含多个 UpdateVariable 块，全部剥离（内容不保留）并依次应用。

### JSONPatch 支持的操作（RFC 6902 子集 + 扩展）

| op | 语义 | 说明 |
|---|---|---|
| `replace` / `set` | 路径写值 | 中间对象/数组自动创建 |
| `delta` / `add` | 数值加 delta | **值为非数字时跳过整条**；路径不存在按 0 起步 |
| `insert` | 写值 | **与 replace/set 语义等价**（后端不设独立类型，并入 replace） |
| `remove` | 删除路径 | 不存在则忽略 |
| `move` | 移动 | `from` → `to`；**源缺失整条跳过**（容错优先，不整批失败） |

### MagVarUpdate `_.set` 兼容

- 语句形如 `_.set('path', old, new);//原因`，`old` 为旧值占位（可任意），`new` 为真实新值。
- 解析取路径与**最后一个实参**为新值；字符串实参需**转义还原**（`\\ \' \" \n \r \t`，未知转义保留）。
- **reason**（原因注释）：`//行注释` 与 `/* 块注释 */` 均提取为补丁的 `reason` 字段
  （前端用于展示；后端持久化不依赖 reason，但解析结果与前端一致以保证可测性）。
- 路径支持点分（`a.b.c`）与斜杠（`/a/b/c`）两种分隔，剥 `stat_data.` 前缀。

## 3. 路径规范（双端一致）

- **危险段过滤**：路径段为 `__proto__` / `constructor` / `prototype` 时整段丢弃
  （LLM 输出不可信，防御原型污染）。
- **数组访问**：数字段访问数组下标；`-` 表示数组追加。
- **自动创建**：写路径时中间标量被穿越 → 替换为对象/数组容器（下一段是数字则为数组）。
- **stat_data 前缀**：`stat_data` 或 `stat_data.xxx` 开头剥前缀；裸路径（`xxx`）等同 `stat_data.xxx`。

## 4. 格式化输出

- `{{format_message_variable::path}}` / `{{get_message_variable::path}}`：大小写不敏感；
  path 省略 = 全树。输出格式：对象/数组 YAML 风格缩进，字符串带双引号，数字/布尔裸输出，null 输出 `null`。
- `<StatusPlaceHolderImpl/>`：替换为全树格式化文本。
- `<status_current_variable>` 起止标签：剥离（内容保留）。
- 数值显示：整数（|f| < 1e15）裸输出整数，浮点保留小数。

## 5. 双端职责边界

| 层 | 职责 | 独有能力 |
|---|---|---|
| 前端 `web/src/mvu/` | 展示/交互：变量面板、快照回放、LLM 宿主全局（`Mvu`/`$`/`_`/`toastr`）、reason/oldValue 展示 | `stat_data`/`display_data` 双树、`replayMvuVariables`、`hasUpdateVariable` 快速判断 |
| 后端 `server-rs/src/parsing/assistant/` | 权威协议 + 持久化 + 注入 LLM + 两步生成状态栏 + EJS 桥接 | `AssistantVars` 持久化、`apply_patches`、EJS `get_js/set_js/add_js`、`parse_patch_array` 被 `update_variables` 工具复用 |

**红线**：
- `reason`/`oldValue` 仅前端展示用，后端不参与落库/注入（`PatchOp.reason` 解析保留但不持久化）；
- 后端是「权威」：生成流程、注入、落库均以后端解析结果为准，前端解析仅用于展示对齐；
- 双端任一改动本协议语义，必须同步更新本文档 + 双端测试（前端 parser.test.ts 为基线）。

## 6. 验收方式

- 前端：`cd web && npm test`（parser/variables 测试全绿）。
- 后端：`cargo test --manifest-path server-rs/Cargo.toml`（assistant 相关测试全绿）。
- 新增协议特性：先在**前端 parser.test.ts 加用例**（作为基线），再后端实现并加对应用例。

## 7. 演进记录

| 日期 | 变更 | 影响面 |
|---|---|---|
| 2026-08-11 | 协议文档建立；后端对齐：字符串转义还原、PatchOp.reason 字段、delta 无效值跳过、Insert 并入 replace | 后端解析层 |
