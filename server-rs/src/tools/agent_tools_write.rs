// Agent 强化工具集 · write/replace/create 域(中层 L3 青层工具域):
//   write    写入主对话气泡/角色文件区文件
//   replace  修改(增加/减少)主对话气泡或文件内容(支持同步多次修改)
//   create   在角色文件区创建文件夹/文件(支持同步多次)
// 依赖共享件:safe_rel_path / character_file_root / read_file_checked / write_file_checked。
use crate::models::types::{ToolContext, ToolDefinition};
use crate::tools::registry::ToolRegistry;
use serde_json::{json, Value};
use std::sync::Arc;

use super::agent_tools::ToolDeps;
use super::agent_tools_shared::{
    character_file_root, read_file_checked, safe_rel_path, write_file_checked,
};

// ==================== write:写入主对话气泡/文件 ====================
pub(super) fn register_write(registry: &ToolRegistry, deps: Arc<ToolDeps>) {
    registry.register(
        ToolDefinition {
            name: "write".into(),
            description: "写入内容:target=bubble 写入当前对话气泡(role: assistant/user/system,默认 assistant);target=file 写入角色文件区文件(path 为相对路径,自动建目录)。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "target": { "type": "string", "enum": ["bubble", "file"] },
                    "role": { "type": "string", "enum": ["assistant", "user", "system"] },
                    "content": { "type": "string" },
                    "path": { "type": "string", "description": "file 目标时必填,如 笔记/plan.md" }
                },
                "required": ["target", "content"]
            }),
        },
        Arc::new(move |args: Value, ctx: ToolContext| {
            let deps = deps.clone();
            Box::pin(async move {
                let target = args.get("target").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if content.is_empty() {
                    return Err("缺少 content 参数".into());
                }
                match target.as_str() {
                    "bubble" => {
                        let role = args.get("role").and_then(|v| v.as_str()).unwrap_or("assistant").to_string();
                        if !matches!(role.as_str(), "assistant" | "user" | "system") {
                            return Err(format!("非法 role: {role}"));
                        }
                        let m = deps.sessions.add_message(
                            &ctx.session_id,
                            &role,
                            &content,
                            json!({ "kind": "agent_write" }),
                        )?;
                        Ok(json!({ "ok": true, "target": "bubble", "message_id": m.id, "role": role }).to_string())
                    }
                    "file" => {
                        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        if path.is_empty() {
                            return Err("file 目标需要 path 参数".into());
                        }
                        let rel = write_file_checked(&deps, &ctx, &path, &content)?;
                        Ok(json!({ "ok": true, "target": "file", "path": rel }).to_string())
                    }
                    _ => Err(format!("非法 target: {target}")),
                }
            })
        }),
    );
}

// ==================== replace:修改(增加/减少)主对话气泡或文件内容(支持同步多次修改) ====================
pub(super) fn register_replace(registry: &ToolRegistry, deps: Arc<ToolDeps>) {
    registry.register(
        ToolDefinition {
            name: "replace".into(),
            description: "修改主对话气泡或角色文件区文件内容,支持一次传入多个操作同步执行(operations 数组)。action: append=追加, prepend=前置, replace=替换(search 子串→content,search 为空则整体替换), delete=删除内容(文件保留、内容清空), remove=删除文件本身(仅 target=file,不可恢复)。气泡用 id 定位,文件用 path 定位。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "operations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "target": { "type": "string", "enum": ["bubble", "file"] },
                                "id": { "type": "integer", "description": "气泡消息 id" },
                                "path": { "type": "string", "description": "文件相对路径" },
                                "action": { "type": "string", "enum": ["append", "prepend", "replace", "delete", "remove"] },
                                "content": { "type": "string" },
                                "search": { "type": "string", "description": "replace 时的查找子串" },
                                "create_if_missing": { "type": "boolean", "description": "文件不存在时是否显式创建,默认 false" }
                            },
                            "required": ["target", "action"]
                        }
                    }
                },
                "required": ["operations"]
            }),
        },
        Arc::new(move |args: Value, ctx: ToolContext| {
            let deps = deps.clone();
            Box::pin(async move {
                let ops = args.get("operations").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                if ops.is_empty() {
                    return Err("缺少 operations 参数".into());
                }
                let mut results: Vec<Value> = Vec::new();
                for op in &ops {
                    let target = op.get("target").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let action = op.get("action").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let content = op.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let search = op.get("search").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let r = match target.as_str() {
                        "bubble" => {
                            let id = op.get("id").and_then(|v| v.as_i64()).ok_or("bubble 操作需要 id")?;
                            replace_bubble(&deps, &ctx, id, &action, &content, &search)
                        }
                        "file" => {
                            let path = op.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let create_if_missing = op.get("create_if_missing").and_then(|v| v.as_bool()).unwrap_or(false);
                            replace_file(&deps, &ctx, &path, &action, &content, &search, create_if_missing)
                        }
                        _ => Err(format!("非法 target: {target}")),
                    };
                    match r {
                        Ok(v) => results.push(v),
                        Err(e) => results.push(json!({ "target": target, "action": action, "ok": false, "error": e })),
                    }
                }
                Ok(json!({ "results": results }).to_string())
            })
        }),
    );
}

fn apply_text_action(
    current: &str,
    action: &str,
    content: &str,
    search: &str,
) -> Result<String, String> {
    match action {
        "append" => Ok(format!("{current}\n{content}")
            .trim_start_matches('\n')
            .to_string()),
        "prepend" => Ok(format!("{content}\n{current}")
            .trim_end_matches('\n')
            .to_string()),
        "replace" => {
            if search.is_empty() {
                Ok(content.to_string())
            } else if current.contains(search) {
                Ok(current.replace(search, content))
            } else {
                Err(format!("未找到要替换的文本: {search}"))
            }
        }
        "delete" => {
            if search.is_empty() {
                Ok(String::new())
            } else if current.contains(search) {
                Ok(current.replace(search, ""))
            } else {
                Err(format!("未找到要删除的文本: {search}"))
            }
        }
        _ => Err(format!("非法 action: {action}")),
    }
}

fn replace_bubble(
    deps: &ToolDeps,
    ctx: &ToolContext,
    id: i64,
    action: &str,
    content: &str,
    search: &str,
) -> Result<Value, String> {
    if action == "remove" {
        return Err("remove 仅支持 target=file(删除整个文件);删除气泡请用 action=delete".into());
    }
    if action == "delete" && search.is_empty() {
        if !deps.sessions.delete_message(&ctx.session_id, id) {
            return Err(format!("气泡 {id} 不存在"));
        }
        return Ok(json!({ "target": "bubble", "id": id, "action": "delete", "ok": true }));
    }
    let m = deps
        .sessions
        .get_message(&ctx.session_id, id)
        .ok_or(format!("气泡 {id} 不存在"))?;
    let new_content = apply_text_action(&m.content, action, content, search)?;
    if new_content.is_empty() {
        deps.sessions.delete_message(&ctx.session_id, id);
        return Ok(
            json!({ "target": "bubble", "id": id, "action": action, "ok": true, "deleted": true }),
        );
    }
    deps.sessions
        .update_message(&ctx.session_id, id, &new_content)
        .ok_or("更新气泡失败")?;
    Ok(
        json!({ "target": "bubble", "id": id, "action": action, "ok": true, "length": new_content.chars().count() }),
    )
}

fn replace_file(
    deps: &ToolDeps,
    ctx: &ToolContext,
    path: &str,
    action: &str,
    content: &str,
    search: &str,
    create_if_missing: bool,
) -> Result<Value, String> {
    let rel = safe_rel_path(path)?;
    // remove:删除文件本身(与 delete 清空内容语义区分)。不存在时明确报错,不静默成功。
    if action == "remove" {
        let full = character_file_root(deps, &ctx.character_id).join(&rel);
        return match std::fs::remove_file(&full) {
            Ok(_) => Ok(json!({ "target": "file", "path": rel, "action": "remove", "ok": true })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(format!("文件 {rel} 不存在,无需删除"))
            }
            Err(e) => Err(format!("删除文件 {rel} 失败: {e}")),
        };
    }
    let existing = match read_file_checked(deps, ctx, &rel) {
        Ok(value) => value,
        Err(_error) if create_if_missing => String::new(),
        Err(error) => return Err(error),
    };
    let new_content = apply_text_action(&existing, action, content, search)?;
    write_file_checked(deps, ctx, &rel, &new_content)?;
    Ok(
        json!({ "target": "file", "path": rel, "action": action, "ok": true, "length": new_content.chars().count() }),
    )
}

// ==================== create:在角色文件区创建文件夹/文件(支持同步多次) ====================
pub(super) fn register_create(registry: &ToolRegistry, deps: Arc<ToolDeps>) {
    registry.register(
        ToolDefinition {
            name: "create".into(),
            description: "在角色文件区创建文件夹或文件(items 数组,支持一次多个)。type=dir 建目录(含父级);type=file 创建文件并写入 content(已存在则报错)。path 为角色文件区内的相对路径。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "type": { "type": "string", "enum": ["dir", "file"] },
                                "path": { "type": "string", "description": "相对路径,如 笔记/plan.md" },
                                "content": { "type": "string", "description": "type=file 时的初始内容" }
                            },
                            "required": ["type", "path"]
                        }
                    }
                },
                "required": ["items"]
            }),
        },
        Arc::new(move |args: Value, ctx: ToolContext| {
            let deps = deps.clone();
            Box::pin(async move {
                let items = args.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                if items.is_empty() {
                    return Err("缺少 items 参数".into());
                }
                let mut results: Vec<Value> = Vec::new();
                for it in &items {
                    let ty = it.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let path = it.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let rel = match safe_rel_path(&path) {
                        Ok(r) => r,
                        Err(e) => {
                            results.push(json!({ "path": path, "type": ty, "ok": false, "error": e }));
                            continue;
                        }
                    };
                    let full = character_file_root(&deps, &ctx.character_id).join(&rel);
                    let r: Result<Value, String> = match ty.as_str() {
                        "dir" => {
                            if full.exists() {
                                Err("目录已存在".into())
                            } else {
                                match std::fs::create_dir_all(&full) {
                                    Ok(_) => Ok(json!({ "ok": true, "path": rel, "type": "dir" })),
                                    Err(e) => Err(format!("创建目录失败: {e}")),
                                }
                            }
                        }
                        "file" => {
                            if full.exists() {
                                Err("文件已存在".into())
                            } else {
                                if let Some(parent) = full.parent() {
                                    if let Err(e) = std::fs::create_dir_all(parent) {
                                        results.push(json!({ "path": rel, "type": "file", "ok": false, "error": format!("创建目录失败: {e}") }));
                                        continue;
                                    }
                                }
                                let content = it.get("content").and_then(|v| v.as_str()).unwrap_or("");
                                match std::fs::write(&full, content) {
                                    Ok(_) => Ok(json!({ "ok": true, "path": rel, "type": "file", "bytes": content.len() })),
                                    Err(e) => Err(format!("创建文件失败: {e}")),
                                }
                            }
                        }
                        _ => Err(format!("非法 type: {ty}")),
                    };
                    match r {
                        Ok(v) => results.push(v),
                        Err(e) => results.push(json!({ "path": rel, "type": ty, "ok": false, "error": e })),
                    }
                }
                Ok(json!({ "results": results }).to_string())
            })
        }),
    );
}
