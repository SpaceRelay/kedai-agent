//! 上游/传输错误的分类词汇（L1 契约层）。
//!
//! ## 为什么在 L1 而不是 connectors/ 或 agents/
//!
//! 按三结合「代际判定程序」（见 `docs/契约-架构与数据.md` §2.2）：
//! - **P2 依赖方向**：本词汇被 `connectors/*`（L2 边界处构造：把 reqwest 错误与
//!   HTTP 状态映射为分类）与 `agents/engine`（L2，消费分类决定 SSE 错误终态的
//!   `code`/`retryable`）**共同**使用。放 L1 是两者唯一可共同依赖处。
//! - **P5 复用度**：SSE `Error` 事件的错误码是前后端线格式契约（`code` 的五个取值
//!   冻结为 `request_timeout` / `rate_limited` / `auth_failed` / `upstream_error` /
//!   `generation_failed`），属「数据契约」而非某一层私有实现细节。
//!
//! ## 边界：这里只放「词汇 + 纯映射」，不放 HTTP 客户端
//!
//! - 本文件放：分类枚举、[`LlmErrorKind::code`]/[`retryable`](LlmErrorKind::retryable)、
//!   HTTP 状态与传输失败形态的**纯映射函数**、携带文案的 [`LlmError`]。
//! - **不放**：`reqwest`/`hyper` 类型（L1 不得依赖 HTTP crate），也不发请求。
//!   真实连接器在自己的边界处把 `reqwest::Error::is_timeout()/is_connect()/…`
//!   折算成 [`TransportFailure`] 再调用本模块。
//! - 本模块取代了原先 `agents/engine/util.rs` 的 `classify_engine_error`：错误分类
//!   不再靠对错误文案做子串匹配（那条路径连同文案匹配一起删除）。

use std::ops::Deref;

/// 上游失败分类（唯一决定错误码与可重试语义的词汇）。
///
/// 线格式：**不直接序列化本枚举**，对外始终走 [`LlmErrorKind::code`] 得到的字符串，
/// 保持 `SseEvent::Error.code` 的既有取值不变（前端与 `tools/check-contract.mjs` 对锁）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmErrorKind {
    /// 请求超时 / 流空闲超时
    Timeout,
    /// 限流（429）
    RateLimited,
    /// 鉴权失败（401/403）
    AuthFailed,
    /// 其他上游/传输失败（5xx、连接中断等，可重试）
    Upstream,
    /// 协议层失败（SSE 不是合法 JSON/UTF-8、工具参数非法等，不可重试）
    Generation,
}

impl LlmErrorKind {
    /// SSE `Error` 事件的线格式错误码（**五个取值冻结**，改动即破坏前端契约）。
    pub fn code(self) -> &'static str {
        match self {
            LlmErrorKind::Timeout => "request_timeout",
            LlmErrorKind::RateLimited => "rate_limited",
            LlmErrorKind::AuthFailed => "auth_failed",
            LlmErrorKind::Upstream => "upstream_error",
            LlmErrorKind::Generation => "generation_failed",
        }
    }

    /// 是否可重试（前端据此提示「重试」；鉴权/参数类错误重试无意义）。
    pub fn retryable(self) -> bool {
        match self {
            LlmErrorKind::Timeout | LlmErrorKind::RateLimited | LlmErrorKind::Upstream => true,
            LlmErrorKind::AuthFailed | LlmErrorKind::Generation => false,
        }
    }

    /// HTTP 状态码 → 分类（纯函数，不依赖 HTTP crate）。
    ///
    /// 映射表：
    /// - `401`/`403` → [`LlmErrorKind::AuthFailed`]
    /// - `408` → [`LlmErrorKind::Timeout`]
    /// - `429` → [`LlmErrorKind::RateLimited`]
    /// - 其余 `5xx` → [`LlmErrorKind::Upstream`]（可重试）
    /// - 其余 `4xx` → [`LlmErrorKind::Generation`]（请求本身有问题，重试无意义）
    /// - 其余（异常 1xx/2xx/3xx）→ [`LlmErrorKind::Upstream`]
    pub fn from_http_status(status: u16) -> Self {
        match status {
            401 | 403 => LlmErrorKind::AuthFailed,
            408 => LlmErrorKind::Timeout,
            429 => LlmErrorKind::RateLimited,
            500..=599 => LlmErrorKind::Upstream,
            400..=499 => LlmErrorKind::Generation,
            // 非成功但也不是 4xx/5xx(异常 1xx/2xx/3xx):按上游故障处理,保留可重试语义
            _ => LlmErrorKind::Upstream,
        }
    }

    /// 传输层失败形态 → 分类（纯函数；调用方在边界处把 reqwest 错误折算成形态）。
    pub fn from_transport(failure: TransportFailure) -> Self {
        match failure {
            TransportFailure::Timeout => LlmErrorKind::Timeout,
            TransportFailure::Connect | TransportFailure::Body | TransportFailure::Other => {
                LlmErrorKind::Upstream
            }
            // 请求构造/发送被拒(URL、TLS、重定向等)重试无意义
            TransportFailure::Request => LlmErrorKind::Generation,
        }
    }
}

/// 传输层失败形态（reqwest 错误在边界处的折算结果；L1 不感知 reqwest 类型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFailure {
    /// 超时（`reqwest::Error::is_timeout`）
    Timeout,
    /// 建连/连接中断（`is_connect`）
    Connect,
    /// 响应体读取中断（`is_body`）
    Body,
    /// 请求构造/发送被拒（`is_request`：URL、TLS、重定向等）
    Request,
    /// 其他（无法细分的传输失败，按上游故障处理）
    Other,
}

/// 上游/传输错误：分类 + 面向用户的文案。
///
/// 实现 [`Deref<Target = str>`] 是有意为之：仓库内既有大量 `Result<_, String>` 消费点
/// 直接对错误做 `contains`/`format!`/`&e` 使用，Deref 让这些位置无需改写即可继续工作，
/// 从而把「类型化」的改动面限制在**生产者**侧（连接器边界与引擎顶层），避免全仓重写。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmError {
    kind: LlmErrorKind,
    message: String,
}

impl LlmError {
    /// 由分类与文案直接构造。
    pub fn new(kind: LlmErrorKind, message: impl Into<String>) -> Self {
        LlmError {
            kind,
            message: message.into(),
        }
    }

    /// 超时（请求超时 / 流空闲超时）。
    pub fn timeout(message: impl Into<String>) -> Self {
        LlmError::new(LlmErrorKind::Timeout, message)
    }

    /// 限流。
    pub fn rate_limited(message: impl Into<String>) -> Self {
        LlmError::new(LlmErrorKind::RateLimited, message)
    }

    /// 鉴权失败。
    pub fn auth_failed(message: impl Into<String>) -> Self {
        LlmError::new(LlmErrorKind::AuthFailed, message)
    }

    /// 其他上游/传输失败（可重试）。
    pub fn upstream(message: impl Into<String>) -> Self {
        LlmError::new(LlmErrorKind::Upstream, message)
    }

    /// 生成/协议层失败（不可重试）。
    pub fn generation(message: impl Into<String>) -> Self {
        LlmError::new(LlmErrorKind::Generation, message)
    }

    /// HTTP 状态码 + 文案 → 错误（分类走 [`LlmErrorKind::from_http_status`]）。
    pub fn from_http_status(status: u16, message: impl Into<String>) -> Self {
        LlmError::new(LlmErrorKind::from_http_status(status), message)
    }

    /// 传输失败形态 + 文案 → 错误（分类走 [`LlmErrorKind::from_transport`]）。
    pub fn from_transport(failure: TransportFailure, message: impl Into<String>) -> Self {
        LlmError::new(LlmErrorKind::from_transport(failure), message)
    }

    /// 分类。
    pub fn kind(&self) -> LlmErrorKind {
        self.kind
    }

    /// 面向用户的文案（不含分类前缀）。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// SSE 线格式错误码（见 [`LlmErrorKind::code`]）。
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }

    /// 是否可重试（见 [`LlmErrorKind::retryable`]）。
    pub fn retryable(&self) -> bool {
        self.kind.retryable()
    }
}

impl Deref for LlmError {
    type Target = str;

    fn deref(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 线格式冻结：错误码五个取值不得改动（前端 `web/src/api/types.ts` 与
    /// `tools/check-contract.mjs` 与本表对锁）。
    #[test]
    fn kind_code_is_frozen_wire_format() {
        assert_eq!(LlmErrorKind::Timeout.code(), "request_timeout");
        assert_eq!(LlmErrorKind::RateLimited.code(), "rate_limited");
        assert_eq!(LlmErrorKind::AuthFailed.code(), "auth_failed");
        assert_eq!(LlmErrorKind::Upstream.code(), "upstream_error");
        assert_eq!(LlmErrorKind::Generation.code(), "generation_failed");
    }

    /// 表驱动：HTTP 状态 → (分类, 错误码, 可重试)。
    #[test]
    fn http_status_mapping_table() {
        let cases: [(u16, LlmErrorKind, &str, bool); 11] = [
            (401, LlmErrorKind::AuthFailed, "auth_failed", false),
            (403, LlmErrorKind::AuthFailed, "auth_failed", false),
            (408, LlmErrorKind::Timeout, "request_timeout", true),
            (429, LlmErrorKind::RateLimited, "rate_limited", true),
            (400, LlmErrorKind::Generation, "generation_failed", false),
            (404, LlmErrorKind::Generation, "generation_failed", false),
            (422, LlmErrorKind::Generation, "generation_failed", false),
            (500, LlmErrorKind::Upstream, "upstream_error", true),
            (502, LlmErrorKind::Upstream, "upstream_error", true),
            (503, LlmErrorKind::Upstream, "upstream_error", true),
            (504, LlmErrorKind::Upstream, "upstream_error", true),
        ];
        for (status, kind, code, retryable) in cases {
            let err = LlmError::from_http_status(status, format!("上游返回 {status}"));
            assert_eq!(err.kind(), kind, "状态 {status} 分类错误");
            assert_eq!(err.code(), code, "状态 {status} 错误码错误");
            assert_eq!(err.retryable(), retryable, "状态 {status} 可重试性错误");
        }
    }

    /// 表驱动：传输失败形态 → (分类, 错误码, 可重试)。
    #[test]
    fn transport_failure_mapping_table() {
        let cases: [(TransportFailure, LlmErrorKind, &str, bool); 5] = [
            (
                TransportFailure::Timeout,
                LlmErrorKind::Timeout,
                "request_timeout",
                true,
            ),
            (
                TransportFailure::Connect,
                LlmErrorKind::Upstream,
                "upstream_error",
                true,
            ),
            (
                TransportFailure::Body,
                LlmErrorKind::Upstream,
                "upstream_error",
                true,
            ),
            (
                TransportFailure::Request,
                LlmErrorKind::Generation,
                "generation_failed",
                false,
            ),
            (
                TransportFailure::Other,
                LlmErrorKind::Upstream,
                "upstream_error",
                true,
            ),
        ];
        for (failure, kind, code, retryable) in cases {
            let err = LlmError::from_transport(failure, "请求失败");
            assert_eq!(err.kind(), kind, "形态 {failure:?} 分类错误");
            assert_eq!(err.code(), code, "形态 {failure:?} 错误码错误");
            assert_eq!(err.retryable(), retryable, "形态 {failure:?} 可重试性错误");
        }
    }

    /// 文案原样保留、且可当 `&str` 用（Deref 兼容既有字符串消费点）。
    #[test]
    fn message_is_preserved_and_derefs_to_str() {
        let err = LlmError::timeout("请求 OpenAI 兼容接口超时(30s)");
        assert_eq!(err.message(), "请求 OpenAI 兼容接口超时(30s)");
        assert_eq!(err.to_string(), "请求 OpenAI 兼容接口超时(30s)");
        assert!(err.contains("超时"), "Deref 后应可直接用 str 方法");
    }

    /// 语义化构造器与分类一一对应（避免把可重试错误误标为不可重试）。
    #[test]
    fn semantic_constructors_match_kinds() {
        assert_eq!(LlmError::timeout("t").kind(), LlmErrorKind::Timeout);
        assert_eq!(
            LlmError::rate_limited("r").kind(),
            LlmErrorKind::RateLimited
        );
        assert_eq!(LlmError::auth_failed("a").kind(), LlmErrorKind::AuthFailed);
        assert_eq!(LlmError::upstream("u").kind(), LlmErrorKind::Upstream);
        assert_eq!(LlmError::generation("p").kind(), LlmErrorKind::Generation);
        assert!(LlmError::upstream("u").retryable());
        assert!(!LlmError::generation("p").retryable());
        assert!(!LlmError::auth_failed("a").retryable());
    }
}
