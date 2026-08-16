# Kedai「计划五:音频与渲染面板」详细设计

> 兼容对象:JS-Slash-Runner「酒馆助手」4.9.1 的音频面板(bgm/ambient 双通道 + 8 个 TavernHelper 音频 API + 可折叠 AudioPlayer)与 TH-render 消息渲染面板(消息内 HTML 代码块约定式识别 → iframe 挂载)。
> 前置:计划一(EJS 上下文真实化)、计划三(角色卡脚本 iframe 沙箱、EvalBridge)、计划四(slash/快速回复)已完成;沙箱宿主文档三件套(sandbox.html + sandbox_headers + sandbox_document)为既有先例。
> 目标(路线图原文):「bgm/ambient 播放器、消息渲染面板(TH-render 等价物)。」
> 硬约束:不引入 eval/新依赖;注释/文档用简体中文;不提交 git;渲染面板与角色脚本沙箱同等级安全(无宿主 DOM/token 访问)。
> 本设计中「事实」指代码中可直接引证的行为(附 文件:行号);「建议」指基于经验的判断(标注 [建议])。

---

## 1. 背景结论(基于探索)

- **酒馆音频契约**已确认(`sandbox/JS-Slash-Runner-main/@types/function/audio.d.ts`):8 个 API 为同步签名;AudioSettings 含 enabled/mode(repeat_one|repeat_all|shuffle|play_one_and_stop)/muted/volume(0-100);默认 bgm `repeat_all`、ambient `play_one_and_stop`、volume 50(`dist/index.js:73`);onended 按 mode 切歌(`dist/index.js:769`);仅 URL 播放。
- **Kedai 沙箱现状**:`characterScriptSandbox.ts` 的 `sandboxScript()`(:86-191)以模板字面量注入全局;`SandboxRequest` type 仅 ready/rpc/batch/done/error/warn/jq-event;宿主 `onMessage`(:422-483)白名单分发。沙箱无 DOM 之外的宿主能力(无音频/网络/存储),音频是全新扩展点。
- **主页面 CSP**:`security.rs:144` 全站 CSP 为 `media-src 'none'`,会拦 `<audio>` 远程加载,须放开到 https/http(与 `resource_frame_headers` :205 媒体放开同级)。
- **宿主文档三件套先例**:`sandbox.html`(security.rs:156-179 全禁网络)+ `resource_frame.html`(:188-212 网络放开)均为「独立 src 文档 + 专用 header 函数 + URL fragment nonce + ready/boot 双向认证」;新增渲染面板宿主文档完全可仿照。
- **消息渲染集成点**:`ChatWindow.vue` 的 `assistantHtml()`(:240-252)判断链:资源界面(extractBodyLoadUrl)→ scoped HTML → markdown;`hydrateRemoteResources`(:95-162)提供 ready/boot 投递 + 300ms×50 重试 + 超时兜底模式;`renderScopedScripts` 的 `buildScopedScriptHtml`(:374-400)剥 ` ```html ` 围栏(与「整页 HTML 代码块」识别不同,后者需自行实现)。
- **TH-render 参考**:sandbox/ 与 docs/ 无 TH-render 源码;iframe 高度自适应可参考 `ST-Prompt-Template-main/src/utils/iframe.ts` 的 `parent.postMessage({type:'iframeResize', height})` 机制。

## 2. 交付物总览

| 模块 | 交付物 | 独立可测 |
|---|---|---|
| 5a 后端 | `services/audio_service.rs`(JSON 持久化 + 校验)+ `api/audio.rs`(3 路由)+ AppState 接线 + CSP media 放开 | ✅ service 单测 7 个 + `audio_crud` 集成测试 |
| 5a 前端 | `api/audio.ts` + types + store 接线 + `AudioPlayer.vue`(折叠面板 + 双通道)+ App.vue 挂载 + `audioController.ts` 注册表 + 沙箱 TavernHelper 音频桥 | ✅ 沙箱生成物回归(既有测试)+ build |
| 5b 后端 | `api/render_frame_template.html` + `render_frame_headers` + `/render-frame.html` 路由 | ✅ `render_frame_document_is_served` 集成测试 |
| 5b 前端 | `renderPanel.ts` 纯函数 + `ChatWindow.vue` 集成(占位 HTML + 投递 + 高度自适应 + 折叠)+ style.css | ✅ `renderPanel.test.ts` 6 个 |
| 文档 | `docs/plan5-audio-render-panels.md`(本文件) | — |

---

## 3. 5a 音频(bgm/ambient 播放器)

### 3.1 后端服务(`server-rs/src/services/audio_service.rs`)

- 模型:`AudioState { bgm: ChannelState, ambient: ChannelState }`;`ChannelState { enabled, mode: AudioMode, muted, volume: u8, playlist: Vec<AudioTrack> }`;`AudioTrack { title, url }`。serde `rename_all = "snake_case"`(JSON 契约与酒馆一致)。
- 持久化仿 `prompt_inject_service.rs`(:282-307 load 缺失→默认/损坏→logger::error+默认;:332-336 save = create_dir_all + pretty + write)到 `data/audio.json`。
- 校验:`update_settings` 部分字段合并 + volume clamp 0-100;`update_playlist` URL 协议白名单(仅 http/https,拒 file/javascript/data,空串拒绝),校验失败不落盘不改内存。
- AppState:`pub audio: Arc<Mutex<AudioService>>`(`app_state.rs:25-60` 字段 + `new()` 初始化),路由:`GET /api/audio`、`PUT /api/audio/settings`、`PUT /api/audio/playlist`(`api/audio.rs` handler 风格对齐 quick_replies.rs:match + `Json({error})` + with_status)。

### 3.2 CSP 放开

`security.rs:144` 主页面 `media-src 'none'` → `media-src 'self' https: http:`(音频来源为用户提供的 URL,媒体不执行脚本,风险可控;与 resource frame 的媒体放开 :205 同级)。

### 3.3 前端 API 与 store

- `web/src/api/audio.ts`:`getAudio()` / `updateAudioSettings(type, Partial)` / `updateAudioPlaylist(type, tracks)`(`request` 封装,仿 settings.ts);`types.ts` 加 `AudioTrack`/`AudioMode`/`AudioChannelSettings`/`AudioChannelState`/`AudioState`/`CurrentAudio`;`index.ts` 聚合导出。
- `store.ts`:`audio = ref<AudioState | null>(null)` + `audioOpen = ref(false)`;action `loadAudio()`(失败静默,仿 loadSettings :535-557)、`saveAudioSettings`/`saveAudioPlaylist`(成功回填全量;失败 throw 供 UI 提示);return 块导出。

### 3.4 AudioPlayer.vue 与挂载

- 折叠面板(标题栏 + 收起按钮);双 Channel Controller(bgm/ambient):启用开关、静音 + 音量滑条(拖动本地生效、change 落盘)、播放/暂停、上一首/下一首、进度条、mode 循环按钮、曲目下拉、列表编辑器(增删 title/url,保存过滤空行、服务端校验协议)。
- 原生 `<audio>` 单实例,两通道复用(切通道停播换 src);`onended` 按 mode 切歌:repeat_one 重播 / repeat_all 下一首(到头回 0)/ shuffle 随机 / play_one_and_stop 停止(对齐 `dist/index.js:769`)。
- 挂载:App.vue 右下角 fixed「♪」悬浮按钮(`.sv-audio-toggle`;`style.css` 新增 `right:16px;bottom:16px;z-index:40`),点击切 `store.audioOpen` → 弹 `AudioPlayer`(`.sv-audio-float`);onMounted `store.loadAudio()`。

### 3.5 沙箱音频桥(TavernHelper 兼容)

- `audioController.ts`:模块级单例注册表——AudioPlayer 挂载时 `registerAudioController(impl)`,卸载时置 null。沙箱音频 API 经 `getAudioController()` 取实现(面板折叠/未挂载时 no-op,getter 回退默认,与酒馆音频 slash 命令无 UI 可用的语义一致)。
- `characterScriptSandbox.ts`:`SandboxRequest` type 加 `'audio'`;`sandboxScript()` 注入 `globalThis.TavernHelper`,8 个 API 签名对齐 `audio.d.ts`:
  - setter 类(playAudio/pauseAudio/replaceAudioList/appendAudioList/setAudioSettings):fire-and-forget 发 `send('audio', {op, args})`。
  - getter 类(getAudioList/getAudioSettings/getCurrentAudio):启动时 `rpc('audio-snapshot')` 拉一次缓存,同步读缓存返回;就绪前回退默认(与 jQuery 子集 `hasClass→false` 同步降级哲学一致,不阻塞脚本)。
- 宿主 `onMessage` 加 `'audio'` 分支 → `handleAudioRpc`(op 白名单,type 非 ambient 一律按 bgm 对齐酒馆 HF 分发);`rpc` 分支特判 `audio-snapshot` 走 `audioSnapshotValue()`(AudioPlayer 未挂载时回退默认)。

> **同步/异步权衡(建议)**:酒馆 8 个 API 是同步签名,但 Kedai 沙箱是 iframe + postMessage 异步 RPC。getter 无法真正同步返回宿主状态,故「启动拉取缓存 + 同步读缓存 + 就绪前默认值」是与既有 jQuery 子集降级哲学一致的可行折中;setter 无返回值,fire-and-forget 无感知差异。

---

## 4. 5b 渲染面板(TH-render 等价物)

### 4.1 识别与构建(纯函数,`web/src/renderPanel.ts`)

- `isRenderCodeBlock(text)`:代码块文本含 `<html` 且含 `<head` 或 `<body`(TH-render 约定式识别;只含 `<body` 或 `<head` 的片段不命中,走既有 scoped 注入)。
- `extractCodeText(container)`:取 `pre code` 的 textContent(未转义原文)。
- `buildRenderDocument(code, channel, nonce)`:半截文档自动补齐 `<!doctype>/<html>/<head>/<body>`(含 `<html` 原样保留,剥离重复 doctype),body 标注 `data-kd-render-panel="channel#nonce"`。
- `RENDER_PANEL_CHANNEL = 'kedai-render-panel-v1'`(与宿主文档模板一致)。

### 4.2 宿主文档三件套(仿 sandbox 先例)

- `server-rs/src/api/render_frame_template.html`:nonce URL fragment 认证 + `kedai-render-panel-v1` channel + booted 闩锁 + 接收面板 HTML 经 `appendChild` 注入 body + 首条 ready 回执。面板内脚本经 `parent.postMessage` 上报 `kd-panel-resize {height}`(高度自适应)与 `kd-panel-event`(事件上报,扩展位)。
- `security.rs` 新增 `render_frame_headers`:与 `sandbox_headers`(:156-179)同级——`default-src 'none'` + `script-src 'unsafe-inline'` + connect/img/style/font/media 全 'none' + `frame-ancestors 'self'` + x-frame-options SAMEORIGIN;`guard`(:56-62)加 `/render-frame.html` 分派分支。
- `api/mod.rs`:`render_frame_document()`(include_str + no-store 头,仿 :311-322)+ 路由 `.route("/render-frame.html", get(render_frame_document))`。

### 4.3 ChatWindow 集成

- `assistantHtml()` 判断链扩展:资源界面(extractBodyLoadUrl)→ **渲染面板**:`isRenderCodeBlock(renderTextFor(m))` 时 `buildRenderPanelHtml(text, seed)`(面板壳:标题栏 + 折叠按钮 + `iframe /render-frame.html#nonce` + 说明;code 存 `data-kd-render-code` HTML 转义)→ scoped HTML → markdown。面板不依赖 `renderHtml` 开关(与资源卡片同语义:iframe 隔离执行)。
- `hydrateRenderPanels()`(仿 hydrateRemoteResources :95-162):扫描 `[data-kd-render-panel="1"]` → contentWindow 就绪(300ms×50 重试)→ 等 ready → postMessage `{channel, nonce, type:'boot', html}`(12s 超时兜底)→ 监听 `kd-panel-resize` 调 iframe 高度(clamp 80-3000)、`kd-panel-event` console 记录。
- 折叠:scrollArea 全局事件委托(`onRenderPanelToggleClick`)——收起 = 隐藏 iframe + 展示代码块原文,展开 = 恢复 iframe(已注入宿主文档保留);onMounted 绑定 / onBeforeUnmount 移除。
- 样式:`style.css` 新增 `.sv-render-panel` 系列(仿 `.sv-resource-card` :1564-1606)。

---

## 5. 测试计划

| 位置 | 内容 | 命令 |
|---|---|---|
| `server-rs` service 单测 | 默认值/roundtrip/损坏回退/volume clamp/协议拒绝/通道解析/部分合并 7 个 | `cargo test`(需先加载 vcvars64,见 MAINTENANCE.md:99-101) |
| `server-rs` 集成 | `audio_crud`(默认双通道、settings 合并、非法通道/协议 400 不落盘)+ `render_frame_document_is_served`(CSP 断言) | 同上 |
| `web/src/renderPanel.test.ts` | isRenderCodeBlock 命中/未命中/边界 + buildRenderDocument 补齐/原样 6 个 | `npm test` |
| 回归 | 沙箱生成物 7 个既有测试(sandboxScript 可解析)+ 全量 158 个 | `npm test` |
| 构建 | 全量类型检查 + 打包 | `npm run build` |

实际结果:`cargo test` 393 单测 + 34 + 11 集成全过;`npm test` 158 个全过;`npm run build` 通过。

## 6. 风险与边界

| 风险/边界 | 处理 |
|---|---|
| 主页面 CSP media 放开 | 与 resource frame 同级(https/http);音频为用户提供 URL,媒体不执行脚本 |
| 渲染面板误命中普通 HTML 代码块 | 判定需同时含 `<html` 与 `<head`/`<body`;面板可折叠回代码块;与资源界面(scoped 注入)优先级明确 |
| 沙箱 getter 同步返回 | 对齐既有 jQuery 子集降级哲学:启动拉取缓存 + 同步读缓存,就绪前返回默认值;setter fire-and-forget |
| 面板消息重渲染 | v-html 重建后 dataset 丢失重扫重注入,与资源卡片行为一致;折叠状态随重建重置(可接受) |
| 音频本地文件上传 | 仅 URL 播放(对标酒馆),本地文件留扩展位 |
| RENDER 标签(世界书条目)引擎跳过 | 本阶段只做消息正文 HTML 代码块 → 渲染面板(TH-render 等价物);`mod.rs:916-919` 的 RENDER 跳过不变,留计划六 |
| 面板内脚本通信最小化 | 首轮仅展示 + 折叠 + 高度自适应 + `kd-panel-event` 上报;TavernHelper API 注入 iframe、事件双向回推留扩展位 |
| 既有 quick-replies PUT 缺失 | 顺带修复:`api/mod.rs` 的 `/api/quick-replies/{id}` 原仅 DELETE,补 `put(quick_replies::update)`(assistant.rs:960 既有用例验证) |
