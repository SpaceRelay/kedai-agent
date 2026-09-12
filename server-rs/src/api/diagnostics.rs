// 诊断路由(缓存感知管线):GET /api/diagnostics/cache
// 返回近 N 条请求的缓存命中率(加权 token 口径)、逐条明细、按可配置单价估算的
// 费用与节省,以及输入 tokens 对照 max_context_tokens 的四级水位报告。
use crate::api::app_state::AppState;
use crate::services::cache_diagnostics::{summarize, watermark, CachePricing, CacheUsageRow};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct CacheQuery {
    /// 会话过滤(缺省 = 全部会话合并统计)
    pub session_id: Option<String>,
    /// 统计窗口条数(默认 20,1..=500)
    pub window: Option<usize>,
}

/// GET /api/diagnostics/cache:缓存命中率、费用节省与上下文水位
pub async fn cache(State(state): State<Arc<AppState>>, Query(q): Query<CacheQuery>) -> Response {
    let window = q.window.unwrap_or(20).clamp(1, 500);
    // 近 N 条请求(时间正序返回,便于前端画趋势)
    let rows: Vec<CacheUsageRow> = {
        let conn = match state.db.read() {
            Ok(c) => c,
            Err(e) => {
                return Json(json!({ "error": e })).into_response();
            }
        };
        let sql = if q.session_id.is_some() {
            "SELECT session_id, created_at, prompt_tokens, completion_tokens,
                    prompt_cache_hit_tokens, prompt_cache_miss_tokens
             FROM llm_requests WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2"
        } else {
            "SELECT session_id, created_at, prompt_tokens, completion_tokens,
                    prompt_cache_hit_tokens, prompt_cache_miss_tokens
             FROM llm_requests ORDER BY id DESC LIMIT ?2"
        };
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<CacheUsageRow> {
            Ok(CacheUsageRow {
                session_id: row.get(0)?,
                created_at: row.get(1)?,
                prompt_tokens: row.get(2)?,
                completion_tokens: row.get(3)?,
                hit: row.get(4)?,
                miss: row.get(5)?,
            })
        };
        let result: Result<Vec<_>, _> = if let Some(sid) = &q.session_id {
            let mut stmt = match conn.prepare(sql) {
                Ok(s) => s,
                Err(e) => {
                    return Json(json!({ "error": format!("读取缓存统计失败: {e}") }))
                        .into_response();
                }
            };
            stmt.query_map(rusqlite::params![sid, window as i64], map_row)
                .and_then(|rows| rows.collect())
        } else {
            let mut stmt = match conn.prepare(sql) {
                Ok(s) => s,
                Err(e) => {
                    return Json(json!({ "error": format!("读取缓存统计失败: {e}") }))
                        .into_response();
                }
            };
            stmt.query_map(rusqlite::params![window as i64], map_row)
                .and_then(|rows| rows.collect())
        };
        match result {
            Ok(mut v) => {
                v.reverse(); // DESC 取最近 N 条 → 反转为时间正序
                v
            }
            Err(e) => {
                return Json(json!({ "error": format!("读取缓存统计失败: {e}") })).into_response();
            }
        }
    };
    // 单价:服务内常量默认(DeepSeek 参考价),后续可挪到设置
    let pricing = CachePricing::default();
    let summary = summarize(&rows, &pricing);
    // 水位:以窗口内最新一条请求的 prompt_tokens 为输入侧 token,
    // 对照设置中的 max_context_tokens(设置快照:不留锁跨 await)
    let max_context = state.settings_snapshot().max_context_tokens;
    let latest_prompt = rows.last().map(|r| r.prompt_tokens).unwrap_or(0);
    let level = watermark(latest_prompt, max_context);
    Json(json!({
        "window": window,
        "session_id": q.session_id,
        "totals": {
            "count": summary.count,
            "total_prompt": summary.total_prompt,
            "total_completion": summary.total_completion,
            "total_hit": summary.total_hit,
            "total_miss": summary.total_miss,
            "hit_rate": summary.hit_rate,
        },
        "entries": rows.iter().map(|r| json!({
            "session_id": r.session_id,
            "created_at": r.created_at,
            "prompt_tokens": r.prompt_tokens,
            "completion_tokens": r.completion_tokens,
            "hit": r.hit,
            "miss": r.miss,
        })).collect::<Vec<_>>(),
        "pricing": {
            "cache_hit_per_m": pricing.cache_hit_per_m,
            "input_per_m": pricing.input_per_m,
            "output_per_m": pricing.output_per_m,
            "currency_hint": "CNY/1M",
        },
        "cost": summary.cost,
        "saved": summary.saved,
        "watermark": {
            "level": level.level,
            "ratio": level.ratio,
            "input_tokens": latest_prompt,
            "max_context_tokens": max_context,
            "next_level": level.next_level,
            "gap_tokens": level.gap_tokens,
        },
    }))
    .into_response()
}
