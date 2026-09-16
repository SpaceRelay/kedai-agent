// 引擎辅助小函数:SSE Step 事件构造、UpdateVariable 标签定位与正文替换保留、
// SSE 事件发送与中断检查。
// (顶层生成错误分类原在此处,批次 4.3 改为连接器边界显式携带分类,
//  见 models/llm_error.rs 与 types.rs 的 EngineError)
// (自 engine/mod.rs 拆分迁入,纯代码移动,逻辑不变)
// 可见性说明:原为 engine/mod.rs 私有函数,此处改为 pub(super)(= 对 engine 可见),
// 可见范围与拆分前完全一致,未放宽。
use super::*;

/// 构造 SSE Step 事件(自定义流程步骤附带 index/total 进度;其余模式不携带)
pub(super) fn step_evt(
    step: &str,
    detail: Option<String>,
    index: Option<usize>,
    total: Option<usize>,
) -> SseEvent {
    SseEvent::Step {
        step: step.into(),
        detail,
        index,
        total,
    }
}

/// 在正文中定位 UpdateVariable 标签起始位置(大小写不敏感,容错 <updatevariable> 等变体)。
/// 返回标签名首字符的字节下标;找不到返回 None。
/// 用字节窗口匹配:TAG 全 ASCII,与中文等多字节字符(字节 ≥0x80)不会重叠,
/// 匹配起点必落在字符边界,避免按字节切片多字节字符导致 panic。
pub(super) fn find_update_variable_tag(content: &str) -> Option<usize> {
    const TAG: &str = "updatevariable";
    let tag = TAG.as_bytes();
    content
        .as_bytes()
        .windows(tag.len())
        .position(|w| w.eq_ignore_ascii_case(tag))
        .filter(|&p| content.is_char_boundary(p))
}

/// 把正文替换为修正后文本,同时保留 <UpdateVariable> 补丁块。
/// 以标签名 "UpdateVariable" 为锚点定位块起始(大小写不敏感),向前取最近 '<'
/// 作为标签头;块及之后内容原样保留,块前视为正文被替换。避免 censor 修正误伤
/// JSONPatch 中的变量键/值。
pub(super) fn rebuild_content_keeping_blocks(content: &str, new_body: &str) -> String {
    match find_update_variable_tag(content) {
        Some(i) => {
            let tag_start = content[..i].rfind('<').unwrap_or(i);
            format!("{new_body}{}", &content[tag_start..])
        }
        None => new_body.to_string(),
    }
}

/// 发送 SSE 事件;中断时返回 Err("已中断");
/// 客户端断开(tx.send 失败)时置位 abort,使后续流程按中断处理
pub(super) async fn send_event(
    event: SseEvent,
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
) -> Result<(), String> {
    if *abort.borrow() {
        return Err("已中断".to_string());
    }
    match tx.send(event).await {
        Ok(_) => Ok(()),
        Err(_) => {
            flag.abort();
            Err("已中断".to_string())
        }
    }
}

/// 检查中断
pub(super) fn check_aborted(abort: &watch::Receiver<bool>) -> Result<(), String> {
    if *abort.borrow() {
        Err("已中断".to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::llm_error::{LlmError, LlmErrorKind, TransportFailure};

    /// 表驱动:上游错误形态 → SSE 错误终态的 (code, retryable)。
    ///
    /// 批次 4.3 前这张表由 `classify_engine_error` 对错误文案做子串匹配实现;
    /// 现改为连接器边界携带分类、引擎只做映射，**不再解析文案**——本测试锁定映射语义
    /// （含「未知/内部错误」兜底为 generation_failed 且不可重试）。
    #[test]
    fn engine_error_terminal_mapping_table() {
        let cases: [(EngineError, &str, bool); 8] = [
            (
                EngineError::Llm(LlmError::timeout("请求 OpenAI 兼容接口超时(30s)")),
                "request_timeout",
                true,
            ),
            (
                EngineError::Llm(LlmError::from_http_status(429, "上游返回 429")),
                "rate_limited",
                true,
            ),
            (
                EngineError::Llm(LlmError::from_http_status(401, "上游返回 401")),
                "auth_failed",
                false,
            ),
            (
                EngineError::Llm(LlmError::from_http_status(403, "上游返回 403")),
                "auth_failed",
                false,
            ),
            (
                EngineError::Llm(LlmError::from_http_status(503, "上游返回 503")),
                "upstream_error",
                true,
            ),
            (
                EngineError::Llm(LlmError::from_transport(
                    TransportFailure::Connect,
                    "请求 OpenAI 兼容接口失败: connection reset",
                )),
                "upstream_error",
                true,
            ),
            // 未知/内部错误兜底:无分类可用时不可重试
            (
                EngineError::Internal("状态机非法迁移".into()),
                "generation_failed",
                false,
            ),
            (EngineError::Interrupted, "generation_failed", false),
        ];
        for (err, code, retryable) in cases {
            assert_eq!(err.error_code(), code, "错误码错误:{err}");
            assert_eq!(err.retryable(), retryable, "可重试性错误:{err}");
        }
    }

    /// 分类只来自生产者:同样文案的错误,分类不同则结果不同——
    /// 若仍有「按文案猜测」的路径,本用例会失败。
    #[test]
    fn classification_does_not_depend_on_message_text() {
        let as_timeout = EngineError::Llm(LlmError::timeout("上游返回 500"));
        let as_upstream = EngineError::Llm(LlmError::upstream("上游返回 500"));
        assert_eq!(as_timeout.error_code(), "request_timeout");
        assert_eq!(as_upstream.error_code(), "upstream_error");

        // 鉴权文案里出现「超时」二字也不得被误判为超时(旧子串匹配的典型误判)
        let auth = EngineError::Llm(LlmError::auth_failed("等待响应超时后返回 401 Unauthorized"));
        assert_eq!(auth.error_code(), "auth_failed");
        assert!(!auth.retryable());
    }

    /// 分类可从错误透出（供日志/诊断），内部错误无分类。
    #[test]
    fn error_kind_is_exposed() {
        assert_eq!(
            EngineError::Llm(LlmError::rate_limited("429")).error_kind(),
            Some(LlmErrorKind::RateLimited)
        );
        assert_eq!(EngineError::Internal("x".into()).error_kind(), None);
        assert_eq!(EngineError::Interrupted.error_kind(), None);
    }

    /// 内部 `Result<_, String>` 经 `?` 自动转为无分类的内部错误（改动面收敛用）。
    #[test]
    fn string_errors_convert_to_internal() {
        let err: EngineError = "状态机校验失败".to_string().into();
        assert_eq!(err.message(), "状态机校验失败");
        assert_eq!(err.error_code(), "generation_failed");
    }
}
