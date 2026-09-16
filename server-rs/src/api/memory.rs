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
use crate::api::{db_err, not_found, validation, WithStatus};
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
        return validation("跨会话记忆蒸馏未开启,请先在设置中打开 memory_distill_enabled");
    }
    let session_id = body.session_id.trim().to_string();
    if session_id.is_empty() {
        return validation("缺少 session_id");
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
            // Phase 3:只为本次新增的记忆补向量(不重扫全表)。
            // 2026-09-16 性能批次 P-5:此前是「逐条 get + 逐条 embed」的两层 N+1,
            // 且每次都新建 EmbeddingService(丢连接池)。现改为一次批量取回、
            // 一请求批量嵌入,并把向量一次事务写回;顺序由 new_ids 对齐保证。
            embed_entries_batch(&state, &outcome.new_ids).await;
            Json(json!({
                "ok": true,
                "inserted": outcome.inserted,
                "skipped": outcome.skipped,
                "character_id": outcome.character_id,
            }))
            .into_response()
        }
        Err(e) => validation(e),
    }
}

/// GET /api/memory?character_id=:角色全部记忆(最新在前,含未选中条目与计数)
pub async fn list(State(state): State<Arc<AppState>>, Query(query): Query<ListQuery>) -> Response {
    let Some(character_id) = query.character_id.map(|c| c.trim().to_string()) else {
        return validation("缺少 character_id");
    };
    if character_id.is_empty() {
        return validation("缺少 character_id");
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
        return validation("缺少 character_id");
    }
    let q = query.q.map(|q| q.trim().to_string()).unwrap_or_default();
    if q.is_empty() {
        return validation("缺少 q");
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
        return validation("缺少 character_id");
    }
    let svc = state.memory.clone();
    match state.db_call(move || svc.prune(&character_id)).await {
        Err(e) => db_err(&e),
        Ok(Err(e)) => validation(e),
        Ok(Ok(removed)) => Json(json!({ "ok": true, "removed": removed })).into_response(),
    }
}

/// POST /api/memory:手动添加一条记忆(kind='manual')
pub async fn create(State(state): State<Arc<AppState>>, Json(body): Json<CreateBody>) -> Response {
    let character_id = body.character_id.trim().to_string();
    if character_id.is_empty() {
        return validation("缺少 character_id");
    }
    if body.content.trim().is_empty() {
        return validation("记忆内容不能为空");
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
            embed_one_entry(&state, entry.id).await;
            Json(json!({ "ok": true, "memory": entry }))
                .into_response()
                .with_status(StatusCode::CREATED)
        }
        Ok(Err(e)) => validation(e),
    }
}

/// 为单条记忆生成并写入向量(Phase 3 写入链路)。
/// 未启用/未配置/调用失败一律静默返回:向量是增强能力,不能影响记忆写入主流程。
/// 内容按 id 从库中读(不接收调用方传入的 content,避免出现「传进来的内容」
/// 与「库中实际内容」两个真值源分叉)。
async fn embed_one_entry(state: &Arc<AppState>, id: i64) {
    embed_entries_batch(state, &[id]).await;
}

/// 批量为若干记忆补向量(2026-09-16 性能批次 P-5)。
///
/// 此前蒸馏后的补向量是两层 N+1:外层逐 id `db_call(get)` 取内容,内层逐条
/// `embed_one`(每次还新建 EmbeddingService)。改为:
///   ① 一次 `get_many` 取回全部内容(单条 SELECT);
///   ② 一次 `embed`(内部已按 `EMBED_BATCH_SIZE=32` 分片)拿到全部向量;
///   ③ 一次 `upsert_vectors`(单事务)写回。
/// 顺序由 `get_many` 的入参顺序保证,向量与 id 一一对齐。
///
/// 失败语义与逐条版一致:未启用直接返回;取库/嵌入/写库失败只 warn,
/// **绝不阻断**记忆写入主流程(向量是增强能力)。
async fn embed_entries_batch(state: &Arc<AppState>, ids: &[i64]) {
    if ids.is_empty() {
        return;
    }
    let settings = state.settings_snapshot();
    if !settings.embedding_enabled {
        return;
    }
    // ① 一次取回全部内容(保持与 ids 同序,跳过已不存在的 id)
    let memory = state.memory.clone();
    let ids_owned: Vec<i64> = ids.to_vec();
    let entries = match state.db_call(move || memory.get_many(&ids_owned)).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = e, "记忆内容批量读取失败,跳过向量生成");
            return;
        }
    };
    if entries.is_empty() {
        return;
    }
    let texts: Vec<String> = entries.iter().map(|e| e.content.clone()).collect();
    // ② 一次批量嵌入(内部按批大小自动分片)
    let svc = EmbeddingService::new();
    let vecs = match svc.embed(&settings, &texts).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = e.to_string(),
                count = entries.len(),
                "记忆向量批量生成失败(可在设置中手动重建索引)"
            );
            return;
        }
    };
    if vecs.len() != entries.len() {
        tracing::warn!(
            expected = entries.len(),
            got = vecs.len(),
            "向量返回条数与请求不一致,本次放弃写入"
        );
        return;
    }
    // ③ 一次事务写回
    let items: Vec<(i64, Vec<f32>)> = entries
        .iter()
        .zip(vecs)
        .filter(|(_, v)| !v.is_empty())
        .map(|(e, v)| (e.id, v))
        .collect();
    if items.is_empty() {
        return;
    }
    let memory = state.memory.clone();
    if let Err(e) = state.db_call(move || memory.upsert_vectors(&items)).await {
        tracing::warn!(error = e, "记忆向量批量写入失败");
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
            return validation("记忆内容不能为空");
        }
    }
    let svc = state.memory.clone();
    let updated = state
        .db_call(move || svc.update(id, body.content.as_deref(), body.selected, body.pinned))
        .await;
    match updated {
        Err(e) => db_err(&e),
        Ok(Some(entry)) => Json(json!({ "ok": true, "memory": entry })).into_response(),
        Ok(None) => not_found(format!("记忆 {id} 不存在或更新被拒绝")),
    }
}

/// DELETE /api/memory/:id
pub async fn delete(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    let svc = state.memory.clone();
    match state.db_call(move || svc.delete(id)).await {
        Err(e) => db_err(&e),
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found(format!("记忆 {id} 不存在")),
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
        return validation("向量化未开启,请先在「向量化模型」中配置并启用");
    }
    // 先做一次连接测试,拿到真实维度并校验配置(避免中途失败才报错)
    let svc = EmbeddingService::new();
    let (probe_dim, _ms) = match svc.test(&settings).await {
        Ok(v) => v,
        Err(e) => {
            return validation(e.to_string());
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
        let items: Vec<(i64, Vec<f32>)> =
            pending.iter().zip(vecs).map(|(e, v)| (e.id, v)).collect();
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
