// 自定义 Agent 执行流程(custom 模式)集成测试:
//  - PUT/GET /api/agent-flows 往返;非法流程 400
//  - custom 模式执行:2 步流程 + 步骤提示词(宏)→ mock [[floors]] 回显断言
//    system 含「[本步指令]」、步骤顺序与跨步骤 vars 持久化、Step 事件 index/total
//  - custom 未启用 → 400
// 流程配置为全局共享,测试间用 test_lock 串行化(仿 prompt_inject.rs)。
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kedai_server::build_test_app;
use serde_json::{json, Value};
use std::sync::OnceLock;
use tokio::sync::{Mutex, MutexGuard};
use tower::ServiceExt;

fn test_app() -> &'static axum::Router {
    static APP: OnceLock<axum::Router> = OnceLock::new();
    // 测试进程级:运行时主提示词目录指向空目录,避免真实项目根 AGENTS_RUNTIME.md 混入断言。
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // uuid 唯一名(避免多测试二进制并发共用同名目录);守卫随本闭包析构即回收。
        // 目录是否存在不影响语义:with_dir 关闭内置默认回退,读不到文件即视为「无提示词」,
        // 与「指向一个空目录」等价(全仓无测试写该文件)。
        let dir = kedai_server::utils::test_support::TempDataDir::new("test-runtime-prompt-empty");
        std::env::set_var("KEDAI_RUNTIME_PROMPT_DIR", dir.path());
    });
    APP.get_or_init(|| build_test_app().expect("构建测试应用失败"))
}

/// 串行化全局共享状态(流程配置)的测试
async fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().await
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
            "name": "测试角色",
            "description": "测试描述",
            "first_mes": "你好,我是测试角色",
            "custom_field": { "unknown": true }
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

/// 上传角色 + 建会话,返回 (sid, cid)
async fn new_session(app: &axum::Router) -> (String, String) {
    let (_, char) = upload_character(app, "流程测试.json").await;
    let cid = char["id"].as_str().unwrap().to_string();
    let (_, session) = send_json(
        app,
        "POST",
        "/api/chat/sessions",
        json!({ "character_id": cid }),
    )
    .await;
    let sid = session["id"].as_str().unwrap().to_string();
    (sid, cid)
}

/// 以指定 agent_mode 发送消息,解析 SSE 事件列表
async fn sse_custom_events(
    app: &axum::Router,
    sid: &str,
    cid: &str,
    message: &str,
    agent_mode: &str,
) -> Vec<Value> {
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/send")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "session_id": sid,
                "character_id": cid,
                "message": message,
                "agent_mode": agent_mode,
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "chat/send 应 200");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    text.split("\n\n")
        .filter_map(|block| {
            let block = block.trim();
            if block.is_empty() {
                return None;
            }
            let data_line = block.lines().find(|l| l.starts_with("data: "))?;
            serde_json::from_str(&data_line[6..]).ok()
        })
        .collect()
}

/// 合法的两步流程:分析(生成,带步骤提示词 1)→ 成文(生成,带步骤提示词 2)
fn two_step_flow() -> Value {
    json!({
        "enabled": true,
        "steps": [
            {
                "id": "s1", "name": "分析", "goal": "分析用户意图",
                "action": "direct", "generates": true, "enabled": true,
                "system_prompt": "{{setvar::步骤计数::1}}先分析意图",
                "temperature": 0.7, "max_tokens": 1024, "tools": null,
                "tool_choice": "auto", "parallel_tool_calls": false
            },
            {
                "id": "s2", "name": "成文", "goal": "生成正文",
                "action": "direct", "generates": true, "enabled": true,
                "system_prompt": "基于分析成文,步骤数={{getvar::步骤计数}}",
                "temperature": 0.8, "max_tokens": 2048, "tools": null
            }
        ]
    })
}

/// 恢复默认(未启用):固定 id 覆盖同一流程,避免流程库膨胀
async fn reset_flow(app: &axum::Router) {
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/agent-flows",
        json!({ "config": { "id": "test-reset-flow", "name": "重置测试流程", "enabled": false, "steps": [] } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "重置流程配置应 200");
}

#[tokio::test]
async fn agent_flows_crud_roundtrip() {
    let _guard = test_lock().await;
    let app = test_app();
    reset_flow(app).await;

    // 默认未启用
    let (status, def) = send_json(app, "GET", "/api/agent-flows", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(def["config"]["enabled"], false);

    // PUT 两步流程 → GET 回读一致
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/agent-flows",
        json!({ "config": two_step_flow() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, got) = send_json(app, "GET", "/api/agent-flows", json!({})).await;
    assert_eq!(got["config"]["enabled"], true);
    let steps = got["config"]["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["id"], "s1");
    assert_eq!(
        steps[0]["system_prompt"],
        "{{setvar::步骤计数::1}}先分析意图"
    );
    assert_eq!(steps[1]["max_tokens"], 2048);
    assert_eq!(steps[0]["tool_choice"], "auto");
    assert_eq!(steps[0]["parallel_tool_calls"], false);
    assert_eq!(
        steps[0]["tools"],
        Value::Null,
        "tools=null 表示该步不使用工具"
    );

    reset_flow(app).await;
}

#[tokio::test]
async fn agent_flows_library_select_delete() {
    let _guard = test_lock().await;
    let app = test_app();
    reset_flow(app).await;

    // PUT 无 id → 新建并选中(library.flows 增加,config 指向新流程)
    let (status, resp) = send_json(
        app,
        "PUT",
        "/api/agent-flows",
        json!({ "config": two_step_flow() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let flows = resp["library"]["flows"].as_array().unwrap();
    assert!(
        flows.len() >= 2,
        "库应含内置/重置流程 + 新建流程: {flows:?}"
    );
    let new_id = resp["config"]["id"].as_str().unwrap().to_string();
    assert!(!new_id.is_empty(), "新建流程应自动分配 id");
    assert_eq!(resp["library"]["current_flow_id"], new_id);

    // POST /select 切换回内置流程
    let (status, resp) = send_json(
        app,
        "POST",
        "/api/agent-flows/select",
        json!({ "id": "builtin-coordination" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "resp: {resp}");
    assert_eq!(resp["config"]["name"], "文学创作协调流程");
    assert_eq!(resp["library"]["current_flow_id"], "builtin-coordination");

    // select 不存在的 id → 400
    let (status, _) = send_json(
        app,
        "POST",
        "/api/agent-flows/select",
        json!({ "id": "no-such-flow" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // DELETE 新建流程 → 库减少,当前仍指向内置流程
    let (status, resp) = send_json(
        app,
        "DELETE",
        &format!("/api/agent-flows/{new_id}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "resp: {resp}");
    assert_eq!(resp["library"]["current_flow_id"], "builtin-coordination");
    let ids: Vec<&str> = resp["library"]["flows"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["id"].as_str())
        .collect();
    assert!(
        !ids.contains(&new_id.as_str()),
        "删除后库中不应再有该流程: {ids:?}"
    );

    // DELETE 不存在的 id → 400
    let (status, _) = send_json(app, "DELETE", "/api/agent-flows/no-such-flow", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    reset_flow(app).await;
}

#[tokio::test]
async fn agent_flows_invalid_flow_400() {
    let _guard = test_lock().await;
    let app = test_app();
    reset_flow(app).await;

    // 空步骤 → 400
    let (status, resp) = send_json(
        app,
        "PUT",
        "/api/agent-flows",
        json!({ "config": { "enabled": true, "steps": [] } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "resp: {resp}");
    assert!(
        resp["error"]
            .as_str()
            .unwrap_or("")
            .contains("自定义流程为空"),
        "resp: {resp}"
    );

    // 无生成步骤(仅理解 + 反思)→ 400
    let (status, resp) = send_json(
        app,
        "PUT",
        "/api/agent-flows",
        json!({
            "config": {
                "enabled": true,
                "steps": [
                    { "id": "a", "name": "理解", "goal": "理解意图", "action": "direct", "generates": false, "enabled": true },
                    { "id": "b", "name": "反思", "goal": "检查质量", "action": "reflect", "enabled": true }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "resp: {resp}");
    assert!(
        resp["error"].as_str().unwrap_or("").contains("生成步骤"),
        "resp: {resp}"
    );

    // 未注册白名单工具 → 400,不能在执行阶段静默忽略
    let (status, resp) = send_json(
        app,
        "PUT",
        "/api/agent-flows",
        json!({
            "config": {
                "enabled": true,
                "steps": [
                    { "id": "a", "name": "生成", "goal": "生成正文", "action": "direct", "generates": true, "enabled": true, "tools": ["missing_tool"] }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "resp: {resp}");
    assert!(resp["error"].as_str().unwrap_or("").contains("未注册工具"));

    // function 必须指向步骤有效工具 → 400
    let (status, resp) = send_json(
        app,
        "PUT",
        "/api/agent-flows",
        json!({
            "config": {
                "enabled": true,
                "steps": [
                    { "id": "a", "name": "生成", "goal": "生成正文", "action": "direct", "generates": true, "enabled": true,
                      "tools": ["read"], "tool_choice": "function", "tool_choice_function": "calculator" }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "resp: {resp}");
    assert!(resp["error"]
        .as_str()
        .unwrap_or("")
        .contains("不在有效工具内"));

    // 反思步骤带系统提示词 → 400
    let (status, resp) = send_json(
        app,
        "PUT",
        "/api/agent-flows",
        json!({
            "config": {
                "enabled": true,
                "steps": [
                    { "id": "a", "name": "生成", "goal": "生成正文", "action": "direct", "generates": true, "enabled": true },
                    { "id": "b", "name": "反思", "goal": "检查质量", "action": "reflect", "enabled": true, "system_prompt": "不应支持" }
                ]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "resp: {resp}");
    assert!(
        resp["error"].as_str().unwrap_or("").contains("反思步骤"),
        "resp: {resp}"
    );

    reset_flow(app).await;
}

#[tokio::test]
async fn custom_mode_runs_steps_with_prompts_and_progress() {
    let _guard = test_lock().await;
    let app = test_app();
    reset_flow(app).await;

    // 启用两步流程
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/agent-flows",
        json!({ "config": two_step_flow() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (sid, cid) = new_session(app).await;
    let events = sse_custom_events(app, &sid, &cid, "查看注入 [[floors]]", "custom").await;

    // 1) Step 事件携带 index/total 进度(两步 → 1/2、2/2 各出现)
    let steps: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "step" && e["index"].is_u64())
        .collect();
    assert!(
        !steps.is_empty(),
        "custom 模式应发送带进度的事件: {events:?}"
    );
    let indices: Vec<(u64, u64)> = steps
        .iter()
        .map(|e| (e["index"].as_u64().unwrap(), e["total"].as_u64().unwrap()))
        .collect();
    assert!(indices.contains(&(1, 2)), "应出现 1/2 进度: {indices:?}");
    assert!(indices.contains(&(2, 2)), "应出现 2/2 进度: {indices:?}");

    // 2) finish 内容 = 最后一步生成结果(mock 回显全部 LLM 消息)
    let finish = events
        .iter()
        .find(|e| e["type"] == "finish")
        .expect("缺 finish 事件");
    let content = finish["content"].as_str().unwrap_or("");
    // 第二步步骤提示词追加到 system 末尾(mock 回显 [system] 行内)
    assert!(
        content.contains("[本步指令]"),
        "步骤提示词应带 [本步指令] 标记:\n{content}"
    );
    assert!(
        content.contains("基于分析成文,步骤数=1"),
        "第二步提示词应展开,且 getvar 读到第一步 setvar 的 1:\n{content}"
    );
    // 用户消息与角色开场白在生成视图中
    assert!(
        content.contains("[user] 查看注入 [[floors]]"),
        "生成视图应含用户消息:\n{content}"
    );
    assert!(
        content.contains("[assistant] 你好,我是测试角色"),
        "生成视图应含开场白:\n{content}"
    );

    // 3) 非 custom 模式事件不带进度字段(回归:fast 不携带 index/total)
    let events_fast = sse_custom_events(app, &sid, &cid, "查看注入 [[floors]]", "fast").await;
    let step_evts: Vec<&Value> = events_fast.iter().filter(|e| e["type"] == "step").collect();
    assert!(!step_evts.is_empty());
    assert!(
        step_evts.iter().all(|e| e.get("index").is_none()),
        "fast 模式不应携带 index: {step_evts:?}"
    );

    reset_flow(app).await;
}

#[tokio::test]
async fn custom_mode_disabled_returns_400() {
    let _guard = test_lock().await;
    let app = test_app();
    reset_flow(app).await;

    let (sid, cid) = new_session(app).await;
    // 未启用流程(默认)→ chat/send 400
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/send")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "session_id": sid,
                "character_id": cid,
                "message": "你好",
                "agent_mode": "custom",
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "未启用流程的 custom 请求应 400"
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["error"].as_str().unwrap_or("").contains("自定义模式"),
        "resp: {body}"
    );

    // /api/agent/plan 预览同样 400
    let (status, resp) = send_json(
        app,
        "POST",
        "/api/agent/plan",
        json!({ "message": "你好", "agent_mode": "custom" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "resp: {resp}");
    assert!(
        resp["error"].as_str().unwrap_or("").contains("自定义模式"),
        "resp: {resp}"
    );
}

#[tokio::test]
async fn agent_plan_previews_custom_steps() {
    let _guard = test_lock().await;
    let app = test_app();
    reset_flow(app).await;

    let (status, _) = send_json(
        app,
        "PUT",
        "/api/agent-flows",
        json!({ "config": two_step_flow() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 预览:custom 计划按配置步骤生成(disabled 步骤被过滤)
    let (status, resp) = send_json(
        app,
        "POST",
        "/api/agent/plan",
        json!({ "message": "你好", "agent_mode": "custom" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "resp: {resp}");
    assert_eq!(resp["summary"], "自定义流程:共 2 步");
    let steps = resp["plan"]["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["name"], "分析");
    assert_eq!(steps[1]["name"], "成文");

    reset_flow(app).await;
}
