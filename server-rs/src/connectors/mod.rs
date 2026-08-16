// 连接器模块
pub mod mock;
pub mod openai_compatible;

use crate::models::types::{GenerationParams, LlmMessage, LlmStreamChunk};
use serde_json::Value;
use tokio::sync::{mpsc, watch};

/// 连接器统一枚举(mock / openai-compatible)
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
    pub async fn generate(
        &self,
        messages: &[LlmMessage],
        params: GenerationParams,
        abort: watch::Receiver<bool>,
    ) -> Result<Vec<LlmStreamChunk>, String> {
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
    ) -> Result<(), String> {
        match self {
            Connector::OpenAi(c) => c.generate_stream(messages, params, abort, tx).await,
            Connector::Mock(m) => {
                for chunk in m.generate(messages, params, abort.clone()).await? {
                    if *abort.borrow() {
                        return Err("生成已中断".into());
                    }
                    tx.send(chunk).map_err(|_| "生成接收端已关闭".to_string())?;
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
