# @INJECT 精确消息插入协议

> ST-Prompt-Template(酒馆助手生态)兼容的精确消息插入协议。
> 代码事实源:`server-rs/src/agents/engine/messages/inject.rs`
> (`parse_inject_insertion` 解析 + `apply_inject_insertions` 应用);
> 条目分离与内容渲染在 `server-rs/src/agents/engine/mod.rs`
> (检索 `parse_inject_insertion`)。
> 与 MAINTENANCE.md §0 晋升表的「@INJECT 精确消息插入」条目对应。

---

## 1. 语法

@INJECT 指令写在**世界书条目的 comment(标题)**里;与 ST 原版语义一致,
**条目必须 enabled=false 才生效**(禁用条目不参与普通注入,只提供插入内容)。
条目内容先经 `render_assistant_content_with` 渲染(EJS/宏可用),
渲染后为空白的条目被跳过。

三种插入形式(参数大小写不敏感、逗号分隔、`role` 缺省为 `user`):

```
@INJECT pos=N,role=R
@INJECT target=ROLE,index=N,at=before|after,role=R
@INJECT regex=PATTERN,at=before|after,role=R
```

参数解析规则(以 `parse_inject_insertion` 实现为准):

- comment 中找不到 `@inject`(大小写不敏感)或其后无参数 → 返回 None(不解析)。
- 每个 `key=value` 对独立解析;空值、未知 key 静默忽略。
- 优先级:`pos` > `target+index` > `regex`——`pos` 存在即按 Pos 处理;
  否则 `target` 与 `index` 同时存在才按 Target;再否则 `regex` 存在按 Regex;
  都不满足返回 None(如 `@INJECT pos=abc` 解析失败 → None)。
- `at` 仅接受 `before`/`after`(大小写不敏感),缺省 after;非法值视为未提供。
- `role` 为插入消息的角色(缺省 `user`),与 `target` 的目标角色是两个参数。

## 2. 插入位置语义(apply_inject_insertions)

- **system 锚定**:system 消息始终保持在数组开头;`pos`/`target` 的索引
  只针对**非 system 消息**计数,避免破坏系统提示词首位。
- **Pos**:`pos=0` → 第一条非 system 消息之前;`pos=-1` → 最后一条消息之后;
  负值从尾部数(`rel = n + pos + 1`);最终位置 clamp 到 `[0, n]`(以代码实现为准)。
- **Target**:`target=ROLE` 指定目标角色(该角色的 system 消息不参与);
  `index` 从 **1** 开始,`-1` 表示该角色最后一条(负值从尾部数);
  `at=before|after` 插到目标消息之前/之后。
- **Regex**:pattern 编译为大小写不敏感正则,匹配**第一条**命中的
  非 system 消息,插到其前/后。
- **应用顺序**:所有插入先计算目标完整索引,升序排序后**从后往前**执行
  (先插后面的位置,前面索引不漂移),与 ST 原版「位置从后往前执行」一致。
- **容错**:内容空白、正则编译失败、目标角色不存在、index 越界 →
  **静默跳过该条**,不 panic、不整批失败。
- 插入的消息为 `LlmMessage::plain(role, 渲染后内容)`。

## 3. 执行时机

@INJECT 在引擎**消息构建期**应用:条目分离与渲染完成后、上下文裁剪
(`trim_to_context`)**之前**(位置以构建期消息数组为准;裁剪后再插入会被切掉
或定位漂移,见 engine/mod.rs 注释)。

GENERATE 的两个变体也转为本机制(见 docs/generate-render-protocol.md §3):

- `[GENERATE:{idx}:BEFORE]` → `Pos { pos: idx, role: "user" }`
- `[GENERATE:{idx}:AFTER]` → `Pos { pos: idx + 1, role: "user" }`
- `[GENERATE:REGEX:pattern]` → `Regex { pattern, at: After, role: "user" }`

同批 @INJECT 与 GENERATE 转换条目合并进同一 placements 列表,
统一按「从后往前」语义插入。

## 4. 示例

```jsonc
// 绝对位置:第一条非 system 消息之前插一条 system 级注入
{ "comment": "背景注入 @INJECT pos=0,role=system", "content": "……", "enabled": false }

// 尾部追加(缺省 role=user)
{ "comment": "结语提醒 @INJECT pos=-1", "content": "(别忘了现在是台风夜)", "enabled": false }

// 目标消息:第一条 assistant 回复之前插一条 assistant 消息
{ "comment": "@INJECT target=assistant,index=1,at=before,role=assistant", "content": "……", "enabled": false }

// 正则:第一条提到「图书馆」的消息之后补充设定
{ "comment": "@INJECT regex=图书馆,at=after", "content": "(图书馆周三闭馆)", "enabled": false }
```

无效示例(全部静默跳过,消息数组不变):

```
@INJECT pos=abc            # pos 解析失败 → None,不进插入列表
@INJECT                    # 无参数 → None
@INJECT regex=(            # 正则编译失败 → 应用期跳过
@INJECT target=assistant,index=9   # 越界 → 应用期跳过
```

## 5. 测试锚点

`messages/inject.rs` 内测试覆盖:pos/负 pos 解析、target/regex 解析、
缺省 role、大小写不敏感、pos=0 插首条非 system、pos=-1 插尾部、
target before、regex 匹配第一条、多条从后往前、无效/空白跳过。
