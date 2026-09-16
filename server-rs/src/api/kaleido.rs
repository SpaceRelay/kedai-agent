// 契约引擎 HTTP 出口(P7 方案 A):供 ST 前端与外部工具调用同一契约引擎,
// 与 Agent 多步工具 apply_patch 共用 VariableApplyService(单库无分叉)。
//
//   POST /api/variable/update      统一写入出口(门控/留痕/pending/熔断指纹)
//   GET  /api/variable/state        运行态整行(stat_data/meta/revision)
//   GET  /api/variable/changelog    逐 op 变更流水(最新在前)
use crate::api::app_state::AppState;
use crate::api::{db_err, err_status, not_found, validation};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// POST /api/variable/update 请求体
#[derive(Debug, Deserialize)]
pub struct UpdateBody {
    pub session_id: String,
    /// JSON Patch 数组(replace/set/insert/delta/remove/move;可选 confidence)
    pub patches: Vec<Value>,
    /// 写者子系统 id;缺省 "external"。契约 updateRules 的 writers 白名单
    /// 须包含该 id 或 "*" 才能写入(not_owner 拒绝)。
    #[serde(default)]
    pub writer: Option<String>,
}

/// GET 查询参数:limit 缺省 50(服务层夹取到 [1, 1000])
#[derive(Debug, Default, Deserialize)]
pub struct ChangelogQuery {
    pub session_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
pub struct StateQuery {
    pub session_id: Option<String>,
}

/// POST /api/variable/update — 契约引擎统一写入出口。
///
/// 与多步工具 apply_patch 行为一致:契约门控(unknown_field/not_owner 拒绝、
/// 低置信入 pending)、kaleido_changelog 留痕、meta.pending 维护、熔断指纹。
/// 部分被拦时 HTTP 200 + warnings(与工具的 Err 语义不同:外部调用方通常
/// 需要拿到已生效部分,而非整批失败)。
pub async fn update(State(state): State<Arc<AppState>>, Json(body): Json<UpdateBody>) -> Response {
    if body.patches.is_empty() {
        return validation("patches 不能为空");
    }
    // 会话 → 角色(契约按角色加载);会话读取与 apply 落库合并进同一阻塞任务(DB 并发改造)
    let apply = crate::services::variable_apply::VariableApplyService::new(
        state.sessions.clone(),
        state.contract_registry.clone(),
        state.kaleido_state.clone(),
    );
    let sessions = state.sessions.clone();
    let sid = body.session_id.clone();
    let patches = body.patches.clone();
    let writer = body.writer.clone().unwrap_or_else(|| "external".into());
    let applied = state
        .db_call(move || {
            let session = sessions.get(&sid)?;
            Some(apply.apply(&sid, &session.character_id, &patches, &writer))
        })
        .await;
    match applied {
        Err(e) => db_err(&e),
        Ok(None) => not_found("会话不存在"),
        Ok(Some(Err(e))) => validation(&e),
        Ok(Some(Ok(outcome))) => {
            let mut resp = json!({
                "ok": outcome.ok,
                "stat_data": outcome.tree,
                "entries": outcome.entries,
            });
            if !outcome.warnings.is_empty() {
                resp["warnings"] = json!(outcome.warnings);
            }
            if !outcome.breaker_hashes.is_empty() {
                resp["breaker_hashes"] = json!(outcome.breaker_hashes);
            }
            Json(resp).into_response()
        }
    }
}

/// GET /api/variable/state?session_id= — 运行态整行。
/// 尚未 commit 过返回 404(与「无契约会话无行」的存量语义一致)。
pub async fn get_state(
    State(state): State<Arc<AppState>>,
    Query(q): Query<StateQuery>,
) -> Response {
    let Some(sid) = q.session_id else {
        return validation("缺少 session_id 查询参数");
    };
    let sessions = state.sessions.clone();
    let kaleido = state.kaleido_state.clone();
    let sid_c = sid.clone();
    let loaded = state
        .db_call(move || {
            sessions.get(&sid_c)?;
            Some(kaleido.load_state(&sid_c))
        })
        .await;
    match loaded {
        Err(e) => db_err(&e),
        Ok(None) => not_found("会话不存在"),
        Ok(Some(Err(e))) => err_status(e, StatusCode::INTERNAL_SERVER_ERROR),
        Ok(Some(Ok(None))) => not_found("该会话尚无契约运行态"),
        Ok(Some(Ok(Some(row)))) => {
            // 库内 JSON 损坏属异常态,显式 500 让前端可排查(静默空对象会掩盖)
            let parse = |raw: &str, field: &str| -> Result<Value, String> {
                serde_json::from_str(raw).map_err(|e| format!("运行态 {field} 损坏: {e}"))
            };
            let (stat_data, meta) = match (
                parse(&row.stat_data, "stat_data"),
                parse(&row.meta_json, "meta"),
            ) {
                (Ok(s), Ok(m)) => (s, m),
                (Err(e), _) | (_, Err(e)) => {
                    return err_status(e, StatusCode::INTERNAL_SERVER_ERROR)
                }
            };
            Json(json!({
                "session_id": sid,
                "contract_version": row.contract_version,
                "stat_data": stat_data,
                "meta": meta,
                "revision_seq": row.revision_seq,
                "revision_hash": row.revision_hash,
                "updated_at": row.updated_at,
            }))
            .into_response()
        }
    }
}

/// GET /api/variable/changelog?session_id=&limit= — 逐 op 变更流水(最新在前)。
pub async fn changelog(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ChangelogQuery>,
) -> Response {
    let Some(sid) = q.session_id else {
        return validation("缺少 session_id 查询参数");
    };
    let sessions = state.sessions.clone();
    let kaleido = state.kaleido_state.clone();
    let sid_c = sid.clone();
    let limit = q.limit.unwrap_or(50);
    let listed = state
        .db_call(move || {
            sessions.get(&sid_c)?;
            Some(kaleido.list_entries(&sid_c, limit))
        })
        .await;
    match listed {
        Err(e) => db_err(&e),
        Ok(None) => not_found("会话不存在"),
        Ok(Some(Err(e))) => err_status(e, StatusCode::INTERNAL_SERVER_ERROR),
        Ok(Some(Ok(entries))) => {
            // 单条序列化失败跳过该条而非整表清空(ChangelogEntry 实际不会失败)
            let items = entries
                .iter()
                .filter_map(|e| serde_json::to_value(e).ok())
                .collect::<Vec<Value>>();
            Json(json!({ "entries": items })).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_support::TempDataDir;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// 构建含三个契约引擎 HTTP 出口路由的测试应用(与 contract_history 测试
    /// 同款隔离:独立 temp 目录 + mock 连接器 + 裸角色直插)。
    fn app() -> (TempDataDir, axum::Router, Arc<AppState>) {
        let mut config = crate::config::AppConfig::from_env();
        config.auth_required = false;
        let dir = TempDataDir::new("kaleido-api");
        config.data_dir = dir.path().to_path_buf();
        config.connector = "mock".into();
        let state = AppState::new(config).unwrap();
        let router = axum::Router::new()
            .route("/api/variable/update", axum::routing::post(update))
            .route("/api/variable/state", axum::routing::get(get_state))
            .route("/api/variable/changelog", axum::routing::get(changelog))
            .with_state(state.clone());
        (dir, router, state)
    }

    /// 直插带契约的角色(extensions.nlkaleido 内嵌于 data_raw 顶层,与
    /// contracts_e2e 的提取口径一致);好感度对外开放 external 写权,
    /// 信任度保持默认 [agent, manual](供 not_owner 断言)。
    fn seed_contract_character(state: &AppState) -> (String, String) {
        let cid = uuid::Uuid::new_v4().to_string();
        let contract = json!({
            "version": 1,
            "id": "kaleido-api-contract",
            "schema": { "properties": {} },
            "guardrails": { "minConfidence": "medium" },
            "updateRules": {
                "心之所向.好感度": {
                    "path": "心之所向.好感度", "type": "number",
                    "updateMode": "every_turn", "display": true,
                    "ownership": { "owner": "agent", "writers": ["agent", "external"] }
                },
                "心之所向.信任度": {
                    "path": "心之所向.信任度", "type": "number",
                    "updateMode": "every_turn", "display": true
                }
            }
        });
        let data_raw = json!({ "extensions": { "nlkaleido": contract }, "name": "契约角色" });
        // 单连接 Mutex 非重入:insert 的守卫须在调 session 服务前释放(防死锁)
        {
            let conn = state.db.write();
            conn.execute(
                "INSERT INTO characters (id, name, chara_name, description, file_path, data_raw, created_at) \
                 VALUES (?1, ?2, ?2, '', '', ?3, ?4)",
                rusqlite::params![
                    cid,
                    "契约角色",
                    data_raw.to_string(),
                    crate::models::db::now_iso()
                ],
            )
            .unwrap();
        }
        let sid = state.sessions.ensure_session(&cid).unwrap().id;
        (cid, sid)
    }

    async fn post(router: &axum::Router, path: &str, body: &Value) -> (StatusCode, Value) {
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get(router: &axum::Router, path: &str) -> (StatusCode, Value) {
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// 出口校验:未知会话 404、空 patches 400、缺 session_id 400;
    /// 错误响应须带与状态码匹配的结构化 code(errors.rs 收尾)。
    #[tokio::test]
    async fn update_rejects_invalid_requests() {
        let (_dir, router, state) = app();
        let (_, sid) = seed_contract_character(&state);

        let (status, body) = post(
            &router,
            "/api/variable/update",
            &json!({ "session_id": "no-such", "patches": [{ "op": "replace", "path": "a", "value": 1 }] }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
        assert_eq!(body["code"], "NOT_FOUND", "body: {body}");

        let (status, body) = post(
            &router,
            "/api/variable/update",
            &json!({ "session_id": sid, "patches": [] }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
        assert_eq!(body["code"], "VALIDATION", "body: {body}");

        let (status, body) = get(&router, "/api/variable/state").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "VALIDATION", "body: {body}");
    }

    /// 主路径:external 写者经所有权校验写入 → 留痕 → state/changelog 可读;
    /// 默认 writers 的字段被 not_owner 拒(部分被拦仍 200 + warnings)。
    /// 低置信全拦时 ok:false 但提议入 meta.pending(自纠闭环)。
    #[tokio::test]
    async fn update_full_contract_engine_roundtrip() {
        let (_dir, router, state) = app();
        let (cid, sid) = seed_contract_character(&state);

        // 第一轮:external 写好感度成功;信任度 not_owner 拒
        let (status, body) = post(
            &router,
            "/api/variable/update",
            &json!({
                "session_id": sid,
                "writer": "external",
                "patches": [
                    { "op": "replace", "path": "心之所向.好感度", "value": 5 },
                    { "op": "replace", "path": "心之所向.信任度", "value": 3 }
                ]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["ok"], json!(true), "body: {body}");
        assert_eq!(body["stat_data"]["心之所向"]["好感度"], json!(5));
        let warnings = body["warnings"].as_array().cloned().unwrap_or_default();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap_or("").contains("not_owner")),
            "warnings: {warnings:?}"
        );
        let entries = body["entries"].as_array().cloned().unwrap_or_default();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["path"], json!("心之所向.好感度"));

        // 运行态:GET state 返回整行
        let (status, srow) = get(&router, &format!("/api/variable/state?session_id={sid}")).await;
        assert_eq!(status, StatusCode::OK, "srow: {srow}");
        assert_eq!(srow["stat_data"]["心之所向"]["好感度"], json!(5));
        assert_eq!(srow["contract_version"], json!(1));
        assert_eq!(srow["meta"]["lastContractVersion"], json!(1));

        // changelog:一条记录,source=agent(共享层以 Agent 来源留痕)
        let (status, log) = get(
            &router,
            &format!("/api/variable/changelog?session_id={sid}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "log: {log}");
        let log_items = log["entries"].as_array().cloned().unwrap_or_default();
        assert_eq!(log_items.len(), 1, "log: {log}");
        assert_eq!(log_items[0]["path"], json!("心之所向.好感度"));
        assert_eq!(log_items[0]["source"], json!("agent"));

        // 第二轮:低置信(minConfidence=medium)→ 全拦,ok:false,提议入 pending
        let (status, body) = post(
            &router,
            "/api/variable/update",
            &json!({
                "session_id": sid,
                "writer": "external",
                "patches": [{ "op": "replace", "path": "心之所向.好感度", "value": 9, "confidence": "low" }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["ok"], json!(false), "低置信全拦应为 ok:false");
        let (_, srow) = get(&router, &format!("/api/variable/state?session_id={sid}")).await;
        let pending = srow["meta"]["pending"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(pending.len(), 1, "pending 应入队: {srow}");
        assert_eq!(pending[0]["op"]["path"], json!("心之所向.好感度"));
        // 树未变
        assert_eq!(srow["stat_data"]["心之所向"]["好感度"], json!(5));
        let _ = cid;
    }

    /// 无契约角色(存量卡):补丁原样放行(零行为变化),无 kaleido 行。
    #[tokio::test]
    async fn update_without_contract_passes_through() {
        let (_dir, router, state) = app();
        let cid = uuid::Uuid::new_v4().to_string();
        {
            let conn = state.db.write();
            conn.execute(
                "INSERT INTO characters (id, name, chara_name, description, file_path, data_raw, created_at) \
                 VALUES (?1, ?2, ?2, '', '', '{}', ?3)",
                rusqlite::params![cid, "无契约角色", crate::models::db::now_iso()],
            )
            .unwrap();
        }
        let sid = state.sessions.ensure_session(&cid).unwrap().id;

        let (status, body) = post(
            &router,
            "/api/variable/update",
            &json!({
                "session_id": sid,
                "patches": [{ "op": "replace", "path": "任意.字段", "value": 1 }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["stat_data"]["任意"]["字段"], json!(1));
        // 无契约 → 无留痕、无运行态行
        assert!(
            body["entries"].as_array().unwrap().is_empty(),
            "无契约不应留痕: {body}"
        );
        let (status, _) = get(&router, &format!("/api/variable/state?session_id={sid}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
