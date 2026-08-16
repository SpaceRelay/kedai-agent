// 世界书 API 集成测试:上传/列表/条目预览/开关/绑定/删除
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

/// 世界书测试共享同一 app/DB,须串行执行避免列表计数断言相互干扰
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

async fn send_empty(app: &axum::Router, method: &str, path: &str) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    resp.status()
}

/// 上传独立世界书(multipart:file + 可选 character_id)
async fn upload_world_book(
    app: &axum::Router,
    book: &Value,
    character_id: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"世界书.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n",
        book
    );
    if let Some(cid) = character_id {
        body.push_str(&format!(
            "--BOUND\r\nContent-Disposition: form-data; name=\"character_id\"\r\n\r\n{cid}\r\n"
        ));
    }
    body.push_str("--BOUND--\r\n");
    let req = Request::builder()
        .method("POST")
        .uri("/api/world-books/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn sample_book() -> Value {
    json!({
        "name": "日向台街区",
        "entries": [
            { "uid": 0, "comment": "地点", "keys": ["图书馆", "时雨堂"], "content": "图书馆位于日向台,安静。", "constant": false, "disable": false, "position": 0 },
            { "uid": 1, "comment": "人物", "keys": [], "content": "芽衣是兔族少女。", "constant": true, "disable": false, "position": 0 },
            { "uid": 2, "comment": "禁用", "keys": ["公园"], "content": "不应出现。", "constant": false, "disable": true, "position": 0 },
            { "uid": 3, "comment": "正则", "keys": [], "regex": "\\d{4}-\\d{2}-\\d{2}", "use_regex": true, "content": "日期触发的内容。", "constant": false, "disable": false, "position": 0 }
        ]
    })
}

#[tokio::test]
async fn world_book_crud() {
    let _guard = test_lock().await;
    let app = test_app();

    // 上传(全局,未绑定角色)
    let (status, book) = upload_world_book(app, &sample_book(), None).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = book["id"].as_str().unwrap().to_string();
    assert_eq!(book["name"], json!("日向台街区"));
    assert_eq!(book["source"], json!("upload"));
    assert_eq!(book["entry_count"], json!(4));
    assert_eq!(book["enabled"], json!(true));
    assert!(book.get("character_id").is_none() || book["character_id"].is_null());

    // 列表包含本次上传(共享 DB,不依赖绝对计数)
    let (_, list) = send_json(app, "GET", "/api/world-books", json!({})).await;
    let ids: Vec<&str> = list["world_books"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["id"].as_str())
        .collect();
    assert!(ids.contains(&id.as_str()), "列表应包含本次上传的世界书");
    // 列表不含 data_raw(体积小)
    let mine = list["world_books"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == id)
        .unwrap();
    assert!(mine.get("data_raw").is_none());

    // 条目预览:4 条,禁用条目保留在 data_raw 但预览含全部(过滤在注入侧)
    let (status, entries) = send_json(
        app,
        "GET",
        &format!("/api/world-books/{id}/entries"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(entries["entries"].as_array().unwrap().len(), 4);
    let first = &entries["entries"][0];
    assert_eq!(first["comment"], json!("地点"));
    assert_eq!(first["keys"][0], json!("图书馆"));
    assert!(!first["constant"].as_bool().unwrap());
    let disabled = entries["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["comment"] == "禁用")
        .unwrap();
    assert!(!disabled["enabled"].as_bool().unwrap());
    // 正则条目:regex 与 use_regex 保留
    let regex_entry = entries["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["comment"] == "正则")
        .unwrap();
    assert_eq!(regex_entry["regex"], json!("\\d{4}-\\d{2}-\\d{2}"));

    // 更新:停用
    let (status, updated) = send_json(
        app,
        "PUT",
        &format!("/api/world-books/{id}"),
        json!({ "enabled": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["enabled"], json!(false));

    // 更新:改名 + 绑定角色(id 不存在则绑定仍落库,此处仅验证字段)
    let (_, updated) = send_json(
        app,
        "PUT",
        &format!("/api/world-books/{id}"),
        json!({ "name": "街区手册", "character_id": "" }),
    )
    .await;
    assert_eq!(updated["name"], json!("街区手册"));
    assert!(updated.get("character_id").is_none() || updated["character_id"].is_null());

    // 删除
    let status = send_empty(app, "DELETE", &format!("/api/world-books/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, list) = send_json(app, "GET", "/api/world-books", json!({})).await;
    let ids_after: Vec<&str> = list["world_books"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["id"].as_str())
        .collect();
    assert!(
        !ids_after.contains(&id.as_str()),
        "删除后列表不应包含该世界书"
    );
    // 删除后条目预览 404
    let status = send_empty(app, "GET", &format!("/api/world-books/{id}/entries")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn world_book_invalid_upload_rejected() {
    let _guard = test_lock().await;
    let app = test_app();
    // 非 JSON
    let body = "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"bad.json\"\r\n\r\nnot json\r\n--BOUND--\r\n";
    let req = Request::builder()
        .method("POST")
        .uri("/api/world-books/upload")
        .header("content-type", "multipart/form-data; boundary=BOUND")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 合法 JSON 但无有效条目
    let (status, _) = upload_world_book(app, &json!({ "name": "空书", "entries": [] }), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn world_book_bind_to_character() {
    let _guard = test_lock().await;
    let app = test_app();
    // 上传角色
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"角色.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        json!({
            "spec": "chara_card_v2", "spec_version": "1.0", "name": "测试角色",
            "description": "测试", "first_mes": "你好"
        })
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
    let cid = char["id"].as_str().unwrap().to_string();

    // 绑定上传
    let (status, book) = upload_world_book(app, &sample_book(), Some(&cid)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(book["character_id"], json!(cid));
    assert_eq!(book["character_name"], json!("测试角色"));

    // 清除绑定 → 全局
    let id = book["id"].as_str().unwrap().to_string();
    let (_, updated) = send_json(
        app,
        "PUT",
        &format!("/api/world-books/{id}"),
        json!({ "character_id": "" }),
    )
    .await;
    assert!(updated.get("character_id").is_none() || updated["character_id"].is_null());
}

/// 独立世界书条目编辑:PUT /entries 全量回写(关键词/常驻/激发/位置)→ GET 验证
#[tokio::test]
async fn world_book_entries_editable() {
    let _guard = test_lock().await;
    let app = test_app();
    let (status, book) = upload_world_book(app, &sample_book(), None).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = book["id"].as_str().unwrap().to_string();

    // 回写:改关键词、开常驻、停用、改位置、改内容
    let (status, saved) = send_json(
        app,
        "PUT",
        &format!("/api/world-books/{id}/entries"),
        json!({ "entries": [
            { "id": 0, "comment": "地点", "keys": ["车站", "铁路"], "regex": null, "use_regex": false, "constant": false, "enabled": true, "content": "车站很繁忙。", "position": 2 },
            { "id": 1, "comment": "人物", "keys": [], "regex": null, "use_regex": false, "constant": true, "enabled": true, "content": "芽衣是兔族少女。", "position": 0 },
            { "id": 2, "comment": "禁用", "keys": ["公园"], "regex": null, "use_regex": false, "constant": false, "enabled": false, "content": "不应出现。", "position": 0 }
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["ok"], json!(true));

    let (_, entries) = send_json(
        app,
        "GET",
        &format!("/api/world-books/{id}/entries"),
        json!({}),
    )
    .await;
    let arr = entries["entries"].as_array().unwrap();
    assert_eq!(arr.len(), 3, "回写后条目数应为 3(原 4 条 → 覆盖 3 条)");
    let loc = arr.iter().find(|e| e["comment"] == "地点").unwrap();
    assert_eq!(loc["keys"], json!(["车站", "铁路"]));
    assert_eq!(loc["position"], json!(2));
    let person = arr.iter().find(|e| e["comment"] == "人物").unwrap();
    assert!(person["constant"].as_bool().unwrap());
    let disabled = arr.iter().find(|e| e["comment"] == "禁用").unwrap();
    assert!(!disabled["enabled"].as_bool().unwrap());

    // POST 新增条目:分配新 id;空条目(无内容无关键词)按约定不参与注入,列表暂不显示
    let (status, added) = send_json(
        app,
        "POST",
        &format!("/api/world-books/{id}/entries"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let new_id = added["entry"]["id"]
        .as_i64()
        .expect("新增条目应返回数字 id");
    assert!(new_id >= 3, "新 id 应递增分配,实际 {new_id}");
    let (_, entries) = send_json(
        app,
        "GET",
        &format!("/api/world-books/{id}/entries"),
        json!({}),
    )
    .await;
    assert_eq!(
        entries["entries"].as_array().unwrap().len(),
        3,
        "空条目被过滤,列表仍 3 条"
    );

    // 编辑该新条目(填入内容与关键词)→ 回写后生效且出现
    let (status, _) = send_json(
        app,
        "PUT",
        &format!("/api/world-books/{id}/entries"),
        json!({ "entries": [
            { "id": 0, "comment": "地点", "keys": ["车站", "铁路"], "regex": null, "use_regex": false, "constant": false, "enabled": true, "content": "车站很繁忙。", "position": 2 },
            { "id": 1, "comment": "人物", "keys": [], "regex": null, "use_regex": false, "constant": true, "enabled": true, "content": "芽衣是兔族少女。", "position": 0 },
            { "id": 2, "comment": "禁用", "keys": ["公园"], "regex": null, "use_regex": false, "constant": false, "enabled": false, "content": "不应出现。", "position": 0 },
            { "id": new_id, "comment": "新增", "keys": ["雨天"], "regex": null, "use_regex": false, "constant": false, "enabled": true, "content": "下雨天设定。", "position": 0 }
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, entries) = send_json(
        app,
        "GET",
        &format!("/api/world-books/{id}/entries"),
        json!({}),
    )
    .await;
    let arr = entries["entries"].as_array().unwrap();
    assert_eq!(arr.len(), 4, "编辑新条目后应为 4 条");
    assert!(arr
        .iter()
        .any(|e| e["comment"] == "新增" && e["keys"] == json!(["雨天"])));
}

/// 角色卡内嵌世界书:GET/PUT /api/characters/{id}/world-entries
#[tokio::test]
async fn character_card_world_entries_editable() {
    let _guard = test_lock().await;
    let app = test_app();
    let body = format!(
        "--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"带世界书.json\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--BOUND--\r\n",
        json!({
            "spec": "chara_card_v2", "spec_version": "1.0", "name": "书娘",
            "description": "测试", "first_mes": "你好",
            "character_book": {
                "name": "书娘的世界",
                "entries": [
                    { "id": 0, "comment": "世界观", "keys": [], "content": "兽人社会。", "constant": true, "disable": false, "position": 0 },
                    { "id": 1, "comment": "地点", "keys": ["图书馆"], "content": "图书馆安静。", "constant": false, "disable": false, "position": 0 }
                ]
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
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let char: Value = serde_json::from_slice(&bytes).unwrap();
    let cid = char["id"].as_str().unwrap().to_string();

    // GET 内嵌条目
    let (status, entries) = send_json(
        app,
        "GET",
        &format!("/api/characters/{cid}/world-entries"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(entries["entries"].as_array().unwrap().len(), 2);
    let world = entries["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["comment"] == "世界观")
        .unwrap();
    assert!(world["constant"].as_bool().unwrap());
    assert!(world["enabled"].as_bool().unwrap());

    // PUT 回写:改关键词/位置/停用
    let (status, saved) = send_json(
        app,
        "PUT",
        &format!("/api/characters/{cid}/world-entries"),
        json!({ "entries": [
            { "id": 0, "comment": "世界观", "keys": [], "regex": null, "use_regex": false, "constant": true, "enabled": false, "content": "兽人社会。", "position": 1 },
            { "id": 1, "comment": "地点", "keys": ["书库", "书架"], "regex": null, "use_regex": false, "constant": false, "enabled": true, "content": "图书馆安静。", "position": 0 },
            { "id": 2, "comment": "新条目", "keys": ["雨天"], "regex": null, "use_regex": false, "constant": false, "enabled": true, "content": "下雨天不出门。", "position": 0 }
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["ok"], json!(true));

    // 回读验证(条目经 character_book_entries 重新提取,3 条)
    let (_, entries) = send_json(
        app,
        "GET",
        &format!("/api/characters/{cid}/world-entries"),
        json!({}),
    )
    .await;
    let arr = entries["entries"].as_array().unwrap();
    assert_eq!(arr.len(), 3);
    let world = arr.iter().find(|e| e["comment"] == "世界观").unwrap();
    assert_eq!(world["position"], json!(1));
    assert!(!world["enabled"].as_bool().unwrap());
    let loc = arr.iter().find(|e| e["comment"] == "地点").unwrap();
    assert_eq!(loc["keys"], json!(["书库", "书架"]));
}

/// 上传自动转换:变体(逗号分隔字符串 keys、字符串 constant、keywords 字段)在
/// 上传时被规范化,响应携带 conversion 统计,条目接口返回规范字段。
#[tokio::test]
async fn upload_converts_variant_entries() {
    let _guard = test_lock().await;
    let app = test_app();
    let book = json!({
        "name": "变体书",
        "entries": [
            { "uid": 0, "comment": "逗号拆分", "keys": "车站, 铁路、码头", "content": "交通设定。", "constant": "true", "disable": false },
            { "uid": 1, "comment": "keywords变体", "keywords": ["火车站"], "content": "车站设定。", "constant": 0 },
            { "uid": 2, "comment": "缺失constant", "content": "世界观设定。" }
        ]
    });
    let (status, rec) = upload_world_book(app, &book, None).await;
    assert_eq!(status, StatusCode::CREATED);
    // 转换统计
    let conv = &rec["conversion"];
    assert_eq!(conv["total_count"], json!(3));
    assert_eq!(conv["converted_count"], json!(3), "三条均有变体需转换");
    assert_eq!(
        conv["key_normalized_count"],
        json!(2),
        "条目0 逗号拆分 + 条目1 keywords 变体"
    );
    assert_eq!(
        conv["constant_auto_count"],
        json!(1),
        "条目2 缺失 constant 自动判定"
    );

    // 条目接口返回规范化字段
    let id = rec["id"].as_str().unwrap();
    let (_, entries) = send_json(
        app,
        "GET",
        &format!("/api/world-books/{id}/entries"),
        json!({}),
    )
    .await;
    let arr = entries["entries"].as_array().unwrap();
    assert_eq!(arr.len(), 3);
    let e0 = arr.iter().find(|e| e["comment"] == "逗号拆分").unwrap();
    assert_eq!(e0["keys"], json!(["车站", "铁路", "码头"]));
    assert_eq!(e0["constant"], json!(true), "字符串 \"true\" 转布尔");
    let e1 = arr.iter().find(|e| e["comment"] == "keywords变体").unwrap();
    assert_eq!(e1["keys"], json!(["火车站"]));
    assert_eq!(e1["constant"], json!(false), "数字 0 转布尔");
    let e2 = arr.iter().find(|e| e["comment"] == "缺失constant").unwrap();
    assert_eq!(e2["constant"], json!(true), "无关键词无正则 → 常驻");
    assert!(
        e2["role"].is_null() || e2.get("role").is_none(),
        "role 缺失 → 自动"
    );
}

/// 自动分配机制自检:GET /api/world-books/auto-assign-check 返回 ok:true
/// 且含 parse / auto_role / convert / ui_auto 全部检查项。
#[tokio::test]
async fn auto_assign_check_reports_ok() {
    let _guard = test_lock().await;
    let app = test_app();
    let (status, result) =
        send_json(app, "GET", "/api/world-books/auto-assign-check", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        result["ok"],
        json!(true),
        "自动分配机制应可用,summary={}",
        result["summary"]
    );
    let names: Vec<String> = result["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap_or("").to_string())
        .collect();
    for expect in ["parse", "auto_role", "convert", "ui_auto"] {
        assert!(
            names.contains(&expect.to_string()),
            "缺少检查项 {expect}:{names:?}"
        );
    }
    assert!(
        result["checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["status"] == "ok"),
        "所有检查项应为 ok:{result}"
    );
}
