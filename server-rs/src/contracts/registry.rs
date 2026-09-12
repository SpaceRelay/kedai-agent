// 契约注册表:character_id → Contract 的惰性加载 + 内存缓存,内聚两源提取逻辑。
//
// 角色卡(extensions.nlkaleido 优先)与世界书([nlkaleido_contract] 兜底)数据在
// SQLite,每轮生成都重新 collect 成本高;注册表首次使用时提取并缓存,后续 O(1) 命中。
// 作者改卡/改世界书后由 API 写路径调用 invalidate/clear 失效,下次生成重新提取
// (修复「改卡后引擎用旧契约直到重启」的脏缓存问题)。
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{extract_from_character_card, extract_from_world_entries, Contract};
use crate::services::character_service::CharacterService;
use crate::services::world_book_service::WorldBookService;

/// 契约注册表(character_id → Contract)。
///
/// 引擎与多步工具共享同一实例(经 AppState 构造注入),保证缓存一致;
/// 提取逻辑只在注册表内实现一份(此前引擎与 multistep 各持一份会漂移)。
pub struct ContractRegistry {
    characters: Arc<CharacterService>,
    world_books: Arc<WorldBookService>,
    cache: Mutex<HashMap<String, Contract>>,
}

impl ContractRegistry {
    pub fn new(characters: Arc<CharacterService>, world_books: Arc<WorldBookService>) -> Self {
        ContractRegistry {
            characters,
            world_books,
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
        let card = self.characters.get(character_id)?;
        let card_contract = extract_from_character_card(card.data_raw.as_ref()?);
        if card_contract.is_some() {
            return card_contract;
        }
        let entries = self.world_books.collect_entries_for_character(character_id);
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
    use serde_json::json;
    use std::path::PathBuf;

    fn services() -> (
        Arc<Db>,
        Arc<CharacterService>,
        Arc<WorldBookService>,
        PathBuf,
    ) {
        let dir = std::env::temp_dir().join(format!("kedai-reg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db"), &dir).unwrap());
        (
            db.clone(),
            Arc::new(CharacterService::new(db.clone(), dir.clone())),
            Arc::new(WorldBookService::new(db)),
            dir,
        )
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
        let (db, characters, world_books, _dir) = services();
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
        let (db, characters, world_books, _dir) = services();
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
        let (db, characters, world_books, _dir) = services();
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
        let (_db, characters, world_books, _dir) = services();
        let reg = ContractRegistry::new(characters, world_books);
        assert!(reg.load("ghost").is_none());
    }
}
