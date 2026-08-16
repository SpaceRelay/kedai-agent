// 宏调试端点端到端测试(阶段六 6b):POST /api/macros/expand
// 覆盖:{{char}}/{{getvar}}/{{random}} 展开、未知宏保留原样、ctx 传参、
// 空 text、history 对 {{lastMessage}}/{{firstMessage}} 生效。
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

async fn expand(app: &axum::Router, text: &str, ctx: Value) -> (StatusCode, Value) {
    send_json(
        app,
        "POST",
        "/api/macros/expand",
        json!({ "text": text, "ctx": ctx }),
    )
    .await
}

#[tokio::test]
async fn expands_common_macros_with_ctx() {
    let app = test_app();
    let (status, res) = expand(
        app,
        "{{char}} 对 {{user}} 说 {{getvar::好感}} {{var::flag}}",
        json!({
            "character_name": "雪芽",
            "user_name": "玩家",
            "vars": { "好感": 88, "flag": true }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "展开失败: {res}");
    assert_eq!(
        res["expanded"],
        json!("雪芽 对 玩家 说 88 true"),
        "常见宏应展开: {res}"
    );
}

#[tokio::test]
async fn random_roll_expand_in_range() {
    let app = test_app();
    let (status, res) = expand(app, "随机 {{random}} 骰子 {{roll:1d6}}", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let expanded = res["expanded"].as_str().unwrap();
    let prefix = expanded.strip_prefix("随机 ").unwrap();
    let (rand, rest) = prefix.split_once(' ').unwrap();
    assert!(rest.starts_with("骰子 "), "展开结构异常: {expanded}");
    let n: i64 = rand.parse().unwrap();
    assert!((0..1000).contains(&n), "{{random}} 应在 0..1000: {n}");
    let dice: i64 = rest.trim_start_matches("骰子 ").parse().unwrap();
    assert!((1..=6).contains(&dice), "{{roll:1d6}} 应在 1..=6: {dice}");
}

#[tokio::test]
async fn unknown_macro_kept_as_is() {
    let app = test_app();
    let (status, res) = expand(app, "未知 {{noSuchMacro}} 原样", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        res["expanded"],
        json!("未知 {{noSuchMacro}} 原样"),
        "未知宏应保留原样: {res}"
    );
}

#[tokio::test]
async fn empty_text_returns_empty_string() {
    let app = test_app();
    let (status, res) = expand(app, "", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["expanded"], json!(""));
}

#[tokio::test]
async fn history_feeds_last_and_first_message_macros() {
    let app = test_app();
    let (status, res) = expand(
        app,
        "首条 {{firstMessage}};末条 {{lastMessage}};用户末条 {{lastUserMessage}}",
        json!({
            "history": [
                { "role": "user", "content": "第一句" },
                { "role": "assistant", "content": "第二句" },
                { "role": "user", "content": "第三句" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        res["expanded"],
        json!("首条 第一句;末条 第三句;用户末条 第三句"),
        "history 应驱动消息宏: {res}"
    );
}

#[tokio::test]
async fn ctx_fields_flow_into_personality_scenario_and_input() {
    let app = test_app();
    let (status, res) = expand(
        app,
        "个性 {{personality}} 情景 {{scenario}} 输入 {{input}} 描述 {{character_description}}",
        json!({
            "personality": "害羞",
            "scenario": "图书馆",
            "user_input": "你好",
            "character_description": "兔族少女"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        res["expanded"],
        json!("个性 害羞 情景 图书馆 输入 你好 描述 兔族少女"),
        "ctx 字段应传入宏上下文: {res}"
    );
}

#[tokio::test]
async fn setvar_writes_are_scoped_to_request_only() {
    let app = test_app();
    // setvar 写入后 getvar 可读;两次请求之间不持久化(无引擎会话耦合)
    let (s1, r1) = expand(app, "{{setvar::a::42}}{{getvar::a}}", json!({})).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(r1["expanded"], json!("42"), "setvar 写入后应可读: {r1}");
    let (s2, r2) = expand(app, "{{getvar::a}}", json!({})).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(r2["expanded"], json!(""), "变量不应跨请求残留: {r2}");
}
