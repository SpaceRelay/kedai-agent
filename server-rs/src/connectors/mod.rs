// 连接器模块
//
// 代际: L2(中层·干 / Orchestration)——LLM 后端适配(协议翻译)。
// 判据: 把各提供商的线格式翻译成本项目内部类型;只依赖 L1(models)。
// 纪律: 新增提供商在此适配,不得把提供商特有字段泄漏到上层。
pub mod mock;
pub mod openai_compatible;

use crate::models::llm_error::LlmError;
use crate::models::types::{GenerationParams, LlmMessage, LlmStreamChunk};
use serde_json::Value;
use tokio::sync::{mpsc, watch};

/// 连接器统一错误类型：分类（决定 SSE `Error.code`/`retryable`）+ 面向用户文案。
///
/// 连接器是**唯一的错误分类边界**：上游 HTTP 状态、reqwest 传输失败、SSE 协议异常
/// 都在此折算成 [`LlmError`]，上层（引擎/任务/API）只消费分类，不再解析文案
/// （旧的字符串子串分类已删除；见 `models/llm_error.rs`）。
pub type ConnectorError = LlmError;

/// 连接器统一枚举(mock / openai-compatible)。
///
/// `Clone` 语义(2026-09-13 批次 3):内部只持 String 与 reqwest Client(Arc 支撑),
/// clone 是廉价快照 —— 调用方应 `read().await.clone()` 取快照后**立即释放读锁**,
/// 不要把 RwLockGuard 跨流式生成持有(否则生成期间保存设置会阻塞)。
#[derive(Clone)]
pub enum Connector {
    Mock(mock::MockConnector),
    OpenAi(openai_compatible::OpenAiCompatibleConnector),
}

impl Connector {
    /// 类型名,与 Node 版 connector.type 一致
    pub fn type_name(&self) -> &'static str {
        match self {
            Connector::Mock(_) => "mock",
            Connector::OpenAi(_) => "openai-compatible",
        }
    }

    /// 当前模型名
    pub fn model(&self) -> &str {
        match self {
            Connector::Mock(_) => "mock-demo",
            Connector::OpenAi(c) => c.model(),
        }
    }

    pub async fn available_models(&self) -> Vec<String> {
        match self {
            Connector::Mock(m) => m.list_models(),
            Connector::OpenAi(c) => c.list_models_async().await,
        }
    }

    /// 测试连接:返回结构化诊断,不把模型列表回退误报为真实连通。
    pub async fn test(&self) -> Value {
        match self {
            Connector::Mock(m) => {
                let (ok, message) = m.test();
                serde_json::json!({
                    "ok": ok, "message": message, "models": m.list_models(),
                    "endpoint_reachable": true, "authenticated": true,
                    "fallback_used": false, "http_status": 200
                })
            }
            Connector::OpenAi(c) => c.test_connection().await,
        }
    }

    /// 流式生成
    ///
    /// 错误经 [`ConnectorError`] 携带分类（超时/限流/鉴权/上游/生成），
    /// 供引擎直接映射为 SSE `Error` 终态而无需猜测。
    pub async fn generate(
        &self,
        messages: &[LlmMessage],
        params: GenerationParams,
        abort: watch::Receiver<bool>,
    ) -> Result<Vec<LlmStreamChunk>, ConnectorError> {
        match self {
            Connector::Mock(m) => m.generate(messages, params, abort).await,
            Connector::OpenAi(c) => c.generate(messages, params, abort).await,
        }
    }

    /// 真正逐块生成:OpenAI 连接器读取网络响应时立即发送;Mock 保持原行为并逐项转发。
    pub async fn generate_stream(
        &self,
        messages: &[LlmMessage],
        params: GenerationParams,
        abort: watch::Receiver<bool>,
        tx: mpsc::UnboundedSender<LlmStreamChunk>,
    ) -> Result<(), ConnectorError> {
        match self {
            Connector::OpenAi(c) => c.generate_stream(messages, params, abort, tx).await,
            Connector::Mock(m) => {
                for chunk in m.generate(messages, params, abort.clone()).await? {
                    if *abort.borrow() {
                        // 中断不是上游失败(取消由调用方按 abort 标志判定),分类仅作占位
                        return Err(LlmError::generation("生成已中断"));
                    }
                    // 有意丢弃 SendError:接收端关闭即原因本身,错误文案已等价表达该语义
                    tx.send(chunk)
                        .map_err(|_| LlmError::generation("生成接收端已关闭"))?;
                }
                Ok(())
            }
        }
    }
}

/// 构建连接器(按 config.connector;未知回退 mock)
pub fn build_connector(connector: &str, base_url: &str, api_key: &str, model: &str) -> Connector {
    if connector == "openai-compatible" {
        Connector::OpenAi(openai_compatible::OpenAiCompatibleConnector::new(
            base_url, api_key, model,
        ))
    } else {
        Connector::Mock(mock::MockConnector::new())
    }
}

/// 运行时切换模型:mock 保持不变,openai-compatible 重建(保留 base_url/api_key)
pub fn with_model(c: &Connector, model: &str) -> Connector {
    match c {
        Connector::Mock(_) => Connector::Mock(mock::MockConnector::new()),
        Connector::OpenAi(o) => Connector::OpenAi(o.with_model(model)),
    }
}

/// 可用连接器列表(与 Node 版 CONNECTOR_TYPES 对齐)
pub fn available_connector_types() -> Vec<Value> {
    serde_json::json!([
        { "type": "openai-compatible", "label": "OpenAI 兼容" },
        { "type": "mock", "label": "演示(Mock)" }
    ])
    .as_array()
    .cloned()
    .unwrap_or_default()
}
