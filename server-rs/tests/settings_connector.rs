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

/// 模式隔离 API 级回归(WP7 双向污染矩阵 D-2):写 task 覆盖层 agent_system_prompt
/// 不得改变 roleplay 扁平值;GET ?mode=task 返回覆盖值,?mode=roleplay 返回扁平权威值。
/// 本 binary 无任务执行测试,task 覆盖层残留无副作用(结束时仍写回空串保持卫生)。
#[tokio::test]
async fn task_overlay_does_not_leak_into_roleplay_settings() {
    let app = test_app();

    // 记录 roleplay 扁平权威值(共享 app,可能已被同 binary 其他测试写动,取现场值)
    let (status, before_rp) = send_json(app, "GET", "/api/settings?mode=roleplay", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let flat_before = before_rp["agent_system_prompt"].clone();
    assert!(flat_before.is_string(), "扁平值应为裸字符串(线格式不变)");

    // 写 task 覆盖层
    let (status, r) = send_json(
        app,
        "PUT",
        "/api/settings?mode=task",
        json!({ "agent_system_prompt": "TASK-OVERLAY-MARKER 任务专用提示词" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "写 task 覆盖层失败: {r}");

    // task 视角返回覆盖值;roleplay 视角扁平值不变(双向隔离)
    let (_, task_view) = send_json(app, "GET", "/api/settings?mode=task", json!({})).await;
    assert_eq!(
        task_view["agent_system_prompt"],
        json!("TASK-OVERLAY-MARKER 任务专用提示词"),
        "task 应读到覆盖层值"
    );
    let (_, rp_view) = send_json(app, "GET", "/api/settings?mode=roleplay", json!({})).await;
    assert_eq!(
        rp_view["agent_system_prompt"], flat_before,
        "roleplay 扁平值不得被 task 覆盖层污染"
    );

    // 卫生复位:写回空串(API 三态语义中无「复位 None」操作;Some("") = 显式清空)
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/settings?mode=task",
        json!({ "agent_system_prompt": "" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, rp_after) = send_json(app, "GET", "/api/settings?mode=roleplay", json!({})).await;
    assert_eq!(rp_after["agent_system_prompt"], flat_before);
}

/// 提示词预览按模式合并(批次 2,docs/模式提示词边界.md 第五节):
/// task 模式追加规划器/执行者/汇总者三层固定提示词层(文本与 task_service/prompt.rs
/// 单一来源逐字一致,预览即真实下发);roleplay(缺省)不含。三层为内置指令,
/// 恒注入,与 task 覆盖层状态无关(共享 app 下不受同 binary 其他测试写动影响)。
#[tokio::test]
async fn prompt_preview_follows_mode() {
    let app = test_app();

    // 缺省 = roleplay:不得含任务固定提示词层
    let (status, rp) = send_json(app, "GET", "/api/settings/prompt-preview", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let rp_layers = rp["layers"].as_array().expect("layers 应为数组");
    assert!(
        !rp_layers
            .iter()
            .any(|l| l["source"].as_str() == Some("task_planner_prompt")),
        "roleplay 预览不得含任务固定提示词层"
    );

    // task:三层固定提示词齐备,文本含内置指令原文
    let (status, t) = send_json(
        app,
        "GET",
        "/api/settings/prompt-preview?mode=task",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let layers = t["layers"].as_array().expect("layers 应为数组");
    for (src, needle) in [
        ("task_planner_prompt", "你是任务规划器"),
        ("task_executor_prompt", "你是任务执行者"),
        ("task_summarizer_prompt", "你是任务汇总者"),
    ] {
        let hit = layers.iter().any(|l| {
            l["source"].as_str() == Some(src)
                && l["content"]
                    .as_str()
                    .map(|c| c.contains(needle))
                    .unwrap_or(false)
        });
        assert!(hit, "task 预览缺 {src} 层");
    }
}

/// MCP 设置(批次 6.2):GET 透出默认 关/空列表;PUT 全量替换并做卫生清理
/// (缺命令条目被丢弃);GET 往返还原;task 覆盖层与扁平层互不影响。
#[tokio::test]
async fn mcp_settings_put_get_roundtrip() {
    let app = test_app();

    // 默认:关 + 空列表
    let (status, s) = send_json(app, "GET", "/api/settings", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s["mcp_enabled"], json!(false), "MCP 默认关闭: {s}");
    assert_eq!(s["mcp_servers"], json!([]), "MCP 服务器列表默认空: {s}");

    // PUT:开关 + 服务器列表(含一个缺命令的坏条目,应被清理)
    let (status, r) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({
            "mcp_enabled": true,
            "mcp_servers": [
                { "name": " fs ", "command": "npx", "args": ["-y", "@mcp/fs"] },
                { "name": "broken", "command": "" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(r["settings"]["mcp_enabled"], json!(true));
    let servers = r["settings"]["mcp_servers"].as_array().unwrap();
    assert_eq!(servers.len(), 1, "缺命令条目应被清理: {servers:?}");
    assert_eq!(servers[0]["name"], json!("fs"), "名称应 trim");
    assert_eq!(servers[0]["enabled"], json!(true), "条目 enabled 缺省 true");

    // GET 往返还原
    let (_, after) = send_json(app, "GET", "/api/settings", json!({})).await;
    assert_eq!(after["mcp_enabled"], json!(true));
    assert_eq!(after["mcp_servers"].as_array().unwrap().len(), 1);

    // task 覆盖层独立:PUT ?mode=task 只写覆盖层,扁平层(roleplay 视图)不变
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/settings?mode=task",
        json!({ "mcp_enabled": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, task_view) = send_json(app, "GET", "/api/settings?mode=task", json!({})).await;
    assert_eq!(task_view["mcp_enabled"], json!(false), "task 覆盖应生效");
    let (_, rp_view) = send_json(app, "GET", "/api/settings", json!({})).await;
    assert_eq!(
        rp_view["mcp_enabled"],
        json!(true),
        "扁平层不受 task 覆盖影响"
    );

    // 还原现场:共享 app,别给同 binary 其他测试留状态(扁平层回默认;
    // task 覆盖层无「清除」语义,留着 Some(false) 不影响其他测试——无断言依赖 task 的 mcp 字段)
    let _ = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "mcp_enabled": false, "mcp_servers": [] }),
    )
    .await;
}

/// R3a:task_persona_full 执行者人设开关的 API 往返。
/// 默认(旧配置)= false(精简);PUT ?mode=task true → task 视图 true、roleplay 视图
/// 仍为扁平默认 false(覆盖层不污染扁平);还原后回 false。
#[tokio::test]
async fn task_persona_full_roundtrip_via_settings_api() {
    let app = test_app();

    // 默认:两视图均为 false(精简,旧配置兼容)
    let (status, s0) = send_json(app, "GET", "/api/settings?mode=task", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        s0["task_persona_full"],
        json!(false),
        "默认应为精简(false): {s0}"
    );

    // PUT task 覆盖层 true → task 视图 true;roleplay 视图读扁平值不受影响
    let (status, r) = send_json(
        app,
        "PUT",
        "/api/settings?mode=task",
        json!({ "task_persona_full": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "写入设置应 200: {r}");
    assert_eq!(r["settings"]["task_persona_full"], json!(true));
    let (_, task_view) = send_json(app, "GET", "/api/settings?mode=task", json!({})).await;
    assert_eq!(
        task_view["task_persona_full"],
        json!(true),
        "task 覆盖应生效(完整人设)"
    );
    let (_, rp_view) = send_json(app, "GET", "/api/settings?mode=roleplay", json!({})).await;
    assert_eq!(
        rp_view["task_persona_full"],
        json!(false),
        "扁平层(roleplay 视图)不受 task 覆盖影响"
    );

    // 还原现场(覆盖层无「清除」语义,Some(false) 与 None 同效 = 精简)
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/settings?mode=task",
        json!({ "task_persona_full": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, after) = send_json(app, "GET", "/api/settings?mode=task", json!({})).await;
    assert_eq!(after["task_persona_full"], json!(false));
}

/// 新装/首装(含 Android)设置接口返回非空的角色扮演默认提示词:
/// build_test_app 用全新临时数据目录(无 settings.json)→ 走 from_config 路径,
/// 正是手机端首装场景。修复前该字段为空串,面板与执行时都拿不到默认人设词。
#[tokio::test]
async fn roleplay_default_prompt_visible_on_fresh_install() {
    let app = test_app();
    let (status, s) = send_json(app, "GET", "/api/settings?mode=roleplay", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let p = s["agent_system_prompt"].as_str().unwrap_or("");
    assert!(!p.trim().is_empty(), "首装角色扮演默认提示词不得为空: {s}");
    for ph in ["{{char}}", "{{personality}}", "{{scenario}}", "{{world_info}}"] {
        assert!(p.contains(ph), "默认词应含占位符 {ph} 供宏展开");
    }
    assert!(p.contains("【创作总纲】"), "默认词应含创作总纲段: {p:.80}");

    // 模式隔离不因新默认退化:task 视角仍是任务向默认词,不含角色扮演宏
    let (_, t) = send_json(app, "GET", "/api/settings?mode=task", json!({})).await;
    let tp = t["agent_system_prompt"].as_str().unwrap_or("");
    assert!(!tp.trim().is_empty(), "task 缺省应有任务向默认词: {t}");
    assert!(!tp.contains("{{char}}"), "task 默认词不得继承角色扮演宏: {tp:.80}");
}
