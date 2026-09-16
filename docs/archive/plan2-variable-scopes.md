# Kedai「计划二:7 作用域变量」详细设计

> 兼容对象:JS-Slash-Runner「酒馆助手」4.9.1 的 7 作用域变量模型与宏族、ST-Prompt-Template 的 setvar/getvar 语义。
> 前置:计划一(EJS 上下文真实化 RenderCtx、injectPrompt 引擎、@@ 装饰器、宏扩展、快速回复模型)已完成。
> 目标(路线图原文):「global/chat/character/preset/message/script/extension 作用域模型,融合现有 stat_data 树 + session_vars 宏表,含存储迁移与前端状态。」
> 硬约束:不引入 eval/新解释器;EJS 保持 AST 白名单子集;纯 Rust 可测试;不做前端大改;注释/文档用简体中文。
> 本设计中「事实」指代码/插件源码中可直接引证的行为(附 文件:行号);「建议」指基于经验的判断(标注 [建议])。

---

## 1. 作用域模型定义

7 个作用域各自语义、生命周期、数据来源、存储位置如下表。存储列中 `scope_variables` 为本计划新增的通用表(见 §4)。

| 作用域 | TavernHelper 语义(源码事实) | 生命周期 | 数据来源(现状) | 存储位置 | 备注 |
|---|---|---|---|---|---|
| **global** | 全局变量表 `extension_settings.variables.global`(dist `rL` global 分支) | 跨会话永久 | 无既有数据 | `scope_variables('global','')` | 首写懒创建 |
| **chat** | 当前聊天变量表 `S.variables`(dist `rL` chat 分支) | 会话生命周期 | `session_assistant_vars`(stat_data 树)+ `session_vars`(扁平宏表) | `session_assistant_vars`(树,保留原表)+ `session_vars`(扁平,保留原表) | 两套既有存储在此融合,见 §3 |
| **character** | 角色卡 `settings.variables`(dist `rL` character 分支) | 角色生命周期 | 角色卡 V2 JSON `data_raw.extensions.variables`(首次读取时种子) | `scope_variables('character', char_id)` | 写不落回角色卡 `data_raw` [建议] |
| **preset** | 预设 `settings.variables`(dist `rL` preset 分支) | 预设生命周期 | 无既有数据(ST 预设导入时预留,见 `parsing/preset.rs:28-126` 现有解析不含 variables) | `scope_variables('preset', preset_id)` | preset_id 取楼层配置 id [建议] |
| **message** | 消息楼层 `chat[msg].variables[swipe]`(dist `rL` message 分支;ST `variables.ts:330-369` 默认写 message) | 消息生命周期 | `messages.extra.mvu.stat_data`(既有快照,`engine/mod.rs:574-576` 写入) | 回填镜像到 `scope_variables('message', msg_rowid)`;读时消息表优先、`extra.mvu` 兜底 | kedai 无 swipe,以消息 rowid 为 scope_id |
| **script** | 脚本数据 `getScriptId()` 定位的 `data`(dist `rL` script 分支;`@types/function/variables.d.ts:23-28`) | 脚本生命周期 | 无(kedai 无 JS 脚本运行时,硬约束不引入 eval) | `scope_variables('script', script_id)` | 预留存储层;不参与宏读链 |
| **extension** | 扩展表 `ht[extension_id]`(dist `rL` extension 分支;`variables.d.ts:29-34`) | 扩展生命周期 | 无 | `scope_variables('extension', ext_id)` | 预留存储层;不参与宏读链 |

要点说明:

1. **宏族只覆盖 5 个作用域**(message/chat/character/preset/global),script/extension 无对应宏 —— 依据 dist 宏注册正则 `{{get_(message|chat|character|preset|global)_variable::…}}`(dist 628552-628730)与 `@types/function/macro_like.d.ts`。因此 script/extension 作用域在本计划中只做「存储层 + API」落地,不进入宏与 EJS 读链 [建议,与插件事实一致]。
2. **message 作用域的数据语义**:TavernHelper/ST 中消息变量是「该消息时刻的变量状态」(ST 的 `clonePreviousMessage`,`variables.ts:798-818`,每条新消息克隆前序状态)。kedai 现有 `messages.extra.mvu.stat_data` 恰是每条 assistant 消息生成后的整树快照,语义吻合,作为 message 作用域天然数据源。
3. **global 的兼容落点**:kedai 目前没有任何跨会话变量(settings.json 是配置非变量)。新增 global 作用域不破坏任何既有行为,只扩大能力。

---

## 2. 读写规则与优先级

### 2.1 读优先级(合并视图)

参考 TavernHelper `getAllVariables` 的合并顺序(global→character→script→chat→message,dist `oL` 于 522421;文档注释见 `@types/iframe/variables.d.ts:3-8`)与 ST `precacheVariables` 顺序(global→initial→chat→message,`variables.ts:52-59`),定义 kedai 统一读视图 **View**,由低优先级到高优先级逐层覆盖:

```
global < preset < character < chat扁平(session_vars) < chat树(stat_data) < message
```

具体语义:

- 任一读取入口(宏 `get_*_variable`、EJS `getvar`、`variables` 常量)对路径 `p` 的查找顺序:**message 表 → chat 树 → chat 扁平表 → character → preset → global**,返回第一个命中(点路径语义,复用 `vars.rs:54-60` 的 `get_value`)。
- **message → chat 的强制回退规则**[建议,兼容性关键]:`{{get_message_variable::path}}` 在消息表未命中时回退 chat 树。理由:kedai 现有卡片大量使用 `{{get_message_variable::stat_data.好感度}}` 读会话树(计划一产物),若严格只读消息表会大面积丢失值。这是对插件语义的**唯一有意偏离**,以兼容既有内容为优先,回退链保证行为是现有行为的超集。
- **script/extension 不进读链**:二者只能经作用域 API/存储显式访问 [建议]。

### 2.2 写目标规则

| 写入入口 | 目标作用域 | 规则来源 |
|---|---|---|
| EJS `setvar(key, value)`(无 scope 参数) | **message**(默认) | 与 ST `setVariable` 默认 scope='message' 一致(`variables.ts:307/329`) |
| EJS `setGlobalVar` / `setLocalVar` / `setMessageVar` | global / chat / message | ST 别名语义(`ejs.ts:278-285`) |
| EJS `setvar('stat_data.…')` 或路径已存在于 chat 树 | **chat**(兼容覆盖) | [建议] 保持计划一「树路径写 chat」既有行为,避免已上线的 `@@var`/EJS 内容行为漂移 |
| EJS `incvar/decvar/addvar` | 先按读链定位变量所在作用域就地写;不存在 → 默认 message | ST `increaseVariable` inscope/outscope 逻辑(`variables.ts:641-672`)与默认写 message 一致 |
| 宏 `{{setvar::k::v}}` / `{{addvar::k::v}}` | **chat 扁平表**(session_vars),保持现状 | 兼容既有预设内容与 `LAST_*` 统计变量(`macros.rs:150-195`、`engine/mod.rs:1656-1684`) |
| `{{get_*_variable::}}` / `{{format_*_variable::}}` 宏 | 只读,不写 | 插件语义 |
| `<UpdateVariable>` / `update_variables` 工具 | **chat 树**(现状) | 兼容计划一协议(`vars.rs:112-136`、`tools/variables.rs:36-51`) |

### 2.3 同名覆盖策略

- 读:按 §2.1 优先级,高层作用域同名变量**遮蔽**低层(读到的即优先级最高的值)。
- 写:写入口显式指定作用域时直接写目标作用域,不读不覆盖其他作用域;**不跨作用域传播**(写 message 不会影响 chat 树)。这是插件语义:各作用域独立(`sL` 按 type 各写各的,dist 522421)。
- 兼容例外:§2.2 中「stat_data.* 或已存在于 chat 树 → 写 chat」属于显式兼容规则,优先级高于「默认写 message」。

---

## 3. 融合方案(现有 stat_data 树 + session_vars 宏表 → 7 作用域)

### 3.1 映射总则

1. **保留原字段/原表,不删不迁**:`session_assistant_vars` 继续作为 **chat 作用域树**的规范存储,`session_vars` 继续作为 **chat 作用域扁平层**的规范存储,全部读写入口(`session_service.rs:348-431`、`tools/variables.rs`、`api/sessions.rs:420-437`)**一行不改**。兼容成本为零,这是「融合」而非「重构」。
2. **新增一层作用域容器 `ScopeVars`**(建议新文件 `server-rs/src/parsing/scopes.rs`,归属 L2 中层,参照 `ARCHITECTURE-3H.md` 三结合分层):持有
   - `chat_tree: AssistantVars`(与 `session_assistant_vars` 同步),
   - `chat_flat: HashMap<String,String>`(与 `session_vars` 同步),
   - `others: HashMap<(Scope, ScopeId), Value>`(global/character/preset/message/script/extension,懒加载)。
   - 提供 `view(path) -> Option<Value>`(§2.1 合并视图)、`write(scope, path, value)`(§2.2)、`format_scope(scope, path)` 等纯 Rust 方法。
3. **`AssistantVars` 类型不动**(它是 L1 老层、被计划一多处引用),`ScopeVars` 以可选字段挂接到 `RenderCtx` 与 `MacroCtx`,见 §6。
4. **stat_data 前缀兼容**:`split_path`(`vars.rs:144-171`)已剥离 `stat_data.` 前缀。在新模型中,`stat_data` 视作 **chat/message 作用域的虚拟根别名**——读 `get_message_variable::stat_data.x` 等价于在 message(回退 chat)作用域读 `x`,规则不变,只是作用域定位变化。
5. **message 作用域数据来源**:优先读 `scope_variables('message', rowid)`(回填自 `extra.mvu`),缺失回退 `messages.extra.mvu.stat_data`(原样读取,`session_service.rs:255-282` 已提供 `merge_message_extra` 同类访问),再回退 chat 树。
6. **character 作用域种子**:首次读取 `character` 作用域时,若 `scope_variables` 无记录,尝试从 `characters.data_raw.extensions.variables`(`models/types.rs:7-32` data_raw 无损保留)提取作为种子并落库 [建议]。这是 V2 角色卡规范字段,属于插件生态通用约定。

### 3.2 各作用域字段归属示例

| 现有数据 | 新模型归属 |
|---|---|
| `session_assistant_vars.data_raw`(整树) | chat 作用域树 |
| `session_vars` 全部键(含 `LAST_SEND_TOKENS` 等) | chat 作用域扁平层(读时并入视图,写时仍落扁平表) |
| `messages.extra.mvu.stat_data` | message 作用域(回填镜像 + 读兜底) |
| `characters.data_raw.extensions.variables` | character 作用域(种子) |
| — | global / preset / script / extension(全新,懒创建) |

---

## 4. 存储迁移

### 4.1 新增表

沿用项目「`CREATE TABLE IF NOT EXISTS` + 启动幂等回填」模式(`models/db.rs:6-126`),在 `CREATE_TABLES` 追加:

```sql
-- 7 作用域变量通用表:scope=global|chat|character|preset|message|script|extension
-- chat 作用域以 session_assistant_vars 为规范存储,本表仅为兼容镜像(可选);
-- 其余作用域以本表为规范存储。
CREATE TABLE IF NOT EXISTS scope_variables (
  scope      TEXT NOT NULL,
  scope_id   TEXT NOT NULL DEFAULT '',
  data_raw   TEXT NOT NULL DEFAULT '{}',   -- 该作用域变量对象(点路径访问,与 stat_data 约定一致)
  updated_at TEXT NOT NULL,
  PRIMARY KEY (scope, scope_id)
);
```

> 说明:message 作用域以 `scope_id = messages.id`(整数转字符串)。`extra.mvu` 列不删,保留作为消息快照与前端回放数据源(`web/src/mvu/mvuStore.ts:30-44` 依赖它)。

### 4.2 启动回填(幂等)

在 `Db::open`(`models/db.rs:133-149`)执行 `CREATE_TABLES` 后追加幂等回填(全部 `INSERT OR IGNORE`,可重复执行):

1. chat 镜像:`INSERT OR IGNORE INTO scope_variables(scope,scope_id,data_raw,updated_at) SELECT 'chat', session_id, data_raw, updated_at FROM session_assistant_vars;`
2. message 回填:遍历 `messages`,凡 `extra` 为 JSON 且含 `mvu.stat_data` 的,取其 `stat_data` 子树写入 `scope_variables('message', id)`(Rust 侧逐行 `serde_json` 解析;一次性量小,不追求 SQL 表达式)。
3. 无既有数据的作用域(global/character/preset/script/extension)不做回填,首写懒创建。

### 4.3 数据合并工具兼容(`migration.rs`)

事实: `merge_data_dirs`(`migration.rs:72-113`)要求两侧数据库 schema 逐表逐列一致,否则报错「基线缺少源表/表 schema 冲突」(`migration.rs:180-192`)。新表加入后,旧版本数据库与新版本数据库合并会触发该错误。缓解方案 [建议]:

1. **合并前对齐 schema**:`merge_databases`(`migration.rs:173-205`)在比对 schema 之前,对基线库与源快照各执行一次 `CREATE_TABLES` DDL(或仅执行本表 `CREATE TABLE IF NOT EXISTS scope_variables`),保证两侧 schema 一致。
2. **复合主键冲突策略**:`merge_table` 对复合主键内容冲突采取「保守停止」(`migration.rs:257-259`)。`scope_variables` 主键为 `(scope, scope_id)`,合并两侧同名作用域时**保留基线并跳过**,参照 `global_usage` 的处理方式(`migration.rs:262-265`),保证合并幂等且不停止。
3. `logical_database_signature`(`migration.rs:115-140`)已把 schema 纳入哈希,新增表自动改变签名,幂等合并机制(`read_merge_signatures`)不受影响。

### 4.4 前端存储

事实:前端变量状态全在内存 Pinia(`store.ts:91-94`),持久化走后端 API,无 localStorage/IndexedDB 变量存储(仅脚本授权在 `store.ts:79`)。因此**前端无需任何存储迁移**;仅当后续「变量查看/编辑面板」计划需要展示多作用域时,经 §5 新增 API 拉取,见 §7。

---

## 5. API 与宏

### 5.1 后端 API(新增,均为增量)

| 接口 | 语义 | 说明 |
|---|---|---|
| `GET /api/variables?session_id=&scope=&scope_id=` | 读指定作用域整表(JSON) | scope 缺省 = `chat`(保持既有语义);message 作用域回退链见 §2.1 |
| `PUT /api/variables` | 整树覆写指定作用域 | body `{scope, scope_id?, data}`;`scope=chat` 时内部走 `save_assistant_vars`(`api/sessions.rs:420-437`)保持既有路径 |
| `PATCH /api/variables` | 对指定作用域应用 JSON Patch 子集 | 复用 `apply_patches`(`vars.rs:112-136`)与 `tools/variables.rs:54-109` 的校验逻辑 [建议] |
| 既有 `PUT /api/chat/sessions/{id}/assistant-vars`、`PATCH /api/chat/messages/:id/variables` | 保持原样,语义上分别成为 chat、message 作用域别名 | 不改,兼容前端 |

`SseEvent::Vars`(`models/types.rs:369-371`)不变,仍推送 chat 树;可选扩展字段 `scoped`(map[scope]→data)留给脚本/扩展作用域主动推送 [建议,本轮可不做]。

### 5.2 宏族(parsing/macros.rs)

在 `expand_one`(`parsing/macros.rs:91-212`)扩展:

- 新增 `get_chat_variable::` / `get_character_variable::` / `get_preset_variable::` / `get_global_variable::`(及 `format_*` 变体),按前缀读对应作用域,缺失回退链(§2.1)。
- `get_message_variable::` / `format_message_variable::`(`macros.rs:196-204`)升级为「message 优先、chat 回退」,`format` 输出沿用 `AssistantVars::format`(`vars.rs:99-107`)的 YAML 风格。
- `getvar::`(`macros.rs:134-149`)改为读合并视图(现有「树优先、扁平兜底」逻辑自然推广为 §2.1 全链,无需改既有分支)。`setvar::`/`addvar::`(`macros.rs:150-195`)行为不变(仍写扁平表)。
- `MacroCtx`(`macros.rs:11-25`)新增字段 `scopes: Option<&mut ScopeVars>`;`None` 时走既有逻辑,零行为变化。

### 5.3 EJS builtin

- `env.rs:builtin_global`(`parsing/assistant/ejs/env.rs:104-185`)新增: `getCharacterVar`/`getPresetVar`/`getMessageVar`/`getGlobalVar`(现有 `getGlobalVar`/`getMessageVar` 别名当前一律映射 `GetVar`,`env.rs:109-115`,本计划改为真实作用域语义——仅在 `scopes` 存在时生效,缺省回退旧行为)。`setGlobalVar`/`setLocalVar`/`setMessageVar`(`env.rs:144-147`)同理。
- `value.rs` 的 `Builtin` 枚举(`ejs/value.rs:36-39`)新增 `GetCharacterVar/GetPresetVar/GetMessageVar/GetGlobalVar/SetGlobalVar/SetMessageVar` 等成员(或复用现有 `GetVar/SetVar` 加作用域参数 [建议],实现时择一;推荐复用枚举加参数,减少分支面)。
- `exec.rs:call_builtin`(`ejs/exec.rs:277-292`)分发到 `ScopeVars` 读写;`variables` 常量(`env.rs:47-52`)在 `scopes` 存在时改为合并视图,否则保持 `vars.tree()`。
- `@@var` 装饰器(`parsing/assistant/ctx.rs:140-176`)写路径不变(写 chat 树),因 `stat_data.*` 兼容规则(§2.2)天然成立。

---

## 6. 引擎接线点

1. **加载**:`AgentEngine::run`(`agents/engine/mod.rs:415-416`)在现有 `load_assistant_vars`/`load_session_vars` 之外,由 `ScopeVars::load(db, session_id, character_id)` 一并加载 7 作用域(character 作用域含 data_raw 种子逻辑)。
2. **RenderCtx 扩展**:`RenderCtx`(`parsing/assistant/ctx.rs:78-91`)新增可选字段 `pub scopes: Option<&mut ScopeVars>`(或经 `RenderCtxData` 携带只读视图 + 可变写句柄 [建议]);`RenderCtx::new`(`ctx.rs:85-90`)保持单一 `vars` 参数签名不变,`scopes` 由 `collect_context` 构造时按需附加。**计划一的既有调用点全部不动**(`collect_generate_entries_with`、`render_assistant_content_with`、`apply_entry_decorators` 的 `RenderCtx` 构造不传 scopes 即走旧路径)。
3. **Env 扩展**:`Env`(`ejs/env.rs:20-30`)新增 `scopes: Option<&mut ScopeCtx>`;`Env::new`(`env.rs:35-60`)签名扩展,缺省 `None`。EJS 渲染无 scopes 时,`getvar`/`setvar` 完全走现有 `env.vars`(chat 树)路径——保证计划一测试(`parsing/assistant/mod.rs` 内大量 `render_assistant_content` 用例)零改动通过。
4. **宏上下文**:`build_llm_messages_with_position`(`agents/engine/messages.rs:164-199`)构造的 `MacroCtx` 附带 `scopes`;`finalize_messages`(`engine/mod.rs:968-1068`)把 `ScopeVars` 传入。`setvar::` 宏仍只写 `rctx.session_vars`(扁平),`get_*_variable` 宏读合并视图。
5. **持久化**:本轮渲染后,引擎把 `ScopeVars` 的 chat 树/扁平层照旧落库(现有 `save_assistant_vars`/`save_session_vars` 调用点不动);message 作用域在 `<UpdateVariable>` 应用后镜像写入 `scope_variables('message', msg_id)`(与 `extra.mvu` 双写,见 §9 风险 4)。global/character/preset 在写入后立即落库。
6. **与计划一衔接**:计划一的 `RenderCtx.vars: &mut AssistantVars` 就是 chat 作用域树,语义不变;本计划只在其上叠加 `scopes` 可选层。**不冲突,无重复状态**(chat 树的唯一权威仍在 `session_assistant_vars`)。

---

## 7. 前端状态变更(最小集)

- **不改动**:`store.ts` 的 `mvuVariables`(`store.ts:91-94`)、`loadHistory` 回放(`store.ts:241-252`)、`applyMvuUpdate`(`store.ts:268-288`)、`resendMessage` 变量回滚(`store.ts:428-445`)全部不动——chat 树仍是前端唯一的变量显示/回放对象。
- **可选新增(本轮可留空)[建议]**:`store.ts` 增加 `scopeVars: ref<Record<string, unknown>>({})`,仅当「变量查看/编辑面板」计划落地时经 `GET /api/variables` 按需加载各作用域;默认不请求,不影响现有行为。
- `sseReducer.ts` 的 `vars` 分支(`sseReducer.ts:170-178`)保持只吃 `event.stat_data`;如 §5.1 的 `scoped` 扩展字段落地,在此追加合并(向后兼容,缺字段时跳过)。
- **前端无 localStorage/IndexedDB 同步需求**(事实:变量存储权威在后端,见 §4.4)。
- npm 回归点:本轮前端改动为零或仅新增可选字段,`npm test` 重点回归 `web/src/mvu/variables.test.ts`、`mvuStore`(回放逻辑未动)与 `sseReducer.test.ts`。

---

## 8. 测试计划

### 8.1 Rust 单元测试(新模块 `parsing/scopes.rs`,纯函数,无 DB 亦可测)

| # | 用例 | 断言要点 |
|---|---|---|
| 1 | 合并视图优先级 | 同路径在 global/preset/character/chat扁平/chat树/message 逐层放置,读回最高层值 |
| 2 | message→chat 回退 | message 表缺路径时读回 chat 树;`stat_data.` 前缀等价路径 |
| 3 | 写目标判定 | 默认写 message;`stat_data.*` 与「已存在于 chat 树」写 chat;别名 `setGlobalVar/setLocalVar/setMessageVar` 各自落目标作用域 |
| 4 | 作用域隔离 | 写 message 不影响 chat 树;写 global 不污染其他作用域 |
| 5 | 宏族展开 | `{{get_global_variable::x}}`/`{{format_character_variable::y}}`/`{{get_preset_variable::z}}` 按作用域取值;未知路径返回空;script/extension 宏原样保留 |
| 6 | EJS scoped builtin | `getGlobalVar('x')`/`getMessageVar('stat_data.a.b')` 在新 Builtin 分发下正确取值;无 scopes 时回退旧 `getvar` 行为 |
| 7 | 迁移回填幂等 | 构造含 `session_assistant_vars`、`session_vars`、`messages.extra.mvu` 的库,跑两次回填,`scope_variables` 行数不变;`INSERT OR IGNORE` 语义 |
| 8 | 合并 schema 缓解 | 旧 schema 库 + 新 schema 库合并前补 DDL 后不再报「基线缺少源表」;复合主键冲突保留基线 |
| 9 | 无 scopes 兼容回归 | 全部既有 `parsing/assistant` 测试(`mod.rs`、`vars.rs`、`ctx.rs` 内)在 `scopes=None` 下通过,零改动 |

### 8.2 Rust 集成测试

- `tests/api_integration.rs`(或新增 `tests/variables_scopes.rs`):`PUT/GET /api/variables` 读写回环;`scope=chat` 与旧 `PUT /api/chat/sessions/{id}/assistant-vars` 结果等价。
- `tests/assistant.rs` / `tests/prompt_inject.rs`:一次完整引擎生成后,chat 树与 `scope_variables('message', msg_id)` 同步;`{{get_chat_variable}}` 在系统提示词/楼层中正确展开。

### 8.3 前端

- 无改动时:`npm test`(variables/mvuStore/sseReducer/chatStream 等)全绿即回归通过。
- 若 §7 可选字段落地:补 `sseReducer.test.ts` 中 `vars` 事件带 `scoped` 字段的用例。

---

## 9. 风险与兼容性

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| 1 | `{{get_message_variable::…}}` 语义从「会话树」变为「message 优先」 | 计划一产出卡片可能在消息表缺失时取不到值 | §2.1 强制回退链:message 缺失 → chat 树;且 message 表由 `extra.mvu` 快照回填,天然含全树 → 行为为现状超集 |
| 2 | EJS `variables` 常量可见范围变大(树 → 合并视图) | 模板可能看到之前不可见的 global/character 键 | 超集安全;且 `scopes=None` 时完全不变;文档标注差异 |
| 3 | `migration.rs` 数据合并对新增表 schema 冲突 | 旧目录合并新库直接报错 | §4.3 合并前补 DDL;复合主键按 `global_usage` 模式「保留基线跳过」 |
| 4 | message 作用域双写(`extra.mvu` 与 `scope_variables`)漂移 | 读取不一致 | 引擎为唯一写者(会话级串行,`Db.conn` 互斥);读时 `scope_variables` 优先、`extra.mvu` 兜底,漂移表现为取到较旧快照而非错误值;后续可降级为单写 |
| 5 | `getGlobalVar/getMessageVar` 等现有别名行为改变 | 现有 EJS 内容若依赖「别名=会话树」 | 仅 `scopes` 存在时启用新语义;`scopes=None`(引擎默认加载前)走旧路径;且新语义下 message 回退 chat,语义覆盖旧行为 |
| 6 | 未知变量/缺失作用域 | 宏/EJS 读取报错导致整条内容丢失 | 沿用既有容错:读不到返回 `""`/`undefined`(宏 `macros.rs:209` 未知宏保留原文、EJS `render.rs:41-52` 渲染失败回退原文);写缺失作用域首写懒创建,不报错 |
| 7 | script/extension 作用域无宏无运行时 | 插件若依赖它们无法运行 | 本计划只保证存储与 API 层;是否引入 JS 沙箱属后续独立计划(超出本轮范围),在文档中明示为能力缺口 |

**兼容性总承诺**:全部既有表、既有 API、`AssistantVars`/`RenderCtx`/`MacroCtx` 既有字段、既有宏行为均不删除不改变;新能力以「可选挂接 + 缺失回退」方式叠加。`scope_variables` 表、合并视图、作用域宏与 builtin 全部为增量。

---

## 附:关键引用索引

- 现状存储与读写:`models/db.rs:86-97`、`services/session_service.rs:348-431`
- 变量树:`parsing/assistant/vars.rs:13-17,54-77,99-107,144-171`
- EJS 接线:`parsing/assistant/ejs/env.rs:20-60,104-185`、`ejs/exec.rs:258-292`、`ejs/value.rs:36-104`
- 宏系统:`parsing/macros.rs:11-25,134-209`、`parsing/assistant/render.rs:64-77`
- 引擎:`agents/engine/mod.rs:415-424,667-962,968-1068,1656-1684`、`agents/engine/messages.rs:164-199`
- API:`api/sessions.rs:205-286,382-469`
- 迁移:`migration.rs:72-113,115-140,173-205,257-265`
- 前端:`web/src/store.ts:79,91-94,241-252,268-288,428-445`、`web/src/mvu/mvuStore.ts:30-44`、`web/src/sseReducer.ts:170-178`
- 插件(事实):JS-Slash-Runner `dist/index.js`(rL 字节 521578 / oL 522421 / sL 522421 / 宏 628552-628730)、`@types/function/variables.d.ts:1-35`、`@types/iframe/variables.d.ts:3-9`;ST-Prompt-Template `src/function/variables.ts:36-65,178-390,418-501,798-818`、`src/function/ejs.ts:268-309`

## 落地顺序建议

分阶段,每阶段可独立验证(`cargo test` 全绿 + `tests/assistant.rs` 回归为验收闸门):

- **阶段 A**:新增 `scope_variables` 表 + 启动幂等回填 + `ScopeVars` 纯函数与单测(不动任何既有代码路径)。
- **阶段 B**:宏族与 EJS builtin 升级(带 `scopes=None` 回归保护)。
- **阶段 C**:引擎接线与 message 双写同步。
