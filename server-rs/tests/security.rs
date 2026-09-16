use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use kedai_server::{build_secure_test_app, build_strict_test_app};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use tower::ServiceExt;

const TOKEN: &str = "secure-test-token-0123456789abcdef";

#[tokio::test]
async fn token_origin_content_type_and_security_headers_are_enforced() {
    let app = build_secure_test_app(TOKEN).unwrap();

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/characters")
                .header("host", "127.0.0.1:3001")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(unauthorized.headers()["x-content-type-options"], "nosniff");

    let hostile = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/characters")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("host", "127.0.0.1:3001")
                .header("origin", "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hostile.status(), StatusCode::FORBIDDEN);

    let simple = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/chat/stop")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("host", "127.0.0.1:3001")
                .header("content-type", "text/plain")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(simple.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let allowed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/characters")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("origin", "http://localhost:5173")
                .header("host", "127.0.0.1:3001")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
}

fn bootstrap_request(peer: SocketAddr, host: &str) -> Request<Body> {
    let mut request = Request::builder()
        .uri("/api/bootstrap")
        .header("host", host)
        .header("x-forwarded-for", "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    request
}

#[tokio::test]
async fn bootstrap_requires_real_loopback_peer_even_with_forged_headers() {
    let app = build_secure_test_app(TOKEN).unwrap();
    let remote = app
        .clone()
        .oneshot(bootstrap_request(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)), 4567),
            "127.0.0.1:3001",
        ))
        .await
        .unwrap();
    assert_eq!(remote.status(), StatusCode::FORBIDDEN);

    let loopback = app
        .oneshot(bootstrap_request(
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 4567),
            "[::1]:3001",
        ))
        .await
        .unwrap();
    assert_eq!(loopback.status(), StatusCode::OK);
    assert_eq!(loopback.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn bootstrap_rejects_missing_peer_metadata() {
    let response = build_secure_test_app(TOKEN)
        .unwrap()
        .oneshot(
            Request::builder()
                .uri("/api/bootstrap")
                .header("host", "127.0.0.1:3001")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// 浏览器跨站请求即便带合法 Origin 也应被 Sec-Fetch-Site=cross-site 拒绝
/// (堵「缺 Origin 直接放行」之外,带 Origin 的跨站表单/图片 GET 旁路)。
#[tokio::test]
async fn cross_site_fetch_site_is_rejected() {
    let app = build_secure_test_app(TOKEN).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/characters")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("host", "127.0.0.1:3001")
                .header("origin", "http://localhost:5173")
                .header("sec-fetch-site", "cross-site")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// /api/ 响应默认注入 no-store 与 CORP same-origin(数据保护头)。
#[tokio::test]
async fn api_responses_default_no_store_and_corp() {
    let app = build_secure_test_app(TOKEN).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/characters")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("host", "127.0.0.1:3001")
                .header("origin", "http://localhost:5173")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        response.headers()["cross-origin-resource-policy"],
        "same-origin"
    );
}

/// strict_client_header 开启后:写请求必须同时携带 X-Kedai-Client 头与 Origin。
#[tokio::test]
async fn strict_client_header_gates_writes() {
    let app = build_strict_test_app(TOKEN).unwrap();

    // 缺 X-Kedai-Client 头 → 拒绝
    let missing_header = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/chat/stop")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("host", "127.0.0.1:3001")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_header.status(), StatusCode::FORBIDDEN);

    // 有 X-Kedai-Client 但缺 Origin → 拒绝
    let missing_origin = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/chat/stop")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("host", "127.0.0.1:3001")
                .header("content-type", "application/json")
                .header("x-kedai-client", "kedai-web")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_origin.status(), StatusCode::FORBIDDEN);

    // 两者齐备 → 通过安全校验(进入后续处理,不再被安全中间件拦截)
    let allowed = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/chat/stop")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("host", "127.0.0.1:3001")
                .header("content-type", "application/json")
                .header("x-kedai-client", "kedai-web")
                .header("origin", "http://localhost:5173")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(allowed.status(), StatusCode::FORBIDDEN);
}

/// 批次 2(可观测性):请求关联 ID 必须落在**早退路径**上。
///
/// 为什么这条断言是层序的回归护栏:`api/request_id.rs::attach` 注册在 `build_router` 链尾
/// (CORS 之后),axum 的 `.layer()` 是「后加者在外层」,故 401/403 由 `security::guard`
/// 提前返回时,响应仍穿过 request_id 层。此后若有人把 request_id 层挪到 guard 之前
/// (或误以为顺序无关而调整),这两个早退响应会最先失去 X-Request-Id——本测试即失败。
#[tokio::test]
async fn early_return_responses_carry_request_id() {
    let app = build_secure_test_app(TOKEN).unwrap();

    // 401:缺 bearer token(security::guard 早退)
    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/characters")
                .header("host", "127.0.0.1:3001")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let generated = unauthorized.headers()["x-request-id"]
        .to_str()
        .expect("401 响应应带 X-Request-Id(层序:request_id 在 guard 之外)")
        .to_string();
    assert!(
        generated.starts_with("req-"),
        "生成 ID 应带 req- 前缀: {generated}"
    );

    // 403:Origin 不受信任(同为 guard 早退)
    let forbidden = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/characters")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("host", "127.0.0.1:3001")
                .header("origin", "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    assert!(
        forbidden.headers().contains_key("x-request-id"),
        "403 响应应带 X-Request-Id"
    );

    // 客户端自带 ID 时沿用(而非另生成)——跨端排查靠它串起前后端日志
    let echoed = app
        .oneshot(
            Request::builder()
                .uri("/api/characters")
                .header("host", "127.0.0.1:3001")
                .header("x-request-id", "trace-me-123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(echoed.headers()["x-request-id"], "trace-me-123");
}
