# 角色模式 bug 修复变更说明(2026-09-06)

> 注:本文档为 2026-09-06 的历史快照,其中测试计数(如 vitest 445/445)为当时数值;
> 最新计数以 MAINTENANCE.md 为准(2026-09-07 实测:cargo 805 / vitest 485)。

本次修复覆盖用户报告的 5 个 bug 及排查中发现的隐藏问题,另含实测阶段新发现的 3 个
阻断性 bug。全部修复均经真实应用(IAB 实测)或探针环境验证。

## 用户报告的 5 个 bug

### 1. 重进后角色聊天记录丢失
- **根因(三重叠加)**:① 前端不记住上次角色/会话,重进永远落在最新创建的角色;
  ② 双数据目录分叉:桌面版写 `%APPDATA%\com.kedai.app\data`,浏览器版写 `项目\data`,
  两个库各自写入,一边写的聊天另一边永远看不到;③ 加载失败只 console 不提示。
- **修复**:记住上次位置(localStorage 存最后角色 id + 每角色最后会话 id,启动恢复);
  数据目录统一 `%APPDATA%\com.kedai.app\data`,项目库数据经一次性合并工具并入
  (合并前双备份、dry-run 验证);`/api/health` 增加 `data_dir`;加载失败显示可见错误条。
- **实测**:wuwa 卡聊 2 轮 → 刷新页面 → 角色/会话/消息/token 统计原样恢复。

### 2. 提示词查看功能失效(三个入口)
- **根因**:`web/src/store.ts` 门面漏转发 `queueSettingsSave`,OptimizeModal 调用必抛
  TypeError 且无 UI 反馈;弹窗懒加载 chunk 失败无兜底。
- **修复**:门面补转发 + 新增「门面完整性」回归测试(遍历子 store 导出键断言门面全部
  暴露);`defineAsyncComponent` 弹窗加加载失败兜底(提示+重试)。
- **实测**:综合设置→Agent 设置→最终提示词预览 ✓;提示词管理(6 层顺序面板)✓;
  优化面板→提示词检查(分层预览 #0 runtime_prompt ≈702token / #1 custom_template
  ≈1210token)✓。

### 3. 吸血鬼卡下载资源后不跳游戏界面
此卡修复链条最长,实测中逐层剥出 **6 个串联故障**,任一存在都会卡死:

| # | 故障 | 根因与修复 |
|---|------|-----------|
| 3a | iframe reload 后停在空白页 | nonce 一次性消费后丢失。改为 window.name 持有(reload 存活)+ 每次文档加载都发 ready + 宿主常驻监听重新 boot |
| 3b | reload 后下载成果丢失 | Cache/localStorage shim 全在内存。新增 postMessage 持久化桥:Cache 体存 IndexedDB、storage 快照存 localStorage,boot 时随快照下发(重启 App 仍有效) |
| 3c | 「宿主不允许访问父页面样式,无法进入宽屏」alert | 作者页宽屏机制读 `window.parent.document` 往父文档注入撑满样式(为同源酒馆设计),沙箱 iframe 下必抛 SecurityError。模板用 `[Replaceable]` 赋值遮蔽 `window.parent` 为代理(document 指向自身,postMessage 转发真身) |
| 3d | 开场消息/立绘/数据丢失 | 作者页解密 96MB 资源包后经 `URL.createObjectURL` 生成 blob: URL 加载资源,资源帧 CSP 不含 `blob:` 全部拦截。CSP 的 img/font/media/connect 四处放行 blob: |
| 3e | 开场白「文本为空喵」 | ① 角色列表接口不带 data_raw(体积大),世界书从未下发——boot 时补详情接口兜底;② 世界书条目名在 `comment` 字段(酒馆内部格式),作者页读 `name`——TavernHelper shim 归一化 `name ?? comment` |
| 3f | 点「进入游戏」后定格卡死 | **rAF 停发**:沙箱 iframe(无 allow-same-origin)在站点隔离下是独立进程,宿主窗口遮挡/最小化或 WebView 面板非激活时 rAF 无限期停发(探针实测 23 秒才触发一次,「RAF NEVER FIRED in 4s!」警告);作者页下载进度/解密分片/初始化动画全挂 rAF。模板把 rAF 映射到 setTimeout(16ms≈60fps,遮挡下仅降频不停摆) |

- **实测**:标题页 → 宽屏 → 进入游戏 → 完整游戏界面:世界书开场白(多段叙事)、
  梨奈立绘、数据面板(疲劳 68/欲望 5/意志 98/理智 100、5月1日 04:30)、特质雷达图、
  顶栏导航提示全部渲染。

### 4. wuwa 卡只有文字没界面
- **根因(四层)**:① 渲染引导无可发现性(已修:含界面脚本但纯文本显示时顶部引导条,
  一键开渲染/授权 JS);② CSS 清洗剥掉 position/fixed/z-index 等(渲染保真放宽,
  仅限用户逐卡主动开启渲染的卡);③ jQuery 读操作全是假值桩(已真实化:宿主 RPC 镜像);
  ④ **楼层深度语义丢失(本次实测新发现)**:酒馆脚本 `minDepth/maxDepth` 字段在归一化时
  被丢弃,「删除远楼层开场标记」(minDepth=2,空替换串)对当前开场(depth 0)误生效,
  先把占位符删成空串,「鸣潮开场」渲染脚本(65KB 界面 HTML)永远匹配不到 → 空白气泡。
- **修复**:后端归一化保留 `min_depth/max_depth`;前端渲染管线
  (renderScopedScripts/renderScriptedHtml/stripHiddenPlaceholders)按消息楼层深度过滤
  脚本;消息列表传 depth(0 = 最新);流式消息恒 depth 0。
- **实测**:引导条出现 → 开 HTML 渲染 → 授权 JS → 主开场渲染出鸣潮角色创建界面;
  聊天回复底部 MVU 交互栏(状态/剧情/牵绊/行动)渲染。
- **注意**:重进会话后第一条(主开场)气泡空白是**酒馆原生语义**——作者设计旧楼层
  不再显示创建界面(minDepth=2 的本意),与酒馆行为一致,不是回归。

### 5. 聊天记录面板失效
- 失效观感来自 bug 1(会话里只剩开场白)。随 bug 1 修复,实测刷新后消息完整恢复 ✓。

## 排查中发现的隐藏 bug(一并修复)
- `api/client.ts`:tokenPromise 缓存 rejected promise,bootstrap 一次失败后全部 API
  永久失败 → 失败不缓存,下次重试。
- `characters.rs`:expect panic → 改 500。
- 脚本沙箱 localStorage/sessionStorage 共享同一内存 store → 拆独立。
- `resource.rs`:GBK 页面 from_utf8_lossy 乱码 → 按 charset 解码。
- 头像一律存 .png 导致 mime 不符 → 按真实字节嗅探。
- 渲染面板同款 kdLoaded/reload 脆弱 → 随 3a/3b 链路修复。
- 作者页 TavernHelper API 面补齐:getCharData/getVariables/insertOrAssignVariables/
  getCharWorldbookNames(同步)/getWorldbook/updateWorldbookWith/generate/generateRaw
  (经 postMessage RPC 桥到宿主,宿主调新端点 `POST /api/chat/generate-raw`)/
  getPreset/replacePreset/eventOn 等。
- 宿主宽屏按钮:资源卡片注入「宽屏/退出宽屏」切换(fixed 撑满视口,层叠阶梯
  `--z-resource-wide: 70`);宽屏切换时作者页收不到 resize(实测切换后舞台缩在左上角),
  模板轮询 innerWidth/innerHeight 补发 resize,双向切换均自动适配。

## 安全边界(未退步)
- 资源 iframe 仍 `sandbox="allow-scripts allow-modals"`,**无 allow-same-origin**;
  作者脚本运行在不透明来源,拿不到宿主 DOM/localStorage/API token。
- blob: 放行仅限资源帧文档自身创建的 URL(opaque origin 内),不放宽跨源边界。
- CSS/脚本放宽、JS 执行仅限用户逐卡主动开启渲染/授权的卡;授权按脚本内容版本绑定,
  脚本变化后自动失效。
- 数据合并前双备份;合并工具先 dry-run 打印统计再实写。

## 测试
- 前端 vitest **445/445**(新增:门面完整性、nonce 稳定、snake_case 键、清洗放宽、
  楼层深度过滤回归用例)。
- 后端 cargo test:模板断言(parent shim/rAF 兜底/resize 监护/TavernHelper 归一化)、
  深度字段归一化等全过。
- 探针环境(kdprobe/):probe5(parent 遮蔽 5/5)、probe6(模板链路 6/6)、
  probe2(全链路带 rAF shim)、probe7(无 rAF shim 复现冻结实锤)。

## 环境注意事项
- **IAB/内嵌 WebView 测试**:沙箱资源帧的 rAF 依赖宿主 WebView 可见性;窗口最小化或
  面板非激活时旧版会卡死。模板 rAF→setTimeout 兜底后该问题已消除,但浏览器自动化
  截图仍可能因 WebContents 忙而瞬时失败,重试即可。
- **构建顺序**:`include_dir!` 在编译期嵌入 web/dist,必须先 `vite build` 再
  `cargo build`,否则内嵌旧前端(本次实测踩过:服务端首页引用旧 bundle hash)。
  日常开发可用 `KEDAI_WEB_DIST=<项目>\web\dist` 让服务读磁盘最新 dist 免重编。

---

# 角色模式 bug 修复变更说明(2026-09-07 第二轮)

覆盖用户 2026-09-07 报告的 4 个 bug:吸血鬼卡「掉格式 + 变量不更新」、wuwa 卡
「主开场开场设置不显示 / 备用开场状态栏加载失败 / 悬浮窗丢失」。

## 6. 吸血鬼卡:进游戏后「布豪!丢格式了!」+ 状态面板不更新(bug①)
- **根因(同一根因的双症状)**:游戏界面 v2.1.html 的回合流程是作者页自组 prompt
  调 `TavernHelper.generate()` 直连 LLM,输出格式规范(`<response_format_guidance>`
  6837 字符,要求整个回复有且仅有一个 JSON)是世界书触发条目,由关键字
  `system log`(游戏历史 assistant 消息携带的注释行)触发,**依赖酒馆核心在
  generate 时做世界书匹配注入**。kedai 的 `POST /api/chat/generate-raw` 只做透传,
  从不注入世界书 → 模型自由输出叙事文本 → 游戏 JSON 解析失败报「丢格式」,面板
  数值(在 JSON 的 context 字段里)也随之不更新。
- **叠加缺陷**:该卡全部 54 个世界书条目 `use_regex: true` 且无独立 regex 字段
  (ST 语义:keys 即正则模式);kedai 解析器把 use_regex 与 `regex.is_some()` 相与,
  此类条目正则标记被压成 false(仅歪打正着退化为子串匹配)。
- **修复**:
  - `parsing/world_book.rs`:use_regex 保留声明值;新增共用判定
    `entry_matches_texts`(独立 regex 优先,缺失时 keys/keys_secondary 按正则编译,
    坏模式跳过不拖垮整条目;大小写按条目设置)与 `entry_probability_pass`,
    主引擎命中判定改调共用函数。
  - `api/chat.rs generate_raw`:请求体加可选 `character_id`;提供时收集角色内嵌
    character_book + 绑定/全局世界书做匹配注入(constant 条目 system 置顶,
    命中条目插到最后一条 user 消息之前;扫描窗口按条目 depth、不分角色;
    上限 40 条/128KB),响应带 `injected` 清单。
  - `ChatWindow.vue handleTavernCall`:桥接请求带当前角色 id。
- **实测(API 通道,模拟游戏调用形态)**:注入 19 条(17 常驻 + 🔗故事编写🔗 +
  🔗响应格式🔗),模型输出严格 JSON(`{` 起 `}` 止,JSON.parse 通过,键
  thinking/self_check/context/end_output 全合卡模板,context.current_status 含
  fatigue/desire/will/san 数值——游戏面板数据源)。

## 7. wuwa 卡:备用开场状态栏加载失败(bug③)
三层根因叠加,全部修复并实测:
- **MVU initvar 不支持**:14 个备用开场尾部
  `<UpdateVariable><initvar>YAML</initvar></UpdateVariable>` 整段被丢弃。修复链:
  parser 提取 initvar → YAML 标量补 `{}`/`[]` → applyInitVarContent 合并 stat_data
  → replay/applyMvuCommands 在变量树空时播种(有快照不覆盖)。
- **数组叶子 vs 裸值**:kedai stat_data 叶子存 `[值,原因]`,wuwa 脚本按裸值直读
  (`u.性别||'男'`),顶层值显示带「,初始」尾巴、嵌套布尔全错位。沙箱模板加
  `__kdUnwrap`/`__kdStatView` 裸值视图,getAllVariables/getMvuData 等门面全部返回解包视图。
- **attr('id') 恒 ''**:wuwa render() 读 `$('.tab-page.active').attr('id')` 决定渲染
  分支,沙箱读路径硬编码空串 → tab 页永远停在骨架 `--`。修复:沙箱加 id 反向索引
  (__kdById/__kdByClassLookup 回查链)+ 宿主 gatherIdMap 随 boot 注入(boot 即索引,
  同步读无需等 batch ack)。
- **实测**:状态栏全部真值(地点/时间/性别/漂泊者/好感度 ❤80 等),renderUserTab
  确证执行。残留「当前标题 --」是 initvar 无 `剧情显示` 键,卡预期降级。

## 8. wuwa 卡:主开场第一个回复的开场设置不显示(bug②)
- 前序已修(渲染链 + 楼层深度),本轮补 **{{user}}/{{char}} 宏替换**:kedai 前端原本
  没有任何宏替换,而 wuwa 状态栏脚本 replace_string 自身含 `{{user}}`(JS 与 HTML 内)。
  render.ts 新增 `expandDisplayMacros`,在 expandReplaceRefs 之后对替换串展开
  (覆盖 HTML 与内嵌 `<script>` 源码),调用方传角色名。
- 实测:「{{user}}档案」正确展开为「用户档案」。

## 9. wuwa 卡:自带悬浮窗丢失(bug④)
- **定论:非 bug,是卡的折叠设计**。全卡无 position:fixed 悬浮元素;所指面板即
  tide-card,卡 CSS 规定初始 `height:0;opacity:0` 折叠,点导航/头部经 switchTab 加
  expanded 展开至 380px。inline 事件降级桥全链(demote→宿主绑定→沙箱求值→batch
  回写)有单测覆盖,DOM 实测 data-kd-onclick 25 个已绑定就位。

## 测试基线(2026-09-07)
- 前端 vitest **485/485**(53 文件);后端 cargo test **640/640**。
- 新增:MVU initvar 7 用例、parser initvar 3 用例、YAML `{}`/`[]` 标量、
  宏展开 4 用例、裸值视图、attr('id') 回查、keys-as-regex ST 语义、
  generate-raw 注入 3 用例(常驻置顶/触发贴尾/depth 窗口)。

## 环境限制记录(IAB 浏览器自动化)
- **点击/按键通道整体失效**(locator.click 超时、tab.click/CUA 坐标/press Enter 均
  无效,含原生按钮),**fill/check/scroll/reload/DOM 读取可用**。判定为 IAB 环境限制,
  非产品 bug。wuwa 标签页切换交互因此以单测 + DOM 绑定证据闭环,未做物理点击实证。
- `tab.playwright.evaluate` 只读策略不绕过。
- API 实测通道:`/api/bootstrap`(loopback Origin)取 token →
  `/api/chat/sessions`、`/api/chat/history`、`/api/chat/send`(SSE)、
  `/api/chat/generate-raw` 均可 curl 直测,适合 IAB 之外的验证路径。

## 10. 舰娘卡(碧藍航線):状态栏全兜底值(00:00/未知区域/指挥官/0)
- **根因**:该卡变量初始化在世界书条目 `[initvar]变量初始化`(**全小写**标签,
  且条目 disabled——初始化约定允许禁用条目提供初始值)。前端变量源
  `/api/chat/init-vars` 端点用大小写敏感的 `comment.contains("[InitVar]")` 过滤,
  小写标签匹配不到 → 端点恒返回空 → 前端 initVarEntries 空 → 状态栏脚本
  `getAllVariables().stat_data` 全空 → 手机 UI 全部显示兜底默认值。
  对照:引擎侧 `collect_init_vars`(提示词注入用)本就是大小写不敏感,
  两侧谓词不一致是本 bug 本质。
- **修复**:
  - `parsing/assistant/initvar.rs`:抽出共享谓词 `is_init_var_comment`
    (大小写不敏感 + `[InitialVariables]` 别名),`collect_init_vars` 与
    `api/sessions.rs` init-vars 端点统一改用它;`parse_yaml_scalar` 补
    `{}`/`[]` 空容器标量(与前端 variables.ts 对齐,`shipgirls: {}` 不再存成字符串)。
  - 前端 `mvu/initvar.ts isInitVarEntry`:同谓词(端点→前端双过滤,两端都必须宽)。
  - `stores/character.ts`:init-vars 返回空时兜底拉角色详情,从
    `data_raw.character_book.entries` 前端侧再过滤一遍——不依赖后端版本即生效。
  - `scriptRunner.ts`:initvar 内容在解析前做 `{{user}}/{{char}}` 宏展开
    (碧蓝卡 `name: "{{user}}"`,否则状态栏用户名显示字面量 {{user}});
    `ScriptRunContext`/`chatMessageScriptScheduler` 透传 `charName`。
- **验证**:vitest 489/489(新增 initvar.test.ts 3 用例:小写标签识别、
  碧蓝卡真实 YAML 全字段形态、非 InitVar 排除;scriptRunner 宏展开组合用例)。
  本轮按用户要求**未做构建**(vite/cargo),前后端改动下次构建后生效。
- **备注**:该卡另有酒馆助手脚本「随着开场白切换自动开启关闭世界书」
  (swipe 切换时改写条目开关),依赖酒馆事件流,kedai 无此机制,暂不支持。

## 11. swipe 事件流:卡级脚本闭环(舰娘卡「随开场白切换世界书」跑通)
- **需求**:舰娘卡内嵌酒馆助手脚本「随着开场白切换自动开启关闭世界书」监听
  `tavern_events.MESSAGE_SWIPED` + 500ms 轮询兜底,按 swipe 索引批量开关世界书条目
  (uid 99/110/112 ↔ 10/11/12)。上轮备注的「kedai 无此机制」本轮补齐。
- **实现链路**:
  - `cardScripts.ts`(新):卡级脚本收集器——`data_raw.extensions.tavern_helper` 里
    的 script 节点,兼容 ST 导出时数组被对象化的嵌套形态 `{"0":{"1":{...}}}`
    (舰娘卡实测如此),递归拉平 + 特征键弱匹配防 variables 树误判。
  - `scriptAuthorization.ts`:`hashRegexScripts(scripts, cardScripts?)` 把卡级脚本
    `{kind:'th-script',id,name,content,enabled}` 纳入授权哈希;无卡级脚本时哈希
    与旧值逐字节一致(老卡授权不抖动)。卡更新脚本→哈希变→旧授权自动失效。
  - `stores/character.ts`:角色详情缓存(`characterDetails` Map + inflight 去重),
    selectCharacter 后台预拉;授权改异步,先拉详情再算含卡脚本哈希。
  - `characterScriptSandbox.ts`:沙箱模板扩 `tavern_events` 常量(挂载在 const 定义
    之后避开 TDZ)、`TavernHelper` 补 errorCatched/eventOn/eventOff/eventEmit/
    getCurrentCharPrimaryLorebook/getCharWorldbookNames/getLorebookEntries/
    getWorldbook/setLorebookEntries/updateWorldbookWith/getChatMessages;消息循环加
    `card-event` 分支(经事件总线派发);`broadcastCardEvent(name, payload)` 宿主广播;
    `sandboxScript` 第 8 参注入主世界书名 `__KD_LOREBOOK__`;宿主 `rpc` 分支异步化,
    新增 `context.rpcExtensions` 扩展点(纯数据 op 不过 DOM 白名单),同步 op 行为不变。
  - `cardScriptHost.ts`(新):长驻沙箱宿主。键 `characterId:scriptHash` 幂等;
    rpcExtensions 三 op:`lorebook-entries`(GET 内嵌世界书,映射酒馆助手条目形态
    uid←id,key/keys 双写)、`lorebook-set`(GET 全量→只应用 `{uid,enabled}` 白名单
    →PUT 全量;防卡脚本借道改写提示词内容)、`chat-messages`(0-based 楼层下标/
    负数末尾起;extra.swipes `{swipe_id,content,ts}` 降维为文本数组 + swipe_id
    透传,无版本数据退化 `[content]/0`)。ESM 卡级脚本(顶层 import/export,如
    该卡的 MVU Zod schema)跳过并 console.warn——沙箱是经典脚本 + CSP 不开外部源,
    跳过是最诚实行为;正文仍计入授权哈希。
  - `stores/chat.ts swipeMessage`:成功后 `broadcastCardEvent('message_swiped', idx)`,
    payload 为 0-based 楼层序号(对齐酒馆 MESSAGE_SWIPED 的 message_id 语义,脚本
    判定 `messageId === 0` 第一条开场白);历史漂移由脚本自带轮询兜底。
  - `ChatWindow.vue`:watch 角色/授权/哈希,满足条件拉详情取卡级脚本+主世界书名
    启沙箱(await 后复核最新值防快速切卡竞态),失守/卸载清理;卡级脚本只过 JS
    授权门禁,不要求 renderHtml 开关。
- **安全边界(不退步)**:逐卡主动授权、哈希绑定全部执行面(消息脚本+卡级脚本);
  沙箱无 token,世界书读写经宿主转发;lorebook-set 仅放行 enabled。
- **验证**:vitest **523/523**(57 文件;新增 cardScriptHost 16 用例、chat swipe 广播
  3 用例、授权哈希卡级脚本 3 用例、沙箱卡级兼容面 5 用例——含 card-event 总线
  分发、lorebook 名称闸、__KD_LOREBOOK__ 注入、TDZ 顺序)。本轮**未构建**
  (vite/cargo),改动下次构建后生效。

## 环境事故记录(2026-09-07 晚):C 盘写满(ENOSPC)
- 编辑 store.ts 时磁盘写满导致文件被截断为空;`git checkout` 恢复后发现 HEAD 版本
  缺此前未提交的门面转发(storeFacade 动态完整性测试立即暴露:startStream/
  queueSettingsSave/task×7/uiPrefs×2 共 11 个动作键),已按 7 个子 store return 块
  枚举补齐,523/523 全绿确认无残留。**教训:磁盘满时任何写操作都可能截断目标文件,
  先清空间再改码。**
- 清理:Windows Temp 陈旧文件(>1 天)释放约 6.5GB;本会话临时卡数据文件已删。
  `server-rs/target` 13GB 构建缓存未动(用户统一构建时需要);C 盘长期只剩零头,
  建议用户尽快大盘清理,否则后续 cargo 链接可能再次 ENOSPC。
