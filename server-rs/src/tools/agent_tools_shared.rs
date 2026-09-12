// Agent 强化工具集共享件(中层 L3 青层工具域):
//   - 角色文件区路径安全校验(character_file_root / safe_rel_path)
//   - 文件区读写(带父目录创建,read_file_checked / write_file_checked)
//   - 无依赖随机源(Rng / random_u64)
//   - 任务模式虚拟 session 前缀解析(task_session_prefix / subtask_candidates):
//     read(type=subtask) 与 todo 跨 agent 可见性共用,单一出处
//   - role 工具(创建随机数/掷骰子):与 RNG 同源,故随共享件驻留
// 可见性约定:供 read/write 域与 agent_tools.rs 聚合入口使用的项均 pub(super);
// character_file_root 保持 pub(外部模块可能经 agent_tools.rs 重导出引用);
// safe_rel_path 为 pub(crate):批次 6.1 undo_service 恢复快照时复用同一路径安全规则。
use crate::models::types::{AgentSubtaskRecord, ToolContext, ToolDefinition};
use crate::tools::registry::ToolRegistry;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

use super::agent_tools::ToolDeps;

/// 提取任务模式虚拟 session 的「任务前缀」:`task:{id}` / `task:{id}:main:{n}` /
/// `task:{id}:sub:{tid}` 一律返回 `task:{id}`;非 `task:` 前缀(聊天路径)返回 None。
/// 实现:去掉 `task:` 后取第一段作为 id 拼回;空 id(`"task:"`)返回 None。
/// 单一出处:read(type=subtask) 候选集与 todo 跨 agent 可见性共用(审计项 C/E)。
pub(super) fn task_session_prefix(session_id: &str) -> Option<String> {
    let rest = session_id.strip_prefix("task:")?;
    let id = rest.split(':').next().unwrap_or("");
    // 空 id 不是合法任务前缀(避免 "task:" / "task::main:1" 把整个前缀空间误当候选集)
    if id.is_empty() {
        return None;
    }
    Some(format!("task:{id}"))
}

/// 子任务候选集:能取到任务前缀就用前缀列举(覆盖 team 的 `:main:`/`:sub:` 派生
/// 虚拟 session,跨 agent 证据可见),否则退回精确 session(聊天路径,DB 语义)。
pub(super) fn subtask_candidates(deps: &ToolDeps, session_id: &str) -> Vec<AgentSubtaskRecord> {
    match task_session_prefix(session_id) {
        Some(prefix) => deps.subtasks.list_by_session_prefix(&prefix),
        None => deps.subtasks.list_by_session(session_id),
    }
}

/// 角色文件区根目录:data/character_files/{character_id}/
pub fn character_file_root(deps: &ToolDeps, character_id: &str) -> PathBuf {
    deps.data_dir.join("character_files").join(character_id)
}

/// 相对路径安全校验:规范化(反斜杠转正斜杠、去空段),拒绝绝对路径/盘符/上级目录/空
pub(crate) fn safe_rel_path(p: &str) -> Result<String, String> {
    // 绝对路径(Unix 前缀 /、Windows 盘符 C: 或 UNC \\)直接拒绝,防止逃离角色文件区
    if p.starts_with('/') || p.starts_with('\\') || p.contains(':') {
        return Err(format!("非法路径(不允许绝对路径/盘符): {p}"));
    }
    let norm = p.replace('\\', "/");
    let mut segs = Vec::new();
    for seg in norm.split('/') {
        match seg.trim() {
            "" | "." => continue,
            ".." => return Err(format!("非法路径(不允许上级目录): {p}")),
            s => segs.push(s),
        }
    }
    if segs.is_empty() {
        return Err("路径不能为空".into());
    }
    Ok(segs.join("/"))
}

/// 读文件区文件(不存在返回 Err)
pub(super) fn read_file_checked(
    deps: &ToolDeps,
    ctx: &ToolContext,
    rel: &str,
) -> Result<String, String> {
    let rel = safe_rel_path(rel)?;
    let path = character_file_root(deps, &ctx.character_id).join(&rel);
    std::fs::read_to_string(&path).map_err(|e| format!("读取文件 {rel} 失败: {e}"))
}

/// 写文件区文件(自动建父目录)
pub(super) fn write_file_checked(
    deps: &ToolDeps,
    ctx: &ToolContext,
    rel: &str,
    content: &str,
) -> Result<String, String> {
    let rel = safe_rel_path(rel)?;
    let root = character_file_root(deps, &ctx.character_id);
    let path = root.join(&rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    std::fs::write(&path, content).map_err(|e| format!("写入文件 {rel} 失败: {e}"))?;
    Ok(rel)
}

/// 无依赖随机源:RandomState 每次实例化随机种子 + 时间戳混合;xorshift64* 输出
pub(super) fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
    );
    h.write_u64(0x9E37_79B9_7F4A_7C15);
    h.finish()
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// [min, max] 闭区间整数
    fn range(&mut self, min: i64, max: i64) -> i64 {
        if max <= min {
            return min;
        }
        let span = (max - min + 1) as u64;
        min + (self.next() % span) as i64
    }
}

// ==================== role:创建随机数(骰子) ====================
pub(super) fn register_role(registry: &ToolRegistry, _deps: Arc<ToolDeps>) {
    registry.register(
        ToolDefinition {
            name: "role".into(),
            description: "创建随机数/掷骰子。支持两种模式:骰子(sides=面数,count=次数,如 1d20)或范围(min/max)。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "sides": { "type": "integer", "description": "骰子面数(默认 20)" },
                    "count": { "type": "integer", "description": "骰子次数(默认 1,上限 100)" },
                    "min": { "type": "integer", "description": "范围模式最小值" },
                    "max": { "type": "integer", "description": "范围模式最大值" }
                }
            }),
        },
        Arc::new(|args: Value, _ctx: ToolContext| {
            Box::pin(async move {
                let mut rng = Rng(random_u64());
                let sides = args.get("sides").and_then(|v| v.as_i64()).unwrap_or(20);
                let count = args.get("count").and_then(|v| v.as_i64()).unwrap_or(1).clamp(1, 100);
                let mut rolls: Vec<i64> = Vec::new();
                let mut total: i64 = 0;
                if let (Some(min), Some(max)) = (
                    args.get("min").and_then(|v| v.as_i64()),
                    args.get("max").and_then(|v| v.as_i64()),
                ) {
                    if max < min {
                        return Err("min 不能大于 max".into());
                    }
                    for _ in 0..count {
                        let r = rng.range(min, max);
                        rolls.push(r);
                        total += r;
                    }
                } else {
                    if sides <= 1 {
                        return Err("sides 必须大于 1".into());
                    }
                    for _ in 0..count {
                        let r = rng.range(1, sides);
                        rolls.push(r);
                        total += r;
                    }
                }
                Ok(json!({ "rolls": rolls, "total": total, "count": count }).to_string())
            })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::task_session_prefix;

    /// 三种任务模式虚拟 session 形态一律折叠到 `task:{id}` 前缀
    #[test]
    fn task_session_prefix_folds_all_virtual_forms() {
        assert_eq!(
            task_session_prefix("task:t1").as_deref(),
            Some("task:t1"),
            "multi 主 agent 形态"
        );
        assert_eq!(
            task_session_prefix("task:t1:main:2").as_deref(),
            Some("task:t1"),
            "team 主 agent 派生形态"
        );
        assert_eq!(
            task_session_prefix("task:t1:sub:abc").as_deref(),
            Some("task:t1"),
            "子 agent 派生形态"
        );
    }

    /// 非 task 会话(聊天路径)与空 id 边界返回 None
    #[test]
    fn task_session_prefix_rejects_non_task_and_empty_id() {
        assert_eq!(task_session_prefix("sess-123"), None, "聊天会话无前缀语义");
        assert_eq!(task_session_prefix(""), None, "空串");
        assert_eq!(task_session_prefix("task:"), None, "空 id 不是合法前缀");
        assert_eq!(
            task_session_prefix("task::main:1"),
            None,
            "空 id + 派生后缀同样非法"
        );
    }
}
