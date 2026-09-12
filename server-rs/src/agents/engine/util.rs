// 引擎辅助小函数:SSE Step 事件构造、UpdateVariable 标签定位与正文替换保留、
// 顶层生成错误分类、SSE 事件发送与中断检查。
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

/// 顶层生成错误分类:返回 (code, retryable)。
/// 网络/超时/上游 429/5xx 视为可重试;鉴权/参数/工具错误不可重试。
pub(super) fn classify_engine_error(msg: &str) -> (String, bool) {
    let m = msg.to_lowercase();
    let retryable = m.contains("超时")
        || m.contains("timeout")
        || m.contains("429")
        || m.contains("500")
        || m.contains("502")
        || m.contains("503")
        || m.contains("504")
        || m.contains("connection")
        || m.contains("connect")
        || m.contains("eof")
        || m.contains("broken pipe");
    let code = if m.contains("超时") || m.contains("timeout") {
        "request_timeout"
    } else if m.contains("429") || m.contains("rate limit") {
        "rate_limited"
    } else if m.contains("401")
        || m.contains("403")
        || m.contains("apikey")
        || m.contains("api key")
    {
        "auth_failed"
    } else if retryable {
        "upstream_error"
    } else {
        "generation_failed"
    };
    (code.to_string(), retryable)
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
