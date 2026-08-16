// 契约编辑路由(P6 面板后端):/api/characters/{id}/contract
//
// GET    读取角色卡内嵌契约(extensions.nlkaleido 原样返回,含来源信息)
// PUT    写入契约(先 parse_contract 校验,失败 422 带错误列表;成功落卡 + 失效缓存)
// DELETE 移除内嵌契约(世界书来源契约不受影响)
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;

/// GET /api/characters/{id}/contract — 读取内嵌契约
pub async fn get(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(card) = state.characters.get(&id) else {
        return not_found();
    };
    match card
        .data_raw
        .as_ref()
        .and_then(crate::contracts::raw_contract_from_character_card)
    {
        Some(raw) => {
            // 附带校验状态:面板显示「已存但当前解析失败」的警示
            let valid = crate::contracts::parse_contract(raw).is_ok();
            Json(json!({ "contract": raw, "valid": valid, "source": "character_card" }))
                .into_response()
        }
        // 无内嵌契约:合法状态(存量卡/契约在世界书),返回 200 + null 契约
        None => Json(json!({ "contract": null, "valid": false, "source": "none" })).into_response(),
    }
}

/// PUT /api/characters/{id}/contract — 写入(整体替换)内嵌契约
pub async fn put(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if !body.is_object() {
        return Json(json!({ "error": "契约必须是 JSON 对象" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    // 先校验后落库:结构错误不进库,错误信息回显给编辑器
    if let Err(e) = crate::contracts::parse_contract(&body) {
        return Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::UNPROCESSABLE_ENTITY);
    }
    // 变更前旧契约(必须在 set_embedded_contract 之前读取;先移出所有权再借用,
    // 避免闭包内借用临时 card 导致悬垂引用)
    let before = state
        .characters
        .get(&id)
        .and_then(|card| card.data_raw)
        .as_ref()
        .and_then(crate::contracts::raw_contract_from_character_card)
        .cloned();
    match state.characters.set_embedded_contract(&id, Some(&body)) {
        Some(()) => {
            state.invalidate_contracts_for_character(&id);
            // 变更留痕:首次挂载记 contract_init,后续整体替换记 manual。
            // 注意:此时契约已落库,append 失败不能报 500(会误导为「保存失败」,
            // 用户重试还会追加 before==after 的冗余记录),降级为成功响应带 warning。
            let source = if before.is_none() {
                crate::contracts::changelog::ChangelogSource::ContractInit
            } else {
                crate::contracts::changelog::ChangelogSource::Manual
            };
            let warning = state
                .contract_changelog
                .append(&id, source, "replace", before.as_ref(), Some(&body), None)
                .err()
                .map(|e| format!("契约已更新,但变更历史写入失败: {e}"));
            Json(json!({
                "ok": true,
                "id": id,
                "version": body.get("version"),
                "warning": warning,
            }))
            .into_response()
        }
        None => not_found(),
    }
}

/// DELETE /api/characters/{id}/contract — 移除内嵌契约
pub async fn delete(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    // 被移除的旧契约(留痕用;必须在 set_embedded_contract 之前读取)
    let before = state
        .characters
        .get(&id)
        .and_then(|card| card.data_raw)
        .as_ref()
        .and_then(crate::contracts::raw_contract_from_character_card)
        .cloned();
    // 角色不存在 → 404;存在但本无内嵌契约 → 幂等 204(不落空 remove 记录、不失效缓存)
    if before.is_none() {
        if state.characters.get(&id).is_none() {
            return not_found();
        }
        return StatusCode::NO_CONTENT.into_response();
    }
    match state
        .characters
        .set_embedded_contract(&id, None)
    {
        Some(()) => {
            state.invalidate_contracts_for_character(&id);
            // 契约移除已生效,append 失败同样降级为成功响应带 warning(不报 500)
            let warning = state
                .contract_changelog
                .append(
                    &id,
                    crate::contracts::changelog::ChangelogSource::Manual,
                    "remove",
                    before.as_ref(),
                    None,
                    None,
                )
                .err()
                .map(|e| format!("契约已移除,但变更历史写入失败: {e}"));
            Json(json!({ "ok": true, "warning": warning })).into_response()
        }
        None => not_found(),
    }
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

    /// 构建仅含契约路由的测试应用,返回 (Router, 共享 AppState)。
    /// AppState 的 db/engine 均为 pub,种子数据与缓存断言直接经句柄操作。
    fn app() -> (axum::Router, Arc<AppState>) {
        let mut config = crate::config::AppConfig::from_env();
        config.auth_required = false;
        let dir = std::env::temp_dir().join(format!("kedai-contract-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        config.data_dir = dir;
        config.connector = "mock".into();
        let state = AppState::new(config).unwrap();
        let router = axum::Router::new()
            .route(
                "/api/characters/{id}/contract",
                axum::routing::get(get)
                    .put(put)
                    .delete(super::delete),
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

    fn contract_body() -> Value {
        json!({
            "version": 1,
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

    async fn get_contract(router: &axum::Router, id: &str) -> (StatusCode, Value) {
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/characters/{id}/contract"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// PUT 写入 → GET 读回一致 → DELETE 清除后 GET 返回 null。
    #[tokio::test]
    async fn put_get_delete_roundtrip() {
        let (router, state) = app();
        seed_character(&state, "c-rt");
        let body = contract_body();

        let (status, _) = put_contract(&router, "c-rt", &body).await;
        assert_eq!(status, StatusCode::OK, "首次写入应成功");

        let (status, got) = get_contract(&router, "c-rt").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(got["contract"], body, "GET 应读回原样契约");
        assert_eq!(got["valid"], true, "合法契约 valid=true");

        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/characters/c-rt/contract")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // 契约移除成功返回 200 带体(ok/warning),便于携带历史写入失败等降级警告
        assert_eq!(resp.status(), StatusCode::OK);
        let (_, got2) = get_contract(&router, "c-rt").await;
        assert!(got2["contract"].is_null(), "删除后契约应为 null");
    }

    /// PUT 保留角色卡其余字段(name/description 不被抹掉)。
    #[tokio::test]
    async fn put_preserves_other_card_fields() {
        let (router, state) = app();
        seed_character(&state, "c-keep");
        // 起点带其他 extension 字段
        {
            let conn = state.db.conn();
            conn.execute(
                "UPDATE characters SET data_raw = ?1 WHERE id = 'c-keep'",
                rusqlite::params![json!({
                    "name": "测试角色",
                    "description": "人设",
                    "extensions": { "tavern_helper": { "x": 1 } }
                }).to_string()],
            )
            .unwrap();
        }
        let (status, _) = put_contract(&router, "c-keep", &contract_body()).await;
        assert_eq!(status, StatusCode::OK);

        let card = state.characters.get("c-keep").unwrap();
        let raw = card.data_raw.unwrap();
        assert_eq!(raw["description"], "人设", "description 应保留");
        assert_eq!(
            raw["extensions"]["tavern_helper"]["x"], 1,
            "其他 extensions 字段应保留"
        );
        assert!(raw["extensions"]["nlkaleido"].is_object(), "契约已写入");
    }

    /// 非法契约(依赖环)PUT → 422 + 错误信息,且不落库。
    #[tokio::test]
    async fn invalid_contract_rejected_with_422() {
        let (router, state) = app();
        seed_character(&state, "c-bad");
        let mut body = contract_body();
        body["updateRules"]["A"] = json!({
            "path": "A", "type": "number", "updateMode": "every_turn",
            "display": true, "dependencies": ["B"]
        });
        body["updateRules"]["B"] = json!({
            "path": "B", "type": "number", "updateMode": "every_turn",
            "display": true, "dependencies": ["A"]
        });

        let (status, err) = put_contract(&router, "c-bad", &body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(err["error"].as_str().unwrap().contains("dependencies cycle"));

        let (_, got) = get_contract(&router, "c-bad").await;
        assert!(got["contract"].is_null(), "非法契约不应落库");
    }

    /// 不存在的角色 → 404(GET/PUT/DELETE)。
    #[tokio::test]
    async fn missing_character_404() {
        let (router, _state) = app();
        let (status, _) = get_contract(&router, "ghost").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = put_contract(&router, "ghost", &contract_body()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/characters/ghost/contract")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// PUT 成功后契约缓存已失效:registry 下次加载读到新契约(无需重启)。
    #[tokio::test]
    async fn put_invalidates_registry_cache() {
        let (router, state) = app();
        seed_character(&state, "c-cache");
        // 预热(无契约 → None;由于无契约不缓存,直接断言)
        assert!(state.engine.contract_registry.load("c-cache").is_none());

        let (status, _) = put_contract(&router, "c-cache", &contract_body()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            state.engine.contract_registry.load("c-cache").is_some(),
            "PUT 后 registry 应立即加载到新契约"
        );
    }
}
