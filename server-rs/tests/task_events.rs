// 任务模式事件 SSE 集成测试(WP4):事件发射序列(created/status/plan/subtask/usage/deleted)
// 与 GET /api/tasks/events 端点冒烟。
// 使用 build_test_app()(mock 连接器 + 临时数据目录 + 免鉴权),与 tests/tasks.rs 同款基建。
// 订阅方式:直接打开 SSE 端点并逐帧读 body(handler 返回响应头前已完成 subscribe,
// 故拿到响应后再创建任务不会漏事件)。同进程多测试并发共享一个 app,
// 事件一律按 task_id 过滤,其他任务的事件不算失败。
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kedai_server::build_test_app;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;
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

async fn send_empty(app: &axum::Router, method: &str, path: &str) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    resp.status()
}

/// 创建任务并返回 id
async fn create_task(app: &axum::Router, title: &str) -> String {
    let (status, json) = send_json(app, "POST", "/api/tasks", json!({ "title": title })).await;
    assert_eq!(status, StatusCode::CREATED, "创建任务应返回 201: {json}");
    json["task"]["id"].as_str().unwrap().to_string()
}

/// 打开任务事件 SSE 流,返回(状态码, content-type, 流式 body)。
/// handler 在返回响应前已完成 broadcast subscribe,故此后的事件必达本流。
async fn open_event_stream(app: &axum::Router) -> (StatusCode, String, Body) {
    let req = Request::builder()
        .method("GET")
        .uri("/api/tasks/events")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    (status, content_type, resp.into_body())
}

/// 从 SSE body 读下一条 data 帧并解析为 JSON;流结束返回 None。
async fn read_event(body: &mut Body) -> Option<Value> {
    loop {
        let frame = body.frame().await?.ok()?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        let text = String::from_utf8_lossy(&data);
        for line in text.lines() {
            if let Some(payload) = line.strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<Value>(payload.trim()) {
                    return Some(v);
                }
            }
        }
    }
}

/// 收集指定任务的事件,直到出现终态 status(done/partial/error/ended)或超时。
/// 返回按到达顺序排列的 (kind, status) 序列。
async fn collect_until_terminal(body: &mut Body, task_id: &str) -> Vec<(String, Option<String>)> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut seq: Vec<(String, Option<String>)> = Vec::new();
    loop {
        let got = tokio::time::timeout_at(deadline, read_event(body)).await;
        match got {
            Ok(Some(ev)) => {
                if ev["type"].as_str() == Some("task") && ev["task_id"].as_str() == Some(task_id) {
                    let kind = ev["kind"].as_str().unwrap_or("").to_string();
                    let status = ev["status"].as_str().map(|s| s.to_string());
                    let terminal = kind == "status"
                        && matches!(
                            status.as_deref(),
                            Some("done") | Some("partial") | Some("error") | Some("ended")
                        );
                    seq.push((kind, status));
                    if terminal {
                        return seq;
                    }
                }
            }
            _ => panic!("等待任务 {task_id} 事件超时或流中断,已收: {seq:?}"),
        }
    }
}

/// 断言 expected 是 seq 的子序列(保序,中间允许多出其他事件)。
fn assert_subsequence(seq: &[(String, Option<String>)], expected: &[&str]) {
    let mut it = seq.iter();
    for want in expected {
        let found = it.any(|(kind, status)| {
            if let Some(st) = status {
                // 带状态的事件同时匹配 "kind" 与 "kind:status" 两种写法
                *want == *kind || *want == format!("{kind}:{st}")
            } else {
                *want == *kind
            }
        });
        assert!(found, "事件子序列缺少 {want},实际序列: {seq:?}");
    }
}

/// 事件发射序列:subscribe 后创建并执行一个任务(mock 2 步),
/// 应含 created → status(planning) → plan → status(running) → status(done) 保序子序列,
/// 且全部事件 task_id 一致(中间多出 usage/subtask 等其他 kind 不算失败)。
#[tokio::test]
async fn task_events_emitted_on_run() {
    let app = test_app();
    let (status, content_type, mut body) = open_event_stream(app).await;
    assert_eq!(status, StatusCode::OK, "事件流应 200");
    assert!(
        content_type.contains("text/event-stream"),
        "content-type 应为 SSE,实际: {content_type}"
    );

    // mock 规划器返回 2 步(与 tests/tasks.rs 同款 reply 钩子)
    let title =
        r#"[[reply:[{"name":"步骤一","goal":"写第一段"},{"name":"步骤二","goal":"写第二段"}] ]]"#;
    let id = create_task(app, title).await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let seq = collect_until_terminal(&mut body, &id).await;
    assert_subsequence(
        &seq,
        &[
            "created",
            "status:planning",
            "plan",
            "status:running",
            "status:done",
        ],
    );
}

/// usage 与 deleted 事件:完整执行后事件流中应存在 kind=usage;
/// 删除任务后应收到 kind=deleted 且 task_id 一致。
#[tokio::test]
async fn task_events_include_usage_and_deleted() {
    let app = test_app();
    let (status, _, mut body) = open_event_stream(app).await;
    assert_eq!(status, StatusCode::OK);

    let title = r#"[[reply:[{"name":"步骤一","goal":"写第一段"}] ]]"#;
    let id = create_task(app, title).await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let seq = collect_until_terminal(&mut body, &id).await;
    assert!(
        seq.iter().any(|(kind, _)| kind == "usage"),
        "执行过程应发射 kind=usage 事件,实际序列: {seq:?}"
    );
    assert!(
        seq.iter().any(|(kind, _)| kind == "subtask"),
        "执行过程应发射 kind=subtask 事件,实际序列: {seq:?}"
    );

    // 删除 → kind=deleted
    let status = send_empty(app, "DELETE", &format!("/api/tasks/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let got = tokio::time::timeout_at(deadline, read_event(&mut body)).await;
        match got {
            Ok(Some(ev)) => {
                if ev["type"].as_str() == Some("task")
                    && ev["task_id"].as_str() == Some(id.as_str())
                {
                    assert_eq!(
                        ev["kind"].as_str(),
                        Some("deleted"),
                        "删除后本任务的首个事件应为 deleted: {ev}"
                    );
                    break;
                }
            }
            _ => panic!("等待 deleted 事件超时或流中断"),
        }
    }
}

/// 端点冒烟:GET /api/tasks/events 返回 200 + content-type 含 text/event-stream。
/// 同时验证该静态路由未被 /api/tasks/{id} 通配吃掉(否则将走详情逻辑返回 404 JSON)。
#[tokio::test]
async fn task_events_sse_endpoint_smoke() {
    let app = test_app();
    let (status, content_type, _body) = open_event_stream(app).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "事件端点应 200(未被 /api/tasks/{{id}} 吃掉)"
    );
    assert!(
        content_type.contains("text/event-stream"),
        "content-type 应含 text/event-stream,实际: {content_type}"
    );
    // 对照组:不存在的任务详情确为 404,证明 {id} 通配仍各司其职
    let (status, _) = send_json(app, "GET", "/api/tasks/不存在的任务", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// 收集指定任务的全部事件 JSON,直到终态 status 或超时(delta 断言需要
/// detail/phase/step_index 完整字段,collect_until_terminal 只留 kind/status)。
async fn collect_full_until_terminal(body: &mut Body, task_id: &str) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut seq: Vec<Value> = Vec::new();
    loop {
        let got = tokio::time::timeout_at(deadline, read_event(body)).await;
        match got {
            Ok(Some(ev)) => {
                if ev["type"].as_str() == Some("task") && ev["task_id"].as_str() == Some(task_id) {
                    let terminal = ev["kind"].as_str() == Some("status")
                        && matches!(
                            ev["status"].as_str(),
                            Some("done") | Some("partial") | Some("error") | Some("ended")
                        );
                    seq.push(ev);
                    if terminal {
                        return seq;
                    }
                }
            }
            _ => panic!("等待任务 {task_id} 事件超时或流中断,已收 {} 条", seq.len()),
        }
    }
}

/// 批次 R4 流式输出(generate_text 链路):legacy 三段式执行期间事件流应含
/// kind=delta 暂态增量。断言:① planner/step/summarize 三阶段均有 delta 且
/// phase 正确、step 的 delta 带 step_index=0(planner 不带);② 攒批生效——
/// 步骤逐字流式输出约百字(每字一个 token),delta 条数远小于字符数;
/// ③ 权威对齐——各阶段 delta 拼接 == task_llm_calls 对应行 response_summary
/// (delta 暂态不落库,落库行为权威数据)。
#[tokio::test]
async fn task_events_delta_streaming_on_run() {
    let app = test_app();
    let (status, _, mut body) = open_event_stream(app).await;
    assert_eq!(status, StatusCode::OK);

    // 规划器走 [[reply_stream:]] 逐字流式钩子(批次 R4 新增,无 sleep);
    // 计划 JSON 不含连续 "]]",钩子取到行尾(与既有 [[reply:]] 测试同形态);
    // goal 写长使计划 JSON 超过 80 字攒批字符阈值;步骤/汇总走 mock 默认分支逐字流式
    let goal = "写一段关于秋天的长句,写落叶写黄昏写风声,反复铺陈细节直到足够长,再写远山与归鸟";
    let title = format!(r#"[[reply_stream:[{{"name":"步骤一","goal":"{goal}"}}] ]]"#);
    let id = create_task(app, &title).await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let seq = collect_full_until_terminal(&mut body, &id).await;
    let deltas: Vec<&Value> = seq
        .iter()
        .filter(|e| e["kind"].as_str() == Some("delta"))
        .collect();
    assert!(
        !deltas.is_empty(),
        "执行过程应发射 kind=delta 事件,序列: {seq:?}"
    );

    // ① phase/step_index 归属
    let planner: Vec<&&Value> = deltas
        .iter()
        .filter(|e| e["phase"].as_str() == Some("planner"))
        .collect();
    assert!(
        !planner.is_empty(),
        "应有 phase=planner 的 delta: {deltas:?}"
    );
    for d in &planner {
        assert!(
            d.get("step_index").is_none(),
            "planner delta 不应带 step_index: {d}"
        );
    }
    let step: Vec<&&Value> = deltas
        .iter()
        .filter(|e| e["phase"].as_str() == Some("step"))
        .collect();
    assert!(!step.is_empty(), "应有 phase=step 的 delta: {deltas:?}");
    for d in &step {
        assert_eq!(
            d["step_index"].as_u64(),
            Some(0),
            "step delta 应带 step_index=0: {d}"
        );
    }
    let summary: Vec<&&Value> = deltas
        .iter()
        .filter(|e| e["phase"].as_str() == Some("summarize"))
        .collect();
    assert!(
        !summary.is_empty(),
        "应有 phase=summarize 的 delta: {deltas:?}"
    );

    // ② 攒批生效:步骤逐字流式(每字一个 token),delta 条数远小于文本字符数
    let step_concat: String = step.iter().filter_map(|e| e["detail"].as_str()).collect();
    let step_chars = step_concat.chars().count();
    assert!(
        step.len() * 10 < step_chars,
        "攒批后 delta 条数({})应远小于字符数({step_chars})",
        step.len()
    );

    // ③ 权威对齐:delta 拼接 == 调用追踪落库行 response_summary(暂态 vs 权威)
    let (status, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let rows = calls["calls"].as_array().expect("calls 应为数组");
    let find_summary = |phase: &str| {
        rows.iter()
            .find(|r| r["phase"].as_str() == Some(phase))
            .and_then(|r| r["response_summary"].as_str())
            .unwrap_or("")
            .to_string()
    };
    let planner_concat: String = planner
        .iter()
        .filter_map(|e| e["detail"].as_str())
        .collect();
    assert_eq!(
        planner_concat,
        find_summary("planner"),
        "planner delta 拼接应与落库行一致"
    );
    assert_eq!(
        step_concat,
        find_summary("step"),
        "step delta 拼接应与落库行一致"
    );
    let summary_concat: String = summary
        .iter()
        .filter_map(|e| e["detail"].as_str())
        .collect();
    assert_eq!(
        summary_concat,
        find_summary("summarize"),
        "summarize delta 拼接应与落库行一致"
    );
}

/// 批次 R4 流式输出(引擎桥链路):solo 模式主 agent 的 Token 经 sink 事件桥
/// 攒批转发为 delta 事件(phase=agent,复用与 generate_text 同一 DeltaBatcher);
/// [[reply_stream:]] 逐字流式,断言攒批生效(事件数远小于字符数)且拼接完整。
#[tokio::test]
async fn task_events_delta_from_agent_sink() {
    let app = test_app();
    let (status, _, mut body) = open_event_stream(app).await;
    assert_eq!(status, StatusCode::OK);

    let text = "桥".repeat(200);
    let title = format!("[[reply_stream:{text}]]");
    let req = Request::builder()
        .method("POST")
        .uri("/api/tasks")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "title": title, "task_mode": "solo" }).to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let id = created["task"]["id"].as_str().unwrap().to_string();

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let seq = collect_full_until_terminal(&mut body, &id).await;
    let deltas: Vec<&Value> = seq
        .iter()
        .filter(|e| e["kind"].as_str() == Some("delta") && e["phase"].as_str() == Some("agent"))
        .collect();
    assert!(
        !deltas.is_empty(),
        "solo 主 agent 应经 sink 桥发射 delta: {seq:?}"
    );
    let concat: String = deltas.iter().filter_map(|e| e["detail"].as_str()).collect();
    assert_eq!(concat, text, "delta 拼接应等于完整流式文本(首尾无缺)");
    assert!(
        deltas.len() * 10 < text.chars().count(),
        "攒批后 delta 条数({})应远小于 token 数({})",
        deltas.len(),
        text.chars().count()
    );
}

/// 任务模式 bash 可下发(2026-09-13 用户要求):默认策略 `deny_dangerous` 对 bash
/// 按工具名开例外。mock 的 `[[tool:bash]]` 钩子只在工具进入本轮 tools 白名单时才产出
/// ToolCall(见 mock::tool_offered)——若策略仍把 bash 整体剔除,事件流里不会出现
/// 「调用工具 bash」。据此断言策略层确实下发了 bash。
///
/// 注意:本用例只证「工具被下发并进入执行闸门」,不证命令真的执行成功——
/// `exec_enabled` 默认关闭,执行会被总开关拒绝(非策略拒绝),这正是安全语义:
/// 下发 ≠ 放行,受总开关与命令级硬门双重约束。
#[tokio::test]
async fn task_solo_offers_bash_tool() {
    let app = test_app();
    let (status, _, mut body) = open_event_stream(app).await;
    assert_eq!(status, StatusCode::OK);

    let title = r#"[[tool:bash {"command":"echo kd-task-bash"}]] 主目标:执行只读命令"#;
    let req = Request::builder()
        .method("POST")
        .uri("/api/tasks")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "title": title, "task_mode": "solo" }).to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let id = created["task"]["id"].as_str().unwrap().to_string();

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let seq = collect_full_until_terminal(&mut body, &id).await;
    let called_bash = seq.iter().any(|e| {
        e["detail"]
            .as_str()
            .is_some_and(|d| d.contains("调用工具 bash"))
    });
    assert!(
        called_bash,
        "任务模式下 bash 应被下发并进入工具循环(deny_dangerous 例外),实际事件: {seq:?}"
    );
    // 反向确认:不得以「被任务策略拒绝」形态出现(那是策略层剔除的特征)
    let policy_denied = seq.iter().any(|e| {
        e["detail"]
            .as_str()
            .is_some_and(|d| d.contains("被任务策略拒绝") && d.contains("bash"))
    });
    assert!(
        !policy_denied,
        "bash 不应被任务策略拒绝(命令级拒绝应来自总开关/命令硬门,而非策略): {seq:?}"
    );
}

/// 批次 3 调用追踪:执行任务后事件流含 kind=llm_call(落库成功后发射),
/// GET /api/tasks/{id}/calls 返回 planner/step/summarize 三阶段调用行,字段与
/// 前端 TaskLlmCall 契约逐一对应。
#[tokio::test]
async fn task_llm_calls_tracked_on_run() {
    let app = test_app();
    let (status, _, mut body) = open_event_stream(app).await;
    assert_eq!(status, StatusCode::OK);

    let title = r#"[[reply:[{"name":"步骤一","goal":"写第一段"}] ]]"#;
    let id = create_task(app, title).await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let seq = collect_until_terminal(&mut body, &id).await;
    assert!(
        seq.iter().any(|(kind, _)| kind == "llm_call"),
        "执行过程应发射 kind=llm_call 事件,实际序列: {seq:?}"
    );

    let (status, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "calls 端点应 200");
    let rows = calls["calls"].as_array().expect("calls 应为数组");
    let phases: Vec<&str> = rows.iter().filter_map(|r| r["phase"].as_str()).collect();
    for want in ["planner", "step", "summarize"] {
        assert!(
            phases.contains(&want),
            "调用追踪缺 {want} 阶段,实际: {phases:?}"
        );
    }
    let first = &rows[0];
    for key in [
        "id",
        "task_id",
        "model",
        "prompt_summary",
        "response_summary",
        "prompt_tokens",
        "completion_tokens",
        "reasoning_tokens",
        "elapsed_ms",
        "status",
        "created_at",
    ] {
        assert!(first.get(key).is_some(), "调用行缺字段 {key}: {first}");
    }
}

/// 批次 R2a:followup 生命周期事件——终态任务追加指令后,事件流应含
/// status(running,追加开始)→ status(done,追加完成)保序子序列;均为既有 kind
///(不引入新事件类型),消息落库不产生独立事件(详情经 status 事件驱动重拉)。
#[tokio::test]
async fn task_followup_emits_status_events() {
    let app = test_app();
    let (status, _, mut body) = open_event_stream(app).await;
    assert_eq!(status, StatusCode::OK);

    // 首轮:solo 任务跑 done(默认 mock 回复;无钩子)
    let req = Request::builder()
        .method("POST")
        .uri("/api/tasks")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "title": "写一段关于秋天的短文", "task_mode": "solo" }).to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let id = created["task"]["id"].as_str().unwrap().to_string();
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let seq = collect_until_terminal(&mut body, &id).await;
    assert_subsequence(&seq, &["created", "status:running", "status:done"]);

    // 追加:followup 产出确定文本([[reply:]] 钩子),事件 = status:running → status:done
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/followup"),
        json!({ "content": "[[reply:追加成果]] 再补充一点" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "followup 应 200: {json}");
    let seq = collect_until_terminal(&mut body, &id).await;
    assert_subsequence(&seq, &["status:running", "status:done"]);
    // 无新事件 kind:followup 生命周期只出现既有 kind(status/usage/llm_call/delta 等)
    for (kind, _) in &seq {
        assert!(
            matches!(
                kind.as_str(),
                "created"
                    | "status"
                    | "plan"
                    | "subtask"
                    | "usage"
                    | "llm_call"
                    | "deleted"
                    | "agent_status"
                    | "approval_required"
                    | "delta"
            ),
            "followup 不得引入新事件 kind,实际: {seq:?}"
        );
    }
}

/// 批次 R2b:plan-chat 修订后重发 plan + approval_required 事件(保序),
/// 驱动前端刷新批准区计划与对话记录;任务保持 planned(无终态事件)。
#[tokio::test]
async fn task_plan_chat_reemits_plan_and_approval_events() {
    let app = test_app();
    let (status, _, mut body) = open_event_stream(app).await;
    assert_eq!(status, StatusCode::OK);

    // plan 模式:首轮规划产出计划A(双 reply_if 钩子按序区分首轮/修订轮;
    // 修订轮 system 含修订指引独特词「计划修订指引」,首轮仅「任务规划器」)
    let title = concat!(
        r#"[[reply_if:计划修订指引|[{"name":"修订步骤甲","goal":"修订目标甲"}] ]]"#,
        r#"[[reply_if:任务规划器|[{"name":"原步骤一","goal":"原目标一"}] ]]"#,
        " 写一份调研报告"
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/tasks")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "title": title, "task_mode": "plan" }).to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let id = created["task"]["id"].as_str().unwrap().to_string();
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    // 等首轮 approval_required(planned 待批准;plan 模式无终态 status,手动收集)
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut seq: Vec<(String, Option<String>)> = Vec::new();
    loop {
        let got = tokio::time::timeout_at(deadline, read_event(&mut body)).await;
        match got {
            Ok(Some(ev)) => {
                if ev["type"].as_str() == Some("task")
                    && ev["task_id"].as_str() == Some(id.as_str())
                {
                    let kind = ev["kind"].as_str().unwrap_or("").to_string();
                    let status = ev["status"].as_str().map(|s| s.to_string());
                    let hit = kind == "approval_required";
                    seq.push((kind, status));
                    if hit {
                        break;
                    }
                }
            }
            _ => panic!("等待首轮 approval_required 超时,已收: {seq:?}"),
        }
    }
    assert_subsequence(&seq, &["plan", "approval_required"]);

    // plan-chat 修订(同步:响应返回时计划已替换、事件已发射)
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/plan-chat"),
        json!({ "message": "把步骤换成先做竞品调研" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "plan-chat 应 200: {json}");

    // 修订后应重发 plan → approval_required 保序子序列(中间可夹 status/usage/llm_call)
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut seq2: Vec<(String, Option<String>)> = Vec::new();
    loop {
        let got = tokio::time::timeout_at(deadline, read_event(&mut body)).await;
        match got {
            Ok(Some(ev)) => {
                if ev["type"].as_str() == Some("task")
                    && ev["task_id"].as_str() == Some(id.as_str())
                {
                    let kind = ev["kind"].as_str().unwrap_or("").to_string();
                    let status = ev["status"].as_str().map(|s| s.to_string());
                    let hit = kind == "approval_required";
                    seq2.push((kind, status));
                    if hit {
                        break;
                    }
                }
            }
            _ => panic!("等待修订后 approval_required 超时,已收: {seq2:?}"),
        }
    }
    assert_subsequence(&seq2, &["plan", "approval_required"]);
    // 任务保持 planned:修订链路不出现任何终态 status 事件
    assert!(
        !seq2.iter().any(|(k, st)| k == "status"
            && matches!(
                st.as_deref(),
                Some("done") | Some("partial") | Some("error") | Some("ended")
            )),
        "plan-chat 不得产生终态事件: {seq2:?}"
    );
}

/// 裁决用例(2026-09-14 实测核查):每个工具事件是否被重复发射。
///
/// 背景:端到端实测(debug 二进制 + 真实上游)观察到 plan 模式每个 agent_status
/// 事件疑似出现两次,而发射点看只有一处(sink.rs 的 map_event → emit_event),
/// SSE 端点也只有一处 yield。本用例用 mock 确定性裁决「引擎↔sink↔SSE」链路
/// 是否真的存在重复发射,避免把探针脚本的分帧问题误判为产品缺陷。
///
/// 断言口径:一次工具调用应恰好产出 1 条「调用工具」+ 1 条「已返回结果」事件;
/// 若出现 2 条即证明链路上有重复发射(需修);出现 1 条即证明链路干净。
#[tokio::test]
async fn task_tool_events_are_not_duplicated() {
    let app = test_app();
    let (status, _, mut body) = open_event_stream(app).await;
    assert_eq!(status, StatusCode::OK);

    // mock:单轮 read 工具调用后收尾(非死循环形态)
    let title = r#"[[tool_loop:read|1 {"queries":[{"type":"character_prompt"}]}]] 读取角色提示词"#;
    let req = Request::builder()
        .method("POST")
        .uri("/api/tasks")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "title": title, "task_mode": "solo" }).to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let id = created["task"]["id"].as_str().unwrap().to_string();

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let seq = collect_full_until_terminal(&mut body, &id).await;
    let details: Vec<&str> = seq
        .iter()
        .filter(|e| e["kind"].as_str() == Some("agent_status"))
        .filter_map(|e| e["detail"].as_str())
        .collect();

    let called = details
        .iter()
        .filter(|d| d.contains("调用工具 read"))
        .count();
    let returned = details.iter().filter(|d| d.contains("已返回结果")).count();

    assert_eq!(
        called, 1,
        "一次工具调用应恰好 1 条「调用工具」事件(实际 {called} 条): {details:?}"
    );
    assert_eq!(
        returned, 1,
        "一次工具返回应恰好 1 条「已返回结果」事件(实际 {returned} 条): {details:?}"
    );
}
