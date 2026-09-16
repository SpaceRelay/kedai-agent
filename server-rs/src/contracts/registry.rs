// 契约注册表:character_id → Contract 的惰性加载 + 内存缓存,内聚两源提取逻辑。
//
// 角色卡(extensions.nlkaleido 优先)与世界书([nlkaleido_contract] 兜底)数据在
// SQLite,每轮生成都重新 collect 成本高;注册表首次使用时提取并缓存,后续 O(1) 命中。
// 作者改卡/改世界书后由 API 写路径调用 invalidate/clear 失效,下次生成重新提取
// (修复「改卡后引擎用旧契约直到重启」的脏缓存问题)。
//
// 依赖倒置(批次 B.6 L1 去渗透):本模块不再 import CharacterService/WorldBookService
// 具体类型,改为接收两个「取数据」闭包(角色卡 data_raw / 绑定世界书条目),
// 闭包由 L2(api/app_state)注入。注册表只依赖 L1 数据形态(Value / WorldEntry / Contract)。
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{extract_from_character_card, extract_from_world_entries, Contract};
use crate::parsing::world_book::WorldEntry;
use serde_json::Value;

/// 角色卡契约源:character_id → 角色卡 data_raw(取不到返回 None)
type CharacterSource = dyn Fn(&str) -> Option<Value> + Send + Sync;
/// 世界书契约源:character_id → 绑定该角色且启用的世界书条目(可为空)
type WorldBookSource = dyn Fn(&str) -> Vec<WorldEntry> + Send + Sync;

/// 契约注册表(character_id → Contract)。
///
/// 引擎与多步工具共享同一实例(经 AppState 构造注入),保证缓存一致;
/// 提取逻辑只在注册表内实现一份(此前引擎与 multistep 各持一份会漂移)。
pub struct ContractRegistry {
    characters: Arc<CharacterSource>,
    world_books: Arc<WorldBookSource>,
    cache: Mutex<HashMap<String, Contract>>,
}

impl ContractRegistry {
    /// 以两个取数据闭包构造(批次 B.6):注册表不感知服务类型,只消费数据。
    pub fn new<C, W>(characters: C, world_books: W) -> Self
    where
        C: Fn(&str) -> Option<Value> + Send + Sync + 'static,
        W: Fn(&str) -> Vec<WorldEntry> + Send + Sync + 'static,
    {
        ContractRegistry {
            characters: Arc::new(characters),
            world_books: Arc::new(world_books),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// 惰性加载:命中缓存直接返回;未命中从角色卡(优先)与世界书(兜底)提取并缓存。
    /// 来源无契约(存量卡)返回 None 且不缓存——首次添加契约后无需失效即可生效;
    /// 修改已有契约必须经写路径 invalidate,否则命中旧缓存。
    pub fn load(&self, character_id: &str) -> Option<Contract> {
        if let Some(c) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(character_id)
        {
            return Some(c.clone());
        }
        let loaded = self.extract(character_id)?;
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(character_id.to_string(), loaded.clone());
        Some(loaded)
    }

    /// 两源提取:角色卡 data_raw.extensions.nlkaleido 优先,世界书条目兜底。
    fn extract(&self, character_id: &str) -> Option<Contract> {
        let card = (self.characters)(character_id)?;
        let card_contract = extract_from_character_card(&card);
        if card_contract.is_some() {
            return card_contract;
        }
        let entries = (self.world_books)(character_id);
        extract_from_world_entries(&entries)
    }

    /// 失效单条(角色卡/绑定该角色的世界书变更后调用,强制下次重新提取)。
    pub fn invalidate(&self, character_id: &str) {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(character_id);
    }

    /// 清空全部(全局世界书变更会影响所有角色的契约来源,无法定位单条)。
    pub fn clear(&self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::db::Db;
    use crate::utils::test_support::TempDataDir;
    use rusqlite::OptionalExtension;
    use serde_json::json;
    use std::sync::Arc;

    /// 首项为临时目录守卫:解构绑定按**逆序**析构,守卫在前才活到最后(见 test_support 模块头)
    type TestLoaders = (
        TempDataDir,
        Arc<Db>,
        Box<dyn Fn(&str) -> Option<Value> + Send + Sync>,
        Box<dyn Fn(&str) -> Vec<WorldEntry> + Send + Sync>,
    );

    /// 直接用 Db 构造两个取数据闭包:角色卡 data_raw 直查,世界书源本轮不涉及
    /// (注册表单测只覆盖角色卡提取与缓存语义)→ 空列表。生产注入见 api/app_state.rs。
    fn services() -> TestLoaders {
        let dir = TempDataDir::new("reg");
        let db = Arc::new(Db::open(&dir.join("t.db"), &dir).unwrap());
        let db_for_cards = db.clone();
        let characters = move |id: &str| -> Option<Value> {
            let conn = db_for_cards.read().ok()?;
            conn.query_row(
                "SELECT data_raw FROM characters WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
        };
        let world_books = |_id: &str| -> Vec<WorldEntry> { Vec::new() };
        (dir, db, Box::new(characters), Box::new(world_books))
    }

    fn contract_json(id: &str) -> String {
        json!({
            "version": 1, "id": id, "schema": { "properties": {} },
            "updateRules": {}, "guardrails": {}
        })
        .to_string()
    }

    /// 直接插入/更新带契约的角色卡(经测试自持 Db 句柄,不动生产代码)。
    fn upsert_character(db: &Db, id: &str, contract: Option<&str>) {
        let conn = db.write();
        let extensions = contract
            .map(|c| json!({ "nlkaleido": serde_json::from_str::<serde_json::Value>(c).unwrap() }));
        let data_raw = json!({ "name": "测试角色", "extensions": extensions });
        conn.execute(
            "INSERT INTO characters (id, name, chara_name, description, file_path, data_raw, created_at) \
             VALUES (?1, ?2, ?2, '', '', ?3, ?4) \
             ON CONFLICT(id) DO UPDATE SET data_raw = excluded.data_raw",
            rusqlite::params![id, "测试角色", data_raw.to_string(), crate::models::db::now_iso()],
        )
        .unwrap();
    }

    /// 首次 load 提取并缓存;改卡未失效 → 命中旧缓存;invalidate 后 → 重新提取。
    /// (「脏缓存」bug 的回归测试)
    #[test]
    fn load_caches_until_invalidated() {
        let (_dir, db, characters, world_books) = services();
        upsert_character(&db, "c1", Some(&contract_json("v1")));
        let reg = ContractRegistry::new(characters, world_books);

        assert_eq!(reg.load("c1").unwrap().id, "v1");

        // 改卡但未失效 → 仍是旧契约(缓存语义)
        upsert_character(&db, "c1", Some(&contract_json("v2")));
        assert_eq!(reg.load("c1").unwrap().id, "v1", "未失效应命中旧缓存");

        // 失效后 → 重新提取到 v2
        reg.invalidate("c1");
        assert_eq!(reg.load("c1").unwrap().id, "v2");
    }

    /// 无契约角色 → None 且不缓存;添加契约后(无需失效)即可加载到。
    #[test]
    fn no_contract_not_cached_and_later_add_works() {
        let (_dir, db, characters, world_books) = services();
        upsert_character(&db, "c2", None);
        let reg = ContractRegistry::new(characters, world_books);

        assert!(reg.load("c2").is_none());
        // 添加契约(来源从无到有)→ 下次 load 直接提取到(未缓存过 None)
        upsert_character(&db, "c2", Some(&contract_json("new")));
        assert_eq!(reg.load("c2").unwrap().id, "new");
    }

    /// clear 清空全部(全局世界书变更场景)。
    #[test]
    fn clear_drops_all_entries() {
        let (_dir, db, characters, world_books) = services();
        upsert_character(&db, "c3", Some(&contract_json("v1")));
        let reg = ContractRegistry::new(characters, world_books);
        assert!(reg.load("c3").is_some());
        reg.clear();
        upsert_character(&db, "c3", Some(&contract_json("v2")));
        assert_eq!(reg.load("c3").unwrap().id, "v2");
    }

    /// 不存在的角色 → None。
    #[test]
    fn missing_character_returns_none() {
        let (_dir, _db, characters, world_books) = services();
        let reg = ContractRegistry::new(characters, world_books);
        assert!(reg.load("ghost").is_none());
    }
}
