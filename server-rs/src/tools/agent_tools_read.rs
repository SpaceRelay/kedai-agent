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
                        "subtask" => {
                            // 子任务走结构化三态:命中给内容,未命中/歧义给「失败 + 候选」。
                            // 旧实现把候选清单塞进与正常结果同构的 content 字符串,消费方
                            // 必须先解析 JSON 再判 matched_by,漏判就把候选清单当成任务
                            // 结果用;候选还随会话任务数线性增长(实测一次回带 15 条)。
                            match read_subtask(&deps, &ctx, &name) {
                                Ok(SubtaskLookup::Hit(hit)) => {
                                    let hit_text = hit.to_string();
                                    let clipped: String = hit_text.chars().take(max_chars).collect();
                                    let content = if hit_text.chars().count() > max_chars {
                                        format!("{clipped}\n…(已截断)")
                                    } else {
                                        clipped
                                    };
                                    results.push(json!({
                                        "type": qtype, "name": name, "ok": true, "content": content,
                                    }));
                                    continue;
                                }
                                Ok(SubtaskLookup::Miss { reason, candidates }) => {
                                    results.push(json!({
                                        "type": qtype, "name": name, "ok": false,
                                        "error": reason, "candidates": candidates,
                                    }));
                                    continue;
                                }
                                Err(e) => Err(e),
                            }
                        }
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
                        Err(e) => {
                            // 统一结构化失败:ok=false + error 文本(旧实现把它包成
                            // content 字符串,消费方无法一眼区分「读到了内容」与「读取失败」)
                            results.push(json!({
                                "type": qtype, "name": name, "ok": false,
                                "error": format!("读取失败: {e}"),
                            }));
                            continue;
                        }
                    };
                    results.push(json!({ "type": qtype, "name": name, "ok": true, "content": content }));
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
                return Err(format!(
                    "skill「{name}」已停用。下一步:在技能库中启用该技能,或改读其他已启用技能"
                ));
            }
            return Ok(format!("[{}]\n{}", s.name, s.content.trim()));
        }
        return Err(format!(
            "skill「{name}」不存在。下一步:不带 name 调用 read(type=skill) 可列出全部可用技能,从中核对名称"
        ));
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
        return Err(
            "未命中任何 skill。下一步:去掉 keywords 列出全部技能,或更换更宽泛的关键词".into(),
        );
    }
    Ok(parts.join("\n\n"))
}

/// 子任务查询结果三态(2026-09-15 结构化改造):命中给内容,未命中/歧义给候选。
///
/// 为什么不再用「与正常结果同构的 content 字符串」:旧实现把未命中/歧义都拼成
/// JSON 塞进 `content`,与正常命中完全同形,消费方必须先解析再判 `matched_by`,
/// 漏判就会把候选清单当成任务结果;且候选随会话任务数线性增长(实测一次 15 条),
/// 长会话里每次误查都是一笔无谓的 token 开销。
enum SubtaskLookup {
    /// 命中的子任务(含 id/name 双向别名与 matched_by)
    Hit(Value),
    /// 未命中或歧义:带可读原因 + 裁剪后的候选
    Miss {
        reason: String,
        candidates: Vec<Value>,
    },
}

/// 候选裁剪(2026-09-15):默认排除已 `ended` 的记录(它们不再有取用价值,却最占位置),
/// 再截断到 top-3。让「未命中」的返回体保持稳定体积,不随会话任务数膨胀。
fn trimmed_candidates(records: &[&crate::models::types::AgentSubtaskRecord]) -> Vec<Value> {
    records
        .iter()
        .filter(|t| t.status != "ended")
        .take(3)
        .map(|t| json!({ "id": t.id, "name": t.name, "status": t.status }))
        .collect()
}

/// 子任务查询(审计项 C):三键命中,首次命中即返回,避免模型只知子任务名却查不到。
/// 命中顺序:先精确 id(deps.subtasks.get,含内存覆盖层)返回 matched_by:"id";
/// 再在会话作用域候选集内精确 name(trim 相等)返回 matched_by:"name";
/// 最后候选集内 name 大小写不敏感子串命中返回 matched_by:"name_like"。
/// 候选集口径:能取到任务前缀就用 list_by_session_prefix(覆盖 team 的 :main:/:sub:
/// 派生虚拟 session),否则 list_by_session(聊天路径);与 todo 共用 shared helper。
/// 命中唯一时返回 JSON 同时含 id 与 name(双向别名);多命中/未命中返回 `Miss`
///(候选 top-3、默认排除 ended),由调用方落成结构化失败项。
fn read_subtask(deps: &ToolDeps, ctx: &ToolContext, name: &str) -> Result<SubtaskLookup, String> {
    use super::agent_tools_shared::subtask_candidates;

    // 1) 精确 id:内存覆盖层与 DB 都能命中(不依赖会话候选集)
    if let Some(task) = deps.subtasks.get(name) {
        return Ok(SubtaskLookup::Hit(subtask_hit_json(&task, "id")));
    }

    let candidates = subtask_candidates(deps, &ctx.session_id);
    let needle = name.trim();
    // 2) 候选集内精确 name(trim 后相等);同名多条同样走歧义分支
    let exact: Vec<&crate::models::types::AgentSubtaskRecord> = candidates
        .iter()
        .filter(|t| t.name.trim() == needle)
        .collect();
    if exact.len() == 1 {
        return Ok(SubtaskLookup::Hit(subtask_hit_json(exact[0], "name")));
    }
    if exact.len() > 1 {
        return Ok(SubtaskLookup::Miss {
            reason: format!(
                "子任务「{name}」匹配到 {} 条同名记录,请改用 id 精确查询",
                exact.len()
            ),
            candidates: trimmed_candidates(&exact),
        });
    }
    // 3) 候选集内 name 大小写不敏感子串
    let needle_lower = needle.to_lowercase();
    let matched: Vec<&crate::models::types::AgentSubtaskRecord> = candidates
        .iter()
        .filter(|t| t.name.to_lowercase().contains(&needle_lower))
        .collect();
    match matched.len() {
        0 => {
            if candidates.is_empty() {
                return Ok(SubtaskLookup::Miss {
                    reason: format!(
                        "子任务「{name}」不存在,且当前会话无可查询的子任务。\
                         下一步:先用 agentgo 排出子任务,再按返回的 task_id 查询"
                    ),
                    candidates: Vec::new(),
                });
            }
            Ok(SubtaskLookup::Miss {
                reason: format!(
                    "子任务「{name}」未命中任何候选。\
                     下一步:改用 candidates 中的 id 或完整 name 查询"
                ),
                candidates: trimmed_candidates(&candidates.iter().collect::<Vec<_>>()),
            })
        }
        1 => Ok(SubtaskLookup::Hit(subtask_hit_json(
            matched[0],
            "name_like",
        ))),
        // 子串匹配到多条:给候选让模型自选,不随便挑一条
        _ => Ok(SubtaskLookup::Miss {
            reason: format!(
                "子任务「{name}」匹配到 {} 条(子串匹配),请用 id 或完整 name 精确查询",
                matched.len()
            ),
            candidates: trimmed_candidates(&matched),
        }),
    }
}

/// 命中结果 JSON:id 与 name 双向别名 + matched_by 命中方式;字段沿用现状
/// (批次 4 起含 finished_at:终态时刻自证,不必用 ended 反推有无结果)
fn subtask_hit_json(task: &crate::models::types::AgentSubtaskRecord, matched_by: &str) -> Value {
    json!({
        "id": task.id,
        "name": task.name,
        "matched_by": matched_by,
        "status": task.status,
        "instruction": task.instruction,
        "result": task.result,
        "error": task.error,
        "finished_at": task.finished_at,
    })
}
