// 设置路由:/api/settings(连接测试/模型列表/信息/模型切换/运行期设置读写)
use crate::api::app_state::AppState;
use crate::api::WithStatus;
use crate::services::settings_service::{
    normalize_base_url, AppMode, McpServerConfig, RuntimeSettings, DEFAULT_SEARCH_ENDPOINT,
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

/// 设置读写按模式路由:?mode=roleplay|task(缺省 = roleplay,兼容旧客户端)。
#[derive(Deserialize, Default)]
pub struct ModeQuery {
    #[serde(default)]
    pub mode: Option<String>,
}

impl ModeQuery {
    fn app_mode(&self) -> AppMode {
        AppMode::parse(self.mode.as_deref().unwrap_or("roleplay"))
    }
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
    /// 旧放行模式开关(deprecated;true → bypass,false → strict)
    #[serde(default)]
    pub bypass_mode: Option<bool>,
    /// 授权模式三档(strict / loose / bypass);优先于旧 bypass_mode
    #[serde(default)]
    pub authorization_mode: Option<String>,
    /// 「始终需授权」清单
    #[serde(default)]
    pub bypass_blacklist: Option<Vec<String>>,
    /// 授权等待超时(秒;30..=1800)
    #[serde(default)]
    pub tool_authorization_timeout_secs: Option<u32>,
    /// 任务模式工具策略(all / deny_dangerous / allowlist)
    #[serde(default)]
    pub task_tool_policy: Option<String>,
    /// 任务模式工具白名单(allowlist 策略生效)
    #[serde(default)]
    pub task_tool_allowlist: Option<Vec<String>>,
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
    /// 压缩后保留的最近消息条数(2..=200)
    #[serde(default)]
    pub compaction_keep_recent: Option<u32>,
    /// snip 零成本裁剪的消息长度阈值(字节;0 = 禁用,上限 1MB)
    #[serde(default)]
    pub compaction_snip_bytes: Option<u32>,
    /// LLM 请求快照开关(第四点·主题 A)
    #[serde(default)]
    pub llm_request_log: Option<bool>,
    /// 跨会话记忆蒸馏开关(落地项 2)
    #[serde(default)]
    pub memory_distill_enabled: Option<bool>,
    /// 记忆槽注入条数上限(0..=50;0 = 关闭注入)
    #[serde(default)]
    pub memory_inject_limit: Option<u32>,
    /// 记忆槽字符预算(0..=20000;0 = 不限制)
    #[serde(default)]
    pub memory_inject_char_budget: Option<u32>,
    /// 每角色记忆容量上限(0..=10000;0 = 不淘汰)
    #[serde(default)]
    pub memory_max_entries: Option<u32>,
    /// 向量化开关(开启后记忆写入生成向量、召回走混合打分)
    #[serde(default)]
    pub embedding_enabled: Option<bool>,
    /// embedding 服务地址(OpenAI 兼容 /embeddings)
    #[serde(default)]
    pub embedding_base_url: Option<String>,
    /// embedding API Key(空 = 不变更;与聊天 Key 同策略加密存储)
    #[serde(default)]
    pub embedding_api_key: Option<String>,
    /// embedding 模型名
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// 向量维度(0 = 自动探测)
    #[serde(default)]
    pub embedding_dim: Option<u32>,
    /// 技能渐进披露开关(true = system 只注入「name:description」清单,正文按需 read)
    #[serde(default)]
    pub skill_progressive_disclosure: Option<bool>,
    /// 回退快照开关(批次 6.1;true = 写工具执行前留快照,可回退)
    #[serde(default)]
    pub undo_enabled: Option<bool>,
    /// 子智能体最大嵌套深度(1..=4)
    #[serde(default)]
    pub subagent_max_depth: Option<u32>,
    /// 子智能体最大并发数(1..=16)
    #[serde(default)]
    pub subagent_max_concurrency: Option<u32>,
    /// 子智能体结果最大字符数(500..=8000,超出截断带尾注)
    #[serde(default)]
    pub subagent_result_max_chars: Option<u32>,
    /// MCP stdio 客户端总开关(批次 6.2;仅启动时装配,改后重启生效)
    #[serde(default)]
    pub mcp_enabled: Option<bool>,
    /// MCP 服务器列表(全量替换语义,与 bypass_blacklist 一致)
    #[serde(default)]
    pub mcp_servers: Option<Vec<McpServerConfig>>,
    /// 执行者人设完整开关(R3a):true = 完整(含 scenario+mes_example),false = 精简;
    /// 缺省保持不变
    #[serde(default)]
    pub task_persona_full: Option<bool>,
    /// 任务模式是否继承提示词注入(2026-09-10 实测修复):true = 继承 prompt_floors.json
    /// (旧行为),false = 隔离(默认);缺省保持不变
    #[serde(default)]
    pub task_prompt_inject_enabled: Option<bool>,
    /// 工具循环历史保留的最近完整轮数(R3b;1..=32;缺省保持不变)
    #[serde(default)]
    pub tool_history_keep_rounds: Option<u32>,
    /// 工具循环历史 token 预算(R3b;0 = 禁用预算闸门,否则 1024..=1M;缺省保持不变)
    #[serde(default)]
    pub tool_history_budget_tokens: Option<u32>,
}

/// 序列化运行期设置(API Key 脱敏)。
/// 字段数已达 serde_json::json! 宏的递归展开上限,故拆成两段构建再合并
/// (比给整个 crate 提高 recursion_limit 影响面更小)。
fn settings_json(s: &RuntimeSettings) -> Value {
    let mut v = json!({
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
        "authorization_mode": s.authorization_mode.as_str(),
        "bypass_blacklist": s.bypass_blacklist,
        "tool_authorization_timeout_secs": s.tool_authorization_timeout_secs,
        "task_tool_policy": s.task_tool_policy,
        "task_tool_allowlist": s.task_tool_allowlist,
        "max_tool_rounds": s.max_tool_rounds,
        "render_html": s.render_html,
        "compaction_mode": s.compaction_mode,
        "compaction_threshold": s.compaction_threshold,
        "compaction_keep_recent": s.compaction_keep_recent,
        "compaction_snip_bytes": s.compaction_snip_bytes,
        "llm_request_log": s.llm_request_log,
        "memory_distill_enabled": s.memory_distill_enabled,
        "memory_inject_limit": s.memory_inject_limit,
        "memory_inject_char_budget": s.memory_inject_char_budget,
        "memory_max_entries": s.memory_max_entries,
    });
    let rest = json!({
        "embedding_enabled": s.embedding_enabled,
        "embedding_base_url": s.embedding_base_url,
        "embedding_api_key_masked": s.masked_embedding_api_key(),
        "has_embedding_api_key": !s.embedding_api_key.is_empty(),
        "embedding_model": s.embedding_model,
        "embedding_dim": s.embedding_dim,
        "skill_progressive_disclosure": s.skill_progressive_disclosure,
        "undo_enabled": s.undo_enabled,
        "subagent_max_depth": s.subagent_max_depth,
        "subagent_max_concurrency": s.subagent_max_concurrency,
        "subagent_result_max_chars": s.subagent_result_max_chars,
        "mcp_enabled": s.mcp_enabled,
        "mcp_servers": s.mcp_servers,
        "task_persona_full": s.task_persona_full,
        "task_prompt_inject_enabled": s.task_prompt_inject_enabled,
        "tool_history_keep_rounds": s.tool_history_keep_rounds,
        "tool_history_budget_tokens": s.tool_history_budget_tokens,
    });
    if let (Some(dst), Some(src)) = (v.as_object_mut(), rest.as_object()) {
        for (k, val) in src {
            dst.insert(k.clone(), val.clone());
        }
    }
    v
}

/// GET /api/settings:当前运行期设置(供前端表单回填)。?mode= 返回该模式合并后的有效值。
pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ModeQuery>,
) -> Json<Value> {
    let s = state.settings_snapshot();
    let mode = query.app_mode();
    Json(settings_json(&s.for_mode(mode)))
}

/// PUT /api/settings:更新运行期设置(部分字段;Base URL/Key 变更立即重建连接器,模型变更立即切换)。
/// ?mode= 指定写入哪个模式的覆盖层;连接信息(Base URL/Key/模型)始终写共享层。
pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ModeQuery>,
    Json(body): Json<UpdateSettingsBody>,
) -> Response {
    let mode = query.app_mode();
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

        // 连接信息:全局共享一份
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
        // 向量化配置:与连接信息同属全局共享层(不按模式隔离)
        if let Some(v) = body.embedding_enabled {
            s.embedding_enabled = v;
        }
        if let Some(v) = &body.embedding_base_url {
            let t = normalize_base_url(v);
            // 允许清空(关闭向量化时用户可能想抹掉地址)
            s.embedding_base_url = if t.is_empty() { String::new() } else { t };
        }
        if let Some(v) = &body.embedding_api_key {
            let t = v.trim().to_string();
            if !t.is_empty() {
                s.embedding_api_key = t;
            }
        }
        if let Some(v) = &body.embedding_model {
            s.embedding_model = v.trim().to_string();
        }
        if let Some(v) = body.embedding_dim {
            if v == 0 || (16..=8192).contains(&v) {
                s.embedding_dim = v;
            } else {
                return Json(json!({ "error": "embedding_dim 必须为 0(自动)或 16..=8192" }))
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST);
            }
        }

        // 生成参数 + Agent 配置:task 模式写 task 覆盖层;roleplay 模式写扁平字段
        // (引擎直接读扁平字段,故 roleplay 不落覆盖层)。校验逻辑两模式一致。
        {
            let is_task = mode == AppMode::Task;
            // 通过校验后按模式写入:task 覆盖层或扁平字段。
            macro_rules! apply {
                ($s:expr, $is_task:expr, $field:ident, $value:expr) => {{
                    let v = $value;
                    if $is_task {
                        $s.task.$field = Some(v);
                    } else {
                        $s.$field = v;
                    }
                }};
            }
            if let Some(v) = body.default_temperature {
                if (0.0..=2.0).contains(&v) {
                    apply!(s, is_task, default_temperature, v);
                }
            }
            if let Some(v) = body.default_top_p {
                if (0.0..=1.0).contains(&v) {
                    apply!(s, is_task, default_top_p, v);
                }
            }
            if let Some(v) = body.default_max_tokens {
                if v == 0 || v > 65_536 {
                    return Json(json!({ "error": "default_max_tokens 必须在 1..=65536" }))
                        .into_response()
                        .with_status(StatusCode::BAD_REQUEST);
                }
                apply!(s, is_task, default_max_tokens, v);
            }
            if let Some(v) = body.max_context_tokens {
                if !(65_536..=1_048_576).contains(&v) {
                    return Json(
                        json!({ "error": "max_context_tokens 必须在 65536..=1048576(64K~1M)" }),
                    )
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST);
                }
                apply!(s, is_task, max_context_tokens, v);
            }
            // Agent 系统提示词:允许清空(空 = 用内置默认)。
            // 类型级模式隔离(WP7):roleplay 写 RoleplayPromptConfig 扁平字段,
            // task 写 Option<TaskPromptConfig> 覆盖层;两分支类型不同源,不走通用 apply! 宏。
            if let Some(v) = &body.agent_system_prompt {
                if is_task {
                    s.task.agent_system_prompt = Some(
                        crate::services::settings_service::TaskPromptConfig(v.clone()),
                    );
                } else {
                    s.agent_system_prompt =
                        crate::services::settings_service::RoleplayPromptConfig(v.clone());
                }
            }
            // 搜索端点:允许清空(空 = 用默认 DDG 端点)
            if let Some(v) = &body.search_endpoint {
                let t = v.trim().to_string();
                let endpoint = if t.is_empty() {
                    DEFAULT_SEARCH_ENDPOINT.to_string()
                } else {
                    t
                };
                apply!(s, is_task, search_endpoint, endpoint);
            }
            // mvu 变量注入位置:仅接受 system / user_tail
            if let Some(v) = &body.mvu_vars_position {
                let t = v.trim().to_string();
                if t == "system" || t == "user_tail" {
                    apply!(s, is_task, mvu_vars_position, t);
                }
            }
            // 反思提示词:允许清空(空 = 回退机械规则检查)
            if let Some(v) = &body.reflect_prompt {
                apply!(s, is_task, reflect_prompt, v.clone());
            }
            // 预设尾部提示词:允许清空(空 = 禁用位置0 预设尾部)
            if let Some(v) = &body.preset_tail_prompt {
                apply!(s, is_task, preset_tail_prompt, v.clone());
            }
            // 预设尾部注入角色:仅接受 user / assistant
            if let Some(v) = &body.preset_tail_role {
                let t = v.trim().to_string();
                if t == "user" || t == "assistant" {
                    apply!(s, is_task, preset_tail_role, t);
                }
            }
            // 反思失败建议提示词:允许清空(空 = 禁用)
            if let Some(v) = &body.reflect_advice_prompt {
                apply!(s, is_task, reflect_advice_prompt, v.clone());
            }
            // 反思建议注入角色:仅接受 user / assistant
            if let Some(v) = &body.reflect_advice_role {
                let t = v.trim().to_string();
                if t == "user" || t == "assistant" {
                    apply!(s, is_task, reflect_advice_role, t);
                }
            }
            // 授权模式(三档;旧 bypass_mode 仍接受并映射为对应档位)
            if let Some(v) = &body.authorization_mode {
                let parsed = match crate::tools::permissions::AuthorizationMode::parse(v.trim()) {
                    Some(m) => m,
                    None => {
                        return Json(json!({
                            "error": "authorization_mode 仅支持 strict / loose / bypass"
                        }))
                        .into_response()
                        .with_status(StatusCode::BAD_REQUEST);
                    }
                };
                apply!(s, is_task, authorization_mode, parsed);
            }
            // 旧字段兼容:bypass_mode=true → bypass,false → strict
            if let Some(v) = body.bypass_mode {
                let mapped = if v {
                    crate::tools::permissions::AuthorizationMode::Bypass
                } else {
                    crate::tools::permissions::AuthorizationMode::Strict
                };
                apply!(s, is_task, authorization_mode, mapped);
            }
            // 「始终需授权」清单:校验工具名存在性,未知名返回 warning(不阻塞保存)
            if let Some(v) = &body.bypass_blacklist {
                apply!(s, is_task, bypass_blacklist, v.clone());
            }
            // 授权等待超时(秒):30..=1800
            if let Some(v) = body.tool_authorization_timeout_secs {
                if !(30..=1800).contains(&v) {
                    return Json(json!({
                        "error": "tool_authorization_timeout_secs 必须在 30..=1800"
                    }))
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST);
                }
                apply!(s, is_task, tool_authorization_timeout_secs, v);
            }
            // 任务模式工具策略:all / deny_dangerous / allowlist
            if let Some(v) = &body.task_tool_policy {
                if !matches!(v.as_str(), "all" | "deny_dangerous" | "allowlist") {
                    return Json(json!({
                        "error": "task_tool_policy 仅支持 all / deny_dangerous / allowlist"
                    }))
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST);
                }
                apply!(s, is_task, task_tool_policy, v.clone());
            }
            if let Some(v) = &body.task_tool_allowlist {
                apply!(s, is_task, task_tool_allowlist, v.clone());
            }
            // 工具循环轮次上限:仅接受 1..=200(防止误填 0 或超大值打爆模型请求)
            if let Some(v) = body.max_tool_rounds {
                if !(1..=200).contains(&v) {
                    return Json(json!({ "error": "max_tool_rounds 必须在 1..=200" }))
                        .into_response()
                        .with_status(StatusCode::BAD_REQUEST);
                }
                apply!(s, is_task, max_tool_rounds, v);
            }
            // HTML 渲染开关
            if let Some(v) = body.render_html {
                apply!(s, is_task, render_html, v);
            }
            // 上下文压缩模式:仅接受 off / manual / auto
            if let Some(v) = &body.compaction_mode {
                let t = v.trim().to_string();
                if t == "off" || t == "manual" || t == "auto" {
                    apply!(s, is_task, compaction_mode, t);
                }
            }
            // 上下文压缩阈值:仅接受 0.5..=0.95
            if let Some(v) = body.compaction_threshold {
                if !(0.5..=0.95).contains(&v) {
                    return Json(json!({ "error": "compaction_threshold 必须在 0.5..=0.95" }))
                        .into_response()
                        .with_status(StatusCode::BAD_REQUEST);
                }
                apply!(s, is_task, compaction_threshold, v);
            }
            // 压缩保留条数(缓存感知管线):2..=200,缺省保持不变
            if let Some(v) = body.compaction_keep_recent {
                if (2..=200).contains(&v) {
                    apply!(s, is_task, compaction_keep_recent, v);
                }
            }
            // snip 零成本裁剪阈值(字节):0 = 禁用,1MB 上限
            if let Some(v) = body.compaction_snip_bytes {
                if v <= 1_048_576 {
                    apply!(s, is_task, compaction_snip_bytes, v);
                }
            }
            // LLM 请求快照开关
            if let Some(v) = body.llm_request_log {
                apply!(s, is_task, llm_request_log, v);
            }
            // 记忆蒸馏开关(落地项 2)
            if let Some(v) = body.memory_distill_enabled {
                apply!(s, is_task, memory_distill_enabled, v);
            }
            // 记忆注入上限(0..=50;0 = 关闭注入,越界忽略)
            if let Some(v) = body.memory_inject_limit {
                if v <= 50 {
                    apply!(s, is_task, memory_inject_limit, v);
                }
            }
            // 记忆槽字符预算(0..=20000;0 = 不限制,越界忽略)
            if let Some(v) = body.memory_inject_char_budget {
                if v <= 20_000 {
                    apply!(s, is_task, memory_inject_char_budget, v);
                }
            }
            // 每角色记忆容量上限(0..=10000;0 = 不淘汰,越界忽略)
            if let Some(v) = body.memory_max_entries {
                if v <= 10_000 {
                    apply!(s, is_task, memory_max_entries, v);
                }
            }
            // 技能渐进披露开关(落地项 3)
            if let Some(v) = body.skill_progressive_disclosure {
                apply!(s, is_task, skill_progressive_disclosure, v);
            }
            // 回退快照开关(批次 6.1)
            if let Some(v) = body.undo_enabled {
                apply!(s, is_task, undo_enabled, v);
            }
            // 子智能体嵌套深度上限(1..=4,越界拒绝)
            if let Some(v) = body.subagent_max_depth {
                if !(1..=4).contains(&v) {
                    return Json(json!({ "error": "subagent_max_depth 必须在 1..=4" }))
                        .into_response()
                        .with_status(StatusCode::BAD_REQUEST);
                }
                apply!(s, is_task, subagent_max_depth, v);
            }
            // 子智能体并发上限(1..=16,越界拒绝)
            if let Some(v) = body.subagent_max_concurrency {
                if !(1..=16).contains(&v) {
                    return Json(json!({ "error": "subagent_max_concurrency 必须在 1..=16" }))
                        .into_response()
                        .with_status(StatusCode::BAD_REQUEST);
                }
                apply!(s, is_task, subagent_max_concurrency, v);
            }
            // 子智能体结果字符上限(500..=8000,越界拒绝)
            if let Some(v) = body.subagent_result_max_chars {
                if !(500..=8000).contains(&v) {
                    return Json(json!({ "error": "subagent_result_max_chars 必须在 500..=8000" }))
                        .into_response()
                        .with_status(StatusCode::BAD_REQUEST);
                }
                apply!(s, is_task, subagent_result_max_chars, v);
            }
            // MCP 总开关(批次 6.2):仅启动时装配,运行期改动不回溯重连,重启后生效
            if let Some(v) = body.mcp_enabled {
                apply!(s, is_task, mcp_enabled, v);
            }
            // MCP 服务器列表(全量替换):卫生清理同 load(trim 名称/命令,丢弃不可用条目)
            if let Some(v) = &body.mcp_servers {
                let mut servers = v.clone();
                servers.retain_mut(|srv| {
                    srv.name = srv.name.trim().to_string();
                    srv.command = srv.command.trim().to_string();
                    !srv.name.is_empty() && !srv.command.is_empty()
                });
                apply!(s, is_task, mcp_servers, servers);
            }
            // 执行者人设完整开关(R3a;bool 免校验,task 模式写覆盖层)
            if let Some(v) = body.task_persona_full {
                apply!(s, is_task, task_persona_full, v);
            }
            // 任务模式提示词注入继承开关(2026-09-10 实测修复;bool 免校验,task 写覆盖层)
            if let Some(v) = body.task_prompt_inject_enabled {
                apply!(s, is_task, task_prompt_inject_enabled, v);
            }
            // 工具历史回灌上限(R3b):扁平全局字段(引擎 run_tool_loop 直读扁平值,
            // 不入模式覆盖层——任务/聊天工具循环共用同一上限,与 subagent 参数的
            // 引擎侧消费口径一致);越界拒绝,与 load 钳制区间一致
            if let Some(v) = body.tool_history_keep_rounds {
                if !(1..=32).contains(&v) {
                    return Json(json!({ "error": "tool_history_keep_rounds 必须在 1..=32" }))
                        .into_response()
                        .with_status(StatusCode::BAD_REQUEST);
                }
                s.tool_history_keep_rounds = v;
            }
            if let Some(v) = body.tool_history_budget_tokens {
                if v != 0 && !(1024..=1_048_576).contains(&v) {
                    return Json(
                        json!({ "error": "tool_history_budget_tokens 须为 0(禁用)或 1024..=1048576" }),
                    )
                    .into_response()
                    .with_status(StatusCode::BAD_REQUEST);
                }
                s.tool_history_budget_tokens = v;
            }
        }
    }

    // B-1:save 含 API Key 加密(DPAPI)+ 同步 JSON 落盘,挪阻塞线程池;
    // candidate 所有权随闭包往返,后续连接器/模型比较逻辑不变
    let data_dir = state.config.data_dir.clone();
    let save_outcome = state
        .db_call(move || {
            let result = candidate.save(&data_dir);
            (candidate, result)
        })
        .await;
    let (candidate, save_result) = match save_outcome {
        Ok(pair) => pair,
        Err(e) => {
            return Json(json!({ "error": format!("设置保存失败: {e}") }))
                .into_response()
                .with_status(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    if let Err(e) = save_result {
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

    Json(json!({ "ok": true, "settings": settings_json(&candidate.for_mode(mode)) }))
        .into_response()
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

/// POST /api/settings/embedding/test:测试向量化连接(嵌入一条固定文本)。
/// 成功回传实际维度与耗时;失败回传错误原因。不写库、不改配置。
pub async fn test_embedding(State(state): State<Arc<AppState>>) -> Response {
    let settings = state.settings_snapshot();
    let svc = crate::services::embedding_service::EmbeddingService::new();
    match svc.test(&settings).await {
        Ok((dim, ms)) => Json(json!({
            "ok": true,
            "dim": dim,
            "latency_ms": ms,
            "message": format!("连接成功:向量维度 {dim},耗时 {ms} ms"),
        }))
        .into_response(),
        Err(e) => Json(json!({ "ok": false, "message": e.to_string() })).into_response(),
    }
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
    /// 预览按哪个模式的合并设置:roleplay(缺省,兼容旧客户端)| task
    pub mode: Option<String>,
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

    // 预览按模式走 for_mode 合并值(docs/模式提示词边界.md 第五节):
    // 缺省/未知值按 roleplay(旧客户端零变化);task 为覆盖层合并后的有效设置,
    // agent_system_prompt 经类型级隔离转换(None 已注入内置任务默认词)。
    let mode = match query.mode.as_deref() {
        Some("task") => AppMode::Task,
        _ => AppMode::Roleplay,
    };
    // 设置快照:不留锁跨 await(for_mode 为纯计算,锁在 snapshot 内即释放)
    let settings = state.settings_snapshot().for_mode(mode);
    // agent_system_prompt 为 RoleplayPromptConfig(WP7),.0 取字符串
    if settings.agent_system_prompt.0.trim().is_empty() {
        // 空值回退文案按模式区分:roleplay 空 = 用内置人设模板;
        // task 走到这里 = 覆盖层 Some("") 显式清空(None 已被 for_mode 注入任务默认词)
        let fallback = match mode {
            AppMode::Task => "任务模式 Agent 系统提示词已显式清空,执行时仅注入下方三层固定提示词",
            AppMode::Roleplay => "内置默认角色扮演 / 文学创作模板（运行时按角色展开）",
        };
        push_preview_layer(&mut layers, "default_template", "system", 5, fallback);
    } else {
        push_preview_layer(
            &mut layers,
            "custom_template",
            "system",
            5,
            settings.agent_system_prompt.0.clone(),
        );
    }

    // task 模式追加规划器/执行者/汇总者三层固定提示词(单一来源 task_service/prompt.rs)
    if matches!(mode, AppMode::Task) {
        use crate::services::task_service::prompt as task_prompts;
        push_preview_layer(
            &mut layers,
            "task_planner_prompt",
            "system",
            5,
            task_prompts::PLANNER_PROMPT,
        );
        push_preview_layer(
            &mut layers,
            "task_executor_prompt",
            "system",
            5,
            task_prompts::EXECUTOR_PROMPT,
        );
        push_preview_layer(
            &mut layers,
            "task_summarizer_prompt",
            "system",
            5,
            task_prompts::SUMMARIZER_PROMPT,
        );
    }

    // 任务模式注入默认隔离(2026-09-10 实测修复):task 模式且未显式开启继承时,
    // 不推送注入层——预览必须与真实下发一致(docs/模式提示词边界.md 第五节)。
    let inject_gated = matches!(mode, AppMode::Task) && !settings.task_prompt_inject_enabled;
    if !inject_gated {
        let inject = state
            .prompt_inject
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get()
            .clone();
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
    }

    if let Some(character_id) = query.character_id.as_deref() {
        // 角色卡 + 世界书条目读取(同步 SQLite)合并进同一阻塞任务(DB 并发改造)
        let cid = character_id.to_string();
        let characters = state.characters.clone();
        let world_books = state.world_books.clone();
        let character = state
            .db_call(move || {
                let character = characters.get(&cid);
                let entries = character
                    .as_ref()
                    .map(|_| world_books.collect_entries_for_character(&cid))
                    .unwrap_or_default();
                (character, entries)
            })
            .await
            .unwrap_or((None, Vec::new()));
        if let (Some(character), wb_entries) = character {
            let entries = {
                let mut v = character
                    .data_raw
                    .as_ref()
                    .map(crate::parsing::world_book::character_book_entries)
                    .unwrap_or_default();
                v.extend(wb_entries);
                v
            };
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
        // 历史摘要读取(同步 SQLite)挪进阻塞线程池(DB 并发改造)
        let sessions = state.sessions.clone();
        let sid = session_id.to_string();
        let history = state
            .db_call(move || sessions.get_messages(&sid))
            .await
            .unwrap_or_default();
        let summary = history
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

    if let Some(flow) = state
        .flow
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get()
        .filter(|flow| flow.enabled)
    {
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
