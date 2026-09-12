// 角色脚本执行:generate/import 处理器构建与后端脚本串行执行(阶段三 3b-3、
// 阶段六 6g)。自 engine/mod.rs 拆分迁入,纯代码移动,逻辑不变。
// 优化项 B-2:generate 处理器改为「mpsc 投递请求 + std mpsc 阻塞等回执」,
// 由常驻调度任务(generate_dispatch_loop)执行异步生成,消除对
// Handle::current().block_on 隐式线程契约的依赖。
// 依赖经 `use super::*` 取自 engine/mod.rs(与 executor/mvu 等子模块同一约定)。
use super::*;

/// 优化项 B-2:脚本 generate 调度请求。同步 handler 投递,常驻调度任务消费。
pub(super) struct GenerateRequest {
    messages: Vec<LlmMessage>,
    params: GenerationParams,
    abort: watch::Receiver<bool>,
    /// 回执通道:tokio oneshot 无同步等待 API,std mpsc 的 recv 阻塞等在任何
    /// 线程上都安全;调度任务终止(引擎关闭/抢占/运行时退出)时发送端随请求
    /// drop,recv 立即返回 RecvError,handler 转为明确错误而非悬挂。
    reply: std::sync::mpsc::Sender<Result<(String, TokenUsage), String>>,
}

/// 优化项 B-2:常驻 generate 调度任务。串行消费 handler 投递的请求,执行
/// 「读 connector → 流式生成 → 聚合 Token/Usage chunks」(原 make_generate_handler
/// 内 block_on 的异步体原样搬移,行为不变,abort 语义透传),结果经请求自带的
/// 回执通道返回。全部发送端断开后 recv 返回 None,任务自行退出。
async fn generate_dispatch_loop(
    connector: Arc<RwLock<Connector>>,
    mut rx: mpsc::Receiver<GenerateRequest>,
) {
    while let Some(req) = rx.recv().await {
        let GenerateRequest {
            messages,
            params,
            abort,
            reply,
        } = req;
        let conn = connector.read().await;
        let chunks = conn.generate(&messages, params, abort).await;
        drop(conn);
        let result = chunks.map(|chunks| {
            let mut out = String::new();
            let mut usage = TokenUsage::default();
            for chunk in chunks {
                match chunk {
                    LlmStreamChunk::Token(t) => out.push_str(&t),
                    LlmStreamChunk::Usage {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        prompt_cache_hit_tokens,
                        prompt_cache_miss_tokens,
                        ..
                    } => {
                        usage.prompt_tokens += prompt_tokens;
                        usage.completion_tokens += completion_tokens;
                        usage.total_tokens += total_tokens;
                        usage.prompt_cache_hit_tokens += prompt_cache_hit_tokens;
                        usage.prompt_cache_miss_tokens += prompt_cache_miss_tokens;
                    }
                    _ => {}
                }
            }
            (out, usage)
        });
        // 回执发送失败(handler 侧已离开)直接丢弃,继续服务后续请求
        let _ = reply.send(result);
    }
}

/// 由调度通道发送端构建同步 generate 处理器闭包(make_generate_handler 与
/// 回归测试共用)。机制与线程安全性见 make_generate_handler 注释。
fn generate_handler_from_tx(
    tx: mpsc::Sender<GenerateRequest>,
) -> Arc<crate::scripts::bridge::GenerateHandler> {
    Arc::new(move |messages, params, abort| {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let req = GenerateRequest {
            messages,
            params,
            abort,
            reply: reply_tx,
        };
        // 投递一律非阻塞:通道满(并发脚本生成排队超出容量)或已关闭(调度
        // 循环停止)时返回明确错误,绝不阻塞当前线程。
        tx.try_send(req).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                "generate 调度通道已满(并发脚本生成请求过多),请稍后重试".to_string()
            }
            mpsc::error::TrySendError::Closed(_) => {
                "generate 调度循环已停止(引擎关闭或运行时已退出)".to_string()
            }
        })?;
        // 阻塞等回执:std mpsc recv;调度任务终止时发送端 drop → RecvError
        // → 明确错误,不悬挂。
        reply_rx
            .recv()
            .map_err(|_| "generate 调度循环异常终止,未返回生成结果".to_string())?
    })
}

impl AgentEngine {
    /// 阶段六 6g-1(优化项 B-2 改造):构建 generate 处理器。
    /// 不再在同步闭包内 Handle::current().block_on(该写法依赖「调用线程不是
    /// runtime 驱动线程」的隐式契约,worker 线程直调会 panic)。
    /// 新机制为「投递请求 + 等回执」:闭包仅捕获 mpsc 发送端,把请求投递给常驻
    /// 调度任务(generate_dispatch_loop,持 connector 的 Arc clone),再经
    /// std::sync::mpsc 阻塞等结果:
    /// - 投递走非阻塞 try_send。阻塞式发送(blocking_send)在 runtime 驱动线程
    ///   上会 panic(tokio:Cannot block the current thread from within a
    ///   runtime),而「当前线程是否为 runtime 驱动线程」无公共 API 可探测
    ///   (spawn_blocking 线程与 worker 线程上 Handle::try_current 均为 Ok,
    ///   实测 tokio 1.53),故一律不采用;通道满/已关闭即返回明确错误;
    /// - 回执等待用 std mpsc(见 GenerateRequest.reply),对调用线程零假设;
    /// - 引擎关闭/抢占 → 通道断开或回执端 drop → handler 收到明确错误,不悬挂。
    fn make_generate_handler(&self) -> Arc<crate::scripts::bridge::GenerateHandler> {
        generate_handler_from_tx(self.generate_dispatch_tx())
    }

    /// 惰性建立(并复用)generate 调度通道:首次调用时 tokio::spawn 常驻调度
    /// 任务;调用时须处于 runtime 上下文(run_character_scripts 为 async,
    /// 天然满足)。通道容量 4:单会话脚本串行执行,在途请求仅 1 个;余量吸收
    /// 多会话并发排队,又保证异常时堆积有界。调度任务意外退出(发送端 is_closed)
    /// 时下次调用自动重建。
    fn generate_dispatch_tx(&self) -> mpsc::Sender<GenerateRequest> {
        let mut guard = self
            .generate_dispatch
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = guard.as_ref().filter(|tx| !tx.is_closed()) {
            return tx.clone();
        }
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(generate_dispatch_loop(self.connector.clone(), rx));
        *guard = Some(tx.clone());
        tx
    }

    /// 阶段六 6g-2:构建导入处理器(捕获各 service 的 Arc clone,绕过 HTTP 直接调用)。
    /// 五类导入:character/worldbook/preset/chat/regex(暂不支持)。
    fn make_import_handler(&self) -> Arc<crate::scripts::bridge::ImportHandler> {
        let characters = self.characters.clone();
        let world_books = self.world_books.clone();
        let prompt_inject = self.prompt_inject.clone();
        let sessions = self.sessions.clone();
        Arc::new(
            move |kind: String, filename: String, content: String, session_id: String| match kind
                .as_str()
            {
                "character" => {
                    let rec = characters.upload(content.as_bytes(), &filename)?;
                    Ok(format!("已导入角色:{}", rec.id))
                }
                "worldbook" => {
                    let rec = world_books.upload(content.as_bytes(), &filename, None)?;
                    Ok(format!("已导入世界书:{}", rec.id))
                }
                "preset" => {
                    let floors = crate::parsing::preset::parse_st_preset(&content)?;
                    let mut svc = prompt_inject.lock().unwrap_or_else(|e| e.into_inner());
                    let mut cfg = svc.get().clone();
                    cfg.floors = floors.clone();
                    svc.set(cfg)?;
                    Ok(format!("已导入预设:{} 个楼层", floors.len()))
                }
                "chat" => {
                    if session_id.trim().is_empty() {
                        return Err("importRawChat 需指定会话(sessionId)".to_string());
                    }
                    let messages: Vec<crate::models::types::StMessage> =
                        serde_json::from_str(&content)
                            .map_err(|e| format!("解析聊天消息失败: {e}"))?;
                    let n = sessions.import_chat(&session_id, messages)?;
                    Ok(format!("已导入会话:{n} 条消息"))
                }
                "regex" => Err("importRawTavernRegex 暂不支持".to_string()),
                other => Err(format!("未知导入类型:{other}")),
            },
        )
    }

    /// 阶段三 3b-3:执行后端脚本(消息生成完成后)。
    /// 依次收集「全局脚本」与「角色卡脚本」的启用脚本,串行执行;
    /// 脚本经 TavernHelper 兼容桥写回共享 scopes(global/character/script 等),
    /// 由调用方收尾 take_others 统一落库。执行失败仅记日志,不影响主流程。
    pub(super) async fn run_character_scripts(
        &self,
        character_id: &str,
        scopes: &Arc<Mutex<crate::parsing::scopes::ScopeVars>>,
    ) {
        // 全局脚本(scope=global,owner 恒为空)
        let mut all: Vec<crate::scripts::loader::LoadedScript> = Vec::new();
        if let Ok(g) = self.user_scripts.get_tree("global", "") {
            all.extend(crate::scripts::loader::collect_enabled_scripts(&g));
        }
        // 角色卡脚本(含旧字段迁移;角色不存在/无脚本树 → 跳过)
        if let Ok(t) = self.user_scripts.get_character(character_id) {
            all.extend(crate::scripts::loader::collect_enabled_scripts(&t));
        }
        if all.is_empty() {
            return;
        }
        let opts = crate::scripts::runtime::EvalOptions::default();
        // 阶段六 6g:生成/导入处理器在循环外构建一次(捕获最小依赖:connector 与各
        // service 的 Arc clone),所有脚本共享同一份接线。
        let generate_handler = self.make_generate_handler();
        let import_handler = self.make_import_handler();
        for script in all {
            // 每次执行新建 EvalBridge(共享 scopes + 角色 id);运行时为同步阻塞,
            // 经 spawn_blocking 隔离,避免阻塞 tokio 运行时(quickjs 非线程安全,
            // 每次执行独立 Runtime,天然无跨线程共享)。
            let bridge = crate::scripts::bridge::EvalBridge::new(scopes.clone(), script.clone())
                .with_character(character_id.to_string())
                .with_registry(self.slash.clone())
                .with_generate(generate_handler.clone())
                .with_imports(import_handler.clone());
            let source = script.content.clone();
            let opts = opts.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                crate::scripts::runtime::eval_with_bridge(&source, &opts, &bridge)
            })
            .await
            .unwrap_or_else(|_| {
                crate::scripts::runtime::EvalOutcome::Error("脚本执行任务被取消".into())
            });
            if let crate::scripts::runtime::EvalOutcome::Error(msg) = outcome {
                tracing::warn!(
                    character_id = character_id.to_string(),
                    script = script.name,
                    error = msg,
                    "角色脚本执行失败"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 mock connector 的 Arc(调度循环测试用)
    fn mock_connector() -> Arc<RwLock<Connector>> {
        Arc::new(RwLock::new(Connector::Mock(
            crate::connectors::mock::MockConnector::new(),
        )))
    }

    /// 与 bridge.generate_from_config 同形态的生成参数(白名单子集)
    fn test_params() -> GenerationParams {
        GenerationParams {
            temperature: 1.0,
            top_p: 1.0,
            max_tokens: 1024,
            stop: None,
            tools: Vec::new(),
            max_tool_rounds: None,
            tool_choice: crate::models::types::ToolChoice::Auto,
            parallel_tool_calls: None,
        }
    }

    /// 构造一个仅占位的调度请求(测试预填通道用)
    fn dummy_request() -> GenerateRequest {
        let (reply_tx, _reply_rx) = std::sync::mpsc::channel();
        let (_abort_tx, abort_rx) = watch::channel(false);
        GenerateRequest {
            messages: vec![LlmMessage::plain("user", "占位")],
            params: test_params(),
            abort: abort_rx,
            reply: reply_tx,
        }
    }

    /// 回归(优化项 B-2):spawn_blocking 线程内经 handler 正常往返,文本与
    /// usage 聚合正确(mock 的 [[reply:]] 钩子免默认分支逐字 8ms sleep)。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn generate_handler_roundtrip_in_spawn_blocking() {
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(generate_dispatch_loop(mock_connector(), rx));
        let handler = generate_handler_from_tx(tx);
        let result = tokio::task::spawn_blocking(move || {
            let (_abort_tx, abort_rx) = watch::channel(false);
            handler(
                vec![LlmMessage::plain("user", "[[reply:脚本生成OK]]")],
                test_params(),
                abort_rx,
            )
        })
        .await
        .expect("spawn_blocking 任务应正常结束");
        let (text, usage) = result.expect("spawn_blocking 内调用应成功往返");
        assert_eq!(text, "脚本生成OK");
        assert!(usage.total_tokens > 0, "usage 应被聚合:{usage:?}");
    }

    /// 回归(优化项 B-2):在 runtime worker 上下文(非 spawn_blocking)直接调用
    /// handler,通道满时返回明确错误,绝不 panic、绝不阻塞(旧实现此处会经
    /// Handle::block_on / blocking_send panic)。预填占满容量使投递必走 Full 分支。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn generate_handler_on_worker_thread_errors_not_panic() {
        let (tx, _rx) = mpsc::channel(1);
        tx.try_send(dummy_request()).expect("预填应占满容量");
        let handler = generate_handler_from_tx(tx);
        // 直接在 async 测试体(runtime worker 线程)上同步调用 handler
        let (_abort_tx, abort_rx) = watch::channel(false);
        let result = handler(
            vec![LlmMessage::plain("user", "你好")],
            test_params(),
            abort_rx,
        );
        let msg = result.expect_err("通道满应返回明确错误而非 panic/悬挂");
        assert!(msg.contains("调度通道已满"), "{msg}");
    }

    /// 回归(优化项 B-2):通道关闭/调度任务中止时,handler 收到明确错误而非悬挂。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn generate_handler_errors_when_dispatch_gone() {
        // 场景一:接收端已 drop(调度循环不存在)→ try_send Closed → 明确错误
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let handler = generate_handler_from_tx(tx);
        let (_abort_tx, abort_rx) = watch::channel(false);
        let msg = handler(
            vec![LlmMessage::plain("user", "你好")],
            test_params(),
            abort_rx,
        )
        .expect_err("通道关闭应返回明确错误");
        assert!(msg.contains("调度循环已停止"), "{msg}");

        // 场景二:请求在途时调度任务被 abort(引擎抢占/运行时退出)→ 回执
        // sender 随请求 drop → handler 的 recv 返回 RecvError → 明确错误。
        // mock 默认分支逐字 8ms sleep(回复百余字,耗时约秒级),留出 abort 窗口。
        let (tx, rx) = mpsc::channel(4);
        let task = tokio::spawn(generate_dispatch_loop(mock_connector(), rx));
        let handler = generate_handler_from_tx(tx);
        let call = tokio::task::spawn_blocking(move || {
            let (_abort_tx, abort_rx) = watch::channel(false);
            handler(
                vec![LlmMessage::plain("user", "普通输入")],
                test_params(),
                abort_rx,
            )
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        task.abort();
        let msg = call
            .await
            .expect("spawn_blocking 任务应正常结束")
            .expect_err("调度任务中止应返回明确错误而非悬挂");
        assert!(msg.contains("异常终止"), "{msg}");
    }
}
