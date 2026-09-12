// 生成请求自动重试:退避计算 / 可中断等待 / 重试日志(自 openai_compatible.rs 迁入)
use std::time::Duration;
use tokio::sync::watch;

/// 生成请求自动重试次数上限(总尝试次数 = 1 + 重试次数)
pub(super) const MAX_ATTEMPTS: usize = 3;

/// 重试退避:base 500ms 指数增长(2^(attempt-1)),上限 5s,加 ≤250ms jitter;
/// 提供 Retry-After(秒)时以其为准(上限 30s,防止误配过大)。
pub(super) fn retry_delay(attempt: usize, retry_after: Option<u64>) -> Duration {
    if let Some(secs) = retry_after {
        return Duration::from_secs(secs.min(30));
    }
    let base_ms = 500u64.saturating_mul(1u64 << (attempt.saturating_sub(1) as u32));
    let base_ms = base_ms.min(5000);
    // 无 rand 依赖:以系统时钟纳秒做轻量 jitter,避免多请求同时重试打满上游
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 % 250)
        .unwrap_or(0);
    Duration::from_millis(base_ms + jitter)
}

/// 重试等待:可响应中断(abort 时立即返回 Err,不空等退避)
pub(super) async fn wait_retry(
    delay: Duration,
    abort: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(()),
        changed = abort.changed() => {
            if changed.is_err() || *abort.borrow() {
                Err("生成已中断".into())
            } else {
                Ok(())
            }
        }
    }
}

/// 记录重试日志(不向 SSE 流注入事件——LlmStreamChunk 无提示通道,避免协议侵入)
pub(super) fn log_retry(msg: &str, attempt: usize) {
    tracing::info!(message = msg.to_string(), attempt = attempt, "模型请求重试");
}
