// 会话变量工具:update_variables 仅更新 ToolContext 指定的当前会话 stat_data。
use crate::models::types::{ToolContext, ToolDefinition};
use crate::parsing::assistant::{parse_patch_array, PatchOp};
use crate::services::session_service::SessionService;
use crate::tools::registry::ToolRegistry;
use serde_json::{json, Value};
use std::sync::Arc;

pub fn register_update_variables_tool(registry: &ToolRegistry, sessions: Arc<SessionService>) {
    registry.register(
        ToolDefinition {
            name: "update_variables".into(),
            description: "严格按 JSON Patch 更新当前会话的 stat_data 变量树并立即持久化。支持 replace/delta/insert/remove/move。".into(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "patches": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": { "type": "string", "enum": ["replace", "delta", "insert", "remove", "move"] },
                                "path": { "type": "string", "minLength": 1 },
                                "value": {},
                                "from": { "type": "string", "minLength": 1 }
                            },
                            "required": ["op", "path"]
                        }
                    }
                },
                "required": ["patches"]
            }),
        },
        Arc::new(move |args: Value, ctx: ToolContext| {
            let sessions = sessions.clone();
            Box::pin(async move {
                let patches = validate_update_arguments(&args)?;
                let mut vars = sessions.load_assistant_vars(&ctx.session_id);
                vars.apply_patches(&patches)?;
                sessions
                    .save_assistant_vars(&ctx.session_id, &vars)
                    .map_err(|e| format!("变量树落库失败: {e}"))?;
                Ok(json!({
                    "ok": true,
                    "session_id": ctx.session_id,
                    "stat_data": vars.tree().clone()
                })
                .to_string())
            })
        }),
    );
}

fn validate_update_arguments(args: &Value) -> Result<Vec<PatchOp>, String> {
    let obj = args
        .as_object()
        .ok_or_else(|| "update_variables 参数必须是对象".to_string())?;
    if obj.keys().any(|key| key != "patches") {
        return Err("update_variables 仅支持 patches 参数".into());
    }
    let items = obj
        .get("patches")
        .and_then(Value::as_array)
        .ok_or_else(|| "update_variables.patches 必须是非空数组".to_string())?;
    if items.is_empty() {
        return Err("update_variables.patches 不能为空".into());
    }
    for (index, item) in items.iter().enumerate() {
        let patch = item
            .as_object()
            .ok_or_else(|| format!("patches[{index}] 必须是对象"))?;
        let op = patch
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("patches[{index}].op 必须是字符串"))?;
        if !matches!(op, "replace" | "delta" | "insert" | "remove" | "move") {
            return Err(format!("patches[{index}].op 不支持: {op}"));
        }
        let path = patch
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| format!("patches[{index}].path 不能为空"))?;
        if matches!(path, "/" | "stat_data") {
            return Err(format!("patches[{index}].path 不允许覆盖变量树根"));
        }
        match op {
            "replace" | "insert" if !patch.contains_key("value") => {
                return Err(format!("patches[{index}] 的 {op} 操作必须提供 value"));
            }
            "delta" if !patch.get("value").is_some_and(Value::is_number) => {
                return Err(format!("patches[{index}] 的 delta.value 必须是数字"));
            }
            "move"
                if patch
                    .get("from")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .is_none_or(str::is_empty) =>
            {
                return Err(format!("patches[{index}] 的 move 操作必须提供 from"));
            }
            _ => {}
        }
    }
    parse_patch_array(&Value::Array(items.clone()))
        .ok_or_else(|| "update_variables.patches 没有有效操作".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_variables_arguments_are_strictly_validated() {
        assert!(validate_update_arguments(&json!({})).is_err());
        assert!(validate_update_arguments(&json!({"patches": []})).is_err());
        assert!(validate_update_arguments(&json!({
            "patches": [{"op": "delta", "path": "/score", "value": "1"}]
        }))
        .is_err());
        assert!(validate_update_arguments(&json!({
            "patches": [{"op": "replace", "path": "/score", "value": 1}]
        }))
        .is_ok());
    }
}
