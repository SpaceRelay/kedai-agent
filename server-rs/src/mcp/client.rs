// MCP stdio 客户端协议层(批次 6.2,L3 隔离):JSON-RPC 2.0 over stdio。
//
// 行帧说明:MCP stdio 传输是「换行分隔 JSON」(NDJSON)——每条消息一个完整 JSON 对象,
// 以 \n 结尾;不是 LSP 的 Content-Length 头帧。实现与测试都按行读写。
//
// 读写解耦:客户端只依赖 AsyncRead + AsyncWrite(trait object),测试用 tokio::io::duplex
// 双工内存管模拟服务器,不起真实进程;process.rs 负责把子进程 stdin/stdout 接到这里。
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

/// initialize 握手超时(冷启动;超时按「服务器不可用」处理,记 warn 并禁用,不 panic)
pub const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(30);
/// tools/list 超时(启动装配路径,与握手同档)
pub const LIST_TOOLS_TIMEOUT: Duration = Duration::from_secs(30);
/// tools/call 超时:110s,略小于注册表 MCP 档 120s——让客户端先返回可读错误,
/// 注册表 120s 硬超时只做兜底(错误文案带「下一步」指引)。
pub const CALL_TOOL_TIMEOUT: Duration = Duration::from_secs(110);

/// initialize 握手声明的协议版本(2024-11-05 为广泛支持的稳定版)
const PROTOCOL_VERSION: &str = "2024-11-05";

/// tools/list 返回的单个工具描述
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    /// JSON Schema(MCP 字段名 inputSchema;OpenAI 兼容工具定义直接复用)
    pub input_schema: Value,
}

type BoxedReader = BufReader<Box<dyn AsyncRead + Unpin + Send>>;
type BoxedWriter = Box<dyn AsyncWrite + Unpin + Send>;

/// 共享内部状态:reader 任务与 request() 都要访问(等待表按 id 派发响应)
struct Shared {
    writer: tokio::sync::Mutex<BoxedWriter>,
    /// 在飞请求:id → 响应通道。std Mutex 仅在同步代码块内取放,不跨 .await。
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    next_id: AtomicU64,
    /// 关停标志:close()/Drop 后新请求立即失败(执行器闭包可能仍持 Arc,
    /// 不能依赖「最后一个 Arc 析构」来表达关停语义)
    closed: AtomicBool,
}

/// MCP stdio 客户端:一个后台 reader 任务按 id 把响应派发给在飞请求。
/// Drop 时 abort reader 任务并让所有在飞请求以「连接已关闭」失败。
pub struct McpClient {
    shared: Arc<Shared>,
    reader_task: tokio::task::JoinHandle<()>,
}

impl McpClient {
    /// 在给定字节流上启动客户端(立即 spawn reader 任务;初始化握手由 initialize() 显式发起)
    pub fn new(read: Box<dyn AsyncRead + Unpin + Send>, write: BoxedWriter) -> Self {
        let shared = Arc::new(Shared {
            writer: tokio::sync::Mutex::new(write),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        });
        let reader_shared = shared.clone();
        let reader_task = tokio::spawn(async move {
            read_loop(Box::new(BufReader::new(read)), reader_shared).await;
        });
        McpClient {
            shared,
            reader_task,
        }
    }

    /// initialize 握手:发送 initialize(clientInfo: kedai/0.2.0,capabilities 空),
    /// 成功后回 notifications/initialized 通知(无 id,不等响应)。
    pub async fn initialize(&self) -> Result<(), String> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "kedai", "version": env!("CARGO_PKG_VERSION") },
        });
        self.request("initialize", params, INITIALIZE_TIMEOUT)
            .await?;
        self.notify("notifications/initialized", json!({})).await
    }

    /// tools/list:列出服务器全部工具
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, String> {
        let result = self
            .request("tools/list", json!({}), LIST_TOOLS_TIMEOUT)
            .await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(tools
            .into_iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?.to_string();
                Some(McpToolInfo {
                    name,
                    description: t
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    input_schema: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                })
            })
            .collect())
    }

    /// tools/call:调用工具,把 result.content[] 中的 text 项拼接为 String 返回;
    /// result.isError = true 时按工具级失败返回 Err(文本同样取自 content)。
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
                CALL_TOOL_TIMEOUT,
            )
            .await?;
        let text = extract_content_text(&result);
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(format!("MCP 工具 \"{name}\" 返回错误:{text}"));
        }
        Ok(text)
    }

    /// 显式关停:标志位置位 → 新请求立即失败;abort reader 任务;
    /// 清空等待表,在飞请求以「连接已关闭」收场。幂等。
    /// (进程句柄的 kill 由 McpProcess 侧负责,本方法只管协议层)
    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::Relaxed);
        self.reader_task.abort();
        self.shared
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// 发送通知(无 id,不等响应;写失败仅告警——通知不可重试也不影响在飞请求)
    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let mut line = frame.to_string();
        line.push('\n');
        let mut w = self.shared.writer.lock().await;
        w.write_all(line.as_bytes())
            .await
            .map_err(|e| format!("MCP 通知 \"{method}\" 写入失败: {e}"))?;
        w.flush()
            .await
            .map_err(|e| format!("MCP 通知 \"{method}\" 刷写失败: {e}"))
    }

    /// 发送请求并等待响应:先在等待表登记 id,再写行帧,最后带超时等 oneshot。
    /// 超时/写失败都会清掉等待表项,避免泄漏;响应乱序或未知 id 由 reader 任务丢弃。
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        if self.shared.closed.load(Ordering::Relaxed) {
            return Err(format!(
                "MCP 请求 \"{method}\" 失败:连接已关闭(客户端已关停)"
            ));
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);

        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut line = frame.to_string();
        line.push('\n');
        {
            let mut w = self.shared.writer.lock().await;
            let write_result = match w.write_all(line.as_bytes()).await {
                Ok(()) => w.flush().await,
                Err(e) => Err(e),
            };
            if let Err(e) = write_result {
                self.shared
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                return Err(format!("MCP 请求 \"{method}\" 写入失败: {e}"));
            }
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => {
                if let Some(err) = resp.get("error") {
                    let msg = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("未知错误");
                    return Err(format!("MCP 请求 \"{method}\" 被服务器拒绝: {msg}"));
                }
                Ok(resp.get("result").cloned().unwrap_or(Value::Null))
            }
            // oneshot 发送端被 drop = reader 任务结束(连接关闭/进程退出)
            Ok(Err(_)) => Err(format!(
                "MCP 请求 \"{method}\" 失败:连接已关闭(服务器进程可能已退出)"
            )),
            Err(_) => {
                self.shared
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                Err(format!(
                    "MCP 请求 \"{method}\" 超时({}s)。下一步:检查该 MCP 服务器是否正常运行,或在设置中禁用",
                    timeout.as_secs()
                ))
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // 与 close() 同语义:停 reader、置关停标志;在飞请求的 oneshot 发送端
        // 随等待表清空/Shared 析构而 drop,等待方收到「连接已关闭」错误,不悬挂。
        self.close();
    }
}

/// 后台读循环:逐行读取 NDJSON,按 id 派发给等待表;
/// 坏行/通知/未知 id 一律跳过(容错,不中断会话);EOF 或读错误即结束。
async fn read_loop(reader: Box<BoxedReader>, shared: Arc<Shared>) {
    let mut reader = reader;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF:服务器关闭 stdout(进程退出)
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
            tracing::warn!(
                line_preview = trimmed.chars().take(120).collect::<String>(),
                "MCP 收到无法解析的行,已跳过"
            );
            continue;
        };
        // 只关心带 id 的响应;通知(无 id)/请求(服务器→客户端)v1 不处理
        let Some(id) = msg.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let tx = shared
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        if let Some(tx) = tx {
            // 接收端可能已因超时被清走;发送失败忽略
            let _ = tx.send(msg);
        }
        // 未知/已超时 id:丢弃(乱序容错)
    }
    // 读循环结束(EOF/错误):清空等待表,让所有在飞请求以「连接已关闭」失败
    shared
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// 拼接 result.content[] 中所有 type=text 项的 text;无 content 时回退整段 JSON
/// (部分服务器只返回 structuredContent)。空结果给占位文案,避免模型拿到空串困惑。
fn extract_content_text(result: &Value) -> String {
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        let text = content
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            return text;
        }
    }
    if result.is_null() {
        return "(MCP 工具返回空结果)".to_string();
    }
    result.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, DuplexStream};

    /// 内存双工管模拟 MCP 服务器:逐行读请求,按 method 回固定响应;
    /// respond 闭包可自定义(坏行/乱序 id 容错测试用)。
    fn mock_server(
        mut server: DuplexStream,
        respond: impl Fn(&Value) -> Vec<String> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut reader = BufReader::new(&mut server);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(req) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };
                for resp in respond(&req) {
                    let mut out = resp;
                    out.push('\n');
                    if reader.get_mut().write_all(out.as_bytes()).await.is_err() {
                        return;
                    }
                }
                let _ = reader.get_mut().flush().await;
            }
        })
    }

    /// 标准响应器:initialize / tools/list / tools/call 各回固定结果,通知不回。
    fn standard_respond(req: &Value) -> Vec<String> {
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let Some(id) = id else {
            return Vec::new(); // 通知:无响应
        };
        let result = match method {
            "initialize" => {
                json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "serverInfo": { "name": "mock", "version": "0.0.1" } })
            }
            "tools/list" => json!({ "tools": [
                { "name": "echo", "description": "回显", "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } } },
                { "name": "fail", "description": "总是失败" },
            ]}),
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                if name == "fail" {
                    json!({ "content": [{ "type": "text", "text": "炸了" }], "isError": true })
                } else {
                    let arg = req["params"]["arguments"]["text"].as_str().unwrap_or("");
                    json!({ "content": [
                        { "type": "text", "text": "第一行" },
                        { "type": "resource", "resource": { "uri": "x://y" } },
                        { "type": "text", "text": format!("回显:{arg}") },
                    ]})
                }
            }
            _ => json!({}),
        };
        vec![json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()]
    }

    fn client_pair() -> (McpClient, DuplexStream) {
        let (client_io, server_io) = duplex(64 * 1024);
        let (cr, cw) = tokio::io::split(client_io);
        (McpClient::new(Box::new(cr), Box::new(cw)), server_io)
    }

    #[tokio::test]
    async fn handshake_list_and_call_over_ndjson() {
        let (client, server) = client_pair();
        let server_task = mock_server(server, standard_respond);

        client.initialize().await.expect("握手应成功");
        let tools = client.list_tools().await.expect("tools/list 应成功");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].description, "回显");
        assert_eq!(
            tools[1].input_schema,
            json!({ "type": "object" }),
            "缺 inputSchema 应回退空对象"
        );

        let out = client
            .call_tool("echo", json!({ "text": "你好" }))
            .await
            .expect("tools/call 应成功");
        // 只拼接 type=text 项,跳过 resource 项
        assert_eq!(out, "第一行\n回显:你好");

        let err = client.call_tool("fail", json!({})).await.unwrap_err();
        assert!(err.contains("炸了"), "isError 应按失败返回: {err}");

        drop(client);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn request_frames_are_jsonrpc2_ndjson() {
        let (client, server) = client_pair();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
        let server_task = tokio::spawn(async move {
            let mut reader = BufReader::new(server);
            let mut line = String::new();
            // 只抓第一帧(initialize 请求),回完继续吞帧到 EOF
            // (不回完就退出会让客户端的 initialized 通知写进已关管道,握手判失败)
            reader.read_line(&mut line).await.unwrap();
            tx.send(line.trim().to_string()).await.unwrap();
            let resp = json!({ "jsonrpc": "2.0", "id": 1, "result": { "protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "serverInfo": { "name": "m", "version": "1" } } });
            reader
                .get_mut()
                .write_all(format!("{resp}\n").as_bytes())
                .await
                .unwrap();
            let _ = reader.get_mut().flush().await;
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });

        client.initialize().await.expect("握手应成功");
        let frame: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        assert_eq!(frame["jsonrpc"], "2.0");
        assert_eq!(frame["method"], "initialize");
        assert_eq!(frame["id"], 1, "首请求 id 从 1 开始");
        assert_eq!(frame["params"]["clientInfo"]["name"], "kedai");
        assert_eq!(
            frame["params"]["protocolVersion"], PROTOCOL_VERSION,
            "应声明协议版本"
        );

        drop(client);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn bad_lines_and_unknown_ids_are_tolerated() {
        let (client, server) = client_pair();
        // 响应器:先回一行垃圾 + 一个未知 id 响应,再回正常响应
        let server_task = mock_server(server, |req| {
            let mut out = vec![
                "这不是 JSON".to_string(),
                json!({ "jsonrpc": "2.0", "id": 9999, "result": {} }).to_string(),
            ];
            out.extend(standard_respond(req));
            out
        });

        client
            .initialize()
            .await
            .expect("坏行/未知 id 不应打断握手");
        let tools = client.list_tools().await.expect("后续请求应正常");
        assert_eq!(tools.len(), 2);

        drop(client);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn server_disconnect_fails_inflight_request() {
        let (client, server) = client_pair();
        // 服务器不回任何响应,直接关闭
        drop(server);
        let err = client
            .request("tools/list", json!({}), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(
            err.contains("连接已关闭") || err.contains("写入失败"),
            "断连应明确报错: {err}"
        );
    }
}
