// generate-raw 端点级测试(2026-09-13 Android「掉格式」修复的回归护栏):
//   ① 自愈成功:首轮 finish=length 半截 → 翻倍重发返回完整文本,且世界书 injected 保留;
//   ② 重发失败:回退首轮半截文本(200 + ok:true),不整体报错;
//   ③ 自愈用尽:始终 length 时返回最后一次文本;
//   ④ 入口校验:显式 max_tokens=0 返回 400(VALIDATION)。
// mock 钩子 [[trunc_text:]]/[[trunc_fail:]] 按预算区分首轮与自愈重发轮
//(预算门限 12000 落在结构化下限 8192 与翻倍值 16384 之间,见 connectors/mock.rs)。
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

async fn post_json(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
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

/// 上传内嵌世界书(keys=["system log"] 触发条目)的角色:供「injected 保留」断言。
/// 卡片 JSON 直接作 multipart 文件上传(既有测试同款路径),内嵌 character_book
/// 即角色卡世界书,由 generate-raw 的关键字匹配注入消费。
async fn upload_character_with_format_entry(app: &axum::Router) -> String {
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"raw-test.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        json!({
            "spec": "chara_card_v2",
            "spec_version": "1.0",
            "name": "generate-raw 测试角色",
            "description": "掉格式回归测试",
            "first_mes": "你好",
            "character_book": {
                "entries": [{
                    "keys": ["system log"],
                    "content": "<response_format_guidance>仅输出一个 JSON</response_format_guidance>",
                    "enabled": true,
                    "comment": "🔗格式🔗"
                }]
            }
        })
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/characters/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "角色上传应成功");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    v["id"].as_str().expect("上传响应应含角色 id").to_string()
}

/// ① 自愈成功 + injected 保留:首轮预算不足返回半截,重发(预算翻倍)返回完整文本;
/// 世界书注入条目在最终响应中必须保留(修复的关键差异点:注入只做一次、两轮复用)。
#[tokio::test]
async fn generate_raw_heals_truncated_and_keeps_injected() {
    let app = test_app();
    let cid = upload_character_with_format_entry(app).await;
    let (status, body) = post_json(
        app,
        "/api/chat/generate-raw",
        json!({
            "character_id": cid,
            "messages": [
                { "role": "system", "content": "你是角色扮演模型" },
                { "role": "assistant", "content": "{\"thinking\": \"...\"}\n/* system log(IGNORE the line): continue */" },
                { "role": "user", "content": "[[trunc_text:半截JSON文本|完整JSON文本]]" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["ok"], true, "body={body}");
    assert_eq!(
        body["text"], "完整JSON文本",
        "自愈重发后应返回完整文本,body={body}"
    );
    let injected = body["injected"].as_array().expect("injected 应为数组");
    assert!(
        injected.iter().any(|v| v.as_str() == Some("🔗格式🔗")),
        "世界书注入条目应在最终响应保留:{injected:?}"
    );
}

/// ② 重发失败回退半截:首轮 length 半截,重发返回 Err → 200 + 半截文本
/// (卡片解析器能救半截 JSON,报错提示反而更无用)。
#[tokio::test]
async fn generate_raw_falls_back_to_partial_when_retry_fails() {
    let app = test_app();
    let (status, body) = post_json(
        app,
        "/api/chat/generate-raw",
        json!({
            "messages": [
                { "role": "user", "content": "[[trunc_fail:半截文本A]]" }
            ]
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "重发失败应回退半截而非整体报错:body={body}"
    );
    assert_eq!(body["ok"], true, "body={body}");
    assert_eq!(body["text"], "半截文本A", "应回退首轮半截产出");
}

/// ③ 自愈用尽:每轮都返回 length → 最多 2 次重发后用最后一次文本正常返回
/// (而非报错或无限重试)。
#[tokio::test]
async fn generate_raw_returns_last_text_when_heal_exhausted() {
    let app = test_app();
    let (status, body) = post_json(
        app,
        "/api/chat/generate-raw",
        json!({
            "messages": [
                { "role": "user", "content": "[[finish:length|始终半截]]" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["ok"], true, "body={body}");
    assert_eq!(body["text"], "始终半截");
}

/// ④ 入口校验:max_tokens=0 非法(上游要求 ≥1),400 + code VALIDATION;
/// 显式大值不报错、由预算钳制兜底(响应形状不变)。
#[tokio::test]
async fn generate_raw_rejects_zero_max_tokens() {
    let app = test_app();
    let (status, body) = post_json(
        app,
        "/api/chat/generate-raw",
        json!({
            "max_tokens": 0,
            "messages": [{ "role": "user", "content": "hi" }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(body["code"], "VALIDATION", "body={body}");

    let (status, body) = post_json(
        app,
        "/api/chat/generate-raw",
        json!({
            "max_tokens": 4294967295u32,
            "messages": [{ "role": "user", "content": "[[trunc_text:半|完整]]" }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "超大值钳制后应正常生成:body={body}");
    assert_eq!(body["text"], "完整", "钳制到上限 ≥ 门限,应直接返回完整文本");
}
