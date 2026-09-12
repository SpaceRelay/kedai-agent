// 用户脚本(ScriptTree)API 集成测试:GET/PUT /api/scripts/tree(global / character)
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
    APP.get_or_init(|| build_test_app().expect("构建测试应用失败"))
}

/// 脚本测试共享同一 app/DB,须串行执行避免相互干扰
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

/// 上传角色卡(multipart,与 world_books 测试同款)
async fn upload_character(app: &axum::Router, card: &Value) -> String {
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"角色.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        card
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/characters/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let char: Value = serde_json::from_slice(&bytes).unwrap();
    char["id"].as_str().unwrap().to_string()
}

fn sample_tree() -> Value {
    json!([
        {
            "type": "script", "enabled": true, "name": "自动回复",
            "id": "a1", "content": "console.log('hi')", "info": "示例",
            "button": { "enabled": true, "buttons": [ { "name": "点我", "visible": true } ] },
            "data": { "count": 1 }, "export_with": { "data": true, "button": true }
        },
        {
            "type": "folder", "enabled": true, "name": "工具集",
            "id": "f1", "icon": "fa-solid fa-folder", "color": "",
            "scripts": [ { "type": "script", "enabled": true, "name": "子脚本", "id": "a2", "content": "1+1" } ]
        }
    ])
}

/// 全局脚本:PUT 保存(含默认字段补齐)→ GET 回读一致;非法输入拒绝
#[tokio::test]
async fn global_scripts_crud() {
    let _guard = test_lock().await;
    let app = test_app();

    // 清场为确定基线(共享全局存储,先 PUT 空数组再断言)
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/scripts/tree?scope=global",
        json!({ "trees": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, got) = send_json(app, "GET", "/api/scripts/tree?scope=global", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["trees"], json!([]));

    // 保存后回读:补默认字段(button/data/export_with)
    let (status, saved) = send_json(
        app,
        "PUT",
        "/api/scripts/tree?scope=global",
        json!({ "trees": sample_tree() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["ok"], json!(true));

    let (_, got) = send_json(app, "GET", "/api/scripts/tree?scope=global", json!({})).await;
    let trees = got["trees"].as_array().unwrap();
    assert_eq!(trees.len(), 2);
    assert_eq!(trees[0]["name"], json!("自动回复"));
    assert_eq!(trees[0]["button"]["enabled"], json!(true));
    assert_eq!(trees[0]["data"]["count"], json!(1));
    assert_eq!(trees[0]["export_with"]["data"], json!(true));
    assert_eq!(trees[1]["type"], json!("folder"));
    assert_eq!(trees[1]["scripts"][0]["name"], json!("子脚本"));
}

/// 全局脚本:非法输入(非数组顶层 / 未知 type)拒绝且不覆盖既有数据
#[tokio::test]
async fn global_scripts_reject_invalid() {
    let _guard = test_lock().await;
    let app = test_app();

    // 先写入一份合法数据作为基线(不依赖其他测试的执行顺序)
    let (status, _) = send_json(
        app,
        "PUT",
        "/api/scripts/tree?scope=global",
        json!({ "trees": [ { "type": "script", "name": "基线" } ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = send_json(
        app,
        "PUT",
        "/api/scripts/tree?scope=global",
        json!({ "trees": { "not": "array" } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = send_json(
        app,
        "PUT",
        "/api/scripts/tree?scope=global",
        json!({ "trees": [ { "type": "macro" } ] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 保存失败的非法输入不影响既有数据
    let (_, got) = send_json(app, "GET", "/api/scripts/tree?scope=global", json!({})).await;
    let trees = got["trees"].as_array().unwrap();
    assert_eq!(trees.len(), 1, "非法写入不应覆盖既有脚本");
    assert_eq!(trees[0]["name"], json!("基线"));
}

/// 角色级脚本:写角色卡 extensions.tavern_helper,回读一致;缺 character_id 拒绝
#[tokio::test]
async fn character_scripts_read_write() {
    let _guard = test_lock().await;
    let app = test_app();
    let cid = upload_character(
        app,
        &json!({
            "spec": "chara_card_v2", "spec_version": "1.0", "name": "脚本娘",
            "description": "测试", "first_mes": "你好"
        }),
    )
    .await;

    let (status, got) = send_json(
        app,
        "GET",
        &format!("/api/scripts/tree?scope=character&character_id={cid}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["trees"], json!([]), "无脚本时返回空数组");

    let (status, saved) = send_json(
        app,
        "PUT",
        &format!("/api/scripts/tree?scope=character&character_id={cid}"),
        json!({ "trees": sample_tree() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["ok"], json!(true));

    let (_, got) = send_json(
        app,
        "GET",
        &format!("/api/scripts/tree?scope=character&character_id={cid}"),
        json!({}),
    )
    .await;
    assert_eq!(got["trees"].as_array().unwrap().len(), 2);

    // 角色不存在 → 404
    let (status, _) = send_json(
        app,
        "GET",
        "/api/scripts/tree?scope=character&character_id=no-such-id",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 缺 character_id → 400
    let (status, _) = send_json(app, "GET", "/api/scripts/tree?scope=character", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 未知 scope → 400
    let (status, _) = send_json(app, "GET", "/api/scripts/tree?scope=preset", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// 角色卡旧字段迁移:TavernHelper_scripts 首次读取并入 tavern_helper 并删除旧字段;
/// TavernHelper_characterScriptVariables 保留供执行层(3b)读取。
#[tokio::test]
async fn character_legacy_scripts_migrated() {
    let _guard = test_lock().await;
    let app = test_app();
    let cid = upload_character(
        app,
        &json!({
            "spec": "chara_card_v2", "spec_version": "1.0", "name": "旧卡",
            "description": "测试", "first_mes": "你好",
            "extensions": {
                "TavernHelper_scripts": [
                    { "name": "旧脚本", "content": "1+1" }
                ],
                "TavernHelper_characterScriptVariables": { "hp": 100 }
            }
        }),
    )
    .await;

    // 首次读取触发迁移:tavern_helper 含旧脚本且带补齐的 type=script
    let (status, got) = send_json(
        app,
        "GET",
        &format!("/api/scripts/tree?scope=character&character_id={cid}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let trees = got["trees"].as_array().unwrap();
    assert_eq!(trees.len(), 1);
    assert_eq!(trees[0]["name"], json!("旧脚本"));
    assert_eq!(trees[0]["type"], json!("script"));

    // 旧字段已从角色卡删除;变量字段保留
    let (_, char) = send_json(app, "GET", &format!("/api/characters/{cid}"), json!({})).await;
    let ext = &char["data_raw"]["extensions"];
    assert!(
        ext.get("TavernHelper_scripts").is_none(),
        "旧脚本字段应已删除"
    );
    assert_eq!(
        ext["TavernHelper_characterScriptVariables"]["hp"],
        json!(100),
        "角色级变量字段应保留供执行层读取"
    );
}
