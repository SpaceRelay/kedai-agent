# GPU 加速可行性分析

> 分析日期:2026-09-15;基线 commit:`5d04de4`(本轮改动未提交)。
> 方法:全仓静态审计(server-rs 224 文件 / 76,855 行;web/src 227 文件 / 40,605 行)
> + 复用 `docs/perf-baseline.md` 的实测微基准。本文只做可行性判断,未改代码。

## 一、结论

1. **对现有代码做 GPU 加速:不可行,收益为负。** 本地没有任何一项计算的规模达到 GPU 的经济门槛;
   唯二的两个候选(向量余弦 0.3 MFLOP、BPE token 计数)即使被 GPU 算到零耗时,
   端到端也只快千分之几毫秒量级(`fast` 档实测总耗时 7,679 ms)。
2. **前端渲染层不需要动手:** Tauri 2 在 Windows 上走 WebView2,未传 `--disable-gpu`,
   合成与光栅化已经由 Chromium 的 GPU 路径承担;前端瓶颈是 `sanitize-html`(3.057 ms/16.8k 字符)
   与 DOM 节点数,属 JS/DOM 成本,没有 GPU 可编程接口可用。
3. **唯一有价值的 GPU 落点是「新增本地模型推理能力」**,即把远端算力搬回本机
   (本地 LLM / embedding / ASR / TTS / 图像)。这是**架构增量**,不是性能优化:
   其算力需求比现有本地计算高 3~6 个数量级,只有到那个量级 GPU 才有意义。
   其中本地 embedding 的性价比最高,而且**在 CPU 上就能达成目标,并不需要 GPU**。

## 二、事实基础:算力目前分布在哪

### 2.1 远端(占绝对多数)

- 唯一的 LLM 通路是 OpenAI 兼容 HTTP 客户端:`server-rs/src/connectors/mod.rs:27-30`(仅 Mock/OpenAi 两个变体)。
- Embedding 同样是远端接口:`services/embedding_service.rs:1-10,86` 调 `POST {base}/embeddings`。
- 全仓 grep `gguf|onnx|safetensors|llama|candle|ort|tch|whisper|cuda|gpu|vulkan|metal|wgpu|directml|SIMD|AVX`
  在 Rust/TS/Vue 源码与文档中**零命中**(仅 `minijinja` 之外的 `.bin` 路径串误报)。
- `perf-baseline.md:81` 的实测结论最直白:`fast` 档 7,679 ms 中,上游等待 7,025 ms(TTFT),
  「服务端本地预处理约 1 s,其余全在等上游」。

### 2.2 本地真正在算的东西(以及它们的规模)

| 路径 | 位置 | 单次规模 | 实测耗时 |
|---|---|---|---|
| 世界书关键词匹配 | `parsing/world_book.rs:541` | 54 条目 × 扫描窗口(默认最近 4 条) | 0.062 ms(正则预编译后) |
| BPE token 计数 | `services/token_service.rs:117` | 30 条 × 2k token;最坏单次 88 万 token | 29.4 ms → 命中缓存 <1 ms |
| 向量余弦 | `services/embedding_service.rs:191` | 单角色全部记忆(默认上限 200 条,`memory_service.rs:36`)× 维度(16..8192,常见 1536) | <1 ms |
| markdown 渲染 | `web/src`(markdown-it) | 16.5k 字符 | 0.453 ms |
| HTML 净化 | `web/src`(sanitize-html) | 16.8k 字符 | 3.057 ms |
| 角色卡解析 | `parsing/character_card.rs:176-213` | 容器 chunk 头,**不解码像素** | 可忽略 |
| EJS / QuickJS | `parsing/assistant/ejs/`、`scripts/runtime.rs:77` | 单条条目渲染 / 1 s 超时 JS | — |
| 仓库索引粗筛 | `api/repo_index.rs:110-131` | 解析 index.json 后子串过滤,≤2000 条 | — |

**关键推断:** `fast` 档那 「约 1 s 本地预处理」 里,上述各项微基准加起来不足 5 ms。
所以这 1 s 的主体是 SQLite/文件 IO 与「每轮一次远端 embedding 往返」,而非本地算力。
(此推断建议用一次火焰图确认;若属实,治理方向是减少 IO 与调用轮次,与 GPU 无关。)

## 三、逐项可行性(仅就现有代码)

| 候选 | 算术强度 | GPU 判断 | 理由 |
|---|---|---|---|
| 向量余弦 / KNN | 200 × 1536 ≈ 0.3 MFLOP | **不值得** | kernel launch(5~20 µs)+ 传输(1.2 MB ≈ 0.1 ms)已超过全部计算。要撑起 GPU,数据量需再涨约 3 个数量级 |
| BPE token 计数 | 每字节十余次整数运算,且是**串行 merge** | **不适合** | 260 KB 上下文 ≈ 2.6 M 次操作;GPU BPE 的收益在大批量训练吞吐,单文档延迟赢不过已缓存 CPU 路径。正解是「少算/缓存」(已有),不是换处理器 |
| 正则/JSON/PNG chunk/EJS/Migration | 接近零 | **不适用** | 纯串行文本与 IO,无可并行算力 |
| 前端 DOM/字符串(sanitize-html) | 无 GPU 可编程路径 | **不适用** | 合成层已由 WebView2 GPU 路径承担;瓶颈是 JS 字符串与 DOM 操作 |
| Tauri/Rust 侧 | 无图像/媒体计算 | **不适用** | `src-tauri/src/lib.rs` 只有启动后端、健康检查、文件复制、DB 路径改写 |
| 构建耗时(`cargo build`/`vite build`) | CPU 并行编译 | **不适用** | 与 GPU 无交集 |
| SQLite(sqlite-vec `vec0`) | 上游 C 扩展,CPU 标量 | **不适用** | 无 GPU 后端;见第四节(4) |

## 四、唯一有价值的落点:把模型算力搬回本机

只有在引入本地推理后,GPU 才有讨论价值。四档候选,按性价比排序:

1. **本地 embedding(推荐先做)**
   - 现状:`embedding_service.rs:16-18` 每轮最多 32 条 × 4000 字符,走远端 HTTP。
   - 收益:省掉每轮一次的远端往返与网络依赖;聊天正文不再外发。
   - GPU 判断:**不需要 GPU**。小模型(如 bge-small 级)在 CPU 上每批约几十毫秒即达标;
     只有批量大幅上调(如离线重建整个记忆库向量)时 GPU 才有意义。
2. **本地 reranker / ANN 索引**
   - 现状:`memory_service.rs:545-590` 按 id 取全部候选向量,再在 Rust 里逐条算余弦(`:263`),
     候选量受 `DEFAULT_MEMORY_MAX_ENTRIES=200` 约束;`known-limitations.md` L7 明确不建 ANN。
   - 仅在记忆库涨到 10^5 量级、或引入交叉编码器 reranker(每次召回要跑 N 次小型前向)时才值得重估 ——
     那时 GPU 与批处理才第一次成为「结构性必需」。
3. **本地 LLM(改动最大)**
   - 收益是产品级的:「离线可用」「正文不出本机」「无 API 成本」,不是延迟优化。
   - 代价见第五节,建议先做 spike,不直接落地。
4. **本地 ASR / TTS / 图像生成**
   - 现状:`services/audio_service.rs` 只管播放列表状态与 URL,`api/audio.rs` 三个路由,不解码不合成;
     全仓无 canvas/WebGL/`<video>`/OCR/缩略图。
   - 若做语音对话或角色立绘,GPU 是**必需项**(whisper.cpp / sherpa-onnx / 扩散模型),
     这是本仓库未来最可能出现真实 GPU 需求的方向。

## 五、与既有架构约束的冲突

1. **「单 exe、无需分发 dll」是明确设计约束。** `server-rs/Cargo.toml:18-21` 特别说明 sqlite-vec 走静态链接;
   `api/static_files.rs:209` 用 `include_dir!` 把 `web/dist` 内嵌。CUDA 版运行时需随包分发
   cudart/cublas(+200 MB~2 GB),直接与这一约束冲突 → 若必须做,优先选 Vulkan / DirectML / wgpu 后端(单 dll、跨厂商)。
2. **Android 分支已存在。** `docs/android-port-plan.md` 方案已完成到阶段 5,交付形态是「全内嵌单机版」。
   GPU 方案必须在 Windows(WebView2 + D3D/Vulkan)与 Android(Vulkan/OpenCL/NNAPI)双端成立,
   移动端显存与散热约束更硬,后端选择自由度更低。
3. **并发模型是两套资源管理。** 后端为 axum/tokio 异步 + r2d2 读池 + 单写连接(`perf-baseline.md:54-58`);
   本地推理是重阻塞任务,需要独立线程池 + 显存/内存预算,与现有池化模型是平行的另一套,必须设计隔离与排队。
4. **数据面已就绪的部分不多。** 记忆向量存 `vec0`,`embedding_dim` 支持 0(自动探测)与 16..8192
   (`api/settings.rs:333-335`),若要换本地 embedding 模型,维度切换与「重建向量索引」流程已有入口
   (`known-limitations.md` L7:维度不匹配时保留旧数据,由用户手动重建)。

## 六、何时重新评估

满足任一条时应重跑本分析,而不是提前投入:

1. 记忆条目规模从「每角色 ≤200」涨到 10^5 量级,或引入 reranker/交叉编码器。
2. 产品决定上「离线模式 / 隐私优先 / 免 API 成本」——即本地 LLM 进入路线图。
3. 语音对话、本地图像生成、Live2D/表情差分等富媒体功能立项。
4. 出现实测证据表明**本地**阶段(而非上游等待)成为用户可感知的瓶颈。

## 七、若立项的建议路线

1. 先做「本地 embedding」:替换 `embedding_service` 的远端实现,保留远端作为可切换后端;
   验收标准是 CPU 批量耗时与远端往返时延持平,不引入新分发物。
2. 再做「本地推理 spike」:单文件、可开关、独立进程或独立 dll 边界,先用 Vulkan/DirectML 后端验证
   「单 exe + 可选 DLL」的交付形态是否可接受,再谈模型管理(下载/校验/磁盘占用)。
3. 语音/图像另立专项,不与应用主链路捆绑。
4. 全程不动现有 CPU 热路径:世界书正则缓存、token 内容缓存、工具定义快照这些已验证的优化
   (`perf-baseline.md:143-154`)与 GPU 无关,不要借 GPU 立项把它们推翻。

## 八、怎么低成本证伪本文结论

1. 对 `fast` 档做一次 profiler(Windows 上可用 ETW / `tracing` 已有 span 基建),
   拆出那 1 s 本地预处理的构成 —— 若其中算力占比 <5%,则「GPU 无用」成立。
2. 写一个 500 行内的微基准:对 200×1536 的余弦打分,比较 CPU 标量、CPU SIMD、GPU 三条路径的端到端
   (含传输与启动)耗时。预期 GPU 不占优。
3. 对极端 `plan` 任务(累计 150 万 token)统计 token 缓存命中率;若命中率高,
   说明连「最大的本地算术负载」也已被缓存消化。

## 九、核验到的一处文档与代码不一致

`docs/known-limitations.md` L7 描述为「vec0 走精确 KNN」,但代码路径并非 vec0 的 KNN 查询:
`services/memory_service.rs:545-590` 的 `get_vectors` 是按 `WHERE memory_id IN (?)` 取回 BLOB,
再在 `:263` 用 Rust 侧 `cosine_similarity` 打分(`embedding_service.rs:191`)。
即当前实现是「按 id 取向量 + Rust 内标量余弦」,`vec0` 只作存储用。
该差异不影响 L7 的结论(都是精确检索、无 ANN),但描述应更正。
