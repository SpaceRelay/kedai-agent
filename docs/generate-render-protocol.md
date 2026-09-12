# GENERATE / RENDER 内容注入协议

> ST-Prompt-Template(酒馆助手生态)兼容的内容注入标签协议。
> 代码事实源:`server-rs/src/parsing/assistant/inject_tag.rs`(标签解析 + 条目收集)、
> `server-rs/src/parsing/assistant/render.rs`(内容渲染)、
> `server-rs/src/agents/engine/mod.rs`(引擎消费,检索 `collect_generate_entries_with`)。
> 与 MAINTENANCE.md §0 晋升表的「GENERATE/RENDER 内容注入」条目对应。

---

## 1. 标签语法

标签写在**世界书条目的 comment(标题)**里,大小写不敏感、容忍前后空白,
可嵌入标题文字中(如 `分阶段人设 [Render:After] 注入` 也能识别)。

| 标签 | 解析结果(InjectTag) | 含义 |
|---|---|---|
| `[GENERATE:BEFORE]` | GenerateBefore | 注入到 system 提示词开头 |
| `[GENERATE:AFTER]` | GenerateAfter | 注入到 system 提示词末尾 |
| `[GENERATE:{idx}:BEFORE]` | GenerateIndex{idx, before:true} | 第 idx 条消息(0-based,非 system)之前插入 |
| `[GENERATE:{idx}:AFTER]` | GenerateIndex{idx, before:false} | 第 idx 条消息之后插入 |
| `[GENERATE:REGEX:pattern]` | GenerateRegex{pattern} | 正则匹配到的第一条消息之后插入 |
| `[RENDER:BEFORE]` | RenderBefore | 渲染阶段注入(见 §4 的语义差异) |
| `[RENDER:AFTER]` | RenderAfter | 同上 |
| `[InitialVariables]` / `[InitVar]` | InitialVariables | 初始变量条目,**不进**注入收集 |
| 其他 / 畸形(如 `[GENERATE:MEOW]`、`[GENERATE:abc:BEFORE]`) | None | 无标签,走普通世界书注入链路 |

解析细节(以 `parse_inject_tag` 实现为准):

- REGEX 的 pattern **原样保留**(不转大小写);`before` 固定为 true(解析层),
  但引擎消费时实际语义是「匹配消息**之后**插入」(见 §3)。
- 索引变体要求 `{idx}` 可解析为 usize 且后缀恰为 BEFORE/AFTER,否则整体判 None。
- `[InitialVariables]` / `[InitVar]` 独立优先匹配,不会被 GENERATE/RENDER 分支吞掉。

## 2. 条目收集(collect_generate_entries_with)

引擎在消息构建期对世界书条目做分流:

- 仅 **enabled** 且 comment 带 GENERATE/RENDER 标签的条目进入注入收集;
  `[InitVar]` / `[InitialVariables]` 条目分流到初始变量阶段(`collect_init_vars`);
  无标签条目走原普通世界书注入链路。
- 进入收集的条目**在分离阶段即完成渲染**(`render_assistant_content_with`,
  此时变量树已初始化),渲染结果存为 `GenerateEntry.rendered`。
- 排序:`GenerateIndex` 条目按 idx 升序(稳定排序);其余标签保持 entries 原相对顺序。

渲染管线(以 `render.rs` 的 `render_assistant_content_with` 为准),依次执行:

1. **EJS 模板渲染**:内建 `getvar/setvar/addvar` 直接读写变量树;
   `getwi/getchar/getpreset/getqr/getChatMessage` 等经渲染上下文真实读取;
   `injectPrompt(key, prompt, order?, sticky?, uid?)` 登记注入清单(见 §3)。
2. **容错降级**:EJS 渲染出错且输出为空时回退保留原文(内容不丢失,记 warn 日志)。
3. `{{format_message_variable::path}}` / `{{get_message_variable::path}}` 宏
   → 变量树格式化文本(path 可省略 = 全树)。
4. `<status_current_variable>` 起止标签剥离(内部内容保留)。
5. `<StatusPlaceHolderImpl/>` → 全树格式化文本替换。

## 3. 引擎注入时机与落点(agents/engine/mod.rs)

注入发生在**消息构建期**:变量树初始化、世界书分组之后,上下文裁剪
(`trim_to_context`)**之前**。各标签落点:

- **GenerateBefore / RenderBefore** → 拼到首个 system 消息**开头**
  (运行时主提示词 AGENTS_RUNTIME.md 之后、角色内容之前,保持系统契约首位)。
- **GenerateAfter / RenderAfter** → 拼到首个 system 消息**末尾**,
  并计入 `protected_tail`(上下文裁剪时优先保留,防注入先于正文被切掉)。
  多条按收集顺序以 `\n\n` 拼接。
- **GenerateIndex{idx, before}** → 转 @INJECT 机制:
  `InjectInsertion::Pos { pos: idx(before)或 idx+1(after), role: "user" }`,
  由 `apply_inject_insertions` 插入消息数组(详见 docs/inject-protocol.md)。
- **GenerateRegex{pattern}** → 转 @INJECT 机制:
  `InjectInsertion::Regex { pattern, at: After, role: "user" }`,
  即「正则匹配到的第一条非 system 消息**之后**插入」(该消息的回应素材)。
  注意:解析层 `before` 字段当前未被引擎消费(以 engine 实现为准)。
- **injectPrompt 登记清单**:EJS 模板内 `injectPrompt(key, prompt, order?, sticky?, uid?)`
  登记的提示词,渲染后按 `(order, position)` 排序并入 generate_after(system 尾部,
  计入 protected_tail);内容为空的登记被跳过。

## 4. RENDER 与 GENERATE 的语义差异(重要)

ST 原版中 RENDER 标签仅影响**显示渲染**、不影响发送给模型的生成内容。
**kedai 没有独立的显示渲染管道**,因此 RENDER:BEFORE/AFTER 与 GENERATE 同路
并入 system 首/尾(即也影响生成)。这是有意的兼容取舍(以 engine/mod.rs
「RENDER 仅影响显示渲染……kedai 无独立显示渲染管道,故并入生成注入」注释为准);
前端显示渲染留有扩展位(见 docs/plan6-ecosystem-devtools.md 6a 记录)。

## 5. 与变量树 / mvu 的交互

- 渲染在变量树(AssistantVars)**初始化之后**进行:条目 EJS 可读到
  `[InitVar]` 建立的初始树与会话持久树。
- EJS 内 `setvar/addvar` 的写入是**真实副作用**:渲染结束后引擎把变更同步进
  scopes chat 镜像,供后续消息构建的宏展开读取(以 engine 实现为准)。
- 条目渲染读取的是本会话变量树;树内容变化(如 mvu `<UpdateVariable>` 补丁生效后)
  会在**下一轮**渲染中反映。
- mvu 状态块(变量树快照 + 更新协议)是独立注入通道(`make_state_block_with_contract`,
  位置跟随设置 `mvu_vars_position`:system 常态组 / user_tail 激发组),
  与 GENERATE/RENDER 注入互不覆盖。

## 6. 示例

世界书条目(角色卡内嵌 character_book 或独立世界书均可):

```jsonc
{
  "comment": "[GENERATE:BEFORE] 世界观总纲",
  "content": "本作世界观:近未来海滨小城,时间循环每 7 天重置。",
  "enabled": true
}
```

→ 每轮生成时,渲染后的内容拼到 system 提示词开头。

```jsonc
{
  "comment": "好感度播报 [GENERATE:REGEX:好感|心情]",
  "content": "<% const v = getvar('stat_data.affection') || 0; %>当前好感度:<%= v %>",
  "enabled": true
}
```

→ 历史中第一条匹配 `好感|心情`(大小写不敏感)的非 system 消息之后,
插入一条 user 角色的渲染结果(EJS 读取变量树求值)。

```jsonc
{
  "comment": "[GENERATE:0:AFTER] 开场补充设定",
  "content": "(补充:开场白发生时正值台风夜)",
  "enabled": true
}
```

→ 第 0 条非 system 消息(通常为首条 user/assistant)之后插入,role=user。

```jsonc
{
  "comment": "[RENDER:AFTER] 状态栏模板",
  "content": "{{format_message_variable::stat_data}}",
  "enabled": true
}
```

→ 变量树 stat_data 子树格式化文本拼到 system 末尾(计入 protected_tail);
注意 kedai 中 RENDER 也影响生成(见 §4)。
