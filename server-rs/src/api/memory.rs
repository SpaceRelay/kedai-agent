// 记忆库路由(跨会话记忆蒸馏·落地项 2):
//   POST   /api/memory/distill   蒸馏指定会话(需开启 memory_distill_enabled)
//   GET    /api/memory           按角色列出全部记忆(全字段)
//   GET    /api/memory/search    全文检索(FTS5;短查询回退 LIKE)
//   POST   /api/memory           手动添加(kind='manual')
//   POST   /api/memory/prune     硬删除该角色 selected=0 的归档条目
//   PATCH  /api/memory/:id       编辑 content / selected / pinned
//   DELETE /api/memory/:id       删除
// 向量索引(Phase 3):
//   GET    /api/memory/embedding-status    向量索引状态(总数/已嵌入/维度/漂移)
//   POST   /api/memory/rebuild-embeddings  手动重建向量索引(清表 + 全量回填)
use crate::api::app_state::AppState;
use crate::api::{db_err, WithStatus};
use crate::models::types::{GenerationParams, LlmMessage, ToolChoice};
use crate::services::embedding_service::EmbeddingService;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct DistillBody {
    pub session_id: String,
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub character_id: Option<String>,
}

/// GET /api/memory/search?character_id=&q=&limit=(limit 默认 20,上限 100)
#[derive(Deserialize, Default)]
pub struct SearchQuery {
    pub character_id: Option<String>,
    pub q: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct CreateBody {
    pub character_id: String,
    pub content: String,
}

#[derive(Deserialize, Default)]
pub struct UpdateBody {
    pub content: Option<String>,
    pub selected: Option<bool>,
    pub pinned: Option<bool>,
}

#[derive(Deserialize, Default)]
pub struct PruneBody {
    /// 缺字段按空串处理 → 统一走 400(而非 axum Json 的 422)
    #[serde(default)]
    pub character_id: String,
}

/// POST /api/memory/distill:对指定会话蒸馏记忆。
/// LLM 走 engine 当前连接器(mock 可测);历史为空时直接返回 inserted=0 不调模型。
pub async fn distill(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DistillBody>,
) -> Response {
    // 设置快照:不留锁跨 await
    let enabled = state.settings_snapshot().memory_distill_enabled;
    if !enabled {
        return Json(json!({
            "error": "跨会话记忆蒸馏未开启,请先在设置中打开 memory_distill_enabled"
        }))
        .into_response()
        .with_status(StatusCode::BAD_REQUEST);
    }
    let session_id = body.session_id.trim().to_string();
    if session_id.is_empty() {
        return Json(json!({ "error": "缺少 session_id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    // 蒸馏生成参数(与 compaction 同取向:低温度、无工具、非流式)
    let params = GenerationParams {
        temperature: 0.3,
        top_p: 1.0,
        max_tokens: 1024,
        stop: None,
        tools: Vec::new(),
        max_tool_rounds: None,
        tool_choice: ToolChoice::Auto,
        parallel_tool_calls: None,
    };
    // 中止通道:HTTP 端点无 SSE 取消路径,恒 false(不中断)
    let (_abort_tx, abort_rx) = tokio::sync::watch::channel(false);
    let engine = state.engine.clone();
    let sessions = state.sessions.clone();
    let memory = state.memory.clone();
    let llm = move |messages: Vec<LlmMessage>| async move {
        let (text, _usage) = engine.generate_text(&messages, params, abort_rx).await?;
        Ok(text)
    };
    match memory.distill_session(&sessions, &session_id, llm).await {
        Ok(outcome) => {
            // Phase 3:只为本次新增的记忆补向量(不重扫全表)
            for id in &outcome.new_ids {
                let memory = state.memory.clone();
                let id = *id;
                if let Ok(Some(e)) = state.db_call(move || memory.get(id)).await {
                    embed_one_entry(&state, &e.id, &e.content).await;
                }
            }
            Json(json!({
                "ok": true,
                "inserted": outcome.inserted,
                "skipped": outcome.skipped,
                "character_id": outcome.character_id,
            }))
            .into_response()
        }
        Err(e) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
    }
}

/// GET /api/memory?character_id=:角色全部记忆(最新在前,含未选中条目与计数)
pub async fn list(State(state): State<Arc<AppState>>, Query(query): Query<ListQuery>) -> Response {
    let Some(character_id) = query.character_id.map(|c| c.trim().to_string()) else {
        return Json(json!({ "error": "缺少 character_id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    };
    if character_id.is_empty() {
        return Json(json!({ "error": "缺少 character_id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let svc = state.memory.clone();
    let memories = match state.db_call(move || svc.list(&character_id)).await {
        Ok(v) => v,
        Err(e) => return db_err(&e),
    };
    Json(json!({ "memories": memories })).into_response()
}

/// GET /api/memory/search?character_id=&q=&limit=:全文检索(FTS5 bm25 升序;
/// 查询 <3 字符回退 LIKE)。limit 默认 20、上限 100。
pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SearchQuery>,
) -> Response {
    let character_id = query
        .character_id
        .map(|c| c.trim().to_string())
        .unwrap_or_default();
    if character_id.is_empty() {
        return Json(json!({ "error": "缺少 character_id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let q = query.q.map(|q| q.trim().to_string()).unwrap_or_default();
    if q.is_empty() {
        return Json(json!({ "error": "缺少 q" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let svc = state.memory.clone();
    let memories = match state
        .db_call(move || svc.search(&character_id, &q, limit))
        .await
    {
        Ok(v) => v,
        Err(e) => return db_err(&e),
    };
    Json(json!({ "memories": memories })).into_response()
}

/// POST /api/memory/prune:硬删除该角色 selected=0 的归档条目,返回删除条数
pub async fn prune(State(state): State<Arc<AppState>>, Json(body): Json<PruneBody>) -> Response {
    let character_id = body.character_id.trim().to_string();
    if character_id.is_empty() {
        return Json(json!({ "error": "缺少 character_id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let svc = state.memory.clone();
    match state.db_call(move || svc.prune(&character_id)).await {
        Err(e) => db_err(&e),
        Ok(Err(e)) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
        Ok(Ok(removed)) => Json(json!({ "ok": true, "removed": removed })).into_response(),
    }
}

/// POST /api/memory:手动添加一条记忆(kind='manual')
pub async fn create(State(state): State<Arc<AppState>>, Json(body): Json<CreateBody>) -> Response {
    let character_id = body.character_id.trim().to_string();
    if character_id.is_empty() {
        return Json(json!({ "error": "缺少 character_id" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    if body.content.trim().is_empty() {
        return Json(json!({ "error": "记忆内容不能为空" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let svc = state.memory.clone();
    let content = body.content.clone();
    match state
        .db_call(move || svc.create_manual(&character_id, &content))
        .await
    {
        Err(e) => db_err(&e),
        Ok(Ok(entry)) => {
            // Phase 3:向量化开启时为新建记忆补向量(失败仅告警,不阻断写入)
            embed_one_entry(&state, &entry.id, &entry.content).await;
            Json(json!({ "ok": true, "memory": entry }))
                .into_response()
                .with_status(StatusCode::CREATED)
        }
        Ok(Err(e)) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
    }
}

/// 为单条记忆生成并写入向量(Phase 3 写入链路)。
/// 未启用/未配置/调用失败一律静默返回:向量是增强能力,不能影响记忆写入主流程。
async fn embed_one_entry(state: &Arc<AppState>, id: &i64, content: &str) {
    let settings = state.settings_snapshot();
    if !settings.embedding_enabled {
        return;
    }
    let svc = EmbeddingService::new();
    match svc.embed_one(&settings, content).await {
        Ok(v) if !v.is_empty() => {
            let memory = state.memory.clone();
            let id = *id;
            if let Err(e) = state.db_call(move || memory.upsert_vector(id, &v)).await {
                tracing::warn!(error = e, memory_id = id, "记忆向量写入失败");
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(
            error = e.to_string(),
            memory_id = *id,
            "记忆向量生成失败(可在设置中手动重建索引)"
        ),
    }
}

/// PATCH /api/memory/:id:编辑 content 与/或 selected(空白 content 拒绝)
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateBody>,
) -> Response {
    if let Some(c) = &body.content {
        if c.trim().is_empty() {
            return Json(json!({ "error": "记忆内容不能为空" }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST);
        }
    }
    let svc = state.memory.clone();
    let updated = state
        .db_call(move || svc.update(id, body.content.as_deref(), body.selected, body.pinned))
        .await;
    match updated {
        Err(e) => db_err(&e),
        Ok(Some(entry)) => Json(json!({ "ok": true, "memory": entry })).into_response(),
        Ok(None) => Json(json!({ "error": format!("记忆 {id} 不存在或更新被拒绝") }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND),
    }
}

/// DELETE /api/memory/:id
pub async fn delete(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    let svc = state.memory.clone();
    match state.db_call(move || svc.delete(id)).await {
        Err(e) => db_err(&e),
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => Json(json!({ "error": format!("记忆 {id} 不存在") }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND),
    }
}

/// GET /api/memory/embedding-status:向量索引状态(供设置界面展示进度与漂移提示)
pub async fn embedding_status(State(state): State<Arc<AppState>>) -> Response {
    let settings = state.settings_snapshot();
    let svc = state.memory.clone();
    let status = match state.db_call(move || svc.vector_status()).await {
        Ok(s) => s,
        Err(e) => return db_err(&e),
    };
    Json(json!({
        "enabled": settings.embedding_enabled,
        "model": settings.embedding_model,
        "configured_dim": settings.embedding_dim,
        "status": status,
    }))
    .into_response()
}

/// POST /api/memory/rebuild-embeddings:手动重建向量索引。
/// 流程:清空向量表 → 按批取缺失向量的记忆 → 调 embedding → 批量写入。
/// 采用同步阻塞式(记忆量级几千条,单次请求可完成);失败即中断并回报已处理条数。
pub async fn rebuild_embeddings(State(state): State<Arc<AppState>>) -> Response {
    let settings = state.settings_snapshot();
    if !settings.embedding_enabled {
        return Json(json!({ "error": "向量化未开启,请先在「向量化模型」中配置并启用" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    // 先做一次连接测试,拿到真实维度并校验配置(避免中途失败才报错)
    let svc = EmbeddingService::new();
    let (probe_dim, _ms) = match svc.test(&settings).await {
        Ok(v) => v,
        Err(e) => {
            return Json(json!({ "error": e.to_string() }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST);
        }
    };
    // 清空旧表并建新表(维度以实测为准)
    let memory = state.memory.clone();
    if let Err(e) = state.db_call(move || memory.drop_vec_table()).await {
        return db_err(&e);
    }

    const BATCH: usize = 64;
    let mut done = 0usize;
    loop {
        let memory = state.memory.clone();
        let pending = match state
            .db_call(move || memory.entries_missing_vectors(BATCH))
            .await
        {
            Ok(v) => v,
            Err(e) => return db_err(&e),
        };
        if pending.is_empty() {
            break;
        }
        let texts: Vec<String> = pending.iter().map(|e| e.content.clone()).collect();
        let vecs = match svc.embed(&settings, &texts).await {
            Ok(v) => v,
            Err(e) => {
                return Json(json!({
                    "error": format!("{};已成功处理 {done} 条", e),
                    "done": done,
                }))
                .into_response()
                .with_status(StatusCode::BAD_REQUEST);
            }
        };
        let items: Vec<(i64, Vec<f32>)> = pending
            .iter()
            .zip(vecs)
            .map(|(e, v)| (e.id, v))
            .collect();
        let n = items.len();
        let memory = state.memory.clone();
        if let Err(e) = state.db_call(move || memory.upsert_vectors(&items)).await {
            return db_err(&e);
        }
        done += n;
        // 防御:单批返回条数为 0 且仍有 pending,说明无法推进,避免死循环
        if n == 0 {
            break;
        }
    }
    Json(json!({ "ok": true, "embedded": done, "dim": probe_dim })).into_response()
}
