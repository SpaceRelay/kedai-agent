// 契约变更历史与回滚(P6 面板,阶段 C):
// GET  /api/characters/{id}/contract/history?limit=50  历史列表(最新在前)
// POST /api/characters/{id}/contract/rollback {"seq": n} 回滚到指定记录
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use crate::contracts::changelog::ChangelogSource;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// GET 查询参数:limit 缺省 50(服务层夹取到 [1, 500])
#[derive(Debug, Default, Deserialize)]
pub struct HistoryQuery {
    pub limit: Option<usize>,
}

/// GET /api/characters/{id}/contract/history — 历史列表(最新在前)
pub async fn list(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let records = match state
        .contract_changelog
        .list(&id, query.limit.unwrap_or(50))
    {
        Ok(records) => records,
        Err(e) => {
            return Json(json!({ "error": e }))
                .into_response()
                .with_status(StatusCode::INTERNAL_SERVER_ERROR)
        }
    };
    let entries = match records
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<Value>, _>>()
    {
        Ok(entries) => entries,
        Err(e) => {
            return Json(json!({ "error": format!("序列化历史记录失败: {e}") }))
                .into_response()
                .with_status(StatusCode::INTERNAL_SERVER_ERROR)
        }
    };
    Json(json!({ "entries": entries })).into_response()
}

/// POST /api/characters/{id}/contract/rollback — 回滚到指定历史版本
///
/// 校验顺序:请求体 → 记录存在 → 记录含恢复内容 → 内容合法 → 角色存在。
/// 回滚本身也是一次「replace」变更,以 Rollback 来源落账留痕。
pub async fn rollback(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    // 1. 请求体必须为对象且含整数 seq
    let Some(seq) = body.get("seq").and_then(Value::as_i64) else {
        return Json(json!({ "error": "请求体必须包含整数 seq" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    // 2. 记录不存在
    let record = match state.contract_changelog.get(&id, seq) {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Json(json!({ "error": "历史记录不存在" }))
                .into_response()
                .with_status(StatusCode::NOT_FOUND)
        }
        Err(e) => {
            return Json(json!({ "error": e }))
                .into_response()
                .with_status(StatusCode::INTERNAL_SERVER_ERROR)
        }
    };
    // 3. 该记录无可恢复内容(如 remove 记录)
    let Some(after) = record.after else {
        return Json(json!({ "error": "该记录无可恢复内容" }))
            .into_response()
            .with_status(StatusCode::UNPROCESSABLE_ENTITY);
    };
    // 4. 防御性校验(入库时已校验过,理论上不触发)
    if let Err(e) = crate::contracts::parse_contract(&after) {
        return Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::UNPROCESSABLE_ENTITY);
    }
    // 5. 回滚前当前契约(留痕用;必须在 set_embedded_contract 之前读取)
    let before = state
        .characters
        .get(&id)
        .and_then(|card| card.data_raw)
        .as_ref()
        .and_then(crate::contracts::raw_contract_from_character_card)
        .cloned();
    // 6. 写回角色卡
    if state
        .characters
        .set_embedded_contract(&id, Some(&after))
        .is_none()
    {
        return not_found();
    }
    // 7. 失效契约缓存(下次加载读到恢复后的契约)
    state.invalidate_contracts_for_character(&id);
    // 8. 回滚留痕。契约恢复已生效,append 失败不能报 500(会误导为「回滚失败」),
    // 降级为成功响应带 warning、seq 置 null。
    let rationale = format!("回滚自 seq {seq}");
    let append_result = state.contract_changelog.append(
        &id,
        ChangelogSource::Rollback,
        "replace",
        before.as_ref(),
        Some(&after),
        Some(&rationale),
    );
    let (new_seq, warning) = match append_result {
        Ok(seq) => (Some(seq), None),
        Err(e) => (None, Some(format!("契约已回滚,但变更历史写入失败: {e}"))),
    };
    // 9. 返回新 seq(null=留痕失败)与恢复版本的 version
    Json(json!({
        "ok": true,
        "id": id,
        "seq": new_seq,
        "version": after.get("version"),
        "warning": warning,
    }))
    .into_response()
}

fn not_found() -> Response {
    Json(json!({ "error": "角色卡不存在" }))
        .into_response()
        .with_status(StatusCode::NOT_FOUND)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// 构建含契约 CRUD + 历史 + 回滚路由的测试应用,返回 (Router, 共享 AppState)。
    fn app() -> (axum::Router, Arc<AppState>) {
        let mut config = crate::config::AppConfig::from_env();
        config.auth_required = false;
        let dir = std::env::temp_dir().join(format!(
            "kedai-contract-history-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        config.data_dir = dir;
        config.connector = "mock".into();
        let state = AppState::new(config).unwrap();
        let router = axum::Router::new()
            .route(
                "/api/characters/{id}/contract",
                axum::routing::get(crate::api::contracts::get)
                    .put(crate::api::contracts::put)
                    .delete(crate::api::contracts::delete),
            )
            .route(
                "/api/characters/{id}/contract/history",
                axum::routing::get(list),
            )
            .route(
                "/api/characters/{id}/contract/rollback",
                axum::routing::post(rollback),
            )
            .with_state(state.clone());
        (router, state)
    }

    /// 直接插入裸角色记录(不动生产代码接口)
    fn seed_character(state: &AppState, id: &str) {
        let conn = state.db.conn();
        conn.execute(
            "INSERT INTO characters (id, name, chara_name, description, file_path, data_raw, created_at) \
             VALUES (?1, ?2, ?2, '', '', '{}', ?3)",
            rusqlite::params![id, "测试角色", crate::models::db::now_iso()],
        )
        .unwrap();
    }

    fn contract_body(version: u32) -> Value {
        json!({
            "version": version,
            "id": "panel-contract",
            "schema": { "properties": {} },
            "updateRules": {
                "好感度": {
                    "path": "好感度", "type": "number", "updateMode": "every_turn",
                    "display": true
                }
            }
        })
    }

    async fn put_contract(router: &axum::Router, id: &str, body: &Value) -> (StatusCode, Value) {
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/characters/{id}/contract"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn get_history(router: &axum::Router, id: &str, limit: usize) -> (StatusCode, Value) {
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/characters/{id}/contract/history?limit={limit}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn post_rollback(router: &axum::Router, id: &str, body: &Value) -> (StatusCode, Value) {
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/characters/{id}/contract/rollback"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// 1+2:PUT 首次挂载记 contract_init;再次 PUT 记 manual,且 before 为上一版。
    #[tokio::test]
    async fn put_records_init_then_manual() {
        let (router, state) = app();
        seed_character(&state, "c-log");
        let v1 = contract_body(1);
        let (status, _) = put_contract(&router, "c-log", &v1).await;
        assert_eq!(status, StatusCode::OK);

        let entries = state.contract_changelog.list("c-log", 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, ChangelogSource::ContractInit);
        assert_eq!(entries[0].op_kind, "replace");
        assert!(entries[0].before.is_none());
        assert_eq!(entries[0].after.as_ref().unwrap(), &v1);

        let v2 = contract_body(2);
        let (status, _) = put_contract(&router, "c-log", &v2).await;
        assert_eq!(status, StatusCode::OK);
        let entries = state.contract_changelog.list("c-log", 10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].source, ChangelogSource::Manual);
        assert_eq!(entries[0].op_kind, "replace");
        assert_eq!(entries[0].before.as_ref().unwrap(), &v1);
        assert_eq!(entries[0].after.as_ref().unwrap(), &v2);
    }

    /// 3:DELETE 记录 remove(含 before=被移除的契约,after=null)。
    #[tokio::test]
    async fn delete_records_remove() {
        let (router, state) = app();
        seed_character(&state, "c-del");
        let v2 = contract_body(2);
        let (status, _) = put_contract(&router, "c-del", &contract_body(1)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = put_contract(&router, "c-del", &v2).await;
        assert_eq!(status, StatusCode::OK);

        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/characters/c-del/contract")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // 移除成功返回 200 带体(历史写失败时降级携带 warning)
        assert_eq!(resp.status(), StatusCode::OK);

        let entries = state.contract_changelog.list("c-del", 10).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].source, ChangelogSource::Manual);
        assert_eq!(entries[0].op_kind, "remove");
        assert_eq!(entries[0].before.as_ref().unwrap(), &v2);
        assert!(entries[0].after.is_none());
    }

    /// 4:rollback 恢复契约 + 留痕 rollback 记录 + 缓存已失效。
    #[tokio::test]
    async fn rollback_restores_contract_and_records() {
        let (router, state) = app();
        seed_character(&state, "c-rb");
        let v1 = contract_body(1);
        let v2 = contract_body(2);
        let (status, _) = put_contract(&router, "c-rb", &v1).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = put_contract(&router, "c-rb", &v2).await;
        assert_eq!(status, StatusCode::OK);

        let (status, resp) = post_rollback(&router, "c-rb", &json!({ "seq": 2 })).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["id"], "c-rb");
        let new_seq = resp["seq"].as_i64().unwrap();
        assert_eq!(new_seq, 3, "回滚留痕应占第 3 条");
        assert_eq!(resp["version"], 2);

        // 卡片契约恢复为第 2 版
        let card = state.characters.get("c-rb").unwrap();
        let raw = card.data_raw.as_ref().unwrap();
        let restored = crate::contracts::raw_contract_from_character_card(raw).unwrap();
        assert_eq!(restored, &v2);

        // 最新一条(seq=3,即回滚留痕):source=rollback,rationale 含「回滚自 seq 2」
        let entries = state.contract_changelog.list("c-rb", 10).unwrap();
        assert_eq!(entries.len(), 3);
        let last = &entries[0];
        assert_eq!(last.seq, new_seq);
        assert_eq!(last.source, ChangelogSource::Rollback);
        assert_eq!(last.op_kind, "replace");
        assert_eq!(last.before.as_ref().unwrap(), &v2);
        assert_eq!(last.after.as_ref().unwrap(), &v2);
        assert!(last
            .rationale
            .as_deref()
            .unwrap()
            .contains("回滚自 seq 2"));

        // 缓存已失效:registry 直接读到恢复后的契约
        let loaded = state.engine.contract_registry.load("c-rb").unwrap();
        assert_eq!(loaded.version, 2);
    }

    /// 5:回滚错误路径(404 / 422 / 400)。
    #[tokio::test]
    async fn rollback_error_paths() {
        let (router, state) = app();
        seed_character(&state, "c-err");
        let (status, _) = put_contract(&router, "c-err", &contract_body(1)).await;
        assert_eq!(status, StatusCode::OK);
        // DELETE 产生 seq=2 的 remove 记录(after=null)
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/characters/c-err/contract")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 不存在的 seq → 404
        let (status, resp) = post_rollback(&router, "c-err", &json!({ "seq": 999 })).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(resp["error"], "历史记录不存在");

        // remove 记录无可恢复内容 → 422
        let (status, resp) = post_rollback(&router, "c-err", &json!({ "seq": 2 })).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(resp["error"], "该记录无可恢复内容");

        // 请求体非法:缺 seq / 非整数 → 400
        let (status, _) = post_rollback(&router, "c-err", &json!({})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = post_rollback(&router, "c-err", &json!({ "seq": "2" })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// 建议 2:无内嵌契约的角色 DELETE → 幂等 204,不落空 remove 记录、不失效缓存。
    #[tokio::test]
    async fn delete_without_contract_is_noop() {
        let (router, state) = app();
        seed_character(&state, "c-noc");
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/characters/c-noc/contract")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert!(
            state.contract_changelog.list("c-noc", 10).unwrap().is_empty(),
            "无契约角色的 DELETE 不应落账"
        );
    }

    /// 6:list limit 生效:写 3 条,limit=2 返回最新 2 条(倒序)。
    #[tokio::test]
    async fn history_limit_applies() {
        let (router, state) = app();
        seed_character(&state, "c-lim");
        for version in 1..=3 {
            let (status, _) = put_contract(&router, "c-lim", &contract_body(version)).await;
            assert_eq!(status, StatusCode::OK);
        }
        let (status, resp) = get_history(&router, "c-lim", 2).await;
        assert_eq!(status, StatusCode::OK);
        let entries = resp["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["seq"], 3);
        assert_eq!(entries[1]["seq"], 2);
    }

    /// 7:history GET 全链路:字段名 camelCase,最新在前。
    #[tokio::test]
    async fn history_get_full_chain() {
        let (router, state) = app();
        seed_character(&state, "c-his");
        let v1 = contract_body(1);
        let (status, _) = put_contract(&router, "c-his", &v1).await;
        assert_eq!(status, StatusCode::OK);
        // 写入挂点与历史同库:直接经服务断言
        let direct = state.contract_changelog.list("c-his", 10).unwrap();
        assert_eq!(direct.len(), 1);

        let (status, resp) = get_history(&router, "c-his", 50).await;
        assert_eq!(status, StatusCode::OK);
        let entries = resp["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["seq"], 1);
        assert_eq!(entries[0]["source"], "contract_init");
        assert_eq!(entries[0]["op"], "replace");
        assert_eq!(entries[0]["path"], "contract");
        assert!(entries[0]["before"].is_null());
        assert_eq!(entries[0]["after"], v1);
        assert!(entries[0]["rationale"].is_null());
        assert_eq!(entries[0]["createdAt"].as_str().unwrap().len(), 24);
    }
}
