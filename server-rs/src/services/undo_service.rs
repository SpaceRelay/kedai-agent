// 回退快照服务(批次 6.1「undo」,L3):写工具执行前取逆操作负载落 undo_snapshots 表,
// 供「回退到此处」按快照逆序恢复。覆盖范围仅工具写入:
//   write/replace/create 的角色文件区文件与对话气泡、update_variables 变量树、
//   memory_write 记忆行;引擎自动落库的气泡不在范围。
// 两段式生命周期(快照点在 tools/registry.rs run_tool,所有工具执行唯一收口):
//   执行前 snapshot_before 取旧状态(内存暂存,不落库)→ 原执行 →
//   成功 commit(补记 inserted id 等执行后信息并落库)/ 失败 discard(直接丢弃)。
// 依赖注入:与 SessionService 共用同一 Arc<Db>(db 连接池不在 ToolDeps 上,
// 直接持有同源句柄最小侵入);经 ToolRegistry::set_undo(OnceLock)注入注册表,
// run_tool 收口读取;API 经 AppState.undo 访问。未注入(单元测试)时不产快照,
// 行为与旧版一致。
use crate::models::db::{now_iso, Db};
use crate::models::types::ToolContext;
use crate::services::memory_service::MemoryService;
use crate::services::session_service::SessionService;
use crate::services::settings_service::RuntimeSettings;
use crate::tools::agent_tools::safe_rel_path;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 纳入回退快照的写工具名单(其余工具不产生快照)
const WRITE_TOOLS: [&str; 5] = [
    "write",
    "replace",
    "create",
    "update_variables",
    "memory_write",
];

/// 单文件快照内容上限(256KB):超出记 truncated=true,restore 拒绝并中文提示
const MAX_SNAPSHOT_FILE_BYTES: usize = 256 * 1024;

/// 列表项(不含 payload,供 GET /api/chat/sessions/{id}/undo 输出)
#[derive(Debug, Clone, Serialize)]
pub struct UndoSnapshotInfo {
    pub id: String,
    pub tool_name: String,
    pub label: String,
    pub anchor_message_id: Option<i64>,
    pub created_at: String,
}

pub struct UndoService {
    db: Arc<Db>,
    sessions: Arc<SessionService>,
    memory: Arc<MemoryService>,
    /// 运行期设置(读 undo_enabled;全局开关,非双模式覆盖字段,故直接读基础值)
    settings: Arc<Mutex<RuntimeSettings>>,
    /// 数据目录(角色文件区 = data_dir/character_files/{character_id},
    /// 与 tools/agent_tools_shared.rs character_file_root 同规则)
    data_dir: PathBuf,
}

/// 执行前暂存的快照(未落库);commit/discard 两段式收口
pub struct SnapshotStager {
    service: Arc<UndoService>,
    session_id: String,
    tool_name: String,
    label: String,
    anchor_message_id: Option<i64>,
    payload: Value,
}

impl SnapshotStager {
    /// 执行成功:补记执行后信息(write 气泡的 message_id、memory_write 的插入 id)
    /// 并落库。快照落库失败只记日志不上抛——快照是增强能力,不影响主流程。
    pub fn commit(self, tool_output: &str) {
        let service = self.service.clone();
        service.commit_staged(self, tool_output);
    }

    /// 执行失败:快照随本次执行一并丢弃(两段式:未执行的写入不需要逆操作)
    pub fn discard(self) {}
}

impl UndoService {
    pub fn new(
        db: Arc<Db>,
        sessions: Arc<SessionService>,
        memory: Arc<MemoryService>,
        settings: Arc<Mutex<RuntimeSettings>>,
        data_dir: PathBuf,
    ) -> Self {
        UndoService {
            db,
            sessions,
            memory,
            settings,
            data_dir,
        }
    }

    /// 全局开关(锁中毒恢复取值,不 panic;只读取立即释放,不跨 .await)
    fn enabled(&self) -> bool {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .undo_enabled
    }

    fn file_root(&self, character_id: &str) -> PathBuf {
        self.data_dir.join("character_files").join(character_id)
    }

    /// 快照时该会话 max(messages.id)(无消息为 NULL)
    fn max_message_id(&self, session_id: &str) -> Option<i64> {
        let conn = self.db.read().ok()?;
        conn.query_row(
            "SELECT MAX(id) FROM messages WHERE session_id = ?1",
            params![session_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
    }

    /// run_tool 收口调用:写工具且开关开启时取执行前快照;构建失败(参数不合法等,
    /// 工具执行本身也会报错)返回 None——快照 best-effort,绝不挡工具执行。
    pub fn snapshot_before(
        self: &Arc<Self>,
        tool_name: &str,
        args: &Value,
        ctx: &ToolContext,
    ) -> Option<SnapshotStager> {
        if !WRITE_TOOLS.contains(&tool_name) || !self.enabled() {
            return None;
        }
        let (payload, label) = self.build_payload(tool_name, args, ctx)?;
        let anchor_message_id = self.max_message_id(&ctx.session_id);
        Some(SnapshotStager {
            service: self.clone(),
            session_id: ctx.session_id.clone(),
            tool_name: tool_name.to_string(),
            label,
            anchor_message_id,
            payload,
        })
    }

    /// 按工具构建逆操作负载(只读旧状态,不做任何修改);返回 (payload, 中文 label)
    fn build_payload(
        &self,
        tool_name: &str,
        args: &Value,
        ctx: &ToolContext,
    ) -> Option<(Value, String)> {
        match tool_name {
            "write" => self.build_write(args, ctx),
            "replace" => self.build_replace(args, ctx),
            "create" => self.build_create(args, ctx),
            "update_variables" => {
                // 逆操作 = 整树写回(save_assistant_vars);原状态无记录时 before 为空树,
                // restore 写回空树(会留下一行空 data_raw,语义等价,可接受)
                let before = self.sessions.load_assistant_vars(&ctx.session_id).to_json();
                Some((
                    json!({ "kind": "vars", "before": before }),
                    "更新变量树".to_string(),
                ))
            }
            "memory_write" => {
                let fact = args.get("fact").and_then(|v| v.as_str())?.trim();
                if fact.is_empty() {
                    return None; // 工具必然失败,无需快照
                }
                // 执行后补记 inserted id:以本角色执行前 max(id) 为界,commit 查其后的新行
                let max_id_before = self.max_memory_id(&ctx.character_id);
                let label = format!("写入记忆:{}", truncate_chars(fact, 20));
                Some((
                    json!({
                        "kind": "memory_insert",
                        "max_id_before": max_id_before,
                        "inserted_id": Value::Null,
                    }),
                    label,
                ))
            }
            _ => None,
        }
    }

    fn max_memory_id(&self, character_id: &str) -> i64 {
        let Ok(conn) = self.db.read() else {
            return 0;
        };
        conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM memory_entries WHERE character_id = ?1",
            params![character_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    /// 文件快照项:old = 执行前内容;不存在为 null(逆操作 = 删除);
    /// 内容超限或不可读(非 UTF-8)标 truncated=true(逆操作拒绝)
    fn file_entry(&self, character_id: &str, rel: &str, entry_type: &str) -> Value {
        let full = self.file_root(character_id).join(rel);
        if !full.exists() {
            return json!({
                "path": rel, "entry_type": entry_type,
                "existed": false, "truncated": false, "old": Value::Null,
            });
        }
        if entry_type == "dir" {
            // 目录无内容快照;create 目录逆操作 = 删除空目录
            return json!({
                "path": rel, "entry_type": entry_type,
                "existed": true, "truncated": false, "old": Value::Null,
            });
        }
        match std::fs::read_to_string(&full) {
            Ok(content) if content.len() <= MAX_SNAPSHOT_FILE_BYTES => json!({
                "path": rel, "entry_type": entry_type,
                "existed": true, "truncated": false, "old": content,
            }),
            // 超限或读取失败:都无法精确恢复,统一按截断拒绝语义处理
            _ => json!({
                "path": rel, "entry_type": entry_type,
                "existed": true, "truncated": true, "old": Value::Null,
            }),
        }
    }

    fn build_write(&self, args: &Value, ctx: &ToolContext) -> Option<(Value, String)> {
        let target = args.get("target").and_then(|v| v.as_str())?;
        match target {
            "file" => {
                let path = args.get("path").and_then(|v| v.as_str())?;
                let rel = safe_rel_path(path).ok()?;
                let entry = self.file_entry(&ctx.character_id, &rel, "file");
                Some((
                    json!({ "kind": "files", "entries": [entry] }),
                    format!("写入文件 {rel}"),
                ))
            }
            // 气泡插入:执行前无 id 可记,commit 时从工具输出补 message_id(逆操作 = 删除该行)
            "bubble" => Some((
                json!({ "kind": "bubble_insert", "message_id": Value::Null }),
                "写入对话气泡".to_string(),
            )),
            _ => None,
        }
    }

    fn build_replace(&self, args: &Value, ctx: &ToolContext) -> Option<(Value, String)> {
        let ops = args.get("operations")?.as_array()?;
        let mut files: Vec<Value> = Vec::new();
        let mut bubbles: Vec<Value> = Vec::new();
        let mut seen_files: Vec<String> = Vec::new();
        let mut seen_bubbles: Vec<i64> = Vec::new();
        for op in ops {
            match op.get("target").and_then(|v| v.as_str()) {
                Some("file") => {
                    let Some(path) = op.get("path").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let Ok(rel) = safe_rel_path(path) else {
                        continue; // 单项会失败,无状态变化,无需快照
                    };
                    // 同一路径多次操作:以执行前整文件快照为准,只记一次
                    if seen_files.contains(&rel) {
                        continue;
                    }
                    seen_files.push(rel.clone());
                    files.push(self.file_entry(&ctx.character_id, &rel, "file"));
                }
                Some("bubble") => {
                    let Some(id) = op.get("id").and_then(|v| v.as_i64()) else {
                        continue;
                    };
                    if seen_bubbles.contains(&id) {
                        continue;
                    }
                    seen_bubbles.push(id);
                    // 执行前整行快照(行不存在 → before:null,该单项会失败,restore 跳过)
                    let before = self
                        .sessions
                        .get_message(&ctx.session_id, id)
                        .map(|m| {
                            json!({
                                "id": m.id, "role": m.role, "content": m.content,
                                "extra": m.extra, "created_at": m.created_at,
                            })
                        })
                        .unwrap_or(Value::Null);
                    bubbles.push(json!({ "id": id, "before": before }));
                }
                _ => {}
            }
        }
        if files.is_empty() && bubbles.is_empty() {
            return None;
        }
        let label = if !files.is_empty() && !bubbles.is_empty() {
            "修改文件与气泡".to_string()
        } else if !files.is_empty() {
            format!("修改文件 {}", truncate_chars(&seen_files.join("、"), 40))
        } else if seen_bubbles.len() == 1 {
            format!("修改气泡 #{}", seen_bubbles[0])
        } else {
            format!("修改 {} 个气泡", seen_bubbles.len())
        };
        Some((
            json!({ "kind": "replace", "files": files, "bubbles": bubbles }),
            label,
        ))
    }

    fn build_create(&self, args: &Value, ctx: &ToolContext) -> Option<(Value, String)> {
        let items = args.get("items")?.as_array()?;
        let mut entries: Vec<Value> = Vec::new();
        for it in items {
            let Some(ty) = it.get("type").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(path) = it.get("path").and_then(|v| v.as_str()) else {
                continue;
            };
            let Ok(rel) = safe_rel_path(path) else {
                continue;
            };
            if !matches!(ty, "dir" | "file") {
                continue;
            }
            // 已存在的项 create 会单项失败(无状态变化),不需要逆操作
            if self.file_root(&ctx.character_id).join(&rel).exists() {
                continue;
            }
            entries.push(json!({
                "path": rel, "entry_type": ty,
                "existed": false, "truncated": false, "old": Value::Null,
            }));
        }
        if entries.is_empty() {
            return None;
        }
        let label = if entries.len() == 1 {
            let ty = if entries[0]["entry_type"].as_str() == Some("dir") {
                "目录"
            } else {
                "文件"
            };
            format!("新建{} {}", ty, entries[0]["path"].as_str().unwrap_or(""))
        } else {
            format!("新建 {} 项", entries.len())
        };
        Some((json!({ "kind": "files", "entries": entries }), label))
    }

    /// 两段式第二段:补记执行后信息并落库;失败只记日志
    fn commit_staged(&self, stager: SnapshotStager, tool_output: &str) {
        let SnapshotStager {
            session_id,
            tool_name,
            label,
            anchor_message_id,
            mut payload,
            ..
        } = stager;
        match payload.get("kind").and_then(|v| v.as_str()) {
            Some("bubble_insert") => {
                // write 气泡成功的输出为 JSON 且必含 message_id;取不到说明输出契约漂移,丢弃快照
                let mid = serde_json::from_str::<Value>(tool_output)
                    .ok()
                    .and_then(|out| out.get("message_id").and_then(|v| v.as_i64()));
                let Some(mid) = mid else {
                    tracing::warn!(
                        session_id = session_id,
                        "回退快照丢弃:write 气泡输出缺 message_id"
                    );
                    return;
                };
                payload["message_id"] = json!(mid);
            }
            Some("memory_insert") => {
                let before = payload
                    .get("max_id_before")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let inserted = self.new_memory_id_after(&session_id, before);
                let Some(inserted) = inserted else {
                    tracing::warn!(
                        session_id = session_id,
                        "回退快照丢弃:memory_write 后未找到新记忆行"
                    );
                    return;
                };
                payload["inserted_id"] = json!(inserted);
            }
            _ => {}
        }
        let id = uuid::Uuid::new_v4().to_string();
        let payload_str = payload.to_string();
        let conn = self.db.write();
        let r = conn.execute(
            "INSERT INTO undo_snapshots (id, session_id, anchor_message_id, tool_name, label, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                session_id,
                anchor_message_id,
                tool_name,
                label,
                payload_str,
                now_iso()
            ],
        );
        if let Err(e) = r {
            tracing::warn!(
                error = e.to_string(),
                "回退快照落库失败(不影响工具执行结果)"
            );
        }
    }

    /// memory_write 执行后:本角色 max_id_before 之后新插入的第一条记忆
    fn new_memory_id_after(&self, session_id: &str, before: i64) -> Option<i64> {
        let character_id = self.sessions.get(session_id).map(|s| s.character_id)?;
        let conn = self.db.read().ok()?;
        conn.query_row(
            "SELECT MIN(id) FROM memory_entries WHERE character_id = ?1 AND id > ?2",
            params![character_id, before],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 会话快照列表(新→旧;不含 payload)
    pub fn list(&self, session_id: &str) -> Vec<UndoSnapshotInfo> {
        let Ok(conn) = self.db.read() else {
            return Vec::new();
        };
        let mut stmt = match conn.prepare(
            "SELECT id, tool_name, label, anchor_message_id, created_at
             FROM undo_snapshots WHERE session_id = ?1
             ORDER BY created_at DESC, rowid DESC",
        ) {
            Ok(s) => s,
            Err(e) => {
                return super::log_query_failure("回退快照列表 prepare", e);
            }
        };
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(UndoSnapshotInfo {
                id: row.get(0)?,
                tool_name: row.get(1)?,
                label: row.get(2)?,
                anchor_message_id: row.get(3)?,
                created_at: row.get(4)?,
            })
        });
        match rows {
            Ok(r) => r.filter_map(|x| x.ok()).collect(),
            Err(e) => super::log_query_failure("回退快照列表 query_map", e),
        }
    }

    /// 回退:按 payload 逆序执行逆操作;成功后删除本快照及同会话更新的所有快照
    /// (回退到更早状态后,更新的快照语义已失效)。
    /// 返回 Ok(None) = 快照不存在(404 语义);Err = 执行失败(500 语义),
    /// 已执行的部分不回滚(逆操作各自独立幂等,调用方按错误提示手动处理)。
    pub fn restore(&self, id: &str) -> Result<Option<String>, String> {
        let row = {
            let conn = self.db.read().map_err(|e| e.to_string())?;
            conn.query_row(
                "SELECT session_id, label, payload, created_at FROM undo_snapshots WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                }, // 行不存在 → None
            )
            .optional()
            .map_err(|e| format!("读取回退快照失败: {e}"))?
        };
        let Some((session_id, label, payload_str, created_at)) = row else {
            return Ok(None);
        };
        let payload: Value = serde_json::from_str(&payload_str)
            .map_err(|e| format!("快照数据损坏,无法回退: {e}"))?;
        // character_id 以会话记录为权威(文件区/记忆恢复需要)
        let character_id = self
            .sessions
            .get(&session_id)
            .map(|s| s.character_id)
            .ok_or_else(|| "会话不存在,无法回退".to_string())?;

        self.apply_inverse(&session_id, &character_id, &payload)
            .map_err(|e| format!("{e};已完成的部分不会自动回滚,请检查后手动处理"))?;

        // 全部逆操作成功:删除本快照与同会话更新的快照(created_at 毫秒精度,
        // 同会话同毫秒的其它快照一并清理,视为同时刻之后,可接受)
        let conn = self.db.write();
        conn.execute(
            "DELETE FROM undo_snapshots WHERE session_id = ?1 AND created_at >= ?2",
            params![session_id, created_at],
        )
        .map_err(|e| format!("清理回退快照失败: {e}"))?;
        Ok(Some(label))
    }

    /// 按 payload kind 分派逆操作;数组类负载一律逆序(后执行的先撤销)
    fn apply_inverse(
        &self,
        session_id: &str,
        character_id: &str,
        payload: &Value,
    ) -> Result<(), String> {
        match payload.get("kind").and_then(|v| v.as_str()) {
            Some("files") => {
                let entries = payload
                    .get("entries")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                for entry in entries.iter().rev() {
                    self.restore_file_entry(character_id, entry)?;
                }
                Ok(())
            }
            Some("bubble_insert") => {
                let Some(mid) = payload.get("message_id").and_then(|v| v.as_i64()) else {
                    return Err("快照缺少气泡 id,无法回退".into());
                };
                // 幂等删除:行可能已被 truncate/手动删除,不存在不视为失败
                self.sessions.delete_message(session_id, mid);
                Ok(())
            }
            Some("replace") => {
                let files = payload
                    .get("files")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                for entry in files.iter().rev() {
                    self.restore_file_entry(character_id, entry)?;
                }
                let bubbles = payload
                    .get("bubbles")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                for row in bubbles.iter().rev() {
                    self.restore_bubble_row(session_id, row)?;
                }
                Ok(())
            }
            Some("vars") => {
                let before = payload
                    .get("before")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let vars = crate::parsing::assistant::AssistantVars::from_json(before);
                // 与 update_variables 工具同路径:只写 session_assistant_vars,不同步镜像
                self.sessions
                    .save_assistant_vars(session_id, &vars)
                    .map_err(|e| format!("恢复变量树失败: {e}"))?;
                Ok(())
            }
            Some("memory_insert") => {
                let Some(mid) = payload.get("inserted_id").and_then(|v| v.as_i64()) else {
                    return Err("快照缺少记忆 id,无法回退".into());
                };
                // 幂等删除:行可能已被手动删除
                self.memory.delete(mid);
                Ok(())
            }
            _ => Err("未知快照类型,无法回退".into()),
        }
    }

    /// 文件逆操作:old 有值 → 写回原内容;old=null + file → 删除;old=null + dir → 删空目录
    fn restore_file_entry(&self, character_id: &str, entry: &Value) -> Result<(), String> {
        let rel = entry
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "快照缺文件路径".to_string())?;
        let rel = safe_rel_path(rel)?; // 防御:payload 内路径理应合法
        if entry.get("truncated").and_then(|v| v.as_bool()) == Some(true) {
            return Err(format!("文件 {rel} 原始内容超过 256KB,快照不完整,无法回退"));
        }
        let full = self.file_root(character_id).join(&rel);
        match entry.get("old") {
            Some(Value::String(old)) => {
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("恢复文件 {rel} 时创建目录失败: {e}"))?;
                }
                std::fs::write(&full, old).map_err(|e| format!("恢复文件 {rel} 失败: {e}"))?;
                Ok(())
            }
            _ => {
                let is_dir = entry.get("entry_type").and_then(|v| v.as_str()) == Some("dir");
                if is_dir {
                    // 只删空目录:目录内若有后续内容(回退后用户/工具新写入),保守保留
                    match std::fs::remove_dir(&full) {
                        Ok(_) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => {}
                    }
                    Ok(())
                } else {
                    match std::fs::remove_file(&full) {
                        Ok(_) => Ok(()),
                        // 幂等:文件可能已不存在(单项执行失败/后续被删)
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(e) => Err(format!("删除文件 {rel} 失败: {e}")),
                    }
                }
            }
        }
    }

    /// 气泡逆操作:行还在 → UPDATE 回旧 content/extra;行已被删 → 整行插回(含原 id)。
    /// 直接走 SQL 而非 SessionService::update_message(编辑语义会附加 edited 标记;
    /// 整行恢复要求 content+extra 逐字节回到执行前)。
    fn restore_bubble_row(&self, session_id: &str, row: &Value) -> Result<(), String> {
        let Some(before) = row.get("before").filter(|b| !b.is_null()) else {
            return Ok(()); // 执行前行已不存在(该单项已失败),无状态变化
        };
        let id = before
            .get("id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| "快照缺气泡 id".to_string())?;
        let content = before
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let extra_str = before
            .get("extra")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".to_string());
        if self.sessions.get_message(session_id, id).is_some() {
            let conn = self.db.write();
            conn.execute(
                "UPDATE messages SET content = ?1, extra = ?2 WHERE session_id = ?3 AND id = ?4",
                params![content, extra_str, session_id, id],
            )
            .map_err(|e| format!("恢复气泡 #{id} 失败: {e}"))?;
        } else {
            let role = before
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("assistant");
            let created_at = before
                .get("created_at")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(now_iso);
            let conn = self.db.write();
            // 显式 id 插回(sqlite_sequence 不受影响,后续插入仍取 max+1)
            conn.execute(
                "INSERT INTO messages (id, session_id, role, content, extra, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, session_id, role, content, extra_str, created_at],
            )
            .map_err(|e| format!("插回气泡 #{id} 失败: {e}"))?;
        }
        Ok(())
    }
}

/// 中文 label 辅助:按字符数截断(避免超长路径/事实撑爆列表展示)
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}
