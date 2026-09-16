// 请求关联 ID 中间件(批次 2 可观测性最小骨架):让每一次请求都可被定位。
//
// 三件事:
// 1. 读 `X-Request-Id`:客户端带则沿用(白名单净化),否则生成短随机 ID;同名写回响应头;
// 2. 建 `http_request` span 并在其上写 `requestId` 字段 —— utils::logging::RequestIdLayer
//    把它存成 span extension,PinoFormat 自动把 requestId 合并进该请求生命周期内所有
//    日志行(含 service 层与 spawn 出去的引擎/任务日志),无需逐处传参;
// 3. 请求结束打 1 行 INFO 访问日志:method / path / status / duration_ms(+ requestId)。
//
// 注册位置(见 build_router 链尾):必须是**最外层**的 `.layer(...)`。
// axum 的 `Router::layer` 语义是「后加者在外层」,而 security::guard 的 401/403 早退
// 响应同样需要带 requestId,故本层必须包在 guard/CORS 之外。
use axum::extract::Request;
use axum::http::{header::HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;

/// 请求 ID 头名:读入与回写同名
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// 客户端自带 ID 的接受上限:超长头视作异常输入,改为生成(避免日志被塞爆)
const MAX_CLIENT_ID_LEN: usize = 64;

/// 生成 ID 取 UUID v4 的 hex 前 16 位(64 bit 随机度,短且足够区分同机并发请求)
const GENERATED_ID_CHARS: usize = 16;

/// 请求 ID 中间件本体
pub async fn attach(request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(sanitize_client_id)
        .unwrap_or_else(generate_id);

    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = std::time::Instant::now();

    // 字段名 requestId 是 utils::logging::RequestIdLayer 的约定键(两侧同步,勿单独改)
    let span = tracing::info_span!(
        "http_request",
        requestId = request_id.as_str(),
        method = method.as_str(),
        path = path.as_str()
    );

    let mut response = next.run(request).instrument(span.clone()).await;

    // span 在此处已退出作用域,用 in_scope 重新进入:requestId 由 span extension
    // 穿透而来(而非本行显式传参),这正是该机制要验证的传播效果。
    let entry = access_log(
        method.as_str(),
        path.as_str(),
        response.status(),
        started.elapsed(),
    );
    span.in_scope(|| entry.emit());

    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(REQUEST_ID_HEADER), value);
    }
    response
}

/// 一条请求级访问日志的字段(纯数据)。
///
/// 与 `tracing` 发射解耦的原因:字段组装是可单测的确定性逻辑,而「捕获 tracing 输出」
/// 在本仓 lib 测试二进制里不可靠——`tracing` 的 callsite interest 是进程级缓存,且
/// 每个 `Dispatch::new` 触发的全局重建会把所有活跃订阅者的 interest 取 AND,任何并发
/// 运行的限制级订阅者(如 agents::state_machine 单测的 `max_level(WARN)`、调色板
/// `EmitterFilter`)都会把本 callsite 永久压成 never。故这里断言字段,真实日志行由
/// `utils::logging` 的 span extension 单测 + 活服务 curl 实测共同覆盖(见批次 2 汇报)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AccessLog<'a> {
    method: &'a str,
    path: &'a str,
    status: u16,
    duration_ms: u64,
}

impl AccessLog<'_> {
    /// 发射 1 行 INFO:method / path / status / duration_ms。
    /// requestId 不在此显式声明——由当前 span(`http_request`)经 span extension 合并。
    fn emit(&self) {
        tracing::info!(
            method = self.method,
            path = self.path,
            status = self.status,
            duration_ms = self.duration_ms,
            "http"
        );
    }
}

/// 组装访问日志字段:状态取数字码,耗时取整毫秒(u128 → u64,运行期不可能溢出)
fn access_log<'a>(
    method: &'a str,
    path: &'a str,
    status: StatusCode,
    elapsed: std::time::Duration,
) -> AccessLog<'a> {
    AccessLog {
        method,
        path,
        status: status.as_u16(),
        duration_ms: elapsed.as_millis() as u64,
    }
}

/// 客户端 ID 净化:仅接受可见 ASCII(0x21..=0x7e)且长度合规的串,
/// 过滤空串/控制字符/超长值——防日志注入与响应头注入。不合规则返回 None(改为生成)。
fn sanitize_client_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_CLIENT_ID_LEN {
        return None;
    }
    if trimmed.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// 生成短随机 ID(形如 `req-3f9c1a2b4d5e6f70`)
fn generate_id() -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("req-{}", &hex[..GENERATED_ID_CHARS])
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use std::time::Duration;
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/ok", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(attach))
    }

    fn call(uri: &str, request_id: Option<&str>) -> HttpRequest<Body> {
        let mut builder = HttpRequest::builder().uri(uri);
        if let Some(id) = request_id {
            builder = builder.header(REQUEST_ID_HEADER, id);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn 客户端传入的请求id被沿用并回写响应头() {
        let response = app()
            .oneshot(call("/ok", Some("trace-me-123")))
            .await
            .unwrap();
        assert_eq!(response.headers()[REQUEST_ID_HEADER], "trace-me-123");
    }

    #[tokio::test]
    async fn 未带请求id时生成短随机id并回写() {
        let response = app().oneshot(call("/ok", None)).await.unwrap();
        let id = response.headers()[REQUEST_ID_HEADER]
            .to_str()
            .unwrap()
            .to_string();
        assert!(id.starts_with("req-"), "生成 ID 应带 req- 前缀: {id}");
        assert_eq!(id.len(), 4 + GENERATED_ID_CHARS);
    }

    #[tokio::test]
    async fn 非法或超长请求id不被沿用() {
        let too_long = "a".repeat(MAX_CLIENT_ID_LEN + 1);
        for bad in ["", "  ", &too_long, "bad id"] {
            let response = app().oneshot(call("/ok", Some(bad))).await.unwrap();
            let id = response.headers()[REQUEST_ID_HEADER].to_str().unwrap();
            assert_ne!(id, bad, "非法 ID {bad:?} 不应被沿用");
        }
    }

    /// 访问日志字段组装(纯函数,确定性可断言)。
    ///
    /// 为何断言字段而不是捕获 tracing 输出:本仓 lib 测试二进制里 tracing 的 callsite
    /// interest 是进程级缓存,且每个 `Dispatch::new` 触发的全局重建会把**所有活跃订阅者**
    /// 的 interest 取 AND;任何并发运行的限制级订阅者(如 agents::state_machine 单测的
    /// `max_level(WARN)`、palette 的 emitter filter)都会把本 callsite 永久压成 never,
    /// 断言因此会随机失败(已实测复现,`rebuild_interest_cache` 亦无法补救)。
    /// 真实日志行的支撑证据:`utils::logging` 的 span extension 单测 + 活服务 curl 实测。
    #[test]
    fn 访问日志字段取状态码与整毫秒耗时() {
        let entry = access_log(
            "GET",
            "/api/health",
            StatusCode::OK,
            Duration::from_millis(7),
        );
        assert_eq!(
            entry,
            AccessLog {
                method: "GET",
                path: "/api/health",
                status: 200,
                duration_ms: 7,
            }
        );

        // 401 早退路径同样可定位:状态码原样落日志
        let unauthorized = access_log(
            "GET",
            "/api/characters",
            StatusCode::UNAUTHORIZED,
            Duration::ZERO,
        );
        assert_eq!(unauthorized.status, 401);
        assert_eq!(unauthorized.duration_ms, 0);

        // 亚毫秒耗时向下取整(JSON 端口径统一为整数毫秒)
        let sub_ms = access_log("GET", "/ok", StatusCode::OK, Duration::from_micros(999));
        assert_eq!(sub_ms.duration_ms, 0);
    }
}
