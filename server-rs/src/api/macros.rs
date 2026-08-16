// 宏调试路由(阶段六 6b):POST /api/macros/expand。
// 纯扁平 vars 展开:ctx 全可选字段(character/history/vars 等),assistant_vars/scopes 传 None,
// 不耦合引擎 —— 供前端「宏调试」面板离线验证 {{...}} 模板的展开结果。
// 未知宏保持原样(与引擎 expand_macros 策略一致);空 text 返回空串。
use crate::api::app_state::AppState;
use crate::parsing::macros::{expand_macros, MacroCtx};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// 宏展开上下文(全可选,缺省即空/默认,与引擎组装时的缺省语义一致)
#[derive(Deserialize, Default)]
pub struct MacroExpandCtx {
    #[serde(default)]
    pub character_name: Option<String>,
    #[serde(default)]
    pub character_description: Option<String>,
    #[serde(default)]
    pub personality: Option<String>,
    #[serde(default)]
    pub scenario: Option<String>,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub user_input: Option<String>,
    #[serde(default)]
    pub history: Option<Vec<MacroHistoryItem>>,
    /// 扁平变量 map(key → value);值序列化为显示字符串({{getvar::key}}/{{var::key}} 读取)
    #[serde(default)]
    pub vars: Option<Map<String, Value>>,
}

/// 历史消息条目({{lastMessage}}/{{firstMessage}} 等读取)
#[derive(Deserialize)]
pub struct MacroHistoryItem {
    pub role: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct MacroExpandBody {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub ctx: Option<MacroExpandCtx>,
}

/// POST /api/macros/expand:展开模板中的 {{...}} 宏,返回 {expanded}
pub async fn expand(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<MacroExpandBody>,
) -> Response {
    let ctx = body.ctx.unwrap_or_default();
    let history: Vec<(String, String)> = ctx
        .history
        .unwrap_or_default()
        .into_iter()
        .map(|h| (h.role, h.content))
        .collect();
    let mut vars: HashMap<String, String> = HashMap::new();
    if let Some(map) = ctx.vars {
        for (k, v) in map {
            vars.insert(k, value_to_display(&v));
        }
    }
    let expanded = {
        let mut mctx = MacroCtx {
            character_name: ctx.character_name.as_deref().unwrap_or(""),
            character_description: ctx.character_description.as_deref().unwrap_or(""),
            user_name: ctx.user_name.as_deref().unwrap_or("用户"),
            personality: ctx.personality.as_deref().unwrap_or(""),
            scenario: ctx.scenario.as_deref().unwrap_or(""),
            user_input: ctx.user_input.as_deref().unwrap_or(""),
            history: &history,
            vars: &mut vars,
            assistant_vars: None,
            scopes: None,
        };
        expand_macros(&body.text, &mut mctx)
    };
    Json(json!({ "expanded": expanded })).into_response()
}

/// vars 值序列化为显示字符串:字符串原样,数字/布尔/对象走 JSON 序列化
fn value_to_display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_value_keeps_string_serializes_others() {
        assert_eq!(value_to_display(&Value::String("你好".into())), "你好");
        assert_eq!(value_to_display(&Value::from(88)), "88");
        assert_eq!(value_to_display(&Value::Bool(true)), "true");
    }
}
