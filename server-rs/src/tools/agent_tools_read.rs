// Agent 强化工具集 · read 域(中层 L3 青层工具域):
//   read 工具:读取世界书(world_book)/角色提示词(character_prompt)/技能库(skill)/
//   角色文件区文件(file)/子智能体任务结果(subtask),支持一次查询多个目标。
// 依赖共享件:read_file_checked(agent_tools_shared)。
use crate::models::types::{ToolContext, ToolDefinition};
use crate::tools::registry::ToolRegistry;
use serde_json::{json, Value};
use std::sync::Arc;

use super::agent_tools::ToolDeps;
use super::agent_tools_shared::read_file_checked;

// ==================== read:读取世界书/提示词/skill(支持同步多次查询) ====================
pub(super) fn register_read(registry: &ToolRegistry, deps: Arc<ToolDeps>) {
    registry.register(
        ToolDefinition {
            name: "read".into(),
            description: "读取上下文资料:世界书条目(world_book)、角色提示词(character_prompt)、技能库(skill)、角色文件区文件(file)、子智能体任务结果(subtask)。支持一次查询多个目标(queries 数组)。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "queries": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "type": { "type": "string", "enum": ["world_book", "character_prompt", "skill", "file", "subtask"], "description": "查询目标类型" },
                                "name": { "type": "string", "description": "名称或文件相对路径/子任务 id" },
                                "keywords": { "type": "array", "items": { "type": "string" }, "description": "关键词过滤(世界书/skill 按名称与内容匹配)" }
                            },
                            "required": ["type"]
                        }
                    },
                    "max_chars": { "type": "integer", "description": "单条结果最大字符数(默认 4000)" }
                },
                "required": ["queries"]
            }),
        },
        Arc::new(move |args: Value, ctx: ToolContext| {
            let deps = deps.clone();
            Box::pin(async move {
                let queries = args.get("queries").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                if queries.is_empty() {
                    return Err("缺少 queries 参数".into());
                }
                let max_chars = args.get("max_chars").and_then(|v| v.as_u64()).unwrap_or(4000) as usize;
                let mut results: Vec<Value> = Vec::new();
                for q in queries {
                    let qtype = q.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let name = q.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let keywords: Vec<String> = q.get("keywords")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|k| k.as_str().map(|s| s.to_lowercase())).collect())
                        .unwrap_or_default();
                    let content = match qtype.as_str() {
                        "world_book" => read_world_book(&deps, &ctx, &name, &keywords),
                        "character_prompt" => read_character_prompt(&deps, &ctx),
                        "skill" => read_skill(&deps, &name, &keywords),
                        "file" => read_file_checked(&deps, &ctx, &name),
                        "subtask" => read_subtask(&deps, &name),
                        _ => Err(format!("未知查询类型: {qtype}")),
                    };
                    let content = match content {
                        Ok(c) => {
                            let clipped: String = c.chars().take(max_chars).collect();
                            if c.chars().count() > max_chars {
                                format!("{clipped}\n…(已截断)")
                            } else {
                                clipped
                            }
                        }
                        Err(e) => format!("读取失败: {e}"),
                    };
                    results.push(json!({ "type": qtype, "name": name, "content": content }));
                }
                Ok(json!({ "results": results }).to_string())
            })
        }),
    );
}

/// 世界书内容:全部启用条目;提供 keywords 时按 comment/keys/content 过滤
fn read_world_book(
    deps: &ToolDeps,
    ctx: &ToolContext,
    name: &str,
    keywords: &[String],
) -> Result<String, String> {
    let mut entries: Vec<crate::parsing::world_book::WorldEntry> = Vec::new();
    if let Some(raw) = deps
        .characters
        .get(&ctx.character_id)
        .and_then(|c| c.data_raw)
    {
        entries.extend(crate::parsing::world_book::character_book_entries(&raw));
    }
    entries.extend(
        deps.world_books
            .collect_entries_for_character(&ctx.character_id),
    );
    let name_lower = name.to_lowercase();
    let mut parts: Vec<String> = Vec::new();
    for e in entries {
        if !e.enabled || e.content.trim().is_empty() {
            continue;
        }
        let comment_lower = e.comment.to_lowercase();
        if !name_lower.is_empty() && !comment_lower.contains(&name_lower) {
            continue;
        }
        if !keywords.is_empty() {
            let hay: String = format!(
                "{} {} {} {}",
                e.comment,
                e.keys.join(" "),
                e.regex.clone().unwrap_or_default(),
                e.content
            );
            let hay_lower = hay.to_lowercase();
            if !keywords.iter().any(|k| hay_lower.contains(k)) {
                continue;
            }
        }
        parts.push(format!("[{}]\n{}", e.comment, e.content.trim()));
    }
    if parts.is_empty() {
        return Err("未命中任何世界书条目".into());
    }
    Ok(parts.join("\n\n"))
}

fn read_character_prompt(deps: &ToolDeps, ctx: &ToolContext) -> Result<String, String> {
    let c = deps.characters.get(&ctx.character_id).ok_or("角色不存在")?;
    let mut out = format!("角色名: {}\n", c.chara_name);
    if !c.description.trim().is_empty() {
        out.push_str(&format!("\n角色设定:\n{}\n", c.description.trim()));
    }
    if let Some(fm) = c.first_mes.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("\n开场白(first_mes):\n{fm}\n"));
    }
    Ok(out)
}

fn read_skill(deps: &ToolDeps, name: &str, keywords: &[String]) -> Result<String, String> {
    if !name.is_empty() {
        if let Some(s) = deps.skills.get_by_name(name) {
            if !s.enabled {
                return Err(format!("skill「{name}」已停用"));
            }
            return Ok(format!("[{}]\n{}", s.name, s.content.trim()));
        }
        return Err(format!("skill「{name}」不存在"));
    }
    let list = deps.skills.list(true);
    let mut parts: Vec<String> = Vec::new();
    for s in list {
        let hay = format!("{} {} {}", s.name, s.description, s.content).to_lowercase();
        if keywords.is_empty() || keywords.iter().any(|k| hay.contains(k)) {
            parts.push(format!("[{}]\n{}\n", s.name, s.content.trim()));
        }
    }
    if parts.is_empty() {
        return Err("未命中任何 skill".into());
    }
    Ok(parts.join("\n\n"))
}

fn read_subtask(deps: &ToolDeps, name: &str) -> Result<String, String> {
    let task = deps.subtasks.get(name).ok_or("子任务不存在")?;
    Ok(json!({
        "id": task.id,
        "name": task.name,
        "status": task.status,
        "instruction": task.instruction,
        "result": task.result,
        "error": task.error,
    })
    .to_string())
}
