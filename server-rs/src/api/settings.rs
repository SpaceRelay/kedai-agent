// 设置路由:/api/settings(连接测试/模型列表/信息/模型切换/运行期设置读写)
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use crate::services::settings_service::{
    normalize_base_url, RuntimeSettings, DEFAULT_SEARCH_ENDPOINT,
};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct SwitchModelBody {
    pub model: String,
}

#[derive(Deserialize, Default)]
pub struct UpdateSettingsBody {
    #[serde(default)]
    pub openai_base_url: Option<String>,
    /// 非空才替换;空/缺省表示保持不变
    #[serde(default)]
    pub openai_api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub default_temperature: Option<f64>,
    #[serde(default)]
    pub default_top_p: Option<f64>,
    #[serde(default)]
    pub default_max_tokens: Option<u32>,
    #[serde(default)]
    pub max_context_tokens: Option<u32>,
    /// Agent 系统提示词(空 = 用内置默认);设置里编辑,支持占位符
    #[serde(default)]
    pub agent_system_prompt: Option<String>,
    /// 搜索端点(空 = 用默认 DuckDuckGo HTML 接口)
    #[serde(default)]
    pub search_endpoint: Option<String>,
    /// mvu 变量状态注入位置(system / user_tail;非法值忽略)
    #[serde(default)]
    pub mvu_vars_position: Option<String>,
    /// 反思提示词(空 = 机械规则检查;非空 = LLM 反思;允许清空回退规则检查)
    #[serde(default)]
    pub reflect_prompt: Option<String>,
    /// 复杂模式预设尾部提示词(空 = 禁用;允许清空)
    #[serde(default)]
    pub preset_tail_prompt: Option<String>,
    /// 预设尾部提示词注入角色(user / assistant)
    #[serde(default)]
    pub preset_tail_role: Option<String>,
    /// 反思失败建议提示词(空 = 禁用;允许清空)
    #[serde(default)]
    pub reflect_advice_prompt: Option<String>,
    /// 反思失败建议提示词注入角色(user / assistant)
    #[serde(default)]
    pub reflect_advice_role: Option<String>,
    /// 放行模式:true = 除黑名单外自动放行
    #[serde(default)]
    pub bypass_mode: Option<bool>,
    /// 放行模式黑名单
    #[serde(default)]
    pub bypass_blacklist: Option<Vec<String>>,
    /// AGENT/CUSTOM 模式工具循环轮次上限(1..=200;缺省保持不变)
    #[serde(default)]
    pub max_tool_rounds: Option<u32>,
    /// HTML 渲染开关(状态栏脚本执行前置条件)
    #[serde(default)]
    pub render_html: Option<bool>,
    /// 上下文压缩模式(off / manual / auto;非法值忽略)
    #[serde(default)]
    pub compaction_mode: Option<String>,
    /// 上下文压缩触发阈值(0.5..=0.95)
    #[serde(default)]
    pub compaction_threshold: Option<f32>,
    /// LLM 请求快照开关(第四点·主题 A)
    #[serde(default)]
    pub llm_request_log: Option<bool>,
}

/// 序列化运行期设置(API Key 脱敏)
fn settings_json(s: &RuntimeSettings) -> Value {
    json!({
        "openai_base_url": s.openai_base_url,
        "api_key_masked": s.masked_api_key(),
        "has_api_key": !s.openai_api_key.is_empty(),
        "model": s.model,
        "default_temperature": s.default_temperature,
        "default_top_p": s.default_top_p,
        "default_max_tokens": s.default_max_tokens,
        "max_context_tokens": s.max_context_tokens,
        "agent_system_prompt": s.agent_system_prompt,
        "search_endpoint": s.search_endpoint,
        "mvu_vars_position": s.mvu_vars_position,
        "reflect_prompt": s.reflect_prompt,
        "preset_tail_prompt": s.preset_tail_prompt,
        "preset_tail_role": s.preset_tail_role,
        "reflect_advice_prompt": s.reflect_advice_prompt,
        "reflect_advice_role": s.reflect_advice_role,
        "bypass_mode": s.bypass_mode,
        "bypass_blacklist": s.bypass_blacklist,
        "max_tool_rounds": s.max_tool_rounds,
        "render_html": s.render_html,
        "compaction_mode": s.compaction_mode,
        "compaction_threshold": s.compaction_threshold,
        "llm_request_log": s.llm_request_log,
    })
}

/// GET /api/settings:当前运行期设置(供前端表单回填)
pub async fn get_settings(State(state): State<Arc<AppState>>) -> Json<Value> {
    let s = state.settings.lock().unwrap_or_else(|e| e.into_inner());
    Json(settings_json(&s))
}

/// PUT /api/settings:更新运行期设置(部分字段;Base URL/Key 变更立即重建连接器,模型变更立即切换)
pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<UpdateSettingsBody>,
) -> Response {
    // 事务锁覆盖“读取当前值 → 应用 patch → 原子落盘 → 替换内存”，防止并发部分更新丢字段。
    // 只持有 tokio MutexGuard；std::sync::MutexGuard 均在同步代码块内释放，不跨 await。
    let _update_guard = state.settings_update.lock().await;
    let (mut candidate, old_base, old_key, old_model) = {
        let current = state.settings.lock().unwrap_or_else(|e| e.into_inner());
        (
            current.clone(),
            current.openai_base_url.clone(),
            current.openai_api_key.clone(),
            current.model.clone(),
        )
    };
    {
        let s = &mut candidate;

        if let Some(v) = &body.openai_base_url {
            // 自动补全格式:补协议、补 /v1(缺 /v1 会导致 /models 请求 404)
            let t = normalize_base_url(v);
            if !t.is_empty() {
                s.openai_base_url = t;
            }
        }
        if let Some(v) = &body.openai_api_key {
            let t = v.trim().to_string();
            if !t.is_empty() {
                s.openai_api_key = t;
            }
        }
        if let Some(v) = &body.model {
            let t = v.trim().to_string();
            if !t.is_empty() {
                s.model = t;
            }
        }
        if let Some(v) = body.default_temperature {
            if (0.0..=2.0).contains(&v) {
                s.default_temperature = v;
            }
        }
        if let Some(v) = body.default_top_p {
            if (0.0..=1.0).contains(&v) {
                s.default_top_p = v;
            }
        }
        if let Some(v) = body.default_max_tokens {
            if v == 0 || v > 65_536 {
                return Json(json!({ "error": "default_max_tokens 必须在 1..=65536" }))
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST);
            }
            s.default_max_tokens = v;
        }
        if let Some(v) = body.max_context_tokens {
            if v < 65_536 || v > 1_048_576 {
                return Json(json!({ "error": "max_context_tokens 必须在 65536..=1048576(64K~1M)" }))
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST);
            }
            s.max_context_tokens = v;
        }
        // Agent 系统提示词:允许清空(空 = 用内置默认)
        if let Some(v) = &body.agent_system_prompt {
            s.agent_system_prompt = v.clone();
        }
        // 搜索端点:允许清空(空 = 用默认 DDG 端点)
        if let Some(v) = &body.search_endpoint {
            let t = v.trim().to_string();
            s.search_endpoint = if t.is_empty() {
                DEFAULT_SEARCH_ENDPOINT.to_string()
            } else {
                t
            };
        }
        // mvu 变量注入位置:仅接受 system / user_tail
        if let Some(v) = &body.mvu_vars_position {
            let t = v.trim().to_string();
            if t == "system" || t == "user_tail" {
                s.mvu_vars_position = t;
            }
        }
        // 反思提示词:允许清空(空 = 回退机械规则检查)
        if let Some(v) = &body.reflect_prompt {
            s.reflect_prompt = v.clone();
        }
        // 预设尾部提示词:允许清空(空 = 禁用位置0 预设尾部)
        if let Some(v) = &body.preset_tail_prompt {
            s.preset_tail_prompt = v.clone();
        }
        // 预设尾部注入角色:仅接受 user / assistant
        if let Some(v) = &body.preset_tail_role {
            let t = v.trim().to_string();
            if t == "user" || t == "assistant" {
                s.preset_tail_role = t;
            }
        }
        // 反思失败建议提示词:允许清空(空 = 禁用)
        if let Some(v) = &body.reflect_advice_prompt {
            s.reflect_advice_prompt = v.clone();
        }
        // 反思建议注入角色:仅接受 user / assistant
        if let Some(v) = &body.reflect_advice_role {
            let t = v.trim().to_string();
            if t == "user" || t == "assistant" {
                s.reflect_advice_role = t;
            }
        }
        // 放行模式
        if let Some(v) = body.bypass_mode {
            s.bypass_mode = v;
        }
        // 放行模式黑名单
        if let Some(v) = &body.bypass_blacklist {
            s.bypass_blacklist = v.clone();
        }
        // 工具循环轮次上限:仅接受 1..=200(防止误填 0 或超大值打爆模型请求)
        if let Some(v) = body.max_tool_rounds {
            if !(1..=200).contains(&v) {
                return Json(json!({ "error": "max_tool_rounds 必须在 1..=200" }))
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST);
            }
            s.max_tool_rounds = v;
        }
        // HTML 渲染开关
        if let Some(v) = body.render_html {
            s.render_html = v;
        }
        // 上下文压缩模式:仅接受 off / manual / auto
        if let Some(v) = &body.compaction_mode {
            let t = v.trim().to_string();
            if t == "off" || t == "manual" || t == "auto" {
                s.compaction_mode = t;
            }
        }
        // 上下文压缩阈值:仅接受 0.5..=0.95
        if let Some(v) = body.compaction_threshold {
            if !(0.5..=0.95).contains(&v) {
                return Json(json!({ "error": "compaction_threshold 必须在 0.5..=0.95" }))
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST);
            }
            s.compaction_threshold = v;
        }
        // LLM 请求快照开关
        if let Some(v) = body.llm_request_log {
            s.llm_request_log = v;
        }
    }

    if let Err(e) = candidate.save(&state.config.data_dir) {
        return Json(json!({ "error": format!("设置保存失败: {e}") }))
            .into_response()
            .with_status(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let connector_changed =
        candidate.openai_base_url != old_base || candidate.openai_api_key != old_key;
    let model_changed = candidate.model != old_model;
    let model_name = candidate.model.clone();
    *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = candidate.clone();

    // Base URL / API Key / 模型变更 → 重建连接器。
    // 关键:当前为 mock 但保存了非空 Base URL 或 API Key 时,自动切换到 openai-compatible,
    // 否则用户填写的 API 设置永远不会生效(模型列表始终只有 mock-demo)。
    if connector_changed || model_changed {
        let (base_url, api_key, model) = (
            candidate.openai_base_url.clone(),
            candidate.openai_api_key.clone(),
            candidate.model.clone(),
        );
        let type_name = state.engine.connector.read().await.type_name().to_string();
        let target = if type_name == "mock" && (!base_url.is_empty() || !api_key.is_empty()) {
            "openai-compatible"
        } else {
            &type_name
        };
        let new_connector = crate::connectors::build_connector(target, &base_url, &api_key, &model);
        *state.engine.connector.write().await = new_connector;
    }

    // 模型变更 → 同步 engine 与 AppState.model
    if model_changed {
        state.engine.switch_model(&model_name).await;
        *state.model.lock().unwrap_or_else(|e| e.into_inner()) = model_name;
    }

    Json(json!({ "ok": true, "settings": settings_json(&candidate) })).into_response()
}

/// POST /api/settings/refresh-models:向已保存的 API 请求可用模型列表(立即生效,不保存)
pub async fn refresh_models(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let connector = state.engine.connector.read().await;
    let models = connector.available_models().await;
    // openai-compatible 下若只拿到回退的 1 个当前模型,大概率是服务不支持 /models 接口
    let message = if connector.type_name() == "openai-compatible" && models.len() <= 1 {
        Some(format!(
            "API 未返回完整模型列表(该服务可能不支持 /models 接口);已保留当前模型「{}」",
            models.first().cloned().unwrap_or_default()
        ))
    } else {
        None
    };
    Json(json!({ "ok": true, "models": models, "message": message }))
}

/// POST /api/settings/connect:测试连接
pub async fn connect(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let diagnostic = state.engine.connector.read().await.test().await;
    Json(diagnostic)
}

/// GET /api/settings/models:可用模型列表
pub async fn models(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let models = state.engine.connector.read().await.available_models().await;
    Json(json!({ "models": models }))
}

/// GET /api/settings/info:连接器信息 + 模型列表 + 可用连接器
pub async fn info(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let connector = state.engine.connector.read().await;
    let type_name = connector.type_name();
    let models = connector.available_models().await;
    let model = connector.model().to_string();
    Json(json!({
        "connector": type_name,
        "model": model,
        "models": models,
        "availableConnectors": crate::connectors::available_connector_types(),
    }))
}

/// PUT /api/settings/model:切换模型(立即生效)
pub async fn switch_model(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SwitchModelBody>,
) -> Response {
    let model = body.model.trim().to_string();
    if model.is_empty() {
        return Json(json!({ "error": "缺少 model" }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST);
    }
    let changed = state.engine.model() != model;
    if changed {
        state.engine.switch_model(&model).await;
        *state.model.lock().unwrap_or_else(|e| e.into_inner()) = model.clone();
    }
    Json(json!({ "ok": true, "model": model, "changed": changed })).into_response()
}

/// GET /api/settings/model:当前模型
pub async fn get_model(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(json!({ "model": state.engine.model() }))
}

/// GET /api/settings/agent-prompt:读取 DATA_DIR 中的运行时主 Agent 提示词。
/// 新文件缺失时由共享文件服务兼容读取/迁移旧 AGENTS_RUNTIME.md。
pub async fn get_agent_prompt(State(state): State<Arc<AppState>>) -> Response {
    let path = state.runtime_prompt.path().display().to_string();
    match state.runtime_prompt.read() {
        Ok(content) => {
            Json(json!({ "ok": true, "path": path, "content": content })).into_response()
        }
        Err(e) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::NOT_FOUND),
    }
}

#[derive(Deserialize)]
pub struct SaveAgentPromptBody {
    pub content: String,
}

#[derive(Deserialize, Default)]
pub struct PromptPreviewQuery {
    pub session_id: Option<String>,
    pub character_id: Option<String>,
}

#[derive(Serialize)]
struct PromptPreviewLayer {
    source: String,
    role: String,
    layer: u8,
    order: usize,
    content: String,
}

fn push_preview_layer(
    layers: &mut Vec<PromptPreviewLayer>,
    source: impl Into<String>,
    role: impl Into<String>,
    layer: u8,
    content: impl Into<String>,
) {
    let content = content.into();
    if content.trim().is_empty() {
        return;
    }
    layers.push(PromptPreviewLayer {
        source: source.into(),
        role: role.into(),
        layer,
        order: layers.len(),
        content,
    });
}

fn stable_history_hash(text: &str) -> String {
    // 非加密用途：只用于预览中识别“内容是否变化”，不返回原文。
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// GET /api/settings/prompt-preview：按最终注入顺序输出来源层。
/// 历史层仅含 role/字符长度/稳定哈希，绝不返回聊天正文或连接密钥。
pub async fn prompt_preview(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PromptPreviewQuery>,
) -> Response {
    let mut layers = Vec::new();
    match state.runtime_prompt.read_optional() {
        Ok(Some(runtime)) => {
            push_preview_layer(&mut layers, "runtime_prompt", "system", 5, runtime)
        }
        Ok(None) => {}
        Err(error) => {
            return Json(json!({ "error": error }))
                .into_response()
                .with_status(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }

    let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if settings.agent_system_prompt.trim().is_empty() {
        push_preview_layer(
            &mut layers,
            "default_template",
            "system",
            5,
            "内置默认角色扮演 / 文学创作模板（运行时按角色展开）",
        );
    } else {
        push_preview_layer(
            &mut layers,
            "custom_template",
            "system",
            5,
            settings.agent_system_prompt.clone(),
        );
    }

    let inject = state.prompt_inject.lock().unwrap_or_else(|e| e.into_inner()).get().clone();
    match inject.mode {
        crate::services::prompt_inject_service::InjectMode::Simple => push_preview_layer(
            &mut layers,
            "simple_inject",
            "system",
            4,
            inject.simple_inject_text(),
        ),
        crate::services::prompt_inject_service::InjectMode::Complex => {
            for floor in inject.enabled_floors_sorted() {
                push_preview_layer(
                    &mut layers,
                    format!("complex_floor:{}", floor.name),
                    floor.role.as_str(),
                    4,
                    floor.content.clone(),
                );
            }
        }
    }

    if let Some(character_id) = query.character_id.as_deref() {
        if let Some(character) = state.characters.get(character_id) {
            push_preview_layer(
                &mut layers,
                "character_card",
                "system",
                3,
                format!(
                    "角色名：{}\n角色描述：{}",
                    character.chara_name, character.description
                ),
            );
            let mut entries = character
                .data_raw
                .as_ref()
                .map(crate::parsing::world_book::character_book_entries)
                .unwrap_or_default();
            entries.extend(
                state
                    .world_books
                    .collect_entries_for_character(character_id),
            );
            let summary = entries
                .iter()
                .filter(|entry| entry.enabled)
                .map(|entry| {
                    format!(
                        "[{}] kind={} role={} length={} hash={}",
                        entry.comment,
                        if entry.constant {
                            "constant"
                        } else {
                            "triggered"
                        },
                        entry.role.as_deref().unwrap_or(if entry.constant {
                            "system"
                        } else {
                            "user"
                        }),
                        entry.content.chars().count(),
                        stable_history_hash(&entry.content),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            push_preview_layer(&mut layers, "world_book", "metadata", 3, summary);
        }
    }

    if let Some(session_id) = query.session_id.as_deref() {
        let summary = state
            .sessions
            .get_messages(session_id)
            .iter()
            .map(|message| {
                format!(
                    "role={} length={} hash={}",
                    message.role,
                    message.content.chars().count(),
                    stable_history_hash(&message.content)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        push_preview_layer(&mut layers, "history_summary", "metadata", 2, summary);
    }

    if let Some(flow) = state.flow.lock().unwrap_or_else(|e| e.into_inner()).get().filter(|flow| flow.enabled) {
        for (index, step) in flow.steps.iter().filter(|step| step.enabled).enumerate() {
            if let Some(prompt) = step
                .system_prompt
                .as_deref()
                .filter(|text| !text.trim().is_empty())
            {
                push_preview_layer(
                    &mut layers,
                    format!("step_instruction:{}", index + 1),
                    "system",
                    0,
                    prompt,
                );
            }
        }
    }

    if !settings.reflect_prompt.trim().is_empty() {
        push_preview_layer(
            &mut layers,
            "reflection",
            "system",
            0,
            settings.reflect_prompt,
        );
    }
    if !settings.reflect_advice_prompt.trim().is_empty() {
        push_preview_layer(
            &mut layers,
            "reflection_advice",
            settings.reflect_advice_role,
            0,
            settings.reflect_advice_prompt,
        );
    }
    if !settings.preset_tail_prompt.trim().is_empty() {
        push_preview_layer(
            &mut layers,
            "preset_tail",
            settings.preset_tail_role,
            0,
            settings.preset_tail_prompt,
        );
    }

    Json(json!({
        "ok": true,
        "note": "历史仅显示角色、长度与哈希；预览不包含聊天正文或 API Key。步骤指令与工具指南在具体运行步骤确定后追加。",
        "layers": layers,
    }))
    .into_response()
}

/// PUT /api/settings/agent-prompt:原子保存主 Agent 提示词到 DATA_DIR。
pub async fn save_agent_prompt(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SaveAgentPromptBody>,
) -> Response {
    let path = state.runtime_prompt.path().display().to_string();
    match state.runtime_prompt.write(&body.content) {
        Ok(_) => Json(json!({ "ok": true, "path": path })).into_response(),
        Err(e) => Json(json!({ "error": e }))
            .into_response()
            .with_status(StatusCode::BAD_REQUEST),
    }
}
