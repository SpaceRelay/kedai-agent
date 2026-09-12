// DB 并发改造集成测试:r2d2 只读连接池 + 单写连接 + spawn_blocking 阻塞隔离
// 验证:① 高并发读全部成功;② 写进行中并发读不报错;③ 写后读连接立即可见(WAL)
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn upload_character(app: &axum::Router, name: &str) -> Value {
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        json!({
            "spec": "chara_card_v2",
            "spec_version": "1.0",
            "name": "并发测试角色",
            "description": "DB 并发改造验证用",
            "first_mes": "你好,并发测试"
        })
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/characters/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_session(app: &axum::Router, cid: &str) -> Value {
    let (status, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    session
}

/// 并发 20 读 history + 20 读 characters,全部 200
#[tokio::test]
async fn concurrent_reads_all_succeed() {
    let app = test_app();
    let char = upload_character(app, "并发读.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let session = create_session(app, &cid).await;
    let sid = session["id"].as_str().unwrap().to_string();

    let mut handles = Vec::new();
    for _ in 0..20 {
        let app_h = app.clone();
        let sid_h = sid.clone();
        handles.push(tokio::spawn(async move {
            send_json(
                &app_h,
                "GET",
                &format!("/api/chat/history?session_id={sid_h}"),
                json!({}),
            )
            .await
            .0
        }));
        let app_c = app.clone();
        handles.push(tokio::spawn(async move {
            send_json(&app_c, "GET", "/api/characters", json!({}))
                .await
                .0
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap(), StatusCode::OK);
    }
}

/// 写(连续建会话)进行中并发读 characters / sessions 列表,双方全部成功
#[tokio::test]
async fn reads_during_writes_stay_healthy() {
    let app = test_app();
    let char = upload_character(app, "读写并发.json").await;
    let cid = char["id"].as_str().unwrap().to_string();

    let writer = {
        let app = app.clone();
        let cid = cid.clone();
        tokio::spawn(async move {
            for _ in 0..10 {
                create_session(&app, &cid).await;
            }
        })
    };
    let mut readers = Vec::new();
    for _ in 0..20 {
        let app = app.clone();
        let cid = cid.clone();
        readers.push(tokio::spawn(async move {
            let s1 = send_json(&app, "GET", "/api/characters", json!({})).await.0;
            let s2 = send_json(
                &app,
                "GET",
                &format!("/api/chat/sessions?character_id={cid}"),
                json!({}),
            )
            .await
            .0;
            (s1, s2)
        }));
    }
    writer.await.unwrap();
    for r in readers {
        let (s1, s2) = r.await.unwrap();
        assert_eq!(s1, StatusCode::OK);
        assert_eq!(s2, StatusCode::OK);
    }
}

/// 写后立即可读:读连接池(WAL)能看到已提交的写
#[tokio::test]
async fn write_then_read_visible_via_read_pool() {
    let app = test_app();
    let char = upload_character(app, "写后读.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let session = create_session(app, &cid).await;
    let sid = session["id"].as_str().unwrap().to_string();

    // 紧接着走读连接查询,history 应含开场白首条消息
    let (status, history) = send_json(
        app,
        "GET",
        &format!("/api/chat/history?session_id={sid}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let msgs = history["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["content"], json!("你好,并发测试"));
}
