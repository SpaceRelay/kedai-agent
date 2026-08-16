# Kedai 示范 Skill 与工具插件

本目录是 Kedai 的**示范模板**,演示两套可扩展体系的标准写法:

| 体系 | 文件位置 | 生效方式 | 用途 |
|---|---|---|---|
| Skill 库 | `examples/skills/*.json` | `POST /api/skills` 导入(SQLite,同名覆盖) | 给 agent 的提示词技能,agent 用 `read(type=skill)` 按名称/关键词读取 |
| 工具插件 | `examples/plugins/tools/*.json` | 复制到 `data/plugins/tools/` 后 `POST /api/plugins/tools/reload`(或 `POST /api/plugins/tools/upload` 上传) | agent 模式可调用的自定义工具(白名单脚本,不使用 eval) |

> `data/` 已被 `.gitignore` 忽略,因此模板放在 `examples/`,需要时复制过去即可。

## 一、示范 Skill:写作风格指南

文件:`examples/skills/写作风格指南.json`。

- **作用**:deep/agent 模式下,agent 续写/润色前调用 `read`(type=skill) 加载规范,约束文风。
- **示范点**:
  1. 字段结构:`{ name, description, content }` —— `name` 唯一且语义化,`description` 给 agent 判断何时读取,`content` 是正文;
  2. 正文推荐结构:适用范围 → 规则 → 正反示例 → 使用方式 → 元信息;
  3. `name` 与 `content` 里放足关键词(文风/风格/写作/小说/润色),便于 `read` 按 `keywords` 检索。

导入方式(服务运行中):

```powershell
curl.exe -X POST http://127.0.0.1:3001/api/skills `
  -H "Content-Type: application/json" `
  --data-binary "@examples/skills/写作风格指南.json"
```

验证:`curl.exe http://127.0.0.1:3001/api/skills`,删除:`DELETE /api/skills/{id}`(id 从列表取)。

## 二、示范工具插件

### 1. `score_eval.json` — 评分判定(简单)

- **作用**:给草稿/回答打分判定,返回 `{passed, grade, score}`,适合创作自检。
- **示范点**:`parameters` 用 OpenAI 兼容 JSON Schema;`script` 用 `result = <表达式>` 形式,演示**三元表达式 + 对象字面量返回**(对象自动 JSON 序列化)。

### 2. `clamp.json` — 数值限幅(进阶)

- **作用**:把数值约束在 `[min, max]` 区间,适合限制生成参数/章节字数不越界。
- **示范点**:演示**嵌套白名单函数调用** `Math.max(args.min, Math.min(args.max, args.value))`。

启用方式(两种任选):

```powershell
# 方式 A:复制文件到运行时目录后热重载
Copy-Item examples/plugins/tools/*.json data/plugins/tools/
curl.exe -X POST http://127.0.0.1:3001/api/plugins/tools/reload

# 方式 B:直接上传(API 会写入 data/plugins/tools 并注册)
curl.exe -X POST http://127.0.0.1:3001/api/plugins/tools/upload -F "file=@examples/plugins/tools/score_eval.json"
```

验证:`curl.exe http://127.0.0.1:3001/api/plugins/tools` 的 `tools` 列表里出现 `score_eval` / `clamp`。

## 三、脚本语法边界(重要)

白名单求值器只支持以下能力,超出会**报错或静默返回错误结果**:

| 能力 | 示例 | 说明 |
|---|---|---|
| 单级参数读取 | `args.name` / `args.score` | **只支持一层属性**,多级链 `args.text.length` 会静默返回空 |
| 字面量 | `"文本"` / `42` / `true` / `null` / `{...}` / `[...]` | 对象/数组字面量可嵌套,值须为下方「可用表达式」 |
| 算术 | `1 + 2 * 3` / `"a" + "b"` | 仅字面量间可算;**变量不能参与算术**(`args.a + 1` 静默返回空) |
| 比较 + 逻辑 + 三元 | `args.score >= args.pass ? "合格" : "待改进"` | 三元在表达式**最外层**时可用 |
| 白名单函数 | `Math.max/min/floor/ceil/round`、`JSON.stringify/parse`、`String`、`Number` | 参数须为单级变量或字面量 |

**常见陷阱**:

1. **整段脚本不要用引号包住**:`return '你好,' + args.nickname + '...'` 会被整体当作字符串字面量返回,参数不展开(现有 `data/plugins/tools/greeting.json` 即有此问题)。
2. **成员访问只有一层**:`args.text.includes(x)`、`args.text.length` 都不可用;需要字符串方法时,先把字符串放进参数顶层再配合 `JSON.stringify` 处理。
3. **返回值**:字符串原样返回;对象/数组/数字自动 JSON 序列化,模型可直接解析。

> 在 `server-rs/src/plugins/mod.rs` 的 `#[cfg(test)] mod tests` 里可以直接加脚本用例跑 `cargo test tool_plugin_`,验证新插件脚本的真实行为。

## 四、写新模板的检查清单

- [ ] `name` 唯一、小写英文/下划线(插件);中文可读名(skill)
- [ ] `parameters` 每个属性写清 `type` 与 `description`(模型据此填参)
- [ ] `script` 只用上表「可用表达式」,避开三种陷阱
- [ ] 用 `cargo test tool_plugin_` 或运行时 reload + `GET /api/plugins/tools` 验证
- [ ] description 里给出 1 个调用示例,降低模型误用率
