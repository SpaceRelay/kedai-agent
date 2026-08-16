// 连接器切换测试(独立进程/独立 app 实例,不影响 api_integration.rs 的共享 mock app):
// mock 连接器下保存 API 设置(Base URL / Key)→ 自动切换到 openai-compatible,用户配置立即生效。
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kedai_server::build_test_app;
use serde_json::{json, Value};
use std::sync::OnceLock;
use tower::ServiceExt;

fn test_app() -> &'static axum::Router {
    static APP: OnceLock<axum::Router> = OnceLock::new();
    APP.get_or_init(|| build_test_app().expect("构建测试应用失败"))
}

async fn send_json(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn mock_auto_switches_to_openai_on_save() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mock_provider = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(|| async {
            axum::Json(json!({ "data": [{ "id": "local-test-model" }] }))
        }),
    );
    tokio::spawn(async move { axum::serve(listener, mock_provider).await.unwrap() });
    let app = test_app();

    // 初始:mock 连接器
    let (_, info0) = send_json(app, "GET", "/api/settings/info", json!({})).await;
    assert_eq!(info0["connector"], json!("mock"));

    // GET /api/settings:返回脱敏 Key,不含明文
    let (status, s) = send_json(app, "GET", "/api/settings", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(s["openai_base_url"].is_string());
    assert!(s["api_key_masked"].is_string());
    assert!(s["has_api_key"].is_boolean());
    assert!(s["model"].is_string());
    assert!(s["default_temperature"].is_number());
    assert!(s["default_top_p"].is_number());
    assert!(s["default_max_tokens"].is_number());
    assert!(s["max_context_tokens"].is_number());

    // 部分字段更新:默认参数(不触发连接器切换)
    let (status, r) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({
            "default_top_p": 0.7,
            "max_context_tokens": 65536,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "更新设置失败: {r}");
    assert_eq!(r["settings"]["default_top_p"], json!(0.7));
    assert_eq!(r["settings"]["max_context_tokens"], json!(65536));
    // 非法 top_p 被忽略
    let (_, r3) = send_json(app, "PUT", "/api/settings", json!({ "default_top_p": 5.0 })).await;
    assert_eq!(r3["settings"]["default_top_p"], json!(0.7));

    // 任一严格字段非法时整批拒绝,前面的合法字段也不得半提交。
    let (invalid_status, invalid) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "default_top_p": 0.4, "max_tool_rounds": 0 }),
    )
    .await;
    assert_eq!(invalid_status, StatusCode::BAD_REQUEST);
    assert!(invalid["error"]
        .as_str()
        .unwrap()
        .contains("max_tool_rounds"));
    let (_, after_invalid) = send_json(app, "GET", "/api/settings", json!({})).await;
    assert_eq!(after_invalid["default_top_p"], json!(0.7));

    // 保存 API 地址 + Key + 模型 → 自动切换到 openai-compatible,配置生效
    let (status, r) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({
            "openai_base_url": format!("http://{address}/v1"),
            "openai_api_key": "sk-test-123456",
            "model": "deepseek-chat",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "保存失败: {r}");
    assert_eq!(r["ok"], json!(true));
    assert_eq!(
        r["settings"]["openai_base_url"],
        json!(format!("http://{address}/v1"))
    );
    assert_eq!(r["settings"]["model"], json!("deepseek-chat"));
    assert_eq!(r["settings"]["has_api_key"], json!(true));
    // Key 已脱敏,不含明文
    assert_eq!(r["settings"]["api_key_masked"], json!("****3456"));
    assert!(!r["settings"]["api_key_masked"]
        .as_str()
        .unwrap()
        .contains("sk-test"));

    // 连接器已切换为 openai-compatible
    let (_, info) = send_json(app, "GET", "/api/settings/info", json!({})).await;
    assert_eq!(info["connector"], json!("openai-compatible"));

    // 从 API 加载模型列表接口(新连接器下调用可用;离线时回退当前模型)
    let (status, rm) = send_json(app, "POST", "/api/settings/refresh-models", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rm["ok"], json!(true));
    assert!(rm["models"].is_array());

    // 连接测试(无真实网络,Key 已配置即返回 ok)
    let (_, conn) = send_json(app, "POST", "/api/settings/connect", json!({})).await;
    assert_eq!(conn["ok"], json!(true));
    assert_eq!(conn["endpoint_reachable"], json!(true));
    assert_eq!(conn["authenticated"], json!(true));
    assert_eq!(conn["fallback_used"], json!(false));
    assert_eq!(conn["http_status"], json!(200));

    // 持久化文件包含保存的配置
    let settings_path = std::env::temp_dir()
        .join(format!("kedai-test-{}", std::process::id()))
        .join("settings.json");
    let text = std::fs::read_to_string(&settings_path).expect("settings.json 应已持久化");
    assert!(text.contains(&format!("http://{address}/v1")));
    assert!(text.contains("deepseek-chat"));
    assert!(text.contains("65536"));
}
