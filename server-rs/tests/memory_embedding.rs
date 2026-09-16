// 记忆向量写入的批量语义(2026-09-16 性能批次 P-5)。
//
// 独立进程/独立 app 实例(与 settings_connector.rs 同惯例):本文件要 PUT 全局
// embedding_* 设置,放在 api_integration.rs 的共享 mock app 里会污染并行用例。
//
// 被测行为:蒸馏产生 N 条新记忆时,补向量应发 **1 次** `/embeddings` 请求且携带
// N 条 input;此前实现是「逐条 get + 逐条 embed」,会发 N 次请求、每次 1 条
// (同时还每次新建 HTTP 客户端)。这里用一个本地 mock embedding 服务器数请求次数与
// 每批条数,做端到端断言——这是 P-5 的核心收益,单测覆盖不到。
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kedai_server::build_test_app;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
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

async fn upload_character(app: &axum::Router, name: &str) -> (StatusCode, Value) {
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        json!({
            "spec": "chara_card_v2",
            "spec_version": "1.0",
            "name": "向量批量测试角色",
            "description": "d",
            "first_mes": "hi"
        })
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/characters/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// 蒸馏后补向量:多条记忆只发一次 /embeddings,且 input 一次带全
#[tokio::test]
async fn distill_embeds_new_memories_in_one_batch_request() {
    // ===== 本地 mock embedding 服务器:计数请求次数与每批条数 =====
    let calls = Arc::new(AtomicUsize::new(0));
    let batch_sizes: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_h = calls.clone();
    let sizes_h = batch_sizes.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mock = axum::Router::new().route(
        "/v1/embeddings",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let calls = calls_h.clone();
            let sizes = sizes_h.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let n = body["input"].as_array().map(|a| a.len()).unwrap_or(0);
                sizes.lock().unwrap().push(n);
                // 返回与输入等长的 3 维向量(index 乱序下发,顺带覆盖还原逻辑)
                let data: Vec<Value> = (0..n)
                    .rev()
                    .map(|i| {
                        json!({
                            "index": i,
                            "embedding": [i as f64 + 1.0, 0.5, 0.25]
                        })
                    })
                    .collect();
                axum::Json(json!({ "data": data, "usage": { "prompt_tokens": n } }))
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });

    let app = test_app();

    // ===== 开启蒸馏 + 配置 embedding 指向本地 mock =====
    let (status, r) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({
            "memory_distill_enabled": true,
            "embedding_enabled": true,
            "embedding_base_url": format!("http://{address}/v1"),
            "embedding_api_key": "sk-embed-test",
            "embedding_model": "mock-embed",
            "embedding_dim": 0,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "设置保存失败: {r}");

    // ===== 造一个会话,导入带 [[reply:]] 的历史(mock 连接器按行拆 3 条) =====
    let (status, char) = upload_character(app, "向量批量.json").await;
    assert_eq!(status, StatusCode::CREATED, "上传角色失败: {char}");
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    let (status, _) = send_json(
        app,
        "POST",
        "/api/import/chat",
        json!({ "session_id": sid, "messages": [
            { "role": "user", "content": "记录[[reply:甲喜欢雪\n乙怕打雷\n丙爱喝咖啡]]" },
            { "role": "assistant", "content": "好。" }
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // ===== 蒸馏:3 条新记忆 → 期望 1 次 embedding 请求、一次带 3 条 =====
    calls.store(0, Ordering::SeqCst);
    batch_sizes.lock().unwrap().clear();
    let (status, body) = send_json(
        app,
        "POST",
        "/api/memory/distill",
        json!({ "session_id": sid }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "蒸馏失败: {body}");
    assert_eq!(body["inserted"], json!(3), "应按行拆 3 条: {body}");

    let n_calls = calls.load(Ordering::SeqCst);
    let sizes = batch_sizes.lock().unwrap().clone();
    assert_eq!(
        n_calls, 1,
        "3 条新记忆应只发 1 次 /embeddings(批量),实际 {n_calls} 次,批大小 {sizes:?}"
    );
    assert_eq!(
        sizes,
        vec![3],
        "单次请求应携带全部 3 条 input,实际批大小 {sizes:?}"
    );

    // ===== 向量确实落库(批量写入路径生效) =====
    let (status, st) = send_json(app, "GET", "/api/memory/embedding-status", json!({})).await;
    assert_eq!(status, StatusCode::OK, "查询向量状态失败: {st}");
    assert_eq!(
        st["status"]["embedded"],
        json!(3),
        "3 条记忆都应已生成向量: {st}"
    );

    // ===== 收尾:关掉 embedding,避免影响同进程后续用例(进程内共享 app) =====
    let _ = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "embedding_enabled": false }),
    )
    .await;
}
