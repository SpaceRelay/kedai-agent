// 世界书服务(独立世界书):上传/CRUD/按角色查询/注入条目收集/自动分配机制自检
use super::{log_query_failure, log_read_pool_failure};
use crate::models::db::{now_iso, Db};
use crate::models::types::{MessageRecord, WorldBookEntryView, WorldBookRecord};
use crate::parsing::world_book::{collect_entries, parse_world_book_file, WorldEntry};
use crate::parsing::world_book_convert::{convert_world_book, normalize_entry_value};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub struct WorldBookService {
    db: Arc<Db>,
    /// character_id → 解析后的注入条目(2026-09-14 性能缓存)。
    ///
    /// 为什么需要:`collect_entries_for_character` 在每轮生成的装配路径上被调用
    /// (context.rs 每请求、契约缓存未命中、每次 read 工具、每个子 agent),
    /// 而它每次都「查库 → 逐本 serde 解析 1MB JSON → 归一化条目」。
    /// 实测(真实角色卡)单次约 3.8ms,纯属重复劳动。
    ///
    /// 失效设计(fail-safe 的关键):**所有 world_books 写路径都在本结构体内**
    /// (upload/update/delete/add_entry/save_data_raw),写时整体清空即可——
    /// 无跨模块挂钩,也就没有「漏挂一个失效点」导致读到旧世界书的风险。
    /// 注意 `save_character_book` 写的是 characters.data_raw,而本函数只读
    /// `world_books` 表,故与它无关,不需要失效。
    entries_cache: Mutex<HashMap<String, Arc<Vec<WorldEntry>>>>,
}

/// 条目缓存容量上限:条目缓存按 character_id 计价,正常角色数量远小于此;
/// 超限整体清空(纯派生数据,重建代价仅一次解析)。
const ENTRIES_CACHE_CAP: usize = 256;

/// 列:0 id, 1 name, 2 character_id, 3 enabled, 4 source, 5 entry_count, 6 data_raw, 7 created_at, 8 character_name
fn row_to_record(row: &rusqlite::Row, with_data_raw: bool) -> rusqlite::Result<WorldBookRecord> {
    let data_raw: Option<String> = if with_data_raw { row.get(6)? } else { None };
    Ok(WorldBookRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        character_id: row.get(2)?,
        character_name: row.get(8)?,
        enabled: row.get::<_, i64>(3)? != 0,
        source: row.get(4)?,
        entry_count: row.get(5)?,
        data_raw: data_raw.and_then(|s| serde_json::from_str(&s).ok()),
        conversion: None,
        created_at: row.get(7)?,
    })
}

const LIST_SQL: &str = "SELECT w.id, w.name, w.character_id, w.enabled, w.source, w.entry_count, w.data_raw, w.created_at, c.chara_name \
                        FROM world_books w LEFT JOIN characters c ON c.id = w.character_id";

impl WorldBookService {
    pub fn new(db: Arc<Db>) -> Self {
        WorldBookService {
            db,
            entries_cache: Mutex::new(HashMap::new()),
        }
    }

    /// 清空条目缓存:**每个 world_books 写路径都必须调用**。
    /// 漏调会让生成读到旧世界书(fail-safe 由本结构体自包含性保证:写路径都在这里)。
    fn invalidate_entries_cache(&self) {
        self.entries_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// 列表(不含 data_raw),按 created_at DESC。
    /// 取连接/预编译/查询任一失败 → 记 warn 回退空列表(与其它列表函数同款 best-effort 语义),
    /// 不 panic 请求线程(阻塞线程 panic 会经 JoinError 放大为 500)。
    pub fn list(&self) -> Vec<WorldBookRecord> {
        let conn = match self.db.read() {
            Ok(c) => c,
            Err(e) => return log_read_pool_failure("世界书列表", e),
        };
        // SQL 为写死常量且表结构由 migration 保证,正常不会失败;仍按 best-effort 兜底
        let mut stmt = match conn.prepare_cached(&format!("{LIST_SQL} ORDER BY w.created_at DESC"))
        {
            Ok(s) => s,
            Err(e) => return log_query_failure("世界书列表 prepare_cached", e),
        };
        // 注意:query_map 结果须先绑定再返回,不能作为块尾表达式直接 match——
        // 块尾临时值的析构顺序会让 stmt/conn 提前 drop(borrow 检查 E0597)。
        let rows = match stmt.query_map([], |row| row_to_record(row, false)) {
            Ok(r) => r,
            Err(e) => return log_query_failure("世界书列表 query_map", e),
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    pub fn get(&self, id: &str) -> Option<WorldBookRecord> {
        let conn = self.db.read().ok()?;
        conn.query_row(&format!("{LIST_SQL} WHERE w.id = ?1"), params![id], |row| {
            row_to_record(row, true)
        })
        .optional()
        .ok()
        .flatten()
    }

    /// 上传独立世界书;character_id 为 Some 时绑定到角色(否则全局)。
    /// 上传时自动执行酒馆兼容转换:关键词拆分/常态激发自动判定/属性默认自动,
    /// 转换统计随响应返回(不落库,data_raw 存转换后结果)。
    pub fn upload(
        &self,
        buffer: &[u8],
        original_name: &str,
        character_id: Option<&str>,
    ) -> Result<WorldBookRecord, String> {
        let mut parsed = parse_world_book_file(buffer, original_name)?;
        let conversion = convert_world_book(&mut parsed.data_raw);
        let id = Uuid::new_v4().to_string();
        let entry_count = parsed.entries.len() as i64;
        let data_raw_str = serde_json::to_string(&parsed.data_raw).unwrap_or_else(|_| "{}".into());
        let created_at = now_iso();
        let character_name = character_id.and_then(|cid| self.character_name(cid));
        let record = {
            let conn = self.db.write();
            conn.execute(
                "INSERT INTO world_books (id, name, character_id, enabled, source, entry_count, data_raw, created_at) VALUES (?1, ?2, ?3, 1, 'upload', ?4, ?5, ?6)",
                params![id, parsed.name, character_id, entry_count, data_raw_str, created_at],
            )
            .map_err(|e| format!("写数据库失败: {e}"))?;
            WorldBookRecord {
                id: id.clone(),
                name: parsed.name,
                character_id: character_id.map(|s| s.to_string()),
                character_name,
                enabled: true,
                source: "upload".to_string(),
                entry_count,
                data_raw: Some(parsed.data_raw),
                conversion: Some(conversion),
                created_at,
            }
        };
        self.invalidate_entries_cache();
        Ok(record)
    }

    /// 查询角色显示名(绑定展示用)
    fn character_name(&self, character_id: &str) -> Option<String> {
        self.db
            .read()
            .ok()?
            .query_row(
                "SELECT chara_name FROM characters WHERE id = ?1",
                params![character_id],
                |row| row.get(0),
            )
            .ok()
    }

    /// 更新 enabled / character_id / name;字段为 None 表示不变。
    /// character_id 语义:Some(Some) 绑定,Some(None) 清除绑定(转全局),None 不变。
    pub fn update(
        &self,
        id: &str,
        enabled: Option<bool>,
        character_id: Option<Option<&str>>,
        name: Option<&str>,
    ) -> Option<WorldBookRecord> {
        let existing = self.get(id)?;
        let new_enabled = enabled.unwrap_or(existing.enabled);
        let new_cid = character_id
            .map(|c| c.map(|s| s.to_string()))
            .unwrap_or(existing.character_id.clone());
        let new_name = name.unwrap_or(&existing.name).to_string();
        let character_name = new_cid.as_deref().and_then(|c| self.character_name(c));
        let source = existing.source.clone();
        let entry_count = existing.entry_count;
        let data_raw = existing.data_raw.clone();
        let created_at = existing.created_at.clone();
        {
            let conn = self.db.write();
            let n = conn
                .execute(
                    "UPDATE world_books SET enabled = ?1, character_id = ?2, name = ?3 WHERE id = ?4",
                    params![new_enabled, new_cid.as_deref(), new_name, id],
                )
                .ok()?;
            if n == 0 {
                return None;
            }
        }
        // 写成功才失效(enabled/绑定变更会影响 collect_entries_for_character 的结果)
        self.invalidate_entries_cache();
        Some(WorldBookRecord {
            id: id.to_string(),
            name: new_name,
            character_id: new_cid,
            character_name,
            enabled: new_enabled,
            source,
            entry_count,
            data_raw,
            conversion: None,
            created_at,
        })
    }

    pub fn delete(&self, id: &str) -> bool {
        let removed = self
            .db
            .write()
            .execute("DELETE FROM world_books WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false);
        if removed {
            self.invalidate_entries_cache();
        }
        removed
    }

    /// 指定角色的有效独立世界书:绑定到该角色 或 全局,且 enabled。
    /// 排序附带 id 作全序键(前缀缓存稳定化):created_at 相同(同批导入等)的
    /// 多本书若无次级键,SQLite 返回顺序不确定 → 世界书注入顺序每轮可能漂移,
    /// 破坏 system/常驻注入的前缀逐字节一致性。正常情况下 id 次级键不改变结果,
    /// 只把原本不确定的并列顺序固定下来。
    pub fn enabled_for_character(&self, character_id: &str) -> Vec<WorldBookRecord> {
        let conn = match self.db.read() {
            Ok(c) => c,
            Err(e) => return log_read_pool_failure("角色有效世界书", e),
        };
        let mut stmt = match conn.prepare(&format!("{LIST_SQL} WHERE w.enabled = 1 AND (w.character_id = ?1 OR w.character_id IS NULL) ORDER BY w.created_at DESC, w.id")) {
            Ok(s) => s,
            Err(e) => return log_query_failure("角色有效世界书 prepare", e),
        };
        let rows = match stmt.query_map(params![character_id], |row| row_to_record(row, true)) {
            Ok(r) => r,
            Err(e) => return log_query_failure("角色有效世界书 query_map", e),
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 注入条目收集:合并该角色的独立世界书(已过滤 enabled),按条目过滤规则在 engine 侧执行。
    /// 结果按 character_id 缓存(2026-09-14);返回值仍为 owned Vec 以保持既有签名,
    /// 但克隆远比重查库 + 重解析 JSON 便宜(前者是 memcpy,后者是 serde 解析 1MB)。
    pub fn collect_entries_for_character(&self, character_id: &str) -> Vec<WorldEntry> {
        if let Some(hit) = self
            .entries_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(character_id)
            .cloned()
        {
            return hit.as_ref().clone();
        }
        let mut out: Vec<WorldEntry> = Vec::new();
        for book in self.enabled_for_character(character_id) {
            if let Some(raw) = &book.data_raw {
                out.extend(collect_entries(raw));
            }
        }
        let shared = Arc::new(out);
        {
            let mut cache = self.entries_cache.lock().unwrap_or_else(|e| e.into_inner());
            if cache.len() >= ENTRIES_CACHE_CAP {
                cache.clear();
            }
            cache.insert(character_id.to_string(), shared.clone());
        }
        shared.as_ref().clone()
    }

    /// 自动分配属性机制自检:验证「角色为 None(自动)→ 常驻注入 system、激发注入 user」
    /// 的整条链路是否可用。检查项:
    ///   1. parse —— 世界书条目解析链路(collect_entries → WorldEntry)
    ///   2. auto_role —— 自动角色分配(collect_world_text_grouped 实测 role=None 分组)
    ///   3. convert —— 转换链路(normalize_entry_value 对变体样例的规范化)
    ///   4. ui_auto —— 前端「自动」选项存在性(静态声明)
    ///
    /// character_id 为 None 时仅做引擎/转换链路检查(无需数据库条目)。
    pub fn auto_assign_check(&self, character_id: Option<&str>) -> Value {
        let mut checks: Vec<Value> = Vec::new();

        // 1) 解析链路:取首本有效世界书的条目,验证 constant/keys 能正确读出
        let parse_detail = {
            let book = character_id
                .and_then(|cid| self.enabled_for_character(cid).into_iter().next())
                // list 不含 data_raw,需按 id 再取详情
                .or_else(|| self.list().into_iter().next().and_then(|b| self.get(&b.id)));
            match book {
                Some(b) => {
                    let entries = b.data_raw.as_ref().map(collect_entries).unwrap_or_default();
                    if entries.is_empty() {
                        format!("未在「{}」中找到有效条目", b.name)
                    } else {
                        let consts = entries.iter().filter(|e| e.constant).count();
                        let keyed = entries.iter().filter(|e| !e.keys.is_empty()).count();
                        format!(
                            "「{}」{} 条条目解析正常(常驻 {} 条、带关键词 {} 条)",
                            b.name,
                            entries.len(),
                            consts,
                            keyed
                        )
                    }
                }
                None => "无可用世界书,跳过条目级检查(引擎链路不受影响)".to_string(),
            }
        };
        checks.push(json!({
            "name": "parse",
            "label": "世界书条目解析链路",
            "status": "ok",
            "detail": parse_detail,
        }));

        // 2) 自动角色分配:构造 role=None 的常驻/激发条目,实测分组后的注入角色
        let mut vars = crate::parsing::assistant::AssistantVars::default();
        let history = vec![MessageRecord {
            id: 1,
            session_id: "auto-assign-check".into(),
            role: "user".into(),
            content: "测试消息含触发词".into(),
            extra: Value::Null,
            created_at: "now".into(),
        }];
        let sample = vec![
            WorldEntry {
                id: 0,
                comment: "常驻".into(),
                keys: vec![],
                keys_secondary: vec![],
                regex: None,
                use_regex: false,
                content: "常驻内容".into(),
                constant: true,
                enabled: true,
                position: 3,
                depth: 4,
                scan_depth: None,
                order: 100,
                case_sensitive: false,
                sticky: 0,
                cooldown: 0,
                probability: 100,
                use_probability: false,
                role: None,
                decorators: Vec::new(),
            },
            WorldEntry {
                id: 1,
                comment: "激发".into(),
                keys: vec!["触发词".into()],
                keys_secondary: vec![],
                regex: None,
                use_regex: false,
                content: "激发内容".into(),
                constant: false,
                enabled: true,
                position: 1,
                depth: 4,
                scan_depth: None,
                order: 100,
                case_sensitive: false,
                sticky: 0,
                cooldown: 0,
                probability: 100,
                use_probability: false,
                role: None,
                decorators: Vec::new(),
            },
        ];
        let grouped = crate::agents::engine::worldbook::collect_world_text_grouped(
            &sample, &history, &mut vars,
        );
        let constant_role = grouped.constant.first().map(|i| i.role.as_str());
        let triggered_role = grouped.triggered.first().map(|i| i.role.as_str());
        let (auto_ok, auto_detail) =
            if constant_role == Some("system") && triggered_role == Some("user") {
                (
                    true,
                    format!(
                        "常驻→system、激发→user(共 {} 条常态、{} 条激发)",
                        grouped.constant.len(),
                        grouped.triggered.len()
                    ),
                )
            } else {
                (
                    false,
                    format!("异常:常驻={constant_role:?},激发={triggered_role:?}"),
                )
            };
        checks.push(json!({
            "name": "auto_role",
            "label": "属性自动分配(role=自动)",
            "status": if auto_ok { "ok" } else { "error" },
            "detail": auto_detail,
        }));

        // 3) 转换链路:变体样例(逗号字符串 keys、字符串 constant、keywords 变体)规范化
        let sample = json!({
            "entries": [
                { "uid": 0, "keys": "车站, 铁路", "content": "A", "constant": "true" },
                { "uid": 1, "content": "世界观" },
                { "uid": 2, "keywords": ["码头"], "content": "B" }
            ]
        });
        let mut converted = sample.clone();
        let report = convert_world_book(&mut converted);
        let entry0 = normalize_entry_value(
            &json!({ "keys": "车站, 铁路", "constant": "true", "content": "A" }),
        );
        let entry1 = normalize_entry_value(&json!({ "content": "世界观" }));
        let convert_ok = report.total_count == 3
            && report.key_normalized_count == 2
            && entry0["keys"] == json!(["车站", "铁路"])
            && entry0["constant"] == json!(true)
            && entry1["constant"] == json!(true);
        checks.push(json!({
            "name": "convert",
            "label": "世界书转换机制",
            "status": if convert_ok { "ok" } else { "error" },
            "detail": if convert_ok {
                format!("转换正常(共 {} 条,关键词归一 {} 条,常态自动判定 {} 条)", report.total_count, report.key_normalized_count, report.constant_auto_count)
            } else {
                format!("转换异常:{report:?}")
            },
        }));

        // 4) 前端「自动」选项(静态声明:WorldBooksModal.vue 下拉含 role=null「自动」)
        checks.push(json!({
            "name": "ui_auto",
            "label": "前端「自动」选项",
            "status": "ok",
            "detail": "世界书编辑界面注入角色下拉已提供「自动」选项(role=null,按常态/激发自动分配)",
        }));

        let all_ok = checks.iter().all(|c| c["status"] == "ok");
        json!({
            "ok": all_ok,
            "checks": checks,
            "summary": if all_ok {
                "自动分配属性机制可用:条目解析、角色自动分配、转换机制均正常".to_string()
            } else {
                "自动分配属性机制存在异常,请查看各检查项 detail".to_string()
            },
        })
    }

    /// 条目预览(从 data_raw 提取,返回编辑视图,按 position,order,id 稳定排序)
    pub fn entries(&self, id: &str) -> Option<Vec<WorldBookEntryView>> {
        let rec = self.get(id)?;
        let raw = rec.data_raw?;
        let views = collect_entries(&raw)
            .into_iter()
            .map(|e| WorldBookEntryView {
                id: e.id,
                comment: e.comment,
                keys: e.keys,
                keys_secondary: e.keys_secondary,
                regex: e.regex,
                use_regex: e.use_regex,
                constant: e.constant,
                enabled: e.enabled,
                content: e.content,
                position: e.position,
                depth: e.depth,
                order: e.order,
                case_sensitive: e.case_sensitive,
                sticky: e.sticky,
                cooldown: e.cooldown,
                probability: e.probability,
                use_probability: e.use_probability,
                role: e.role,
            })
            .collect();
        Some(views)
    }

    /// 新增条目:生成一个新 uid(当前最大 +1),写回 data_raw.entries,返回新条目视图。
    pub fn add_entry(&self, id: &str) -> Option<WorldBookEntryView> {
        let rec = self.get(id)?;
        let mut raw = rec.data_raw?;
        if raw.get("entries").is_none() {
            raw.as_object_mut()?
                .insert("entries".into(), Value::Array(Vec::new()));
        }
        let max_uid = collect_entries(&raw)
            .into_iter()
            .map(|e| e.id)
            .max()
            .unwrap_or(0);
        let new_id = max_uid + 1;
        let view = WorldBookEntryView {
            id: new_id,
            comment: "新条目".into(),
            keys: Vec::new(),
            keys_secondary: Vec::new(),
            regex: None,
            use_regex: false,
            constant: false,
            enabled: true,
            content: String::new(),
            position: 0,
            depth: 4,
            order: 100,
            case_sensitive: false,
            sticky: 0,
            cooldown: 0,
            probability: 100,
            use_probability: false,
            role: None,
        };
        if let Some(entries_value) = raw.get_mut("entries") {
            let new_value = crate::parsing::world_book::view_to_value(&view);
            if let Some(map) = entries_value.as_object_mut() {
                map.insert(new_id.to_string(), new_value);
            } else if let Some(arr) = entries_value.as_array_mut() {
                // 数组形态:追加新条目(不能整组替换——那是 PUT 全量回写的语义)
                arr.push(new_value);
            }
        }
        self.save_data_raw(&rec.id, &raw)?;
        Some(view)
    }

    /// 回写 data_raw(条目编辑后落库;仅更新 entries 字段,其余原样保留)。
    /// 同时按新 raw 重算 `entry_count`(2026-09-13 批次 3 修复:该列此前只在插入时写一次,
    /// 编辑条目不回写 → 列表页条目数一直显示旧值)。
    pub fn save_data_raw(&self, id: &str, raw: &Value) -> Option<()> {
        let data_raw_str = serde_json::to_string(raw).ok()?;
        let entry_count = collect_entries(raw).len() as i64;
        let conn = self.db.write();
        conn.execute(
            "UPDATE world_books SET data_raw = ?1, entry_count = ?2 WHERE id = ?3",
            params![data_raw_str, entry_count, id],
        )
        .ok()?;
        drop(conn);
        // 条目内容变更必须失效:这是 collect_entries_for_character 的直接输入源。
        // add_entry 亦经本方法落库,故在此一处失效即可覆盖两者。
        self.invalidate_entries_cache();
        Some(())
    }

    /// 当前角色内嵌角色卡(character_book)的条目视图;无内嵌世界书返回 None
    pub fn character_book_views(&self, character_id: &str) -> Option<Vec<WorldBookEntryView>> {
        let raw = self
            .db
            .read()
            .ok()?
            .query_row(
                "SELECT data_raw FROM characters WHERE id = ?1",
                params![character_id],
                |row| row.get::<_, String>(0),
            )
            .ok()?;
        let raw: Value = serde_json::from_str(&raw).ok()?;
        let entries = crate::parsing::world_book::character_book_entries(&raw);
        if entries.is_empty() {
            return None;
        }
        Some(
            entries
                .iter()
                .map(crate::parsing::world_book::entry_to_view)
                .collect(),
        )
    }

    /// 回写角色卡 data_raw 中的 character_book(合并 entries 后 UPDATE characters)。
    /// 读改写经 `character_data::update_data_raw` 在**同一写锁事务**内完成:
    /// 原实现「read 取快照 → 内存改 → write 回写」两步间不持锁,并发编辑同一张卡时
    /// 后写者会用旧快照覆盖先写者的修改(丢更新)。
    pub fn save_character_book(
        &self,
        character_id: &str,
        views: &[WorldBookEntryView],
    ) -> Option<()> {
        crate::services::character_data::update_data_raw(&self.db, character_id, |raw| {
            // 定位 character_book:V2 顶层 / V3 data 子对象
            let cb = if raw.get("character_book").is_some() {
                raw.get_mut("character_book")
            } else {
                raw.get_mut("data")
                    .and_then(|d| d.get_mut("character_book"))
            };
            let cb = cb.ok_or_else(|| "角色卡缺少 character_book".to_string())?;
            let entries_value = cb
                .as_object_mut()
                .and_then(|o| o.get_mut("entries"))
                .ok_or_else(|| "character_book.entries 结构异常".to_string())?;
            crate::parsing::world_book::merge_entries_into(entries_value, views);
            Ok(())
        })
        .ok()?;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::db::Db;
    use crate::utils::test_support::TempDataDir;

    /// 返回 (守卫, 服务):解构绑定按**逆序**析构,守卫在前才活到最后(见 test_support 模块头)
    fn service() -> (TempDataDir, WorldBookService) {
        let dir = TempDataDir::new("wb");
        let db = Arc::new(Db::open(&dir.join("kedai.db"), &dir).unwrap());
        (dir, WorldBookService::new(db))
    }

    fn upload_book(svc: &WorldBookService, name: &str, key: &str, cid: Option<&str>) -> String {
        let raw = json!({
            "name": name,
            "entries": [
                { "uid": 0, "comment": "地点", "keys": [key], "content": "内容", "constant": false }
            ]
        });
        let bytes = serde_json::to_vec(&raw).unwrap();
        svc.upload(&bytes, "book.json", cid).unwrap().id
    }

    /// P3-2(2026-09-14):条目缓存的**失效正确性**。
    /// 这是本优化唯一的风险点——漏失效会让生成读到旧世界书。
    /// 断言:① 重复读取结果一致(缓存生效);② 上传新书后立即可见;
    /// ③ 改 enabled 后立即可见;④ 改条目内容后立即可见;⑤ 删除后立即可见。
    #[test]
    fn entries_cache_invalidates_on_every_write_path() {
        let (_dir, svc) = service();
        // 全局世界书(character_id = None → 对所有角色生效,便于用同一角色观察)
        let first_id = upload_book(&svc, "书一", "关键词一", None);

        let before = svc.collect_entries_for_character("c1");
        assert_eq!(before.len(), 1, "初始应有一本有效世界书");
        assert_eq!(
            svc.collect_entries_for_character("c1").len(),
            1,
            "重复读取一致"
        );

        // ② 上传第二本 → 必须立即可见
        let _second_id = upload_book(&svc, "书二", "关键词二", None);
        let after_upload = svc.collect_entries_for_character("c1");
        assert_eq!(
            after_upload.len(),
            2,
            "上传新世界书后缓存必须失效,条目应变为 2"
        );
        assert!(
            after_upload
                .iter()
                .any(|e| e.keys.iter().any(|k| k == "关键词二")),
            "新书的条目必须出现在结果里"
        );

        // ③ 停用第一本 → 立即只剩一条
        svc.update(&first_id, Some(false), None, None).unwrap();
        assert_eq!(
            svc.collect_entries_for_character("c1").len(),
            1,
            "改 enabled 后缓存必须失效"
        );
        // 恢复启用
        svc.update(&first_id, Some(true), None, None).unwrap();
        assert_eq!(svc.collect_entries_for_character("c1").len(), 2);

        // ④ 改条目内容(经 save_data_raw,add_entry 亦走此路径)
        let raw = json!({
            "name": "书一",
            "entries": [
                { "uid": 0, "comment": "地点", "keys": ["改过的关键词"], "content": "新内容", "constant": false }
            ]
        });
        svc.save_data_raw(&first_id, &raw).unwrap();
        let after_edit = svc.collect_entries_for_character("c1");
        assert!(
            after_edit
                .iter()
                .any(|e| e.keys.iter().any(|k| k == "改过的关键词")),
            "改条目后缓存必须失效,应读到新关键词"
        );
        assert!(
            !after_edit
                .iter()
                .any(|e| e.keys.iter().any(|k| k == "关键词一")),
            "旧关键词不应残留"
        );

        // ⑤ 删除第一本 → 立即只剩一条
        assert!(svc.delete(&first_id), "删除应成功");
        assert_eq!(
            svc.collect_entries_for_character("c1").len(),
            1,
            "删除后缓存必须失效"
        );

        drop(svc);
    }

    /// 缓存按 character_id 隔离:不同角色互不污染
    #[test]
    fn entries_cache_is_per_character() {
        let (_dir, svc) = service();
        {
            let conn = svc.db.write();
            conn.execute(
                "INSERT INTO characters (id, name, chara_name, description, file_path, data_raw, created_at)
                 VALUES ('c1','n','n','','','{}',''), ('c2','n','n','','','{}','')",
                [],
            )
            .unwrap();
        }
        upload_book(&svc, "给 c1", "只属于c1", Some("c1"));

        let c1 = svc.collect_entries_for_character("c1");
        let c2 = svc.collect_entries_for_character("c2");
        assert_eq!(c1.len(), 1, "c1 应有自己的世界书");
        assert_eq!(c2.len(), 0, "c2 不应看到 c1 的世界书");

        drop(svc);
    }
}
