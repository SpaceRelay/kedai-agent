// 7 作用域变量 API(计划二):/api/variables 的 GET/PUT/PATCH。
// - GET:读指定作用域整树(scope 缺省 = chat,保持既有语义);
//   chat 以 session_assistant_vars 为规范存储;message 优先 scope_variables 镜像、
//   缺失回退该消息 extra.mvu.stat_data;其余作用域读 scope_variables 表。
// - PUT:整树覆写(scope=chat 内部走 save_assistant_vars,保持既有路径)。
// - PATCH:对指定作用域应用 JSON Patch 子集(复用 AssistantVars::apply_patches 校验)。
use crate::api::app_state::AppState;
use crate::api::{db_err, internal, not_found, validation};
use crate::parsing::scopes::Scope;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct VariablesQuery {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub scope_id: Option<String>,
}

#[derive(Deserialize)]
pub struct VariablesWriteBody {
    pub session_id: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub scope_id: Option<String>,
    /// 整树数据(PUT);PATCH 时为 JSON Patch 数组
    pub data: Value,
}

/// 解析作用域参数:缺省 = chat(保持既有语义);非法值返回 None
fn resolve_scope(q: &VariablesQuery) -> Option<Scope> {
    match q.scope.as_deref() {
        None => Some(Scope::Chat),
        Some(s) => Scope::from_str(s),
    }
}

/// GET /api/variables?session_id=&scope=&scope_id=:读指定作用域整树。
/// scope 缺省 = chat(既有语义);message 回退链:scope_variables 镜像 → 消息 extra.mvu。
pub async fn get_variables(
    State(state): State<Arc<AppState>>,
    Query(q): Query<VariablesQuery>,
) -> Response {
    let Some(sid) = q.session_id.as_deref() else {
        return validation("缺少 session_id 查询参数");
    };
    let Some(scope) = resolve_scope(&q) else {
        return validation("scope 须为 global|chat|character|preset|message|script|extension");
    };
    let sid_owned = sid.to_string();
    let scope_id = q.scope_id.clone().unwrap_or_default();
    // 三个分支均为同步 SQLite 读取,整体挪进阻塞线程池(DB 并发改造)
    let svc = state.sessions.clone();
    let sid_c = sid_owned.clone();
    let scope_id_c = scope_id.clone();
    let data = match state
        .db_call(move || match scope {
            Scope::Chat => {
                let vars = svc.load_assistant_vars(&sid_c);
                vars.tree().clone()
            }
            Scope::Message => {
                // 优先 scope_variables 镜像,缺失回退该消息 extra.mvu.stat_data
                if let Some(v) = svc.load_scope_variables("message", &scope_id_c) {
                    v
                } else {
                    match svc.get_message(&sid_c, scope_id_c.parse().unwrap_or(-1)) {
                        Some(m) => m
                            .extra
                            .get("mvu")
                            .and_then(|mvu| mvu.get("stat_data"))
                            .cloned()
                            .unwrap_or(Value::Object(Default::default())),
                        None => Value::Object(Default::default()),
                    }
                }
            }
            other => svc
                .load_scope_variables(other.as_str(), &scope_id_c)
                .unwrap_or_else(|| Value::Object(Default::default())),
        })
        .await
    {
        Ok(v) => v,
        Err(e) => return db_err(&e),
    };
    Json(json!({ "scope": scope.as_str(), "scope_id": scope_id, "data": data })).into_response()
}

/// PUT /api/variables:整树覆写指定作用域。scope=chat 内部走 save_assistant_vars
/// (保持既有路径,同步 chat 镜像),其余作用域写 scope_variables 表。
pub async fn put_variables(
    State(state): State<Arc<AppState>>,
    Json(body): Json<VariablesWriteBody>,
) -> Response {
    let scope = match body.scope.as_deref().and_then(Scope::from_str) {
        Some(s) => s,
        None => {
            return validation("scope 须为 global|chat|character|preset|message|script|extension")
        }
    };
    let scope_id = body.scope_id.clone().unwrap_or_default();
    // 存在性校验 + 写入(chat 分支含镜像同步)合并进同一阻塞任务(DB 并发改造)
    let svc = state.sessions.clone();
    let sid = body.session_id.clone();
    let data = body.data.clone();
    let written = state
        .db_call(move || {
            svc.get(&sid)?;
            Some(match scope {
                Scope::Chat => {
                    let vars = crate::parsing::assistant::AssistantVars::from_value(data);
                    if let Err(e) = svc.save_assistant_vars(&sid, &vars) {
                        return Some(Err(e));
                    }
                    // 同步 chat 镜像(scope_variables 表):保持一致,启动回填的幂等语义
                    svc.save_scope_variables("chat", &sid, vars.tree());
                    Ok(())
                }
                other => {
                    svc.save_scope_variables(other.as_str(), &scope_id, &data);
                    Ok(())
                }
            })
        })
        .await;
    match written {
        Err(e) => return db_err(&e),
        Ok(None) => return not_found("会话不存在"),
        Ok(Some(Err(e))) => return internal(&e),
        Ok(Some(Ok(()))) => {}
    }
    Json(json!({ "ok": true })).into_response()
}

/// PATCH /api/variables:对指定作用域应用 JSON Patch 子集
/// (body.data 为 JSON Patch 数组;op 支持 replace/set/insert/delta/remove/move)。
/// 复用 AssistantVars::apply_patches 校验;chat 写回 session_assistant_vars,
/// 其余作用域写 scope_variables 表。
pub async fn patch_variables(
    State(state): State<Arc<AppState>>,
    Json(body): Json<VariablesWriteBody>,
) -> Response {
    let scope = match body.scope.as_deref().and_then(Scope::from_str) {
        Some(s) => s,
        None => {
            return validation("scope 须为 global|chat|character|preset|message|script|extension")
        }
    };
    let Some(ops) = crate::parsing::assistant::parse_patch_array(&body.data) else {
        return validation("patch 须为 JSON Patch 数组(replace/set/insert/delta/remove/move)");
    };
    let scope_id = body.scope_id.clone().unwrap_or_default();
    // 存在性校验 + 读树 + 应用 patch + 写回(含 chat 镜像)合并进同一阻塞任务(DB 并发改造);
    // 结果编码:None=会话不存在;Err((true, msg))=patch 校验失败;Err((false, msg))=写库失败
    let svc = state.sessions.clone();
    let sid = body.session_id.clone();
    let patched = state
        .db_call(move || {
            svc.get(&sid)?;
            Some((|| -> Result<(), (bool, String)> {
                match scope {
                    Scope::Chat => {
                        let mut vars = svc.load_assistant_vars(&sid);
                        vars.apply_patches(&ops)
                            .map_err(|e| (true, format!("应用 patch 失败: {e}")))?;
                        svc.save_assistant_vars(&sid, &vars)
                            .map_err(|e| (false, e))?;
                        svc.save_scope_variables("chat", &sid, vars.tree());
                    }
                    other => {
                        let mut vars = crate::parsing::assistant::AssistantVars::from_value(
                            svc.load_scope_variables(other.as_str(), &scope_id)
                                .unwrap_or_else(|| Value::Object(Default::default())),
                        );
                        vars.apply_patches(&ops)
                            .map_err(|e| (true, format!("应用 patch 失败: {e}")))?;
                        svc.save_scope_variables(other.as_str(), &scope_id, vars.tree());
                    }
                }
                Ok(())
            })())
        })
        .await;
    match patched {
        Err(e) => return db_err(&e),
        Ok(None) => return not_found("会话不存在"),
        Ok(Some(Err((true, e)))) => return validation(&e),
        Ok(Some(Err((false, e)))) => return internal(&e),
        Ok(Some(Ok(()))) => {}
    }
    Json(json!({ "ok": true })).into_response()
}
