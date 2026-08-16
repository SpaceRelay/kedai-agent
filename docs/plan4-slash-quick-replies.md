# Kedai「计划四:slash 命令系统 + 快速回复 UI」详细设计

> 兼容对象:JS-Slash-Runner「酒馆助手」4.9.1 的 slash 命令生态(triggerSlash/命令解析)与快速回复(Quick Replies)管理形态。
> 前置:计划一(EJS 上下文真实化、getqr 渲染数据源、快速回复后端模型)与计划三 3b(角色卡脚本 iframe 沙箱、EvalBridge TavernHelper 兼容桥)已完成。
> 目标(路线图原文):「/命令 解析/注册/执行、输入框联想、快速回复管理面板。」
> 硬约束:不引入 eval/新依赖;既有 triggerSlash 行为向后兼容(阶段三 11 个 bridge 测试不破坏);注释/文档用简体中文;不提交 git。
> 本设计中「事实」指代码中可直接引证的行为(附 文件:行号);「建议」指基于经验的判断(标注 [建议])。

---

## 1. 背景结论(基于探索)

- **快速回复后端已完备**(计划一):`quick_replies` 表 + `QuickReplyService`(`services/quick_reply_service.rs:82-176`)+ `/api/quick-replies` CRUD(`api/quick_replies.rs`)+ `getqr` 渲染数据源已接引擎(`parsing/assistant/ejs/exec.rs:641-653`);缺前端管理 UI 与 API 封装(前端 `src/api/` 原无 quickReplies 模块、store 未接线)。
- **slash 现状**:阶段三 3b-2 已在 `scripts/bridge.rs:178-215` 写死最小实现(`/echo /var /setvar /getvar` 按空白拆分 + `split_key_value`),无注册表、无命令清单 API、无前端联想;`SlashHandler = dyn Fn(&str) -> String` 类型已存在(`bridge.rs:14`),但引擎构造 `EvalBridge` 时**未挂接**(`agents/engine/mod.rs:1819-1820` 仅 `.with_character`)。
- **输入框现状**:`ChatInput.vue` 的 textarea 无 ref、`onKeydown`(96-101)仅处理 Enter 发送;项目无通用下拉组件(UI 自绘,style.css 设计系统变量 + Tailwind v4)。

## 2. 交付物总览

| 模块 | 交付物 | 独立可测 |
|---|---|---|
| 4a 后端 | `server-rs/src/slash/`(注册表 + 内置命令 + 参数解析)、EvalBridge 接线、`GET /api/slash/commands` | ✅ registry 单测 + 集成测试 + bridge 回归 |
| 4b 快速回复 UI | `web/src/api/quickReplies.ts` + `QuickRepliesModal.vue` + store 接线 + ChatInput 快捷填入 | ✅ quickReplies API 单测 |
| 4c 输入框联想 | ChatInput 内联下拉 + `slashSuggest.ts` 纯函数 | ✅ filterCommands 单测 |
| 文档 | `docs/plan4-slash-quick-replies.md`(本文件) | — |

---

## 3. 4a slash 命令后端

### 3.1 模块结构

新增 `server-rs/src/slash/`(`mod.rs` + `registry.rs`),顶层模块在 `src/lib.rs` 声明(`pub mod slash;`)。

- **`SlashCommandMeta`**:`{ name, description, params }`(name 不含前导 `/`),`Serialize` 供 API 与前端联想。
- **`SlashCommand`**:`{ name, description, params, handler }`,`handler = dyn Fn(&str, &mut ScopeVars) -> String + Send + Sync`。签名带 `ScopeVars`,因为变量命令(`/var /getvar /addvar`)需读写当前脚本运行时的变量容器;参数为命令名之后的**原始文本**,还原由各 handler 经 `parse_args` 按需处理。
- **`SlashRegistry`**:`RwLock<HashMap<String, Arc<SlashCommand>>>`;`new()` 注册内置命令并返回 `Arc<Self>`;`register()` 供计划六扩展;`list() -> Vec<SlashCommandMeta>`;`execute(raw, vars) -> Option<String>`(未知命令返回 `None` 由调用方兜底)。

### 3.2 内置命令与 /help 特判

| 命令 | 用法 | 行为 |
|---|---|---|
| `/echo` | `<text>` | 原样回显参数 |
| `/var` | `<key> :: <value>` | 写单个键进 global 作用域(**合并写入**,不覆盖其他键) |
| `/setvar` | 同 `/var` | `/var` 别名,同一 handler |
| `/getvar` | `<key>` | 合并视图读取(`ScopeVars::view`),字符串原样返回、其余 JSON 化,查无返回空串 |
| `/addvar` | `<key> :: <数值>` | 对 global 数值键累加;键不存在初始化为加数;兼容 `/var` 写入的字符串数字(先 `as_f64` 再 `parse::<f64>` 兜底) |
| `/help` | — | 由 `execute` 特判格式化命令清单(避免闭包自引用注册表);`list()` 以虚拟命令补入 |

### 3.3 参数解析(纯函数,handler 与测试复用)

`parse_args(s) -> Vec<String>`:按空白分词;单/双引号包裹段视为整体;`::` 独立成 token。例:`"a b" :: 'c d'` → `["a b", "::", "c d"]`。`split_key_value(tokens)` 从解析结果提取 `key :: value`(分隔符两侧相邻 token 拼接回原值)。会话副作用 slash(`/clear` 等)与生成类 slash(`/continue` 等,计划六)**不实现**,注册表留 `register()` 扩展位。

### 3.4 接线

1. **`scripts/bridge.rs`**:`EvalBridge` 加字段 `registry: Option<Arc<SlashRegistry>>` + `with_registry()`;`trigger_slash` 优先级改为「自定义 `slash` 字段 → `registry.execute`(命中即返回;锁在块内释放避免与内置实现死锁)→ 内置最小实现 → 未知命令错误」。既有字段与 11 个内联测试零改动。
2. **`agents/engine/mod.rs`**:`AgentEngine` 加字段 `slash: Arc<SlashRegistry>` + `new()` 参数;`run_character_scripts` 构造 bridge 时 `.with_registry(self.slash.clone())`。
3. **`api/app_state.rs`**:加 `pub slash: Arc<SlashRegistry>`,`new()` 构造注册表并传入 `AgentEngine::new`(唯一调用点)。
4. **`api/slash_commands.rs` + `api/mod.rs`**:`GET /api/slash/commands` → `Json(json!({"commands": state.slash.list()}))`,路由注册在 `/api/scripts/tree` 之后。

### 3.5 4a 测试

- `registry.rs` 内联单测 13 个:`parse_args`(空白/引号/`::`/`=`)、`split_key_value`、各内置命令(echo/var 写读回/setvar 别名/var 合并不覆盖/addvar 累加与初始化/非数值拒绝/getvar 查无空串/help 含全部命令/list 6 项/未知命令 None/空命令空串)。
- `tests/api_integration.rs` 新增 `slash_commands_list`:校验 6 个命令存在且每项含 name/description/params。
- bridge 既有 11 测试回归(不含注册表,走内置实现路径)。

---

## 4. 4b 快速回复 UI

### 4.1 前端 API 封装

新增 `web/src/api/quickReplies.ts`(`request` 封装,对齐 `scripts.ts`):

| 函数 | 方法/路径 | 说明 |
|---|---|---|
| `listQuickReplies(includeDisabled?)` | GET `/quick-replies[?all=true]` | 缺省仅启用项;管理 UI 传 `true` |
| `createQuickReply(input)` | POST `/quick-replies` | 返回 `quick_reply` |
| `updateQuickReply(id, input)` | PUT `/quick-replies/{id}` | 行内改 |
| `deleteQuickReply(id)` | DELETE `/quick-replies/{id}` | 删除 |

`types.ts` 加 `QuickReplyRecord`(id/name/label/content/enabled/position/sort_order,后端契约 `quick_reply_service.rs:11-33` 的 serde default 缺省语义)+ `SlashCommandMeta`;`index.ts` 聚合导出。

### 4.2 QuickRepliesModal.vue

仿 WorldBooksModal 列表编辑模式(`components/WorldBooksModal.vue` 行内编辑 + 全量保存结构):
- 草稿数组 `drafts`(负 id = 未落库新行)+ `removedIds`(已删除的落库行)。
- 行内编辑:name / label / content / enabled(启用开关)/ position(0-4 下拉)。
- 新增 `＋ 新增`、删除、`↑↓` 排序(数组交换并重排 `sort_order` 升序)。
- 保存 diff:先 DELETE 已删除行 → 负 id 行 POST 创建 → 其余 PUT 行内改 → `store.loadQuickReplies()` 重载。
- 关闭置 `store.quickRepliesOpen = false`;onMounted 加载。

### 4.3 store / 挂载 / 入口

- `store.ts`:state 加 `quickReplies` / `quickRepliesOpen`;action `loadQuickReplies()`(仿 `loadWorldBooks`);return 块同步导出。
- `App.vue`:挂载 `<QuickRepliesModal v-if="store.quickRepliesOpen" />`。
- `SettingsHub.vue`:quickActions(30-40)在「脚本管理」之后加「快速回复」入口。

### 4.4 ChatInput 发送联动

底部工具栏「选择文件」前加「快速回复」按钮 → 弹出启用列表(`enabled && content 非空`)→ 点击**填入输入框**(空则替换、非空则追加换行,**不自动发送**,Enter 确认后发送)。onMounted 拉取启用列表(失败静默)。

---

## 5. 4c 输入框 slash 联想(ChatInput.vue 内联)

- textarea 加 `ref`(取 `selectionStart`);onMounted 拉取 `GET /api/slash/commands`(失败静默)。
- 触发判定:光标前单词(`wordBeforeCursor`,最后一个空白后的连续非空白串)以 `/` 开头且 `filterCommands` 有匹配 → 打开下拉。
- `onKeydown` 扩展:菜单打开时 ↑↓ 导航、Enter/Tab 补全、Esc 关闭(补全命中前不触发发送),再处理 Enter 发送;补全用 `/命令名` 替换光标前单词并把光标移到补全后。
- 失焦关闭;菜单用 `mousedown.prevent` 防止抢走输入框焦点。
- `filterCommands(input, list)` 抽到 `web/src/components/slashSuggest.ts` 纯函数(前缀匹配、大小写不敏感、空输入/无匹配返回空)便于单测。

---

## 6. 测试计划

| 位置 | 内容 | 命令 |
|---|---|---|
| `server-rs` lib 单测 | registry 13 个 + bridge 11 个回归 | `cargo test`(需先加载 vcvars64,见 MAINTENANCE.md:99-101) |
| `server-rs` 集成 | `GET /api/slash/commands` | 同上 |
| `web/src/api/quickReplies.test.ts` | 5 个:URL/query/方法/body(仿 scripts.test.ts 的 fetch mock + `resetApiTokenForTest`) | `npm test -w web` |
| `web/src/components/slashSuggest.test.ts` | filterCommands 5 个 + wordBeforeCursor 5 个 | 同上 |
| 构建 | 全量类型检查 + 打包 | `npm run build -w web` |

## 7. 风险与边界

| 风险/边界 | 处理 |
|---|---|
| triggerSlash 行为兼容 | 注册表优先于内置实现,但既有 bridge 测试绕开注册表走内置路径,行为不变;`slash` 自定义入口优先级最高 |
| 会话副作用/生成类 slash 未实现 | 注册表留 `register()` 扩展位(计划六生成类命令走同一接口) |
| 快速回复 getqr 渲染重复实现 | 不重复;4b 仅补管理 UI + 发送联动 |
| addvar 对字符串数字的处理 | `/var` 写入的是字符串,累加前先 `as_f64` 再字符串解析兜底 |
| 新增依赖 / eval | 零新依赖;slash handler 为纯 Rust 闭包,无 eval |
