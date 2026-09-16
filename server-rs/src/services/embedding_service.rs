// 向量化服务(embedding):调用 OpenAI 兼容的 POST {base}/embeddings 生成句向量,
// 供记忆库的语义召回使用(升级工作流 Phase 3)。
//
// 设计要点:
//   - 独立凭据:Base URL / API Key / 模型 / 维度全部走 RuntimeSettings 的 embedding_* 字段,
//     与聊天连接器完全解耦(可指向不同厂商);Key 与聊天 Key 同策略(内存明文、落盘 DPAPI 加密)。
//   - 零额外依赖:复用 reqwest + normalize_base_url;响应解析按 OpenAI 规范
//     `{ data: [{ embedding: [f32], index }], usage }`。
//   - 维度自适应:embedding_dim=0 表示未探测,首次调用后由调用方回填实际维度。
//   - 批量与顺序无关:按 index 排序还原,避免服务端乱序返回导致向量与文本错位。
use crate::services::settings_service::{normalize_base_url, RuntimeSettings};
use serde_json::json;
use std::time::Duration;

/// 单次请求最多嵌入的文本条数(控制请求体大小与超时风险)
pub const EMBED_BATCH_SIZE: usize = 32;
/// 单条文本最大字符数(超长截断;embedding 模型普遍有 token 上限)
pub const EMBED_MAX_CHARS: usize = 4000;

/// embedding 调用错误(区分「未配置」与「调用失败」,便于上层决定降级还是报错)
#[derive(Debug)]
pub enum EmbedError {
    /// 未启用或凭据不全:调用方应静默降级为纯关键词召回
    NotConfigured(String),
    /// 已配置但调用失败(网络/鉴权/协议)
    Failed(String),
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbedError::NotConfigured(m) => write!(f, "向量化未配置: {m}"),
            EmbedError::Failed(m) => write!(f, "向量化调用失败: {m}"),
        }
    }
}

/// 向量化服务:持有设置快照即可构造,无内部状态(每次调用读最新配置)
pub struct EmbeddingService {
    client: reqwest::Client,
}

/// 进程级共享的 HTTP 客户端(2026-09-16 性能批次 P-5)。
///
/// 为什么共享:构造点遍布热路径(引擎每轮召回、记忆工具每次写、`/api/memory/distill`
/// 与 `/api/memory/:id` 的循环内逐条),而 `EmbeddingService` 无内部状态、每次调用都从
/// 设置读最新配置 —— 客户端没有任何「随实例变化」的状态。此前的 `new()` 每次新建
/// reqwest Client,等于丢掉连接池与 keepalive,让每条 embedding 请求重做 TCP(+TLS)
/// 握手;embedding 是「一轮对话一次 + 批量蒸馏多次」的高频调用,这笔握手成本很显眼。
///
/// 为什么安全:reqwest::Client 内部就是 `Arc<...>`,克隆/复用本就是其设计用法;
/// 超时等 builder 选项是客户端级配置,与凭据无关(凭据走每次请求的 header),
/// 故共享不会串号。
static SHARED_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

/// 请求超时:embedding 多为批量短请求,60s 足够,且避免挂死
const EMBED_TIMEOUT_SECS: u64 = 60;

/// 共享客户端被真正构造的次数(仅测试用)。
/// reqwest::Client 不暴露内部地址,无法用指针比较证明「同一个客户端」;
/// 改为计数**初始化次数**:若每次 new() 都新建,这个值会随构造次数增长。
#[cfg(test)]
static CLIENT_INIT_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn shared_client() -> reqwest::Client {
    SHARED_CLIENT
        .get_or_init(|| {
            #[cfg(test)]
            CLIENT_INIT_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            reqwest::Client::builder()
                .timeout(Duration::from_secs(EMBED_TIMEOUT_SECS))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        })
        .clone()
}

impl EmbeddingService {
    pub fn new() -> Self {
        EmbeddingService {
            client: shared_client(),
        }
    }

    /// 校验配置可用性;返回规范化后的 (base_url, api_key, model, dim)
    fn resolve(s: &RuntimeSettings) -> Result<(String, String, String, u32), EmbedError> {
        if !s.embedding_enabled {
            return Err(EmbedError::NotConfigured("embedding_enabled 未开启".into()));
        }
        let base = normalize_base_url(&s.embedding_base_url);
        if base.is_empty() {
            return Err(EmbedError::NotConfigured("缺少 embedding_base_url".into()));
        }
        if s.embedding_api_key.trim().is_empty() {
            return Err(EmbedError::NotConfigured("缺少 embedding_api_key".into()));
        }
        if s.embedding_model.trim().is_empty() {
            return Err(EmbedError::NotConfigured("缺少 embedding_model".into()));
        }
        Ok((
            base,
            s.embedding_api_key.clone(),
            s.embedding_model.clone(),
            s.embedding_dim,
        ))
    }

    /// 批量嵌入文本;返回与输入等长、顺序一致的向量。
    /// 空输入直接返回空;超长文本按 EMBED_MAX_CHARS 截断。
    pub async fn embed(
        &self,
        s: &RuntimeSettings,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let (base, key, model, _dim) = Self::resolve(s)?;
        let url = format!("{base}/embeddings");
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());

        for chunk in texts.chunks(EMBED_BATCH_SIZE) {
            let input: Vec<String> = chunk
                .iter()
                .map(|t| {
                    let t = t.trim();
                    if t.chars().count() > EMBED_MAX_CHARS {
                        t.chars().take(EMBED_MAX_CHARS).collect()
                    } else {
                        t.to_string()
                    }
                })
                .collect();
            let body = json!({ "model": model, "input": input });
            let resp = self
                .client
                .post(&url)
                .bearer_auth(&key)
                .json(&body)
                .send()
                .await
                .map_err(|e| EmbedError::Failed(format!("请求 {url} 失败: {e}")))?;

            let status = resp.status();
            let text = resp
                .text()
                .await
                .map_err(|e| EmbedError::Failed(format!("读取响应失败: {e}")))?;
            if !status.is_success() {
                return Err(EmbedError::Failed(format!(
                    "HTTP {status}: {}",
                    text.chars().take(300).collect::<String>()
                )));
            }
            let parsed: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| EmbedError::Failed(format!("响应非 JSON: {e}")))?;
            let data = parsed
                .get("data")
                .and_then(|v| v.as_array())
                .ok_or_else(|| EmbedError::Failed("响应缺少 data 数组".into()))?;

            // 按 index 排序还原,防止服务端乱序
            let mut indexed: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
            for item in data {
                let idx = item.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let arr = item
                    .get("embedding")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| EmbedError::Failed("条目缺少 embedding".into()))?;
                let vec: Vec<f32> = arr
                    .iter()
                    .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                    .collect();
                if vec.is_empty() {
                    return Err(EmbedError::Failed("embedding 为空数组".into()));
                }
                indexed.push((idx, vec));
            }
            indexed.sort_by_key(|(i, _)| *i);
            if indexed.len() != chunk.len() {
                return Err(EmbedError::Failed(format!(
                    "返回条数 {} 与请求 {} 不一致",
                    indexed.len(),
                    chunk.len()
                )));
            }
            out.extend(indexed.into_iter().map(|(_, v)| v));
        }
        Ok(out)
    }

    /// 嵌入单条文本(查询向量用)
    pub async fn embed_one(&self, s: &RuntimeSettings, text: &str) -> Result<Vec<f32>, EmbedError> {
        let mut v = self.embed(s, &[text.to_string()]).await?;
        v.pop()
            .ok_or_else(|| EmbedError::Failed("返回空向量".into()))
    }

    /// 连接测试:嵌入一条固定文本,回传实际维度与耗时。
    /// 供设置界面「测试连接」使用;不写库。
    pub async fn test(&self, s: &RuntimeSettings) -> Result<(usize, u128), EmbedError> {
        let started = std::time::Instant::now();
        let vec = self.embed_one(s, "kedai embedding connection test").await?;
        Ok((vec.len(), started.elapsed().as_millis()))
    }
}

impl Default for EmbeddingService {
    fn default() -> Self {
        Self::new()
    }
}

/// 向量 L2 归一化(余弦相似度前置;零向量原样返回避免除零)
pub fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

/// 余弦相似度(输入无需预先归一化)
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom <= f32::EPSILON {
        0.0
    } else {
        dot / denom
    }
}

/// 向量编码为 sqlite-vec 接受的 JSON 文本格式 `[1.0,2.0,...]`
pub fn vec_to_json(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{x}"));
    }
    s.push(']');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_basic() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&a, &c).abs() < 1e-6);
        // 维度不一致返回 0(不 panic)
        assert_eq!(cosine_similarity(&a, &[1.0, 0.0]), 0.0);
        // 零向量不除零
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn l2_normalize_handles_zero_vector() {
        let v = l2_normalize(&[3.0, 4.0]);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
        assert_eq!(l2_normalize(&[0.0, 0.0]), vec![0.0, 0.0]);
    }

    #[test]
    fn vec_to_json_format() {
        assert_eq!(vec_to_json(&[1.0, -0.5, 0.25]), "[1,-0.5,0.25]");
        assert_eq!(vec_to_json(&[]), "[]");
    }

    #[test]
    fn resolve_reports_not_configured() {
        // 用 test_cfg 同构的最小 AppConfig(不依赖 settings_service 的私有测试助手)
        let cfg = crate::config::AppConfig {
            host: "127.0.0.1".into(),
            port: 0,
            data_dir: std::env::temp_dir(),
            log_dir: std::env::temp_dir(),
            web_dist: None,
            connector: "mock".into(),
            openai_base_url: "https://example.com/v1".into(),
            openai_api_key: String::new(),
            openai_model: "test-model".into(),
            default_temperature: 0.8,
            default_top_p: 0.9,
            default_max_tokens: 1024,
            default_max_context_tokens: 65_536,
            log_level: "info".into(),
            api_token: String::new(),
            auth_required: false,
            api_token_injected: false,
            allow_remote: false,
            bootstrap_enabled: true,
            strict_client_header: false,
        };
        let s = RuntimeSettings::from_config(&cfg);
        // 默认关闭 → NotConfigured(调用方据此静默降级)
        assert!(matches!(
            EmbeddingService::resolve(&s),
            Err(EmbedError::NotConfigured(_))
        ));
        // 开启但缺 Key → 仍是 NotConfigured
        let mut s2 = s.clone();
        s2.embedding_enabled = true;
        s2.embedding_base_url = "https://api.example.com/v1".into();
        s2.embedding_model = "m".into();
        assert!(matches!(
            EmbeddingService::resolve(&s2),
            Err(EmbedError::NotConfigured(_))
        ));
    }

    /// HTTP 客户端进程级复用(2026-09-16 性能批次 P-5)。
    ///
    /// 此前每次 `EmbeddingService::new()` 都新建 reqwest Client —— 而 `new()` 的调用点
    /// 遍布热路径:引擎每轮召回一次(`agents/engine/mod.rs:249`)、记忆工具每次写一次、
    /// `/api/memory/distill` 与 `/api/memory/:id` 甚至**在循环内逐条**新建
    /// (`api/memory.rs`)。每次新建都丢掉连接池与 keepalive,等于每条请求重做
    /// TCP(+TLS)握手;`EmbeddingService` 本身无状态(每次调用读最新配置),
    /// 客户端完全可以共享。
    ///
    /// 断言方式:reqwest::Client 不暴露内部地址,故不做指针比较,而是数**真实构造次数**。
    ///
    /// 关键不变量是「反复构造 N 次,底层客户端仍只被构造 1 次」——若每次 new() 都新建,
    /// 这个计数会涨到 N。写成「50 次构造后计数恰为 1」而非「计数不增长」,是为了
    /// 不依赖测试执行顺序(本 crate 测试并行,别处可能已先行初始化过)。
    #[test]
    fn http_client_is_built_only_once_process_wide() {
        for _ in 0..50 {
            let _ = EmbeddingService::new();
        }
        let _ = EmbeddingService::default();
        let n = CLIENT_INIT_COUNT.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            n, 1,
            "51 次构造后共享客户端仍应只构造 1 次,实际 {n}(每次新建=每条请求重做 TCP 握手)"
        );
    }

    /// 共享客户端仍可正常使用(复用不能把配置或可用性丢掉):
    /// 未配置时 embed 必须直接返回 NotConfigured(降级语义不变),而不是网络错误。
    #[test]
    fn shared_client_is_usable() {
        let svc = EmbeddingService::new();
        let cfg = crate::config::AppConfig {
            host: "127.0.0.1".into(),
            port: 0,
            data_dir: std::env::temp_dir(),
            log_dir: std::env::temp_dir(),
            web_dist: None,
            connector: "mock".into(),
            openai_base_url: "https://example.com/v1".into(),
            openai_api_key: String::new(),
            openai_model: "test-model".into(),
            default_temperature: 0.8,
            default_top_p: 0.9,
            default_max_tokens: 1024,
            default_max_context_tokens: 65_536,
            log_level: "info".into(),
            api_token: String::new(),
            auth_required: false,
            api_token_injected: false,
            allow_remote: false,
            bootstrap_enabled: true,
            strict_client_header: false,
        };
        let s = RuntimeSettings::from_config(&cfg);
        assert!(!s.embedding_enabled, "测试前提:默认未启用向量化");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(svc.embed_one(&s, "x")).unwrap_err();
        assert!(
            matches!(err, EmbedError::NotConfigured(_)),
            "未启用时应 NotConfigured 而非网络错误: {err:?}"
        );
    }
}
