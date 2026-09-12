// 任务模式集成测试:任务 CRUD + 执行(规划→子智能体→汇总)+ 停止。
// 使用 build_test_app()(mock 连接器 + 临时数据目录 + 免鉴权)。
// mock 钩子 [[reply:内容]] 让规划器返回 JSON 计划、子智能体返回指定结果,实现确定性端到端。
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

/// 轮询任务详情直到终态(done/partial/error/ended)或超时;返回最终 status 与详情
async fn wait_terminal(app: &axum::Router, id: &str) -> (String, Value) {
    for _ in 0..50 {
        let (status, json) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
        assert_eq!(status, StatusCode::OK);
        let st = json["task"]["status"].as_str().unwrap_or("").to_string();
        if st == "done" || st == "partial" || st == "error" || st == "ended" {
            return (st, json);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("任务 {id} 未在超时内到达终态");
}

#[tokio::test]
async fn task_crud_roundtrip() {
    let app = test_app();

    // 空标题被拒
    let (status, json) = send_json(app, "POST", "/api/tasks", json!({ "title": "   " })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "空标题应 400: {json}");

    // 创建
    let id = create_task(app, "写一篇短文").await;

    // 列表包含
    let (status, json) = send_json(app, "GET", "/api/tasks", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = json["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["id"].as_str())
        .collect();
    assert!(ids.contains(&id.as_str()), "列表应包含新任务");

    // 详情:status=pending,plan 空,subtasks 空
    let (status, json) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["task"]["status"], "pending");
    assert_eq!(json["task"]["plan"].as_array().unwrap().len(), 0);
    assert_eq!(json["subtasks"].as_array().unwrap().len(), 0);

    // 删除
    let status = send_empty(app, "DELETE", &format!("/api/tasks/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 删除后 404
    let (status, _) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// 端到端:mock 规划器返回 2 步,各子智能体返回结果,汇总后 status=done。
#[tokio::test]
async fn task_run_to_done() {
    let app = test_app();

    // title 内嵌 [[reply:...]] 让 mock 规划器直接返回 JSON 计划(2 步,goal 为普通文本);
    // 步骤 goal 无特殊标记,mock 子智能体返回默认非空回复。
    // 注意:mock reply 钩子在首个 "]]" 截断,故数组闭合 "]" 与标记闭合 "]]" 之间须留空格。
    let title =
        r#"[[reply:[{"name":"步骤一","goal":"写第一段"},{"name":"步骤二","goal":"写第二段"}] ]]"#;
    let id = create_task(app, title).await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "任务应完成,详情: {detail}");

    // 计划 2 步均 done
    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 2, "计划应拆为 2 步");
    for step in plan {
        assert_eq!(step["status"], "done");
    }

    // 子任务 2 条均 done 且结果非空
    let subtasks = detail["subtasks"].as_array().unwrap();
    assert_eq!(subtasks.len(), 2);
    for st in subtasks {
        assert_eq!(st["status"], "done");
        assert!(!st["result"].as_str().unwrap().is_empty());
    }

    // 最终结果非空
    assert!(!detail["task"]["result"].as_str().unwrap().is_empty());
}

/// 回归:roleplay 人设词不得污染任务模式执行器/汇总器提示词({{char}} 宏原文不得泄漏)。
/// 防退化背景:旧实现 task 模式无覆盖层时整体继承 roleplay 的 agent_system_prompt,
/// 任务目标「写一首关于秋天的短诗」被执行为言情小说(2026-08-25 实测)。
#[tokio::test]
async fn task_executor_prompt_not_contaminated_by_roleplay_persona() {
    let app = test_app();

    // 写入含宏与扮演标记的 roleplay 人设词(结束后还原)
    let (status, before) = send_json(app, "GET", "/api/settings?mode=roleplay", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let marker = "ROLEPLAY-PERSONA-MARKER";
    let persona = format!("你是 {{{{char}}}} 的扮演者,{marker},与用户进行沉浸式角色扮演");
    let (status, json) = send_json(
        app,
        "PUT",
        "/api/settings?mode=roleplay",
        json!({ "agent_system_prompt": persona }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "写入设置应 200: {json}");

    // 规划器返回 1 步;步骤 goal 带 [[floors]] 钩子,执行器回显其实际收到的完整消息序列。
    // 注意:[[floors]] 含 "]]" 会触发 mock reply 钩子提前截断,故 JSON 中用 unicode 转义。
    let title = r#"[[reply:[{"name":"回显步骤","goal":"\u005b\u005bfloors\u005d\u005d"}] ]]"#;
    let id = create_task(app, title).await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "任务应完成,详情: {detail}");

    // 执行器回显:不含 roleplay 人设词与宏原文,保留内置执行者指令与任务向默认提示词
    let echo = detail["subtasks"][0]["result"].as_str().unwrap_or("");
    assert!(!echo.is_empty(), "回显步骤应有结果: {detail}");
    assert!(
        !echo.contains(marker),
        "执行器提示词不得含 roleplay 人设词,实际: {echo}"
    );
    assert!(
        !echo.contains("{{char}}"),
        "宏原文不得泄漏进 LLM 上下文,实际: {echo}"
    );
    assert!(
        echo.contains("任务执行者"),
        "应保留内置执行者指令,实际: {echo}"
    );
    assert!(
        echo.contains("任务执行智能体"),
        "缺省应注入内置任务向默认提示词,实际: {echo}"
    );
    // WP7:用户可编辑的 Agent 提示词段应有 untrusted 边界包裹(内置执行者指令不包裹)
    assert!(
        echo.contains(r#"<UNTRUSTED_PROMPT_SOURCE source="agent_prompt">"#),
        "Agent 提示词段应有 untrusted 边界包裹,实际: {echo}"
    );

    // 汇总器同样不得被污染(其 user 消息含回显文本会再次触发 [[floors]] 回显自身消息序列)
    let summary = detail["task"]["result"].as_str().unwrap_or("");
    assert!(
        !summary.contains(marker) && !summary.contains("{{char}}"),
        "汇总器提示词不得含 roleplay 人设词,实际: {summary}"
    );

    // 还原 roleplay 人设词
    let prev = before["agent_system_prompt"].as_str().unwrap_or("");
    let _ = send_json(
        app,
        "PUT",
        "/api/settings?mode=roleplay",
        json!({ "agent_system_prompt": prev }),
    )
    .await;
}

/// 上传四段人设字段各带唯一标记的角色卡(R3a 测试夹具),返回角色 id。
/// 平铺(V1 风格)顶层字段:persona_style 读取口径为 data_raw 顶层
/// (description 入 CharacterRecord.description,其余经 data_raw 顶层取;
/// V2 卡的 data 包装层不在本批读取语义内,不动既有行为)。
async fn upload_persona_marked_character(app: &axum::Router) -> String {
    let card = json!({
        "name": "人设标记角色",
        "description": "R3A-DESC-MARK 人设正文",
        "personality": "R3A-PERS-MARK 人格",
        "scenario": "R3A-SCEN-MARK 情境",
        "mes_example": "R3A-MESEX-MARK 文风示例",
        "first_mes": "你好"
    });
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"persona-mark.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        card
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
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "上传角色卡应 2xx,实际 {status}: {json}"
    );
    json["character"]["id"]
        .as_str()
        .or_else(|| json["id"].as_str())
        .expect("上传响应应含角色 id")
        .to_string()
}

/// 创建 solo 任务(绑定执行者角色)并跑到终态,返回任务详情
async fn run_solo_task_with_character(
    app: &axum::Router,
    title: &str,
    character_id: &str,
) -> Value {
    let (status, json) = send_json(
        app,
        "POST",
        "/api/tasks",
        json!({ "title": title, "task_mode": "solo", "character_id": character_id }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建任务应 201: {json}");
    let id = json["task"]["id"].as_str().unwrap().to_string();
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "solo 任务应完成,详情: {detail}");
    detail
}

/// R3a:执行者人设精简/完整自选(task_persona_full)。
/// 默认(None/false)= 精简:solo 执行者 system 只含 description+personality,
/// 不含 scenario/mes_example;打开开关 = 完整:四段全量注入(与改造前一致)。
/// 单函数顺序执行:共享 app 的设置是全局态,拆两个并发测试会互相干扰开关。
#[tokio::test]
async fn task_persona_slim_by_default_and_full_when_enabled() {
    let app = test_app();
    let cid = upload_persona_marked_character(app).await;

    // ===== 1) 默认精简(旧配置兼容:None = 精简)=====
    // [[floors]] 钩子让 mock 执行者回显其实际收到的完整消息序列(system 含人设段)
    let detail = run_solo_task_with_character(app, "[[floors]]", &cid).await;
    let echo = detail["task"]["result"].as_str().unwrap_or("");
    assert!(!echo.is_empty(), "回显应有结果: {detail}");
    assert!(
        echo.contains("R3A-DESC-MARK"),
        "精简模式应保留人设 description: {echo}"
    );
    assert!(
        echo.contains("R3A-PERS-MARK"),
        "精简模式应保留人格 personality: {echo}"
    );
    assert!(
        !echo.contains("R3A-SCEN-MARK"),
        "精简模式不得含 scenario 内容: {echo}"
    );
    assert!(
        !echo.contains("R3A-MESEX-MARK"),
        "精简模式不得含 mes_example 文风示例: {echo}"
    );

    // ===== 2) 打开开关 = 完整(四段全量)=====
    let (status, json) = send_json(
        app,
        "PUT",
        "/api/settings?mode=task",
        json!({ "task_persona_full": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "写入设置应 200: {json}");
    let detail = run_solo_task_with_character(app, "[[floors]]", &cid).await;
    let echo = detail["task"]["result"].as_str().unwrap_or("");
    for marker in [
        "R3A-DESC-MARK",
        "R3A-PERS-MARK",
        "R3A-SCEN-MARK",
        "R3A-MESEX-MARK",
    ] {
        assert!(echo.contains(marker), "完整模式应含 {marker}: {echo}");
    }

    // 还原现场(共享 app;Some(false) 与 None 同效 = 精简)
    let _ = send_json(
        app,
        "PUT",
        "/api/settings?mode=task",
        json!({ "task_persona_full": false }),
    )
    .await;
}

/// 空输出重试(WP2):步骤 goal 触发 [[empty]](mock 只回 Usage 无 Token),
/// 执行器空内容自动重试一次仍空 → 步骤 error 且文案含「空内容」与 finish_reason;
/// 汇总照常产出 → 任务终态 partial(WP3:含 error 步骤但成果已产出)。
#[tokio::test]
async fn task_step_empty_output_retries_then_errors() {
    let app = test_app();

    // 规划器返回 1 步;goal 内 [[empty]] 须 unicode 转义:
    // 一是避免 mock reply 钩子在首个 "]]" 提前截断计划 JSON,
    // 二是避免规划/汇总消息原文含 "[[empty]]" 误触发空输出钩子(钩子按各自消息分别匹配)。
    let title = r#"[[reply:[{"name":"空步骤","goal":"\u005b\u005bempty\u005d\u005d"}] ]]"#;
    let id = create_task(app, title).await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(
        st, "partial",
        "空步骤 error + 汇总成功应为部分完成,详情: {detail}"
    );

    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0]["status"], "error", "空输出步骤应 error: {detail}");
    let result = plan[0]["result"].as_str().unwrap_or("");
    assert!(
        result.contains("空内容") && result.contains("finish_reason"),
        "步骤错误文案应含「空内容」与 finish_reason,实际: {result}"
    );

    // 子任务同样 error 且带原因
    let subtasks = detail["subtasks"].as_array().unwrap();
    assert_eq!(subtasks.len(), 1);
    assert_eq!(subtasks[0]["status"], "error");
    assert!(
        subtasks[0]["error"]
            .as_str()
            .unwrap_or("")
            .contains("空内容"),
        "子任务错误应含「空内容」,实际: {detail}"
    );
}

/// 部分完成终态(WP3):两步中一步失败([[fail:]] 钩子)、一步成功,
/// 汇总仍产出 → 任务终态 partial 而非 done/error。
#[tokio::test]
async fn task_partial_when_step_fails() {
    let app = test_app();

    // [[fail:...]] 同样须 unicode 转义(避免 reply 截断 + 避免规划消息误触发 fail 钩子)
    let title = r#"[[reply:[{"name":"成功步","goal":"写一段正常内容"},{"name":"失败步","goal":"\u005b\u005bfail:上游抖动\u005d\u005d"}] ]]"#;
    let id = create_task(app, title).await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "partial", "部分步骤失败应为部分完成,详情: {detail}");

    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 2);
    assert_eq!(plan[0]["status"], "done", "第一步应成功: {detail}");
    assert_eq!(plan[1]["status"], "error", "第二步应失败: {detail}");
    assert!(
        plan[1]["result"]
            .as_str()
            .unwrap_or("")
            .contains("上游抖动"),
        "失败步骤应保留上游错误原文,实际: {detail}"
    );
    // 汇总成果照常产出
    assert!(!detail["task"]["result"].as_str().unwrap_or("").is_empty());
}

/// 任务 usage 落库与聚合(WP4):一次完整执行(规划+2 步骤+汇总)后,
/// 详情接口 usage_total 聚合非零,全局累计接口口径一致且不小于单任务值。
#[tokio::test]
async fn task_usage_recorded_and_aggregated() {
    let app = test_app();

    let title =
        r#"[[reply:[{"name":"步骤一","goal":"写第一段"},{"name":"步骤二","goal":"写第二段"}] ]]"#;
    let id = create_task(app, title).await;
    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "任务应完成,详情: {detail}");

    // 单任务累计:规划+步骤+汇总均有 prompt/completion 消耗(mock 按字符数估算,恒 > 0)
    let usage = &detail["usage_total"];
    let p = usage["prompt_tokens"].as_i64().unwrap_or(0);
    let c = usage["completion_tokens"].as_i64().unwrap_or(0);
    assert!(
        p > 0,
        "任务 prompt_tokens 应 > 0(规划/步骤/汇总均已落库): {detail}"
    );
    assert!(c > 0, "任务 completion_tokens 应 > 0: {detail}");
    assert_eq!(
        usage["reasoning_tokens"].as_i64().unwrap_or(-1),
        0,
        "mock 无推理 token"
    );

    // 全局累计接口:存在且不小于该任务累计(同进程其他测试也可能写入,共享 DB)
    let (status, total) = send_json(app, "GET", "/api/tasks/usage-total", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let gp = total["usage_total"]["prompt_tokens"].as_i64().unwrap_or(0);
    let gc = total["usage_total"]["completion_tokens"]
        .as_i64()
        .unwrap_or(0);
    assert!(
        gp >= p && gc >= c,
        "全局累计应不小于单任务累计: {total} vs {detail}"
    );
}

/// 汇总路径空输出(WP6):规划与步骤正常、汇总返回空
/// ([[empty_if:任务汇总者]] 钩子:仅当 system 含「任务汇总者」时返回空,
/// 规划器/执行者 system 不含该子串故不受影响)→ 汇总按 finish_reason 分级重试一次
/// 仍空 → 任务终态 error。
/// 语义确认(executor.rs run_task_background):步骤全成功但汇总失败走 set_error
/// → error 终态;partial 仅用于「步骤 error 但汇总成功产出成果」。二者互补勿混淆。
/// (task_step_empty_output_retries_then_errors 仅覆盖步骤路径,本测试覆盖汇总路径。)
#[tokio::test]
async fn task_summary_empty_output_retries_then_errors() {
    let app = test_app();

    // [[empty_if:]] 置于 [[reply:]] 之后:reply 仍在首个 "]]" 截断出计划 JSON,
    // empty_if 不参与截断;汇总 user 消息含整个 title(带钩子)故被命中。
    let title = r#"[[reply:[{"name":"步骤一","goal":"写第一段"}] ]][[empty_if:任务汇总者]]"#;
    let id = create_task(app, title).await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "error", "汇总重试后仍空应为 error 终态,详情: {detail}");

    // 错误文案含「空内容」与 finish_reason(诊断推理耗尽 vs 内容过滤等成因)
    let error = detail["task"]["error"].as_str().unwrap_or("");
    assert!(
        error.contains("空内容") && error.contains("finish_reason"),
        "汇总错误文案应含「空内容」与 finish_reason,实际: {error}"
    );

    // 步骤本身成功(done),失败发生在汇总阶段;最终结果为空(set_result 未被调用)
    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0]["status"], "done", "步骤应成功,失败在汇总: {detail}");
    let subtasks = detail["subtasks"].as_array().unwrap();
    assert_eq!(subtasks.len(), 1);
    assert_eq!(subtasks[0]["status"], "done", "子任务应成功: {detail}");
    assert!(
        detail["task"]["result"].as_str().unwrap_or("").is_empty(),
        "汇总失败不应有最终结果: {detail}"
    );
}

/// 停止运行中任务(WP6):任务进入 running 且首个子任务执行中时 stop →
/// 任务终态 ended;执行中子任务一并 ended,不得残留 running/pending。
/// (task_stop_pending 仅覆盖 pending 态,本测试覆盖 running 态。)
/// 时序依据:步骤 goal 无钩子,mock 走逐字流式默认回复(约 8ms/字符,单步 1s+),
/// 为「轮询观测 running 子任务 → stop」留出充足时间窗。
#[tokio::test]
async fn task_stop_running() {
    let app = test_app();

    let title =
        r#"[[reply:[{"name":"步骤一","goal":"写第一段"},{"name":"步骤二","goal":"写第二段"}] ]]"#;
    let id = create_task(app, title).await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    // 轮询直到任务 running 且有子任务进入 running(执行中)
    let mut saw_running_subtask = false;
    for _ in 0..200 {
        let (status, json) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
        assert_eq!(status, StatusCode::OK);
        let task_running = json["task"]["status"].as_str() == Some("running");
        let sub_running = json["subtasks"]
            .as_array()
            .map(|s| s.iter().any(|st| st["status"].as_str() == Some("running")))
            .unwrap_or(false);
        if task_running && sub_running {
            saw_running_subtask = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(saw_running_subtask, "应观测到 running 中的子任务再 stop");

    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{id}/stop"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "running 态 stop 应 200");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "ended", "stop 后任务应 ended,详情: {detail}");
    let subtasks = detail["subtasks"].as_array().unwrap();
    assert!(!subtasks.is_empty(), "stop 时已存在执行中子任务: {detail}");
    for st in subtasks {
        let s = st["status"].as_str().unwrap_or("");
        assert!(
            s != "running" && s != "pending",
            "子任务不得残留 running/pending: {detail}"
        );
    }
    assert!(
        subtasks
            .iter()
            .any(|s| s["status"].as_str() == Some("ended")),
        "执行中子任务应被 stop 置为 ended: {detail}"
    );
}

/// 停止一个 pending 任务:status 转 ended。
#[tokio::test]
async fn task_stop_pending() {
    let app = test_app();
    let id = create_task(app, "待停止的任务").await;

    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{id}/stop"), json!({})).await;
    assert_eq!(status, StatusCode::OK);

    let (status, json) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["task"]["status"], "ended");
}

/// 不存在的任务:run/stop/get 均 4xx。
#[tokio::test]
async fn task_not_found() {
    let app = test_app();
    let (status, _) = send_json(app, "POST", "/api/tasks/不存在/run", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = send_json(app, "POST", "/api/tasks/不存在/stop", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send_json(app, "GET", "/api/tasks/不存在", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// 规划输出截断打捞:mock 规划器返回 2 个完整步骤对象 + 半截字符串
/// (2026-08-27 exe 实测「解析计划失败: EOF while parsing a string」的形态,
/// 推理模型 max_tokens 被 reasoning 耗尽导致 JSON 尾部缺失),
/// parse_plan 应打捞恢复完整步骤并跑到 done,而不是整个任务报错。
#[tokio::test]
async fn task_plan_truncated_json_salvaged() {
    let app = test_app();

    // 截断形态:第三个对象只有半个字符串;内容不含 "]]",不会触发 mock reply 钩子提前截断。
    let title = r#"[[reply:[{"name":"收集意象","goal":"收集秋天意象"}, {"name":"写初稿","goal":"写出初稿"}, {"name":"润色","goal":"润 ]]"#;
    let id = create_task(app, title).await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "截断计划打捞后任务应完成,详情: {detail}");

    // 打捞恢复 2 步(半截的第三步丢弃),均执行 done
    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 2, "应打捞 2 个完整步骤: {plan:?}");
    for step in plan {
        assert_eq!(step["status"], "done");
    }
    assert!(!detail["task"]["result"].as_str().unwrap().is_empty());
}

// ==================== 批次 4.2/4.3a:六模式(solo / plan / approve) ====================

/// 创建指定模式的任务并返回 id
async fn create_task_with_mode(app: &axum::Router, title: &str, mode: &str) -> String {
    let (status, json) = send_json(
        app,
        "POST",
        "/api/tasks",
        json!({ "title": title, "task_mode": mode }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建任务应返回 201: {json}");
    json["task"]["id"].as_str().unwrap().to_string()
}

/// 轮询任务详情直到出现目标状态或超时
async fn wait_status(app: &axum::Router, id: &str, target: &str) -> Value {
    for _ in 0..50 {
        let (status, json) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
        assert_eq!(status, StatusCode::OK);
        if json["task"]["status"].as_str() == Some(target) {
            return json;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("任务 {id} 未在超时内到达状态 {target}");
}

/// solo 模式:目标直接进 run_tool_loop(mock 不感知 tools,一轮即出),
/// 终态 done,结果 = mock reply;调用追踪含 phase="agent" 行。
#[tokio::test]
async fn task_solo_mode_runs_to_done() {
    let app = test_app();

    let id = create_task_with_mode(app, "[[reply:主agent最终成果]]", "solo").await;

    // create 透传模式:详情应为 solo
    let (_, detail) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    assert_eq!(
        detail["task"]["task_mode"], "solo",
        "创建后模式应为 solo: {detail}"
    );

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "solo 任务应完成,详情: {detail}");
    assert_eq!(
        detail["task"]["result"].as_str().unwrap_or(""),
        "主agent最终成果",
        "结果应为 mock reply 文本: {detail}"
    );

    // 调用追踪:solo 经统一出口落 phase="agent" 行(token 为整轮累计)
    let (status, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let calls = calls["calls"].as_array().unwrap();
    assert!(
        calls
            .iter()
            .any(|c| c["phase"] == "agent" && c["status"] == "ok"),
        "应存在 phase=agent 且 status=ok 的调用追踪行: {calls:?}"
    );
}

/// plan 模式:run 只规划不执行(零副作用)→ planned 待批准;
/// approve(不带 plan)后按已批准计划逐步骤续跑 → done(批次 4.3 回归:
/// approve 不得按 task_mode 重派回规划器,否则再规划一遍回到 planned 死循环)。
/// planned 态 run 重入与非 planned 态 approve 均 400。
#[tokio::test]
async fn task_plan_mode_waits_for_approval_then_executes_plan() {
    let app = test_app();

    let title =
        r#"[[reply:[{"name":"步骤一","goal":"写第一段"},{"name":"步骤二","goal":"写第二段"}] ]]"#;
    let id = create_task_with_mode(app, title, "plan").await;

    // 非 planned 态 approve 应 400(任务尚未规划)
    let (status, json) =
        send_json(app, "POST", &format!("/api/tasks/{id}/approve"), json!({})).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "pending 态 approve 应 400: {json}"
    );

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    // 只规划:到达 planned 待批准态,计划 2 步,零副作用(无子任务)
    let mut detail = wait_status(app, &id, "planned").await;
    assert_eq!(
        detail["task"]["plan"].as_array().unwrap().len(),
        2,
        "计划应拆为 2 步: {detail}"
    );
    assert!(
        detail["subtasks"].as_array().unwrap().is_empty(),
        "plan 模式不得执行任何步骤(零副作用): {detail}"
    );
    // 批次 R1:planned 态 result = 待批准的计划清单(批准前预览,含各步骤名)。
    // 由 run_inner 的 AwaitApproval 分支在状态置 planned 之后落库,须轮询等待写入
    for _ in 0..50 {
        if !detail["task"]["result"].as_str().unwrap_or("").is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, json) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
        detail = json;
    }
    let planned_result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(
        planned_result.contains("计划已产出,共 2 步"),
        "planned 态 result 应为待批准的计划清单: {planned_result}"
    );
    assert!(
        planned_result.contains("步骤一") && planned_result.contains("步骤二"),
        "planned 态 result 应含各计划步骤名: {planned_result}"
    );

    // planned 态拒绝 run 重入(提示走 approve)
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "planned 态 run 应 400: {json}"
    );
    assert!(
        json["error"].as_str().unwrap_or("").contains("approve"),
        "错误文案应提示走 approve: {json}"
    );

    // 批准(不带 plan,按已产出计划原样批准)→ 按计划逐步骤续跑 → done
    let (status, json) =
        send_json(app, "POST", &format!("/api/tasks/{id}/approve"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "approve 应 200: {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "批准后按计划续跑应完成,详情: {detail}");
    assert!(!detail["task"]["result"].as_str().unwrap_or("").is_empty());
    // 续跑消费已批准计划:每步推进到 done 且各有 result(不得残留 pending)
    for step in detail["task"]["plan"].as_array().unwrap() {
        assert_eq!(step["status"], "done", "续跑后步骤应 done: {detail}");
        assert!(
            !step["result"].as_str().unwrap_or("").is_empty(),
            "续跑后步骤应有 result: {detail}"
        );
    }
}

/// plan 模式:approve 携修改后计划 → 计划被整体替换,续跑执行的是修改版计划。
#[tokio::test]
async fn task_plan_approve_with_modified_plan_replaces() {
    let app = test_app();

    let title =
        r#"[[reply:[{"name":"步骤一","goal":"写第一段"},{"name":"步骤二","goal":"写第二段"}] ]]"#;
    let id = create_task_with_mode(app, title, "plan").await;

    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let detail = wait_status(app, &id, "planned").await;
    assert_eq!(detail["task"]["plan"].as_array().unwrap().len(), 2);

    // 携修改后计划批准:2 步 → 1 步(goal 内嵌 [[reply:]] 钩子让该步产出确定文本,
    // 供下方最终计划段断言;标题内首个 [[reply:]] 钩子会被 mock 汇总轮吃到并回显
    // 原始规划 JSON,故「不含步骤二」只能断言在最终计划段内,见下)
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/approve"),
        json!({ "plan": [{ "name": "改写步", "goal": "[[reply:修改版产出]] 产出修改版" }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "批准后按计划续跑应完成,详情: {detail}");
    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 1, "计划应被替换为修改后的 1 步: {detail}");
    assert_eq!(
        plan[0]["name"], "改写步",
        "计划内容应为批准时携带的版本: {detail}"
    );
    assert_eq!(
        plan[0]["status"], "done",
        "修改版步骤应被执行到 done: {detail}"
    );

    // 批次 R1:续跑完成 result = 汇总文本 + 「## 最终计划」段,段内容反映修改版计划
    let result = detail["task"]["result"].as_str().unwrap_or("");
    let section = result.split("## 最终计划").nth(1).unwrap_or("");
    assert!(
        !section.is_empty(),
        "result 应含「## 最终计划」段: {result}"
    );
    assert!(
        section.contains("改写步"),
        "最终计划段应反映修改版计划: {section}"
    );
    assert!(
        section.contains("修改版产出"),
        "最终计划段应含修改版步骤的产出概要: {section}"
    );
    assert!(
        !section.contains("步骤二"),
        "被替换掉的步骤不得出现在最终计划段: {section}"
    );
}

/// plan 批准续跑消费已批准计划(2026-08 真实模型实测修复):approve 后逐步执行——
/// 每步独立 run_agent_loop(步骤 name+goal 为该步指令,整体目标作上下文),完成即
/// 回写该步 status=done/result;全部步骤完成后 SUMMARIZER_PROMPT 汇总产出最终
/// result(对齐 legacy 执行段语义)。旧实现强制 solo 只吃 goal,已批准计划仅落库
/// 存档:实测续跑后 4 个步骤永远 pending、step result 全空,任务却 done。
#[tokio::test]
async fn task_plan_approve_resume_executes_plan_steps() {
    let app = test_app();

    // 步骤 goal 内嵌 \[ \] 转义的 reply 钩子:规划 JSON 经 serde 解析还原为真实钩子,
    // 续跑时各步骤 user 消息(当前步骤在前)首个 [[reply: 即本步钩子 → 步骤专属产出。
    let title = concat!(
        r#"[[reply:[{"name":"步骤一","goal":"\u005b\u005breply:步骤一成果\u005d\u005d 写第一段"},"#,
        r#"{"name":"步骤二","goal":"\u005b\u005breply:步骤二成果\u005d\u005d 写第二段"}] ]]"#,
        "[[reply_if:任务汇总者|计划续跑最终成果]]",
        " 计划总目标"
    );
    let id = create_task_with_mode(app, title, "plan").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let detail = wait_status(app, &id, "planned").await;
    assert_eq!(
        detail["task"]["plan"].as_array().unwrap().len(),
        2,
        "计划应拆为 2 步: {detail}"
    );

    let (status, json) =
        send_json(app, "POST", &format!("/api/tasks/{id}/approve"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "approve 应 200: {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "批准续跑应完成,详情: {detail}");

    // 逐步执行:每步 done 且各有专属 result(旧实现步骤永远 pending、result 全空)
    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 2, "计划仍为 2 步: {detail}");
    assert_eq!(plan[0]["status"], "done", "步骤一应 done: {detail}");
    assert_eq!(plan[1]["status"], "done", "步骤二应 done: {detail}");
    let r0 = plan[0]["result"].as_str().unwrap_or("");
    let r1 = plan[1]["result"].as_str().unwrap_or("");
    assert!(r0.contains("步骤一成果"), "步骤一应有自身产出: {r0}");
    assert!(r1.contains("步骤二成果"), "步骤二应有自身产出: {r1}");

    // 最终 result = SUMMARIZER_PROMPT 汇总产出
    let result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(
        result.contains("计划续跑最终成果"),
        "最终结果应为汇总产出: {result}"
    );

    // 调用追踪:每步一行 phase=agent(step_index=步骤下标)+ 汇总行
    //(phase=summarize,复用 legacy summarize_task_retry 的既有口径)
    let (_, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    let calls = calls["calls"].as_array().unwrap();
    let agent_calls: Vec<&Value> = calls.iter().filter(|c| c["phase"] == "agent").collect();
    assert_eq!(agent_calls.len(), 2, "每步一次 agent 调用: {calls:?}");
    assert!(
        agent_calls.iter().any(|c| c["step_index"] == 0)
            && agent_calls.iter().any(|c| c["step_index"] == 1),
        "agent 调用 step_index 应为步骤下标: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c["phase"] == "summarize" && c["status"] == "ok"),
        "应有汇总调用行: {calls:?}"
    );

    // F4(2026-09-10 实测修复):续跑 goal 显式要求「以已批准计划为准」,
    // 防止用户经 plan-chat 改过步数后,汇总仍沿用原始目标里的旧步数描述。
    assert!(
        agent_calls.iter().any(|c| c["prompt_summary"]
            .as_str()
            .unwrap_or("")
            .contains("步骤数量与内容一律以本清单为准")),
        "续跑 goal 应含「以已批准计划为准」约束: {calls:?}"
    );
}

/// 批次 R1:plan 批准续跑完成的 result = 汇总文本 + 「## 最终计划」段
/// (前端拆卡契约,对齐 team「## 审计结论」):汇总文本在前、最终计划段在后;
/// 段内含各步名称、状态(done)与 result 概要。
#[tokio::test]
async fn task_plan_resume_result_contains_final_plan_section() {
    let app = test_app();

    // 钩子布局同 task_plan_approve_resume_executes_plan_steps:规划器吃首个 [[reply:]]
    // 出计划;各步骤 goal 内嵌转义 [[reply:]] 出步骤专属产出;汇总轮吃 reply_if(任务汇总者)。
    let title = concat!(
        r#"[[reply:[{"name":"步骤一","goal":"\u005b\u005breply:步骤一成果\u005d\u005d 写第一段"},"#,
        r#"{"name":"步骤二","goal":"\u005b\u005breply:步骤二成果\u005d\u005d 写第二段"}] ]]"#,
        "[[reply_if:任务汇总者|批次R1汇总成果]]",
        " 最终计划段总目标"
    );
    let id = create_task_with_mode(app, title, "plan").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    wait_status(app, &id, "planned").await;

    let (status, json) =
        send_json(app, "POST", &format!("/api/tasks/{id}/approve"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "approve 应 200: {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "批准续跑应完成,详情: {detail}");

    let result = detail["task"]["result"].as_str().unwrap_or("");
    // 汇总文本在前、「## 最终计划」段在后
    let sum_idx = result.find("批次R1汇总成果").unwrap_or(usize::MAX);
    let mark_idx = result.find("## 最终计划").unwrap_or(usize::MAX);
    assert!(
        mark_idx != usize::MAX,
        "result 应含「## 最终计划」段: {result}"
    );
    assert!(sum_idx < mark_idx, "汇总文本应在最终计划段之前: {result}");
    // 段内:各步名称 + done 状态 + result 概要
    let section = &result[mark_idx..];
    assert!(
        section.contains("步骤一(done)"),
        "段内应含步骤一及其状态: {section}"
    );
    assert!(
        section.contains("步骤一成果"),
        "段内应含步骤一 result 概要: {section}"
    );
    assert!(
        section.contains("步骤二(done)"),
        "段内应含步骤二及其状态: {section}"
    );
    assert!(
        section.contains("步骤二成果"),
        "段内应含步骤二 result 概要: {section}"
    );
}

/// 批次 R1:含失败步骤时终态 partial(对齐 legacy「有产出则 partial」语义),
/// 「## 最终计划」段内失败步标 error 且概要带失败原因。
/// 注:ApprovedPlanExecutor 每步 user 消息都携带完整计划上下文,[[fail:]]/[[empty]]
/// 等 user 侧无条件钩子会毒化全部步骤;[[empty_if:任务执行者]] 只匹配步骤执行的
/// system 提示词(EXECUTOR_PROMPT),规划器(任务规划器)/汇总器(任务汇总者)不命中,
/// 是 mock 下可精确只让步骤失败的钩子(mock 条件钩子仅匹配 system 消息)。
#[tokio::test]
async fn task_plan_resume_final_plan_section_marks_error_step() {
    let app = test_app();

    // 钩子布局:规划器吃首个 [[reply:]] 出计划;步骤轮 system 含「任务执行者」
    // → empty_if 命中返回空内容,run_agent_loop 报错「步骤 N返回空内容」;
    // 汇总轮吃 reply_if(任务汇总者)出确定性汇总文本。
    let title = concat!(
        r#"[[reply:[{"name":"步骤一","goal":"写第一段"},"#,
        r#"{"name":"步骤二","goal":"写第二段"}] ]]"#,
        "[[reply_if:任务汇总者|有失败步的汇总成果]]",
        "[[empty_if:任务执行者]]",
        " 最终计划段失败步目标"
    );
    let id = create_task_with_mode(app, title, "plan").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    wait_status(app, &id, "planned").await;

    let (status, json) =
        send_json(app, "POST", &format!("/api/tasks/{id}/approve"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "approve 应 200: {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "partial", "含失败步骤应部分完成,详情: {detail}");

    let result = detail["task"]["result"].as_str().unwrap_or("");
    // 汇总文本在前、「## 最终计划」段在后
    let sum_idx = result.find("有失败步的汇总成果").unwrap_or(usize::MAX);
    let mark_idx = result.find("## 最终计划").unwrap_or(usize::MAX);
    assert!(
        mark_idx != usize::MAX,
        "result 应含「## 最终计划」段: {result}"
    );
    assert!(sum_idx < mark_idx, "汇总文本应在最终计划段之前: {result}");
    let section = &result[mark_idx..];
    assert!(
        section.contains("步骤一(error)"),
        "段内步骤一应标 error: {section}"
    );
    assert!(
        section.contains("步骤二(error)"),
        "段内步骤二应标 error: {section}"
    );
    assert!(
        section.contains("返回空内容"),
        "段内失败步概要应带失败原因: {section}"
    );
}

/// plan 批准续跑取消:规划器即时产出计划后,步骤执行走 mock 默认逐字流式回复
///(约 1s/步窗口),running 中 stop → ended;终态后剩余 pending 步骤统一置 error
///「任务已停止」(与 team 口径对齐,不留永远 pending/running 的步骤)。
#[tokio::test]
async fn task_plan_approve_resume_stop_marks_remaining_steps_error() {
    let app = test_app();

    // 规划钩子用 reply_if(仅匹配规划器 system 提示词「任务规划器」):步骤执行的
    // system 提示词不含该子串,故步骤调用落空到默认慢速流式回复,留出 stop 窗口
    let title = concat!(
        r#"[[reply_if:任务规划器|[{"name":"步骤一","goal":"慢慢写第一段"},{"name":"步骤二","goal":"慢慢写第二段"}] ]]"#,
        " 计划取消目标"
    );
    let id = create_task_with_mode(app, title, "plan").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let detail = wait_status(app, &id, "planned").await;
    assert_eq!(
        detail["task"]["plan"].as_array().unwrap().len(),
        2,
        "计划应拆为 2 步: {detail}"
    );

    let (status, json) =
        send_json(app, "POST", &format!("/api/tasks/{id}/approve"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "approve 应 200: {json}");

    // 轮询到续跑 running(步骤一执行中)再 stop
    let mut saw_running = false;
    for _ in 0..100 {
        let (_, json) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
        if json["task"]["status"].as_str() == Some("running") {
            saw_running = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(saw_running, "应观测到续跑 running 态再 stop");

    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{id}/stop"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "running 态 stop 应 200");
    let (st, _) = wait_terminal(app, &id).await;
    assert_eq!(st, "ended", "stop 后任务应 ended");

    // 宽限收尾后:步骤不得残留 pending/running(剩余 pending 应被置 error「任务已停止」)
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let (_, detail) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    for step in detail["task"]["plan"].as_array().unwrap() {
        let s = step["status"].as_str().unwrap_or("");
        assert!(
            s == "done" || s == "error",
            "终态后步骤不得残留 pending/running(取消兜底须置 error): {detail}"
        );
    }
}

/// 创建任务:未知 task_mode 严格拒绝(400),容错回退只用于 DB 读侧。
#[tokio::test]
async fn task_create_unknown_mode_rejected() {
    let app = test_app();
    let (status, json) = send_json(
        app,
        "POST",
        "/api/tasks",
        json!({ "title": "某任务", "task_mode": "bogus" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "未知模式应 400: {json}");
    assert!(
        json["error"]
            .as_str()
            .unwrap_or("")
            .contains("未知任务模式"),
        "错误文案应含「未知任务模式」: {json}"
    );
}

// ==================== 批次 4.3b:六模式(multi / custom / team) ====================

/// 轮询调用追踪直到满足条件或超时;返回最终 calls 数组
async fn wait_calls(app: &axum::Router, id: &str, pred: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    for _ in 0..50 {
        let (status, json) =
            send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
        assert_eq!(status, StatusCode::OK);
        let calls = json["calls"].as_array().cloned().unwrap_or_default();
        if pred(&calls) {
            return calls;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (_, json) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    panic!("任务 {id} 调用追踪未在超时内满足条件: {json:?}");
}

/// multi 模式:主 agent 经 [[tool:agentgo]] 钩子派子 agent——任务模式下子 agent 链路
/// 真实可用(task: 前缀虚拟 session 走内存覆盖层,子 agent 跑 run_tool_loop 白名单),
/// 终态 done;调用追踪含 phase=agent(主)与 phase=subagent(子)行,usage 聚合非零。
#[tokio::test]
async fn task_multi_mode_subagent_runs_to_done() {
    let app = test_app();

    // agentgo 参数内的 [[reply:...]] 用 \[ \] 转义:一是避免 mock tool 钩子在首个
    // "]]" 截断 JSON 参数;二是 agentgo 解析参数 JSON 时还原为真实钩子文本,
    // 使子 agent 的 user 消息(instruction)命中 mock reply 钩子。
    let title = r#"[[tool:agentgo {"tasks":[{"name":"子一","instruction":"\u005b\u005breply:子agent成果\u005d\u005d"}]}]] 主目标:调研并总结"#;
    let id = create_task_with_mode(app, title, "multi").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "multi 任务应完成,详情: {detail}");
    assert!(
        !detail["task"]["result"].as_str().unwrap_or("").is_empty(),
        "主 agent 应有最终产出: {detail}"
    );

    // 主 agent 调用追踪(phase=agent,工具循环一轮游)
    let (status, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        calls["calls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["phase"] == "agent" && c["status"] == "ok"),
        "应存在 phase=agent 且 status=ok 行: {calls:?}"
    );

    // 子 agent 后台执行,可能晚于主循环完成:轮询直至 phase=subagent 行出现
    let calls = wait_calls(app, &id, |cs| {
        cs.iter()
            .any(|c| c["phase"] == "subagent" && c["status"] == "ok")
    })
    .await;
    assert!(
        calls.iter().any(|c| c["phase"] == "subagent"),
        "子 agent 调用追踪应落库: {calls:?}"
    );

    // usage 落库(批次 4.3b 口径):主循环 phase=agent + 子 agent phase=subagent 均有 token
    let (_, detail) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    let usage = &detail["usage_total"];
    assert!(
        usage["prompt_tokens"].as_i64().unwrap_or(0) > 0
            && usage["completion_tokens"].as_i64().unwrap_or(0) > 0,
        "usage_total 应非零(主+子 agent 均已落库): {detail}"
    );
}

/// multi 取消:运行中 stop → ended(主循环走 mock 默认逐字流式回复,约 1s 窗口)。
#[tokio::test]
async fn task_multi_mode_stop_running() {
    let app = test_app();
    let id = create_task_with_mode(app, "慢慢写一篇长文,不要任何钩子", "multi").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    // 轮询到 running 再 stop
    let mut saw_running = false;
    for _ in 0..100 {
        let (_, json) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
        if json["task"]["status"].as_str() == Some("running") {
            saw_running = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(saw_running, "应观测到 running 态再 stop");

    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{id}/stop"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "running 态 stop 应 200");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "ended", "stop 后任务应 ended,详情: {detail}");
}

/// custom 模式:预置两步启用流程(起草→润色,均 direct+generates 无工具),
/// 逐步顺序执行,终态 done;结果 = 末个生成步骤产出;plan 步骤名 = flow 步骤名、
/// 状态全 done;调用追踪 phase=step 行数与步骤数对齐。
/// 同一测试内串行覆盖「未启用流程 → error 终态」路径:流程库是共享全局状态,
/// 拆成两个测试会因并行执行互相覆盖当前流程(实测竞态),故合并。
#[tokio::test]
async fn task_custom_mode_flow_steps_and_disabled_flow() {
    let app = test_app();

    // 预置启用流程(保存即设为当前选中;validate_flow 要求至少一个生成步骤)。
    // 首步「理解」为 generates=false 的内部规划步:2026-09-10 实测修复后,
    // 它用内部规划指令产出要点(不吐正文)、不计入最终成果,但作为下一步上下文。
    let flow = json!({
        "config": {
            "id": "",
            "name": "测试三步流程",
            "enabled": true,
            "steps": [
                {"id":"s0","name":"理解","enabled":true,"goal":"分析目标","action":"direct","generates":false,"system_prompt":"【本步指令·理解本步】只做内部规划"},
                {"id":"s1","name":"起草","enabled":true,"goal":"撰写草稿","action":"direct","generates":true,"system_prompt":"【本步指令·起草本步】直接输出草稿"},
                {"id":"s2","name":"润色","enabled":true,"goal":"润色上一版","action":"direct","generates":true,"system_prompt":"【本步指令·润色本步】输出润色后的最终版"}
            ]
        }
    });
    let (status, json) = send_json(app, "PUT", "/api/agent-flows", flow).await;
    assert_eq!(status, StatusCode::OK, "保存流程应 200: {json}");

    // reply_if 钩子按 system 命中步骤提示词(钩子语法内的 needle 出现不算命中,
    // 故任务目标上下文(untrusted 包裹进 system)里的钩子文本不会自命中)
    let title = "[[reply_if:理解本步|内部规划要点]][[reply_if:起草本步|草稿正文]][[reply_if:润色本步|润色成果]] 自定义任务目标";
    let id = create_task_with_mode(app, title, "custom").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "custom 任务应完成,详情: {detail}");
    assert_eq!(
        detail["task"]["result"].as_str().unwrap_or(""),
        "润色成果",
        "结果应为末个生成步骤产出: {detail}"
    );

    // plan 步骤 = flow 启用步骤,状态全 done(前端 custom 渲染契约)
    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 3, "plan 步骤数 = 流程启用步骤数: {detail}");
    assert_eq!(plan[0]["name"], "理解");
    assert_eq!(plan[1]["name"], "起草");
    assert_eq!(plan[2]["name"], "润色");
    for step in plan {
        assert_eq!(step["status"], "done", "步骤应 done: {detail}");
    }
    // 非生成步骤:产出带「(内部规划)」标注,且不进入最终成果
    let inner = plan[0]["result"].as_str().unwrap_or("");
    assert!(
        inner.contains("(内部规划)") && inner.contains("内部规划要点"),
        "generates=false 步骤应产内部规划要点: {detail}"
    );
    assert!(
        !detail["task"]["result"]
            .as_str()
            .unwrap_or("")
            .contains("内部规划要点"),
        "内部规划产出不得进入最终成果: {detail}"
    );

    // 调用追踪:phase=step 三行,step_index 0/1/2 对齐步骤顺序
    let (_, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    let step_calls: Vec<&Value> = calls["calls"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["phase"] == "step")
        .collect();
    assert_eq!(
        step_calls.len(),
        3,
        "phase=step 行数应与步骤数对齐: {calls:?}"
    );
    assert_eq!(step_calls[0]["step_index"], 0);
    assert_eq!(step_calls[1]["step_index"], 1);
    assert_eq!(step_calls[2]["step_index"], 2);

    // usage 落库:逐步骤一行,聚合非零
    let usage = &detail["usage_total"];
    assert!(
        usage["prompt_tokens"].as_i64().unwrap_or(0) > 0,
        "usage_total 应非零: {detail}"
    );

    // ===== 未启用流程 → error 终态(enabled=false 时 validate 放行但执行拒绝) =====
    let flow = json!({
        "config": {
            "id": "",
            "name": "未启用流程",
            "enabled": false,
            "steps": []
        }
    });
    let (status, json) = send_json(app, "PUT", "/api/agent-flows", flow).await;
    assert_eq!(status, StatusCode::OK, "保存未启用流程应 200: {json}");

    let id = create_task_with_mode(app, "某目标", "custom").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200(后台执行报错): {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "error", "未启用流程应 error 终态: {detail}");
    assert!(
        detail["task"]["error"]
            .as_str()
            .unwrap_or("")
            .contains("启用"),
        "错误文案应提示启用流程: {detail}"
    );

    // 还原内置流程
    let _ = send_json(
        app,
        "POST",
        "/api/agent-flows/select",
        json!({ "id": "builtin-coordination" }),
    )
    .await;
}

/// team 模式:自动拓扑(规划器拆 2 主 agent,步骤名带【主Agent-N】前缀)→ 两主并行
///(goal 内嵌 \[ \] 转义的 reply 钩子确定性产出)→ 审计通过 → 升华整合;
/// result 含「## 审计结论」段(前端拆卡契约);调用追踪覆盖 planner/agent/audit/summary。
#[tokio::test]
async fn task_team_mode_auto_topology_audit_summary() {
    let app = test_app();

    // 规划器拓扑 JSON 内的 [[reply:...]] 用 \[ \] 转义(mock reply_if 内容在首个
    // "]]" 截断;规划输出经 serde 解析时还原为真实钩子,主 agent user 消息命中)。
    let title = concat!(
        r#"[[reply_if:团队规划器|{"mains":["#,
        r#"{"name":"调研","goals":[{"name":"子目标一","goal":"\u005b\u005breply:主一产出\u005d\u005d 做调研"}]},"#,
        r#"{"name":"写作","goals":[{"name":"子目标二","goal":"\u005b\u005breply:主二产出\u005d\u005d 写正文"}]}]"#,
        r#" }]]"#,
        r#"[[reply_if:团队审计员|{"通过":true,"打回":[],"结论":"覆盖完整"}]]"#,
        "[[reply_if:任务汇总者|团队最终成果]]",
        " 团队总目标"
    );
    let id = create_task_with_mode(app, title, "team").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "team 任务应完成,详情: {detail}");

    // plan 步骤名带【主Agent-N】前缀(前端分工卡分组契约),状态全 done
    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 2, "两个主 agent 各 1 子目标: {detail}");
    assert_eq!(plan[0]["name"], "【主Agent-1】子目标一");
    assert_eq!(plan[1]["name"], "【主Agent-2】子目标二");
    for step in plan {
        assert_eq!(step["status"], "done", "步骤应 done: {detail}");
    }

    // result = 整合文本 + ## 审计结论 段(前端按此拆卡)
    let result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(
        result.contains("团队最终成果"),
        "应含升华整合文本: {result}"
    );
    assert!(result.contains("## 审计结论"), "应含审计结论段: {result}");
    assert!(result.contains("覆盖完整"), "应含审计文本: {result}");

    // 调用追踪:planner/agent(2 行,step_index 0/1)/audit/summary 全覆盖
    let (_, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    let calls = calls["calls"].as_array().unwrap();
    for phase in ["planner", "audit", "summary"] {
        assert!(
            calls
                .iter()
                .any(|c| c["phase"] == phase && c["status"] == "ok"),
            "应存在 phase={phase} 行: {calls:?}"
        );
    }
    let agent_calls: Vec<&Value> = calls.iter().filter(|c| c["phase"] == "agent").collect();
    assert_eq!(agent_calls.len(), 2, "两个主 agent 各一行: {calls:?}");
    assert!(
        agent_calls.iter().any(|c| c["step_index"] == 0)
            && agent_calls.iter().any(|c| c["step_index"] == 1),
        "主 agent step_index 应为主序号: {calls:?}"
    );

    // usage 落库:planner/agent/audit/summary 各阶段聚合非零
    let usage = &detail["usage_total"];
    assert!(
        usage["prompt_tokens"].as_i64().unwrap_or(0) > 0,
        "usage_total 应非零: {detail}"
    );
}

/// team 审计打回(2026-08 真实模型实测修复):首轮审计输出打回主 1 → 主 1 带补做
/// 指令重跑(仅 1 轮)→ 补做完成后追加一次终审(TEAM_FINAL_AUDIT_PROMPT,只产出
/// 结论文本、不再打回),最终 result 的「## 审计结论」段用终审结论。旧实现拼进的
/// 永远是首次审计的打回原文——实测任务 done 而 result 结尾仍挂「需要补全」。
/// 断言:补做恰好 1 轮(phase=agent step_index=0 两行)、首次审计 1 行、终审 1 行。
#[tokio::test]
async fn task_team_mode_audit_kickback_redoes_once() {
    let app = test_app();

    let title = concat!(
        r#"[[reply_if:团队规划器|{"mains":["#,
        r#"{"name":"调研","goals":[{"name":"子目标一","goal":"\u005b\u005breply:主一产出\u005d\u005d 做调研"}]},"#,
        r#"{"name":"写作","goals":[{"name":"子目标二","goal":"\u005b\u005breply:主二产出\u005d\u005d 写正文"}]}]"#,
        r#" }]]"#,
        r#"[[reply_if:团队审计员|{"通过":false,"打回":[{"main":1,"instruction":"请补充数据"}],"结论":"主一缺数据"}]]"#,
        r#"[[reply_if:团队终审员|{"通过":true,"结论":"终审结论:补做后已覆盖"}]]"#,
        "[[reply_if:任务汇总者|补做后最终成果]]",
        " 团队总目标"
    );
    let id = create_task_with_mode(app, title, "team").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "team 打回补做后应完成,详情: {detail}");

    let result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(result.contains("补做后最终成果"), "应含整合文本: {result}");
    // 「## 审计结论」段 = 终审口径,不得残留首次打回原文(前端按此拆卡)
    let audit_section = result.rsplit("## 审计结论").next().unwrap_or("");
    assert!(
        audit_section.contains("终审结论:补做后已覆盖"),
        "审计结论段应为终审结论: {result}"
    );
    assert!(
        !audit_section.contains("主一缺数据"),
        "审计结论段不得残留首次打回原文: {result}"
    );

    let (_, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    let calls = calls["calls"].as_array().unwrap();
    // 补做发生且仅 1 轮:主 1(step_index=0)的 phase=agent 行 = 首轮 + 补做轮 = 2
    let main1_calls: Vec<&Value> = calls
        .iter()
        .filter(|c| c["phase"] == "agent" && c["step_index"] == 0)
        .collect();
    assert_eq!(
        main1_calls.len(),
        2,
        "主 1 应恰好跑 2 次(首轮+补做): {calls:?}"
    );
    // 主 2 不受打回影响,仅 1 次
    let main2_calls: Vec<&Value> = calls
        .iter()
        .filter(|c| c["phase"] == "agent" && c["step_index"] == 1)
        .collect();
    assert_eq!(main2_calls.len(), 1, "主 2 不应补做: {calls:?}");
    // 打回仅 1 轮:首次审计 1 行;终审不再打回,单独 1 行(phase=final_audit)
    let audit_calls: Vec<&Value> = calls.iter().filter(|c| c["phase"] == "audit").collect();
    assert_eq!(audit_calls.len(), 1, "首次审计应仅 1 轮: {calls:?}");
    let final_calls: Vec<&Value> = calls
        .iter()
        .filter(|c| c["phase"] == "final_audit")
        .collect();
    assert_eq!(final_calls.len(), 1, "补做后应恰好追加 1 次终审: {calls:?}");
}

/// team 子目标粒度补做(实跑问题 2):审计点名「主 1 第 2 个子目标」→ 仅重跑该
/// 子目标(step_index=1),同主第 1 个子目标不重跑(step_index=0 仅 1 次)且不留
/// running;终审通过后任务 done。
#[tokio::test]
async fn task_team_mode_kickback_scoped_to_subgoal() {
    let app = test_app();

    let title = concat!(
        r#"[[reply_if:团队规划器|{"mains":["#,
        r#"{"name":"调研","goals":[{"name":"子一","goal":"\u005b\u005breply:子一成果\u005d\u005d 做调研一"},{"name":"子二","goal":"\u005b\u005breply:子二旧版\u005d\u005d 做调研二"}]},"#,
        r#"{"name":"写作","goals":[{"name":"子三","goal":"\u005b\u005breply:子三成果\u005d\u005d 写正文"}]}]"#,
        r#" }]]"#,
        // 审计只打回主 1 的第 2 个子目标
        r#"[[reply_if:团队审计员|{"通过":false,"打回":[{"main":1,"step":2,"instruction":"子二需补数据"}],"结论":"主一子二缺数据"}]]"#,
        r#"[[reply_if:团队终审员|{"通过":true,"结论":"补做后已覆盖"}]]"#,
        "[[reply_if:任务汇总者|子目标粒度补做后成果]]",
        " 团队子目标粒度目标"
    );
    let id = create_task_with_mode(app, title, "team").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "终审通过后应 done: {detail}");

    // 未被打回的步骤保持 done(不得因补做轮被置 running 后遗留)
    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan[0]["status"], "done", "子一应保持 done: {detail}");
    assert_eq!(plan[1]["status"], "done", "子二补做后应 done: {detail}");

    let (_, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    let calls = calls["calls"].as_array().unwrap();
    let count = |si: i64| {
        calls
            .iter()
            .filter(|c| c["phase"] == "agent" && c["step_index"] == si)
            .count()
    };
    assert_eq!(count(0), 1, "主 1 子一未被点名,不应重跑: {calls:?}");
    assert_eq!(
        count(1),
        2,
        "主 1 子二被点名,应首轮 + 补做共 2 次: {calls:?}"
    );
    assert_eq!(count(2), 1, "主 2 未被点名,不应重跑: {calls:?}");
}

/// team 补做轮 prior 种子(实跑问题 2):补做子目标必须看得到同主其他子目标成果
/// 与「自己被替换的旧版本」标注,否则重写时无参考、版本冲突原样复现。
/// 标题刻意保持精简:prompt_summary 对每条消息截 800 字符,标题越长 prior 段越容易被
/// 截掉;本测试用最短标题确保 prior 段完整落在摘要内(断言只针对 prior 内容)。
#[tokio::test]
async fn task_team_mode_kickback_prior_seed() {
    let app = test_app();

    let title = concat!(
        r#"[[reply_if:团队规划器|{"mains":["#,
        r#"{"name":"甲","goals":[{"name":"一","goal":"\u005b\u005breply:一果\u005d\u005d"},{"name":"二","goal":"\u005b\u005breply:二果\u005d\u005d"}]},"#,
        r#"{"name":"乙","goals":[{"name":"三","goal":"\u005b\u005breply:三果\u005d\u005d"}]}]}]]"#,
        r#"[[reply_if:团队审计员|{"通过":false,"打回":[{"main":1,"step":2,"instruction":"补"}],"结论":"缺"}]]"#,
        r#"[[reply_if:团队终审员|{"通过":true,"结论":"过"}]]"#,
        r#"[[reply_if:任务汇总者|成]]"#,
        " T"
    );
    let id = create_task_with_mode(app, title, "team").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "终审通过后应 done: {detail}");

    let (_, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    let calls = calls["calls"].as_array().unwrap();
    let redo: Vec<&str> = calls
        .iter()
        .filter(|c| c["phase"] == "agent" && c["step_index"] == 1)
        .filter_map(|c| c["prompt_summary"].as_str())
        .collect();
    let redo = *redo.get(1).expect("应有补做轮的第 2 次 agent 调用");
    // 同主未打回子目标的产出被种入(版本衔接;mock 首轮产出为「一」子目标的结果文本)
    assert!(
        redo.contains("「一」"),
        "补做轮应种入同主未打回子目标的产出: {redo}"
    );
    // 被打回子目标标注旧版本(明确本次是替换而非新增)
    assert!(
        redo.contains("旧版本,本次产出将替换它"),
        "补做轮应标注被打回子目标的旧版本: {redo}"
    );
    // 审计补做指令仍随行
    assert!(
        redo.contains("审计补做指令"),
        "补做轮应携带补做指令: {redo}"
    );
}

/// team 审计未通过且无可补做项 → partial(实跑问题 2):审计是质量闸门,
/// 「通过=false 但打回为空」不得静默判 done;审计结论段保留未通过说明。
#[tokio::test]
async fn task_team_mode_audit_fail_without_kickback_is_partial() {
    let app = test_app();

    let title = concat!(
        r#"[[reply_if:团队规划器|{"mains":["#,
        r#"{"name":"调研","goals":[{"name":"子一","goal":"\u005b\u005breply:主一产出\u005d\u005d 做调研"}]},"#,
        r#"{"name":"写作","goals":[{"name":"子二","goal":"\u005b\u005breply:主二产出\u005d\u005d 写正文"}]}]"#,
        r#" }]]"#,
        // 审计判定未通过,但打回为空(不可经补做修复)
        r#"[[reply_if:团队审计员|{"通过":false,"打回":[],"结论":"产出之间仍存在矛盾,无法通过补做修复"}]]"#,
        "[[reply_if:任务汇总者|未通过审计的成果]]",
        " 团队审计闸门目标"
    );
    let id = create_task_with_mode(app, title, "team").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(
        st, "partial",
        "审计未通过且无可补做项应为 partial(不得静默 done): {detail}"
    );
    let result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(result.contains("## 审计结论"), "应保留审计结论段: {result}");
    assert!(
        result.contains("仍存在矛盾"),
        "审计未通过说明应进入结论段: {result}"
    );
    // partial 须可解释(实跑问题 2 观感修复):error 字段写明未达标原因,前端状态行展示,
    // 不再只有与「执行中」同色的黄标而用户无从得知为何不是完成
    let err = detail["task"]["error"].as_str().unwrap_or("");
    assert!(
        err.contains("审计/终审未通过"),
        "partial 应写入可解释的 error 原因: {err}"
    );
}

/// team 同主多子目标(2026-08 真实模型实测修复):同一主 agent 的多个子目标逐个
/// 独立执行(每子目标一次 agent 调用),各写各自步骤的 result;同一主的多个子目标
/// 复用同一虚拟 session(task:{id}:main:{n})保持人设一致。旧实现对一个主只跑 1 次
/// run_agent_loop、同份产出写进它名下所有步骤——实测步骤 1/2 result 逐字节相同。
#[tokio::test]
async fn task_team_mode_same_main_goals_run_independently() {
    let app = test_app();

    let title = concat!(
        r#"[[reply_if:团队规划器|{"mains":["#,
        r#"{"name":"调研","goals":[{"name":"子一","goal":"\u005b\u005breply:子一成果\u005d\u005d 做调研一"},{"name":"子二","goal":"\u005b\u005breply:子二成果\u005d\u005d 做调研二"}]},"#,
        r#"{"name":"写作","goals":[{"name":"子三","goal":"\u005b\u005breply:子三成果\u005d\u005d 写正文"}]}]"#,
        r#" }]]"#,
        r#"[[reply_if:团队审计员|{"通过":true,"打回":[],"结论":"覆盖完整"}]]"#,
        "[[reply_if:任务汇总者|多子目标最终成果]]",
        " 团队多子目标总目标"
    );
    let id = create_task_with_mode(app, title, "team").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "team 任务应完成,详情: {detail}");

    // 3 步(主 1 领 2 子目标 + 主 2 领 1 子目标),各自独立产出且互不重复
    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 3, "主 1 两子目标 + 主 2 一子目标: {detail}");
    assert_eq!(plan[0]["name"], "【主Agent-1】子一");
    assert_eq!(plan[1]["name"], "【主Agent-1】子二");
    assert_eq!(plan[2]["name"], "【主Agent-2】子三");
    for step in plan {
        assert_eq!(step["status"], "done", "步骤应 done: {detail}");
    }
    let r0 = plan[0]["result"].as_str().unwrap_or("");
    let r1 = plan[1]["result"].as_str().unwrap_or("");
    let r2 = plan[2]["result"].as_str().unwrap_or("");
    assert!(r0.contains("子一成果"), "子目标一应得自身产出: {r0}");
    assert!(r1.contains("子二成果"), "子目标二应得自身产出: {r1}");
    assert!(r2.contains("子三成果"), "子目标三应得自身产出: {r2}");
    assert_ne!(
        r0, r1,
        "同主多子目标结果不得重复(实测逐字节相同回归): {detail}"
    );

    // 调用追踪:每个子目标一次 agent 调用(phase=agent,step_index=全局步骤下标)
    let (_, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    let calls = calls["calls"].as_array().unwrap();
    let agent_calls: Vec<&Value> = calls.iter().filter(|c| c["phase"] == "agent").collect();
    assert_eq!(agent_calls.len(), 3, "每个子目标一次 agent 调用: {calls:?}");
    for si in [0, 1, 2] {
        assert!(
            agent_calls.iter().any(|c| c["step_index"] == si),
            "应有 step_index={si} 的 agent 调用行: {calls:?}"
        );
    }
}

/// team 取消:规划器即时产出拓扑后主 agent 走 mock 默认逐字流式回复(约 1s 窗口),
/// running 中 stop → ended;终态后主 agent 步骤不残留 running/pending(无孤儿后台任务)。
#[tokio::test]
async fn task_team_mode_stop_running() {
    let app = test_app();

    // 主 agent 子目标不带 reply 钩子 → 主循环走默认慢速流式回复,留出 stop 窗口
    let title = concat!(
        r#"[[reply_if:团队规划器|{"mains":["#,
        r#"{"name":"甲","goals":[{"name":"子一","goal":"慢慢写第一段"}]},"#,
        r#"{"name":"乙","goals":[{"name":"子二","goal":"慢慢写第二段"}]}]"#,
        r#" }]]"#,
        " 团队取消目标"
    );
    let id = create_task_with_mode(app, title, "team").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    // 轮询到 running 且 plan 已产出再 stop
    let mut saw_running = false;
    for _ in 0..100 {
        let (_, json) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
        let running = json["task"]["status"].as_str() == Some("running");
        let planned = !json["task"]["plan"].as_array().unwrap().is_empty();
        if running && planned {
            saw_running = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(saw_running, "应观测到 running 态(含 plan)再 stop");

    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{id}/stop"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "running 态 stop 应 200");
    let (st, _) = wait_terminal(app, &id).await;
    assert_eq!(st, "ended", "stop 后任务应 ended");

    // 留一段宽限让后台主 agent 退出收尾,随后步骤不得残留 running/pending
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let (_, detail) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    for step in detail["task"]["plan"].as_array().unwrap() {
        let s = step["status"].as_str().unwrap_or("");
        assert!(
            s == "done" || s == "error",
            "终态后步骤不得残留 running/pending(孤儿后台任务): {detail}"
        );
    }
}

// ==================== 可观测性修复:finish_reason 透出 / multi 子 agent subtasks 可见 ====================

/// 问题①:solo 任务经 mock [[finish:length|...]] 钩子模拟 max_tokens 截断,
/// 任务照样 done(半截文本也是产出),但 GET /api/tasks/{id}/calls 的 agent 行
/// 必须带 finish_reason="length" 标记——修复前该列不存在,截断与正常收尾无从区分。
/// 钩子内容取到首个 "]]"(与 [[reply:]] 同截断语义),故内容内不得含 "]]"。
#[tokio::test]
async fn task_llm_call_finish_reason_marks_length_truncation() {
    let app = test_app();

    let title = "[[finish:length|这段成果在 max_tokens 处被截断,后半]] 截断观测目标";
    let id = create_task_with_mode(app, title, "solo").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "截断产出仍应 done(半截文本也是产出): {detail}");
    assert!(
        detail["task"]["result"]
            .as_str()
            .unwrap_or("")
            .contains("这段成果在 max_tokens 处被截断,后半"),
        "结果应为半截文本: {detail}"
    );

    let (status, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let calls = calls["calls"].as_array().unwrap();
    let agent = calls
        .iter()
        .find(|c| c["phase"] == "agent")
        .expect("应有 phase=agent 调用行");
    assert_eq!(
        agent["finish_reason"].as_str(),
        Some("length"),
        "截断调用行必须带 finish_reason=length 标记: {agent}"
    );
}

/// 问题①(对照组):正常结束的调用 finish_reason="stop"(mock 文本路径补发 Finish{stop});
/// 同时锁定「旧行读取兼容」——finish_reason 列对旧数据默认 ''(未知),新行必有值。
/// legacy 三段式(planner/step/summarize)全部调用行逐一断言。
#[tokio::test]
async fn task_llm_call_finish_reason_stop_on_normal_completion() {
    let app = test_app();

    let title = r#"[[reply:[{"name":"步骤一","goal":"写第一段"}] ]]"#;
    let id = create_task_with_mode(app, title, "legacy").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let (st, _) = wait_terminal(app, &id).await;
    assert_eq!(st, "done");

    let (status, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let calls = calls["calls"].as_array().unwrap();
    assert!(!calls.is_empty(), "应有调用追踪行");
    for c in calls {
        // 字段必须存在(旧客户端缺列时 serde 反序列化不得失败由服务端 String 保证)
        let reason = c["finish_reason"]
            .as_str()
            .unwrap_or_else(|| panic!("调用行缺 finish_reason 字段: {c}"));
        assert_eq!(
            reason,
            "stop",
            "正常完成的调用 finish_reason 应为 stop(phase={}): {c}",
            c["phase"].as_str().unwrap_or("?")
        );
    }
}

/// 问题⑤:multi 模式子 agent 记录走 task: 前缀内存覆盖层(不落 agent_subtasks 表),
/// 修复前 GET /api/tasks/{id} 的 subtasks 只查 task_subtasks 表 → 恒 [];
/// 修复后读路径合并覆盖层记录,子 agent 对 API 可见且字段映射正确。
/// 注意:覆盖层随进程存活,重启后 subtasks 仅 DB 行可见(既定取舍)。
#[tokio::test]
async fn task_multi_mode_subtasks_visible_from_memory_overlay() {
    let app = test_app();

    // 与 task_multi_mode_subagent_runs_to_done 同款钩子:主 agent 派一个子 agent,
    // 子 agent instruction 内嵌 \[ \] 转义的 reply 钩子,产出「子agent成果」。
    let title = r#"[[tool:agentgo {"tasks":[{"name":"子一","instruction":"\u005b\u005breply:子agent成果\u005d\u005d"}]}]] 主目标:调研并总结"#;
    let id = create_task_with_mode(app, title, "multi").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let (st, _) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "multi 任务应完成");

    // 子 agent 后台执行,可能晚于主循环完成:轮询详情直至覆盖层子任务出现且 done
    let mut subtasks: Vec<Value> = Vec::new();
    for _ in 0..50 {
        let (_, detail) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
        subtasks = detail["subtasks"].as_array().cloned().unwrap_or_default();
        if subtasks.iter().any(|s| s["status"] == "done") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        !subtasks.is_empty(),
        "multi 子 agent 记录应对 API 可见(修复前恒 [])"
    );
    let sub = subtasks
        .iter()
        .find(|s| s["name"] == "子一")
        .expect("应存在名为「子一」的子任务");
    assert_eq!(
        sub["task_id"].as_str(),
        Some(id.as_str()),
        "task_id 应回填所属任务: {sub}"
    );
    assert!(
        sub["instruction"]
            .as_str()
            .unwrap_or("")
            .contains("reply:子agent成果"),
        "instruction 应映射覆盖层记录原文: {sub}"
    );
    assert_eq!(sub["status"].as_str(), Some("done"), "子任务应 done: {sub}");
    assert!(
        sub["result"].as_str().unwrap_or("").contains("子agent成果"),
        "result 应携带子 agent 产出: {sub}"
    );
    assert!(sub["created_at"].as_str().is_some(), "缺 created_at: {sub}");
    assert!(sub["updated_at"].as_str().is_some(), "缺 updated_at: {sub}");
}

/// 问题⑤(回归):legacy 模式 subtasks 仍来自 task_subtasks 表(DB 路径),行为不变。
#[tokio::test]
async fn task_legacy_subtasks_still_from_db() {
    let app = test_app();

    let title = r#"[[reply:[{"name":"步骤一","goal":"写第一段"}] ]]"#;
    let id = create_task_with_mode(app, title, "legacy").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done");

    let subtasks = detail["subtasks"].as_array().unwrap();
    assert_eq!(subtasks.len(), 1, "legacy 每步一个 DB 子任务: {detail}");
    assert_eq!(subtasks[0]["name"].as_str(), Some("步骤一"));
    assert_eq!(subtasks[0]["status"].as_str(), Some("done"));
}

// ==================== 2026-08-31 实测修复:截断自愈 / 规划器侦察 / 步骤 error 文本 ====================

/// 问题①截断自愈:solo 任务经 mock [[tool_raw:]] 钩子模拟「max_tokens 把 tool_call
/// 参数 JSON 切成半截 + finish=length」(2026-08-31 deepseek 实测形态:calculator/
/// agentgo 参数截断直接判步骤 error)——run_tool_loop 单轮自愈:max_tokens 翻倍
/// (1024→2048)原样重发,第二轮预算充足返回完整 tool_call,工具正常执行,任务 done;
/// 调用追踪产生两次记录(被截断的 error 标注行 + 最终成功行)。
#[tokio::test]
async fn task_solo_truncated_tool_call_self_heals() {
    let app = test_app();

    // [[tool_raw:]] 钩子:max_tokens < 2048 返回左半 arguments(非法 JSON)+ finish=length;
    // ≥2048(自愈翻倍后)返回完整 tool_call。内容不得含 "]]"(首个 "]]" 截断语义)。
    let title = r#"[[tool_raw:calculator {"expression":"12*34"}]] 截断自愈目标"#;
    let id = create_task_with_mode(app, title, "solo").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "截断自愈后任务应完成,详情: {detail}");
    assert!(
        !detail["task"]["result"].as_str().unwrap_or("").is_empty(),
        "自愈后应有最终产出: {detail}"
    );

    // 调用追踪:phase=agent 恰好两行——被截断调用的 error 标注行 + 重发后的成功行
    let calls = wait_calls(app, &id, |cs| {
        cs.iter().filter(|c| c["phase"] == "agent").count() >= 2
    })
    .await;
    let agent_calls: Vec<&Value> = calls.iter().filter(|c| c["phase"] == "agent").collect();
    assert_eq!(
        agent_calls.len(),
        2,
        "应恰好两次调用记录(截断+重发): {calls:?}"
    );
    let heal_row = agent_calls
        .iter()
        .find(|c| c["status"] == "error")
        .unwrap_or_else(|| panic!("应有截断自愈标注行: {calls:?}"));
    let summary = heal_row["response_summary"].as_str().unwrap_or("");
    assert!(
        summary.contains("截断自愈") && summary.contains("工具参数 JSON 截断"),
        "截断行应标注自愈与成因: {summary}"
    );
    assert!(
        agent_calls.iter().any(|c| c["status"] == "ok"),
        "重发后应有成功行: {calls:?}"
    );
}

/// 问题①截断自愈失败路径:solo 任务经 [[finish:length|]] 钩子模拟「空正文 +
/// finish=length」且重发后依旧(同消息数组原样重发,钩子再次命中)——
/// 单轮只自愈一次,仍失败走原错误路径:任务 error 终态;
/// 错误文案含「返回空内容」与 finish_reason(问题③:错误原因可读落库);
/// 调用追踪两行(截断自愈标注行 + 空内容行)。
#[tokio::test]
async fn task_solo_empty_truncation_heals_once_then_errors() {
    let app = test_app();

    // [[finish:length|]]:内容为空串(标记 '|' 后即 "]]")→ 空正文 + finish=length
    let title = "[[finish:length|]] 空截断目标";
    let id = create_task_with_mode(app, title, "solo").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "error", "自愈重发仍空应为 error 终态,详情: {detail}");
    let error = detail["task"]["error"].as_str().unwrap_or("");
    assert!(
        error.contains("返回空内容") && error.contains("finish_reason=length"),
        "错误文案应含「返回空内容」与 finish_reason(问题③),实际: {error}"
    );

    // 调用追踪:截断自愈标注行(error)+ 空内容行(empty),共两行 phase=agent
    let calls = wait_calls(app, &id, |cs| {
        cs.iter().filter(|c| c["phase"] == "agent").count() >= 2
    })
    .await;
    let agent_calls: Vec<&Value> = calls.iter().filter(|c| c["phase"] == "agent").collect();
    assert_eq!(agent_calls.len(), 2, "应恰好两次调用记录: {calls:?}");
    assert!(
        agent_calls.iter().any(|c| c["status"] == "error"
            && c["response_summary"]
                .as_str()
                .unwrap_or("")
                .contains("截断自愈")),
        "应有截断自愈标注行: {calls:?}"
    );
    assert!(
        agent_calls.iter().any(|c| c["status"] == "empty"),
        "应有空内容行: {calls:?}"
    );
}

/// 问题①截断自愈(plan 批准续跑路径,实测原发形态):计划步骤 goal 内嵌转义的
/// [[tool_raw:]] 钩子 → approve 续跑步骤 agent 调用首轮半截 tool_call + finish=length
/// → 单轮自愈翻倍重发 → 步骤成功 done,任务终态 done。
#[tokio::test]
async fn task_plan_resume_step_truncation_self_heals() {
    let app = test_app();

    // 规划钩子用 reply_if(仅匹配规划器 system「任务规划器」):步骤执行 system 不含该子串;
    // 步骤 goal 内 \[ \] 转义的 [[tool_raw:...]] 经计划 JSON 解析还原为真实钩子
    // (内层 JSON 引号须 \" 转义;内容不得含 "]]")。
    let title = concat!(
        r#"[[reply_if:任务规划器|[{"name":"计算步","goal":"\u005b\u005btool_raw:calculator {\"expression\":\"12*34\"}\u005d\u005d 算一下乘法"}] ]]"#,
        "[[reply_if:任务汇总者|续跑截断自愈成果]]",
        " 续跑截断自愈目标"
    );
    let id = create_task_with_mode(app, title, "plan").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let detail = wait_status(app, &id, "planned").await;
    assert_eq!(
        detail["task"]["plan"].as_array().unwrap().len(),
        1,
        "计划应为 1 步: {detail}"
    );

    let (status, json) =
        send_json(app, "POST", &format!("/api/tasks/{id}/approve"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "approve 应 200: {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "截断自愈后续跑应完成,详情: {detail}");

    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan[0]["status"], "done", "截断步骤应自愈成功: {detail}");
    // 调用追踪:phase=agent 两行(截断 error 标注行 + 成功行)
    let calls = wait_calls(app, &id, |cs| {
        cs.iter().filter(|c| c["phase"] == "agent").count() >= 2
    })
    .await;
    let agent_calls: Vec<&Value> = calls.iter().filter(|c| c["phase"] == "agent").collect();
    assert_eq!(
        agent_calls.len(),
        2,
        "续跑步骤应产生两次调用记录: {calls:?}"
    );
    assert!(
        agent_calls.iter().any(|c| c["status"] == "error"
            && c["response_summary"]
                .as_str()
                .unwrap_or("")
                .contains("截断自愈")),
        "应有截断自愈标注行: {calls:?}"
    );
    let result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(
        result.contains("续跑截断自愈成果"),
        "最终结果应为汇总产出: {result}"
    );
}

/// 问题②规划器只读侦察:plan 模式 run 的规划阶段,模型可先调只读工具收集信息
/// (mock 首轮 [[tool:read]] 命中 → 侦察轮,工具结果回填),次轮产出计划 JSON
/// (has_tool_result 分支的回复钩子守卫,[[reply:计划]] 生效);
/// 侦察轮与计划轮均落 task_llm_calls(phase=planner),计划解析契约不变
///(进 planned 待批准,计划内容 = mock 返回的 JSON)。
#[tokio::test]
async fn task_plan_mode_planner_scout_collects_then_plans() {
    let app = test_app();

    // 首轮:[[tool:read]] 命中(read 在规划器只读白名单内)→ 侦察轮;
    // 次轮:工具结果回填后 has_tool_result 分支回复守卫命中 [[reply:计划]]。
    // read 的文件不存在 → 工具返回错误文本回填(侦察失败不致命,计划照常产出)。
    let title = r#"[[tool:read {"type":"file","name":"notes.md"}]] [[reply:[{"name":"侦察步","goal":"基于收集的信息写作"}] ]] 侦察规划目标"#;
    let id = create_task_with_mode(app, title, "plan").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let detail = wait_status(app, &id, "planned").await;
    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 1, "计划解析契约不变(1 步): {detail}");
    assert_eq!(
        plan[0]["name"], "侦察步",
        "计划内容应为 mock 回复的 JSON: {detail}"
    );

    // 侦察轮 + 计划轮均落调用追踪(phase=planner 恰好 2 行,均 status=ok)
    let calls = wait_calls(app, &id, |cs| {
        cs.iter().filter(|c| c["phase"] == "planner").count() >= 2
    })
    .await;
    let planner_calls: Vec<&Value> = calls.iter().filter(|c| c["phase"] == "planner").collect();
    assert_eq!(
        planner_calls.len(),
        2,
        "侦察轮与计划轮应各落一行 planner 调用: {calls:?}"
    );
    for c in &planner_calls {
        assert_eq!(c["status"], "ok", "侦察/计划轮均应成功: {c}");
    }
    // 计划轮(末行)的响应应为计划 JSON
    let last = planner_calls.last().unwrap();
    assert!(
        last["response_summary"]
            .as_str()
            .unwrap_or("")
            .contains("侦察步"),
        "计划轮应产出计划 JSON: {last}"
    );
    // F6(2026-09-10 实测修复):侦察轮(带工具调用)的 finish_reason 应为 tool_calls,
    // 此前因 sse_parser 在 tool_calls 完成时不发 Finish 块而落空串。
    let scout = planner_calls.first().unwrap();
    assert_eq!(
        scout["finish_reason"].as_str(),
        Some("tool_calls"),
        "带工具调用的侦察轮 finish_reason 应为 tool_calls: {scout}"
    );
}

/// 问题③步骤 error 文本落库(plan 批准续跑):步骤 agent 调用失败(mock [[fail:]]
/// 钩子模拟上游故障)→ 该步 status=error 且 result 携带失败原因(与 status 同一次
/// set_plan 落库),不得只置状态不留文本;步骤全败但汇总仍产出 → 任务终态 partial。
///
/// 剧本说明(2026-09-02 修正):approve 续跑的每步 user 消息 = 当前步骤段 + 「整体目标
/// 与已批准计划」上下文段,计划段携带全部步骤 goal(含他步还原后的钩子文本),而 mock
/// [[fail:]] 按 last_user 全文匹配且优先级最高——「一成一败」剧本不可构造(成功步必然
/// 被计划段的他步 fail 钩子命中)。故本用例设计为两步皆败、各携不同错误文案:
/// extract_fail_marker 取首个出现,而当前步骤段排在计划段之前,每步命中的恰是本步
/// 自己的钩子——可分别断言各步 result 携带本步失败原因(而非串味成他步的)。
///「一成一败 → partial」语义由 team 用例(task_team_mode_failed_main_step_carries_reason_text)
/// 覆盖:team 各主 user 不含他主子目标原文,天然免疫该污染。
#[tokio::test]
async fn task_plan_resume_step_error_carries_reason_text() {
    let app = test_app();

    // 两步 goal 内 \[ \] 转义的 [[fail:...]]:经计划 JSON 还原后在各自步骤 user 消息
    // 的当前步骤段命中(不经截断自愈:「模拟上游故障」非截断形态,直接走原错误路径)
    let title = concat!(
        r#"[[reply_if:任务规划器|[{"name":"步骤甲","goal":"\u005b\u005bfail:模拟上游故障甲\u005d\u005d 写一段"},"#,
        r#"{"name":"步骤乙","goal":"\u005b\u005bfail:模拟上游故障乙\u005d\u005d 写另一段"}] ]]"#,
        "[[reply_if:任务汇总者|续跑部分成果]]",
        " 步骤错误文本目标"
    );
    let id = create_task_with_mode(app, title, "plan").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    wait_status(app, &id, "planned").await;
    let (status, json) =
        send_json(app, "POST", &format!("/api/tasks/{id}/approve"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "approve 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(
        st, "partial",
        "步骤失败但汇总产出应为 partial,详情: {detail}"
    );
    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 2, "计划应为 2 步: {detail}");
    for (i, marker) in ["模拟上游故障甲", "模拟上游故障乙"].iter().enumerate() {
        assert_eq!(
            plan[i]["status"],
            "error",
            "步骤 {} 应 error: {detail}",
            i + 1
        );
        let err_text = plan[i]["result"].as_str().unwrap_or("");
        assert!(
            err_text.contains(marker),
            "步骤 {} error 文本应携带本步失败原因(问题③),实际: {err_text}",
            i + 1
        );
    }
    let result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(
        result.contains("续跑部分成果"),
        "最终结果应为汇总产出: {result}"
    );
}

/// 问题③(team 主循环):某主 agent 失败(mock [[fail:]] 钩子)→ 其步骤 status=error
/// 且 result 携带失败原因;其余主成功 → 任务终态 partial(审计/汇总照常)。
#[tokio::test]
async fn task_team_mode_failed_main_step_carries_reason_text() {
    let app = test_app();

    // 主 1 子目标含转义 [[fail:...]](首轮即失败),主 2 正常产出
    let title = concat!(
        r#"[[reply_if:团队规划器|{"mains":["#,
        r#"{"name":"调研","goals":[{"name":"失败子目标","goal":"\u005b\u005bfail:模拟主一故障\u005d\u005d 做调研"}]},"#,
        r#"{"name":"写作","goals":[{"name":"正常子目标","goal":"\u005b\u005breply:主二产出\u005d\u005d 写正文"}]}]"#,
        r#" }]]"#,
        r#"[[reply_if:团队审计员|{"通过":true,"打回":[],"结论":"主一缺失已知"}]]"#,
        "[[reply_if:任务汇总者|部分团队成果]]",
        " 团队步骤错误文本目标"
    );
    let id = create_task_with_mode(app, title, "team").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "partial", "主一失败主二成功应为 partial,详情: {detail}");
    let plan = detail["task"]["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 2, "两个主 agent 各 1 子目标: {detail}");
    assert_eq!(plan[0]["status"], "error", "主一步骤应 error: {detail}");
    let err_text = plan[0]["result"].as_str().unwrap_or("");
    assert!(
        err_text.contains("模拟主一故障"),
        "team 步骤 error 文本应携带失败原因(问题③),实际: {err_text}"
    );
    assert_eq!(plan[1]["status"], "done", "主二步骤应 done: {detail}");
    // partial 原因写入 error 字段(实跑问题 2 观感修复):前端状态行据此说明为何不是完成
    let task_err = detail["task"]["error"].as_str().unwrap_or("");
    assert!(
        task_err.contains("以下子目标执行失败") && task_err.contains("失败子目标"),
        "partial 应写入可解释的失败子目标清单: {task_err}"
    );
}

// ==================== R3b:工具循环历史回灌上限(集成) ====================

/// 上传「工具输出标记」角色卡(R3b 测试夹具),返回角色 id。
/// 标记放在 first_mes:read(type=character_prompt) 的工具输出含 first_mes,
/// 而 R3a 人设段(persona_style)只取 description/personality(/scenario/mes_example)
/// 四段、不取 first_mes —— 故 prompt_summary 中 R3B-TOOLOUT-MARK 只可能来自
/// tool 消息,可按出现次数精确断言「完整保留了几轮工具结果」。
async fn upload_tool_marked_character(app: &axum::Router) -> String {
    let card = json!({
        "name": "工具循环标记角色",
        "description": "工具循环测试角色(此段进人设,不带标记)",
        "personality": "稳重",
        "first_mes": "R3B-TOOLOUT-MARK 工具输出原文"
    });
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"tool-mark.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        card
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
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "上传角色卡应 2xx,实际 {status}: {json}"
    );
    json["character"]["id"]
        .as_str()
        .or_else(|| json["id"].as_str())
        .expect("上传响应应含角色 id")
        .to_string()
}

/// R3b:工具循环历史回灌上限(trim_tool_history 全链路集成)。
/// solo 任务经 mock [[tool_loop:read|6]] 钩子连续 6 轮工具调用(每轮回填
/// read(character_prompt) 输出,均含 R3B-TOOLOUT-MARK);keep_rounds 调至 1 →
/// 每轮生成前 trim_tool_history 把最老完整轮的 tool 结果原地摘要化,保底最近
/// 1 轮完整(模型必须看到最新工具结果才能续推)。
/// 断言点 = 调用追踪(task_llm_calls)phase=agent 行的 prompt_summary:
/// solo 整轮落一行,messages 为工具循环结束后的最终数组,即 trim 后的真实下发状态。
/// - 省略标记「(较早工具结果已省略」恰好 5 处(轮 1..=5 的 tool 结果已摘要化);
/// - 摘要文案含工具名 "read" 与「原输出约 N 字符」(可读性,集成测试按此口径,
///   与 trim.rs 的 TOOL_HISTORY_SUMMARY_PREFIX 注释约定一致);
/// - 最近轮(第 6 轮)工具结果完整保留:MARK 仍在;
/// - 最老轮原始输出已被移除:6 轮输出内容相同,MARK 恰好出现 1 次
///   = 仅剩最近轮一份,前 5 份原文均被摘要替换(不含最老轮原始输出的等价断言)。
/// 设置为共享全局态:改/还原在同一函数内串行(同 R3a 用例纪律);
/// 同文件其他工具钩子用例最多 1 轮工具循环(full<=1 时 trim 无操作),不受
/// keep_rounds=1 窗口影响。
#[tokio::test]
async fn task_solo_tool_history_trimmed_in_prompt_summary() {
    let app = test_app();
    let cid = upload_tool_marked_character(app).await;

    // keep_rounds = 1:6 轮工具循环只保留最近 1 轮完整,最老 5 轮摘要化
    let (status, json) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "tool_history_keep_rounds": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "写入 keep_rounds 应 200: {json}");

    let args = r#"{"queries":[{"type":"character_prompt"}]}"#;
    let detail =
        run_solo_task_with_character(app, &format!("[[tool_loop:read|6 {args}]]"), &cid).await;
    assert_eq!(
        detail["task"]["status"].as_str(),
        Some("done"),
        "6 轮工具循环后 solo 任务应完成: {detail}"
    );

    // 调用追踪:solo 整轮一行(phase=agent),prompt_summary = trim 后落库的消息数组
    let (status, calls) = send_json(
        app,
        "GET",
        &format!(
            "/api/tasks/{}/calls",
            detail["task"]["id"].as_str().unwrap()
        ),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let agent_rows: Vec<&Value> = calls["calls"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["phase"] == "agent")
        .collect();
    assert_eq!(
        agent_rows.len(),
        1,
        "solo 整轮应落一行 agent 追踪: {calls:?}"
    );
    let prompt = agent_rows[0]["prompt_summary"].as_str().unwrap_or("");
    assert!(!prompt.is_empty(), "prompt_summary 应非空: {calls:?}");

    const MARK: &str = "R3B-TOOLOUT-MARK";
    const SUMMARY_MARK: &str = "(较早工具结果已省略";
    let summary_count = prompt.matches(SUMMARY_MARK).count();
    assert_eq!(
        summary_count, 5,
        "轮 1..=5 的 tool 结果应全部摘要化(5 处省略标记): {prompt}"
    );
    assert!(
        prompt.contains(r#"工具 "read" 原输出约"#),
        "摘要文案应含工具名与原输出长度: {prompt}"
    );
    let mark_count = prompt.matches(MARK).count();
    assert_eq!(
        mark_count, 1,
        "最近轮(第 6 轮)工具结果应完整保留且仅此一份(最老轮原文已移除): {prompt}"
    );

    // 还原现场(共享 app;默认 4)
    let (status, json) = send_json(
        app,
        "PUT",
        "/api/settings",
        json!({ "tool_history_keep_rounds": 4 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "还原 keep_rounds 应 200: {json}");
}

// ==================== 批次 R2a:终态追加输入(followup) ====================

/// followup 全链路:solo 任务完成后追加指令 → 任务回 running 续跑(用户输入落
/// task_messages 后 spawn solo 续跑:原目标 + 上轮 result + 追加指令)→ 终态回 done;
/// result 追加「**追加 1:**」段且保留首轮成果;第二轮追加段序号为「追加 2」。
/// 实跑问题 1 后 messages 语义扩展为「完整对话记录」:创建落 user(kind=goal)、
/// 首轮产出落 assistant(kind=result)、每轮 followup 落 user+assistant(kind=followup),
/// 故一轮追加后 messages = 4 行、两轮追加累计 6 行。
/// 剧本说明:首轮 title 不带钩子(产出 = mock 默认回复),followup 指令带
/// [[reply:...]] 钩子 —— 续跑 user 消息(原目标 + 上轮 result + 追加指令)中
/// 首个 [[reply: 即本论钩子(mock reply 钩子按首个出现命中),产出确定。
#[tokio::test]
async fn task_followup_appends_to_result_and_messages() {
    let app = test_app();

    let id = create_task_with_mode(app, "写一段关于秋天的短文", "solo").await;
    // 实跑问题 1:创建即落 user 目标消息(对话记录首条)
    let (_, created) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    let created_msgs = created["messages"].as_array().expect("详情应含 messages");
    assert_eq!(created_msgs.len(), 1, "创建应落 1 条目标消息: {created}");
    assert_eq!(created_msgs[0]["role"], "user");
    assert_eq!(created_msgs[0]["kind"], "goal");
    assert_eq!(created_msgs[0]["content"], "写一段关于秋天的短文");

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "首轮应完成: {detail}");
    let first_result = detail["task"]["result"].as_str().unwrap_or("").to_string();
    assert!(
        first_result.contains("模拟回复"),
        "首轮产出应为 mock 默认回复: {first_result}"
    );
    // 实跑问题 1:首轮成果落 assistant 消息(kind=result),供前端逐轮气泡渲染
    let after_run = detail["messages"].as_array().expect("详情应含 messages");
    assert_eq!(
        after_run.len(),
        2,
        "首轮后应为「目标 + 成果」两行: {detail}"
    );
    assert_eq!(after_run[0]["kind"], "goal");
    assert_eq!(after_run[1]["role"], "assistant");
    assert_eq!(after_run[1]["kind"], "result");
    assert_eq!(
        after_run[1]["content"], first_result,
        "成果消息应存首轮产出全文: {detail}"
    );

    // ===== 第一轮追加 =====
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/followup"),
        json!({ "content": "[[reply:追加成果甲]] 再补充一点秋色" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "终态任务 followup 应 200: {json}");

    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "追加续跑后应回 done: {detail}");
    let result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(
        result.contains(&first_result),
        "result 应保留首轮成果: {result}"
    );
    assert!(
        result.contains("**追加 1:**"),
        "result 应含「追加 1」段标: {result}"
    );
    assert!(
        result.contains("再补充一点秋色"),
        "段标后应带指令概要: {result}"
    );
    assert!(
        result.contains("追加成果甲"),
        "result 应含本轮追加产出: {result}"
    );
    // 段序:首轮成果在前,追加段在后
    let first_idx = result.find(&first_result).unwrap_or(usize::MAX);
    let append_idx = result.find("**追加 1:**").unwrap_or(usize::MAX);
    assert!(first_idx < append_idx, "首轮成果应在追加段之前: {result}");

    // messages 四行:目标 + 首轮成果 + user 指令 + assistant 产出(created_at 升序)
    let messages = detail["messages"]
        .as_array()
        .expect("详情应含 messages 数组");
    assert_eq!(messages.len(), 4, "一轮追加应为 4 行消息: {messages:?}");
    assert_eq!(messages[0]["kind"], "goal");
    assert_eq!(messages[1]["kind"], "result");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["kind"], "followup");
    assert!(
        messages[2]["content"]
            .as_str()
            .unwrap_or("")
            .contains("再补充一点秋色"),
        "user 消息应存指令原文: {messages:?}"
    );
    assert_eq!(messages[3]["role"], "assistant");
    assert_eq!(messages[3]["kind"], "followup");
    assert_eq!(
        messages[3]["content"], "追加成果甲",
        "assistant 消息应存产出全文: {messages:?}"
    );

    // ===== 第二轮追加(序号递增;第二轮不带钩子,产出为 mock 默认回复) =====
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/followup"),
        json!({ "content": "再润色一遍结尾" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "第二轮 followup 应 200: {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "第二轮追加后应回 done: {detail}");
    let result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(
        result.contains("**追加 2:**"),
        "第二轮段标应为「追加 2」: {result}"
    );
    assert!(
        result.contains("再润色一遍结尾"),
        "第二轮段标应带本轮指令概要: {result}"
    );
    let messages = detail["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 6, "两轮追加应累计 6 行消息: {messages:?}");
    assert_eq!(messages[4]["role"], "user");
    assert_eq!(messages[4]["kind"], "followup");
    assert_eq!(messages[5]["role"], "assistant");

    // 删除任务级联清消息(FK ON DELETE CASCADE;DB 层断言见 migration.rs 专测)
    let status = send_empty(app, "DELETE", &format!("/api/tasks/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// followup 状态门禁:仅终态(done/partial/error/ended)可追加;
/// pending/running/planned 409(code=CONFLICT);不存在 404(NOT_FOUND);空指令 400(VALIDATION)。
/// ended 终态可追加(stop 后仍可继续对话)。
#[tokio::test]
async fn task_followup_state_gate() {
    let app = test_app();

    // 404:任务不存在
    let (status, json) = send_json(
        app,
        "POST",
        "/api/tasks/不存在/followup",
        json!({ "content": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "不存在应 404: {json}");
    assert_eq!(json["code"], "NOT_FOUND");

    // 400:空指令(校验先于状态门禁)
    let id = create_task_with_mode(app, "门禁测试目标", "solo").await;
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/followup"),
        json!({ "content": "   " }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "空指令应 400: {json}");
    assert_eq!(json["code"], "VALIDATION");

    // 400:非法 mode(严格解析,与 task_mode 同口径;前置于状态门禁)
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/followup"),
        json!({ "content": "补充", "mode": "rewrite" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "非法 mode 应 400: {json}");
    assert_eq!(json["code"], "VALIDATION");

    // 409:pending(未启动)
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/followup"),
        json!({ "content": "补充" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "pending 应 409: {json}");
    assert_eq!(json["code"], "CONFLICT");

    // 409:running(默认慢速回复留出窗口)
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let mut saw_running = false;
    for _ in 0..100 {
        let (_, detail) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
        if detail["task"]["status"].as_str() == Some("running") {
            saw_running = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(saw_running, "应观测到 running 态");
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/followup"),
        json!({ "content": "补充" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "running 应 409: {json}");
    assert_eq!(json["code"], "CONFLICT");

    // stop → ended 终态可追加(stop 后仍可继续对话;产出 = mock 默认回复)
    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{id}/stop"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (st, _) = wait_terminal(app, &id).await;
    assert_eq!(st, "ended", "stop 后应 ended");
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/followup"),
        json!({ "content": "结束后继续写" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "ended 终态应可追加: {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "ended 追加续跑后应回 done: {detail}");
    assert!(
        detail["task"]["result"]
            .as_str()
            .unwrap_or("")
            .contains("**追加 1:**"),
        "ended 追加应产出追加段: {detail}"
    );

    // 409:planned(plan 模式待批准)
    let title = r#"[[reply:[{"name":"步骤一","goal":"写第一段"}] ]]"#;
    let pid = create_task_with_mode(app, title, "plan").await;
    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{pid}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    wait_status(app, &pid, "planned").await;
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{pid}/followup"),
        json!({ "content": "补充" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "planned 应 409(待批准请走批准/对话): {json}"
    );
    assert_eq!(json["code"], "CONFLICT");
}

/// partial 历史不被抹平:原 partial 任务(含失败步骤)追加续跑完成后仍回 partial
///(失败步骤历史仍存),追加段照常入 result;done/error/ended 追加后回 done
///(done/ended 路径见上两例)。
#[tokio::test]
async fn task_followup_keeps_partial_status() {
    let app = test_app();

    // legacy 两步一成一败 → partial。规划钩子用 reply_if(仅匹配规划器 system
    // 「任务规划器」):followup 续跑轮的 system 是「任务执行者」不命中它,本轮
    // 唯一命中的是追加指令里的 [[reply:]] —— 若用无条件 [[reply:]] 出计划,
    // 续跑 user 消息(含 title 原文)会先命中旧钩子,追加产出被污染(与
    // task_plan_resume_step_error_carries_reason_text 剧本说明同款规避)。
    let title = concat!(
        r#"[[reply_if:任务规划器|[{"name":"成功步","goal":"写一段正常内容"},"#,
        r#"{"name":"失败步","goal":"\u005b\u005bfail:上游抖动\u005d\u005d"}] ]]"#,
        " partial 追加目标"
    );
    let id = create_task_with_mode(app, title, "legacy").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let (st, _) = wait_terminal(app, &id).await;
    assert_eq!(st, "partial", "前置应为 partial");

    // 追加(钩子续跑产出确定文本;追加期间 running,完成回 partial)
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/followup"),
        json!({ "content": "[[reply:partial追加成果]] 把失败步补上" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "partial 应可追加: {json}");
    let (st, detail) = wait_terminal(app, &id).await;
    assert_eq!(
        st, "partial",
        "含失败步骤历史的任务追加后仍应为 partial: {detail}"
    );
    let result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(
        result.contains("**追加 1:**"),
        "result 应含追加段: {result}"
    );
    assert!(
        result.contains("partial追加成果"),
        "result 应含追加产出: {result}"
    );
    let messages = detail["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        4,
        "目标 + 首轮成果 + 一轮追加两行 = 4 行: {messages:?}"
    );
}

// ==================== 批次 R2b:批准环节规划对话(plan-chat) ====================

/// plan-chat 修订全链路:planned 态用户反馈 → 规划器携带(原始目标 + 当前计划
/// JSON + plan_chat 对话历史)重新产出修订计划 → set_plan 替换 + planned 态
/// result 计划清单文本同步刷新 + assistant 修订说明落 task_messages →
/// 任务保持 planned;第二轮反馈时历史随提示词携带(经 GET calls 的
/// prompt_summary 断言历史出现在规划器入参)。
/// mock 剧本:title 双 reply_if 钩子按序区分轮次(reply_if 只匹配 system,
/// title 在 user 消息,钩子自身出现不算命中):
/// 修订轮 system = PLANNER_PROMPT + 修订指引段(含独特词「计划修订指引」)
///   → 钩子1 命中 → 计划B;
/// 首轮规划 system 仅 PLANNER_PROMPT(含「任务规划器」,不含修订指引)
///   → 钩子1 跳过、钩子2 命中 → 计划A。
#[tokio::test]
async fn task_plan_chat_revises_plan_and_keeps_planned() {
    let app = test_app();

    let title = concat!(
        r#"[[reply_if:计划修订指引|[{"name":"修订步骤甲","goal":"修订目标甲"}] ]]"#,
        r#"[[reply_if:任务规划器|[{"name":"原步骤一","goal":"原目标一"}] ]]"#,
        " 请帮我写一份调研报告"
    );
    let id = create_task_with_mode(app, title, "plan").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    wait_status(app, &id, "planned").await;

    // 首轮:计划A 落库,planned 态 result = 计划清单文本,messages 为空
    let (_, detail) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    assert_eq!(detail["task"]["status"], "planned");
    assert_eq!(
        detail["task"]["plan"][0]["name"], "原步骤一",
        "首轮应为计划A: {detail}"
    );
    assert!(
        detail["task"]["result"]
            .as_str()
            .unwrap_or("")
            .contains("计划已产出"),
        "planned 态 result 应为计划清单: {detail}"
    );
    assert_eq!(
        detail["messages"].as_array().map(|m| m.len()),
        Some(1),
        "对话前应只有创建时落的 1 条目标消息: {detail}"
    );

    // ===== 第一轮反馈:修订为计划B,任务保持 planned =====
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/plan-chat"),
        json!({ "message": "把步骤换成先做竞品调研" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "planned 态 plan-chat 应 200: {json}"
    );
    assert_eq!(
        json["plan"][0]["name"], "修订步骤甲",
        "响应应携修订后计划: {json}"
    );

    let (_, detail) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    assert_eq!(
        detail["task"]["status"], "planned",
        "修订后任务应保持 planned: {detail}"
    );
    assert_eq!(
        detail["task"]["plan"][0]["name"], "修订步骤甲",
        "计划应被替换: {detail}"
    );
    assert_eq!(detail["task"]["plan"][0]["goal"], "修订目标甲");
    let result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(
        result.contains("计划已修订"),
        "planned 态清单文本应随修订刷新: {result}"
    );
    assert!(
        result.contains("修订目标甲"),
        "清单应含新步骤目标: {result}"
    );

    // messages 三行:目标 + user 反馈 + assistant 修订说明(后两行 kind=plan_chat)
    let messages = detail["messages"].as_array().expect("详情应含 messages");
    assert_eq!(
        messages.len(),
        3,
        "目标 + 一轮对话两行 = 3 行: {messages:?}"
    );
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["kind"], "goal");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["kind"], "plan_chat");
    assert_eq!(messages[1]["content"], "把步骤换成先做竞品调研");
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["kind"], "plan_chat");
    let note = messages[2]["content"].as_str().unwrap_or("");
    assert!(
        note.contains("计划已修订") && note.contains("修订步骤甲"),
        "assistant 说明应含步数与步骤名: {note}"
    );

    // ===== 第二轮反馈:历史随提示词携带(prompt_summary 可见首轮反馈与本轮反馈) =====
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/plan-chat"),
        json!({ "message": "粒度再细一点" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "第二轮 plan-chat 应 200: {json}");
    let (_, detail) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    assert_eq!(
        detail["task"]["status"], "planned",
        "多轮修订后仍应 planned: {detail}"
    );
    let messages = detail["messages"].as_array().unwrap();
    // 目标(goal)+ 两轮 plan_chat 各两行 = 5 行
    assert_eq!(
        messages.len(),
        5,
        "目标 + 两轮对话应累计 5 行: {messages:?}"
    );
    assert_eq!(messages[0]["kind"], "goal");
    assert_eq!(messages[3]["role"], "user");
    assert_eq!(messages[3]["content"], "粒度再细一点");

    // 历史携带:第二轮规划器调用的 prompt_summary 应含首轮反馈与本轮反馈
    let (status, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let rows = calls["calls"].as_array().expect("calls 应为数组");
    let planner_rows: Vec<&Value> = rows
        .iter()
        .filter(|r| r["phase"].as_str() == Some("planner"))
        .collect();
    let last = planner_rows.last().expect("应有 planner 调用行");
    let prompt = last["prompt_summary"].as_str().unwrap_or("");
    assert!(
        prompt.contains("把步骤换成先做竞品调研"),
        "第二轮规划入参应携带首轮反馈(历史): {prompt}"
    );
    assert!(
        prompt.contains("粒度再细一点"),
        "第二轮规划入参应含本轮反馈: {prompt}"
    );
}

/// plan-chat 状态门禁:仅 planned 态可用;不存在 404(NOT_FOUND)、
/// 空反馈 400(VALIDATION,先于门禁)、pending/done 均 409(CONFLICT)。
#[tokio::test]
async fn task_plan_chat_state_gate() {
    let app = test_app();

    // 404:任务不存在
    let (status, json) = send_json(
        app,
        "POST",
        "/api/tasks/不存在/plan-chat",
        json!({ "message": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "不存在应 404: {json}");
    assert_eq!(json["code"], "NOT_FOUND");

    // 400:空反馈(校验先于状态门禁;用 pending 任务验证校验顺序)
    let id = create_task_with_mode(app, "门禁测试目标", "solo").await;
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/plan-chat"),
        json!({ "message": "  " }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "空反馈应 400: {json}");
    assert_eq!(json["code"], "VALIDATION");

    // 409:pending(未启动)
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/plan-chat"),
        json!({ "message": "改下计划" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "pending 应 409: {json}");
    assert_eq!(json["code"], "CONFLICT");

    // 409:done(终态非 planned;终态追加请走 followup)
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let (st, _) = wait_terminal(app, &id).await;
    assert_eq!(st, "done");
    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{id}/plan-chat"),
        json!({ "message": "改下计划" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "done 应 409(规划对话仅 planned 态): {json}"
    );
    assert_eq!(json["code"], "CONFLICT");
}

/// 2026-09-10 六模式实跑修复(F2):任务模式提示词注入默认隔离。
/// 角色扮演的 prompt_floors.json 通常承载「1200 字/第三人称/禁词表」等文章要求,
/// 注入任务 system 会与任务目标冲突(实测 legacy 追加「压缩到 200 字」后结果反而变长)。
/// 本用例:注入配置含唯一哨兵文本 → 默认(隔离)时任务 system 不含哨兵;
/// 显式开启 task_prompt_inject_enabled 后哨兵出现;最后恢复默认(隔离)。
#[tokio::test]
async fn task_prompt_inject_isolated_by_default() {
    let app = test_app();
    const NEEDLE: &str = "注入哨兵ABCXYZ";

    // 注入配置设唯一哨兵(simple 模式;perspective 会进 system_inject_text;
    // prompt_inject 为全局共享,用例末恢复默认)
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({
            "config": {
                "mode": "simple",
                "simple": {
                    "word_count_enabled": false,
                    "word_count": 1200,
                    "paraphrase_enabled": false,
                    "dialogue_enabled": false,
                    "perspective_enabled": true,
                    "perspective": NEEDLE,
                    "banned_words_enabled": false,
                    "banned_prompt": "",
                    "banned_words": []
                },
                "floors": []
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "设置注入配置应 200");

    // 关闭继承(默认)→ solo 任务 system 不含哨兵
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/settings?mode=task",
        json!({ "task_prompt_inject_enabled": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "关闭注入继承应 200");

    let id = create_task_with_mode(app, "写一句关于秋天的话", "solo").await;
    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (_st, _) = wait_terminal(app, &id).await;
    let (_, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    let off_text = calls.to_string();
    assert!(
        !off_text.contains(NEEDLE),
        "默认隔离时任务调用提示词不得含注入哨兵: {off_text}"
    );

    // 显式开启继承 → system 含哨兵
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/settings?mode=task",
        json!({ "task_prompt_inject_enabled": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "开启注入继承应 200");

    let id = create_task_with_mode(app, "写一句关于春天的话", "solo").await;
    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (_st, _) = wait_terminal(app, &id).await;
    let (_, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    assert!(
        calls.to_string().contains(NEEDLE),
        "显式开启后任务调用提示词应含注入哨兵: {calls}"
    );

    // 恢复默认:关闭继承 + 清空注入内容(避免影响后续并行用例)
    let _ = send_json(
        app,
        "PUT",
        "/api/settings?mode=task",
        json!({ "task_prompt_inject_enabled": false }),
    )
    .await;
    let _ = send_json(
        app,
        "PUT",
        "/api/prompt-inject",
        json!({
            "config": {
                "mode": "simple",
                "simple": {
                    "word_count_enabled": false,
                    "word_count": 1200,
                    "paraphrase_enabled": false,
                    "dialogue_enabled": false,
                    "perspective_enabled": false,
                    "perspective": "第三人称",
                    "banned_words_enabled": false,
                    "banned_prompt": "",
                    "banned_words": []
                },
                "floors": []
            }
        }),
    )
    .await;
}

/// 2026-09-10 六模式实跑修复(F3):调用记账口径统一。
/// 验收标准:task_llm_calls 各行 token 求和 == 详情接口 usage_total(逐字段)。
/// 覆盖两个此前漏计的缺口:规划器侦察轮、截断自愈行。
#[tokio::test]
async fn task_usage_total_matches_call_rows_with_scout_and_heal() {
    let app = test_app();

    // 场景 A:规划器侦察轮(plan 模式,plan_scout_loop 记侦察轮 usage)
    let title = r#"[[tool:read {"type":"file","name":"notes.md"}]] [[reply:[{"name":"侦察步","goal":"基于收集的信息写作"}] ]] 侦察记账目标"#;
    let id = create_task_with_mode(app, title, "plan").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let _ = wait_status(app, &id, "planned").await;
    let (_, detail) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    let (_, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    let planner_rows = calls["calls"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["phase"] == "planner")
        .count();
    assert!(planner_rows >= 2, "应含侦察轮与计划轮: {calls:?}");
    assert_usage_matches_calls(&detail, &calls);

    // 场景 B:截断自愈(solo 工具循环单轮被截断 → 翻倍重发;heal 行记 usage)
    let title = r#"[[tool_raw:read {"type":"file","name":"notes.md"}]] 截断自愈记账目标"#;
    let id = create_task_with_mode(app, title, "solo").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let (_st, _) = wait_terminal(app, &id).await;
    let (_, detail) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
    let (_, calls) = send_json(app, "GET", &format!("/api/tasks/{id}/calls"), json!({})).await;
    assert!(
        calls["calls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["response_summary"]
                .as_str()
                .unwrap_or("")
                .contains("截断自愈")),
        "应有截断自愈标注行: {calls:?}"
    );
    assert_usage_matches_calls(&detail, &calls);
}

/// 断言 usage_total 与 task_llm_calls 逐行 token 求和一致(修复后为强不变量)。
fn assert_usage_matches_calls(detail: &Value, calls: &Value) {
    let sum = |field: &str| -> i64 {
        calls["calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c[field].as_i64().unwrap_or(0))
            .sum()
    };
    let usage = &detail["usage_total"];
    assert_eq!(
        usage["prompt_tokens"].as_i64().unwrap_or(-1),
        sum("prompt_tokens"),
        "usage_total.prompt_tokens 应等于调用明细求和: usage={usage} calls={calls}"
    );
    assert_eq!(
        usage["completion_tokens"].as_i64().unwrap_or(-1),
        sum("completion_tokens"),
        "usage_total.completion_tokens 应等于调用明细求和: usage={usage} calls={calls}"
    );
}

/// 2026-09-10 六模式实跑修复(F5):followup replace 模式整体替换结果。
/// 背景:append 语义下「压缩到 200 字」这类指令无法表达——新产出以追加段附加,
/// 原文仍在(实测 result 反而变长)。replace 模式用新产出整体替换 result(段标

/// 2026-09-10 六模式实跑修复(F5):followup replace 模式整体替换结果。
/// 背景:append 语义下「压缩到 200 字」这类指令无法表达——新产出以追加段附加,
/// 原文仍在(实测 result 反而变长)。replace 模式用新产出整体替换 result(段标
/// 「修订 N」),旧内容不保留;messages 仍按 user/assistant 各一行落库。
/// 用两个独立任务分别验证 replace 与默认 append,避免 title 内的回复钩子
/// 在多轮之间互相污染(mock [[reply:]] 按最后一条 user 消息中首个命中)。
#[tokio::test]
async fn task_followup_replace_mode_replaces_result() {
    let app = test_app();

    // ===== 任务 A:replace 整体替换 =====
    // 首轮无钩子 → mock 默认回复(内含目标前 60 字,作为「旧内容」标记)
    let a = create_task_with_mode(app, "独有标记QAQ 写一段内容", "solo").await;
    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{a}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let (st, detail) = wait_terminal(app, &a).await;
    assert_eq!(st, "done");
    assert!(
        detail["task"]["result"]
            .as_str()
            .unwrap_or("")
            .contains("独有标记QAQ"),
        "首轮应含旧内容标记: {detail}"
    );

    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{a}/followup"),
        json!({ "content": "[[reply:替换后成果]]", "mode": "replace" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "replace 追加应 200: {json}");
    let (st, detail) = wait_terminal(app, &a).await;
    assert_eq!(st, "done", "replace 完成后应为 done: {detail}");
    let result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(
        result.contains("**修订 1:**"),
        "replace 模式应用「修订 N」段标: {result}"
    );
    assert!(
        result.contains("替换后成果"),
        "result 应含新版产出: {result}"
    );
    assert!(
        !result.contains("独有标记QAQ"),
        "replace 模式旧结果不得保留: {result}"
    );
    let messages = detail["messages"].as_array().unwrap();
    // 目标 + 首轮成果 + 一轮追加两行 = 4 行
    assert_eq!(
        messages.len(),
        4,
        "目标 + 首轮成果 + 一轮追加两行 = 4 行: {messages:?}"
    );
    assert_eq!(messages[0]["kind"], "goal");
    assert_eq!(messages[1]["kind"], "result");

    // ===== 任务 B:缺省 mode = append,累积而非替换 =====
    let b = create_task_with_mode(app, "第二标记QBQ 写点东西", "solo").await;
    let (status, _) = send_json(app, "POST", &format!("/api/tasks/{b}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (st, _) = wait_terminal(app, &b).await;
    assert_eq!(st, "done");

    let (status, json) = send_json(
        app,
        "POST",
        &format!("/api/tasks/{b}/followup"),
        json!({ "content": "[[reply:追加内容段]]" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "缺省 mode 追加应 200: {json}");
    let (st, detail) = wait_terminal(app, &b).await;
    assert_eq!(st, "done");
    let result = detail["task"]["result"].as_str().unwrap_or("");
    assert!(
        result.contains("**追加 1:**") && result.contains("第二标记QBQ"),
        "缺省 append 应保留旧结果并附加段: {result}"
    );
    assert!(
        result.contains("追加内容段"),
        "append 应含本轮产出: {result}"
    );
}

/// 审计 B(端到端):multi 子 agent 被 max_tokens 截断 → 子任务 status=error、
/// error 含「截断」、result 保留半截正文。修复前只要正文非空就静默 done,是假成功主通道。
/// 钩子内容取到首个 "]]"(与 [[reply:]] 同截断语义),故内容内不得含 "]]"。
#[tokio::test]
async fn task_multi_mode_subagent_truncation_marks_error() {
    let app = test_app();

    // 子 agent instruction 内嵌 \[ \] 转义的 finish 钩子:一是避免主 agent 的 tool 钩子
    // 在首个 "]]" 截断参数 JSON;二是 agentgo 解析参数时还原为真实钩子文本,使子 agent
    // 的 user 消息(instruction)命中 mock finish 钩子 → finish_reason=length + 半截正文。
    let title = r#"[[tool:agentgo {"tasks":[{"name":"截断子","instruction":"\u005b\u005bfinish:length|半截成果在前\u005d\u005d"}]}]] 主目标:调研"#;
    let id = create_task_with_mode(app, title, "multi").await;

    let (status, json) = send_json(app, "POST", &format!("/api/tasks/{id}/run"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "run 应 200: {json}");
    let (st, _) = wait_terminal(app, &id).await;
    assert_eq!(st, "done", "主任务应完成(子任务失败不阻塞主循环)");

    // 子 agent 后台执行,可能晚于主循环:轮询详情直至子任务落到终态
    let mut sub: Option<Value> = None;
    for _ in 0..50 {
        let (_, detail) = send_json(app, "GET", &format!("/api/tasks/{id}"), json!({})).await;
        let subs = detail["subtasks"].as_array().cloned().unwrap_or_default();
        if let Some(s) = subs
            .iter()
            .find(|s| s["name"] == "截断子")
            .filter(|s| s["status"] != "pending" && s["status"] != "running")
        {
            sub = Some(s.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let sub = sub.expect("截断子任务应出现在任务详情");
    assert_eq!(
        sub["status"].as_str(),
        Some("error"),
        "截断应记 error 而非 done: {sub}"
    );
    assert!(
        sub["error"].as_str().unwrap_or("").contains("截断"),
        "error 应含截断定性: {sub}"
    );
    assert!(
        sub["result"]
            .as_str()
            .unwrap_or("")
            .contains("半截成果在前"),
        "被截断的正文必须保留在 result: {sub}"
    );
}
