// 角色卡脚本授权台账服务(2026-09-14,补 known-limitations L12「后端授权门」)。
//
// ## 为什么需要它
//
// 角色卡内嵌脚本(`data_raw.extensions.tavern_helper`)是**不可信输入**——卡来自网络,
// 脚本能经 TavernHelper 兼容桥写变量、导入数据、发起 LLM 生成,并由引擎落库。
// 前端沙箱路径已有授权台账(`web/src/scriptAuthorization.ts`,localStorage,
// 哈希绑定脚本正文),但**后端执行路径没有门槛**:同一张卡,前端要授权、后端自动跑。
//
// ## 设计要点
//
// 1. **哈希由后端唯一计算并对外暴露**(`GET /api/script-authorizations/{id}` 返回
//    `current_hash`),前端授权时把它**回传**。这样跨语言(hash 算法/字段裁剪)
//    不存在「两端各算一遍、算法漂移导致永久不匹配」的风险——只有一份实现。
// 2. **哈希绑定脚本正文**:卡更新脚本 → 哈希变 → 旧授权自动失效(fail-closed)。
// 3. **global 脚本不受此表约束**:全局脚本是用户在前端主动添加的自有内容,视为可信;
//    受门禁的只有**角色卡携带**的脚本(不可信来源)。
// 4. **默认拒绝**:台账无记录即不执行(fail-closed)。这是**有意的兼容性收紧**
//    (旧行为是自动执行),已在 `docs/遗留.md` L12 与变更说明中登记;
//    前端提供「重新授权」入口。
use crate::models::db::{now_iso, Db};
// LoadedScript 已在 L1(models/types):L2 不得依赖 L3 的 scripts/。
use crate::models::types::LoadedScript;
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// 已登记的授权(角色 → 被授权时的脚本哈希)。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ScriptAuthorizationGrant {
    pub character_id: String,
    pub script_hash: String,
    pub authorized_at: String,
}

/// 某角色当前的授权态(GET 接口返回体;`current_hash` 恒为**实时计算值**)。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ScriptAuthorizationStatus {
    /// 是否已授权**当前**脚本内容
    pub authorized: bool,
    /// 当前脚本内容的哈希(前端授权时原样回传)
    pub current_hash: String,
    /// 已登记的哈希(与 `current_hash` 不同表示脚本已变更、授权失效)
    pub stored_hash: Option<String>,
    pub authorized_at: Option<String>,
}

pub struct ScriptAuthorizationService {
    db: Arc<Db>,
}

impl ScriptAuthorizationService {
    pub fn new(db: Arc<Db>) -> Self {
        ScriptAuthorizationService { db }
    }

    /// 计算脚本内容哈希(canonical SHA-256 hex,64 字符)。
    ///
    /// **口径**:对 `collect_enabled_scripts` 的产物(已按 enabled 过滤、已剔除空正文)
    /// 取树序,每条贡献 `{id, name, content, data}` 四项,整体 JSON 序列化后哈希。
    ///
    /// 三个稳定性前提:
    /// - `collect_enabled_scripts` 保持树序(不排序),同一棵树 → 同一序列;
    /// - `serde_json` 默认 `Map = BTreeMap`,故 `data` 的对象键是**有序**的,跨端稳定;
    /// - 纳入 `data` 是因为脚本可读写自身 data,改 data 也是行为变更,应使授权失效。
    ///
    /// 空脚本集返回空串(调用方据此短路,不做无意义查询)。
    pub fn compute_hash(scripts: &[LoadedScript]) -> String {
        if scripts.is_empty() {
            return String::new();
        }
        let canonical: Vec<serde_json::Value> = scripts
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "name": s.name,
                    "content": s.content,
                    "data": s.data,
                })
            })
            .collect();
        // serde_json 序列化不会失败(Value 树恒可序列化);失败时回退空串 = 视为不可授权。
        let bytes = match serde_json::to_vec(&canonical) {
            Ok(b) => b,
            Err(_) => return String::new(),
        };
        let digest = Sha256::digest(&bytes);
        // 十六进制小写(与前端 localStorage 台账的 64-hex 形态一致,便于人工比对)
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 查询某角色的授权态(实时计算 `current_hash` 与台账比对)。
    pub fn status(
        &self,
        character_id: &str,
        scripts: &[LoadedScript],
    ) -> Result<ScriptAuthorizationStatus, String> {
        let current_hash = Self::compute_hash(scripts);
        let stored = self.get(character_id)?;
        let (stored_hash, authorized_at) = match stored {
            Some(g) => (Some(g.script_hash), Some(g.authorized_at)),
            None => (None, None),
        };
        Ok(ScriptAuthorizationStatus {
            authorized: !current_hash.is_empty()
                && stored_hash.as_deref() == Some(current_hash.as_str()),
            current_hash,
            stored_hash,
            authorized_at,
        })
    }

    /// 读取台账行(无记录返回 None)。
    pub fn get(&self, character_id: &str) -> Result<Option<ScriptAuthorizationGrant>, String> {
        let conn = self.db.read()?;
        conn.query_row(
            "SELECT character_id, script_hash, authorized_at FROM script_authorizations \
             WHERE character_id = ?1",
            params![character_id],
            |row| {
                Ok(ScriptAuthorizationGrant {
                    character_id: row.get(0)?,
                    script_hash: row.get(1)?,
                    authorized_at: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|e| format!("读取脚本授权失败: {e}"))
    }

    /// 授权:写入「角色 → 哈希」。调用方须先校验 `hash` 与实时计算值一致
    /// (防前端回传陈旧/伪造哈希把卡锁在错误版本上)。
    pub fn grant(&self, character_id: &str, script_hash: &str) -> Result<(), String> {
        if character_id.trim().is_empty() {
            return Err("character_id 不能为空".into());
        }
        if script_hash.is_empty() {
            return Err("脚本哈希为空(该角色没有可授权的脚本)".into());
        }
        let conn = self.db.write();
        conn.execute(
            "INSERT INTO script_authorizations (character_id, script_hash, authorized_at) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(character_id) DO UPDATE SET script_hash = ?2, authorized_at = ?3",
            params![character_id, script_hash, now_iso()],
        )
        .map_err(|e| format!("写入脚本授权失败: {e}"))?;
        Ok(())
    }

    /// 撤销授权(幂等:无记录也返回 Ok)。
    pub fn revoke(&self, character_id: &str) -> Result<(), String> {
        let conn = self.db.write();
        conn.execute(
            "DELETE FROM script_authorizations WHERE character_id = ?1",
            params![character_id],
        )
        .map_err(|e| format!("撤销脚本授权失败: {e}"))?;
        Ok(())
    }

    /// 列出全部已授权角色(供设置页展示)。
    pub fn list(&self) -> Result<Vec<ScriptAuthorizationGrant>, String> {
        let conn = self.db.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT character_id, script_hash, authorized_at FROM script_authorizations \
                 ORDER BY authorized_at DESC",
            )
            .map_err(|e| format!("查询脚本授权失败: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ScriptAuthorizationGrant {
                    character_id: row.get(0)?,
                    script_hash: row.get(1)?,
                    authorized_at: row.get(2)?,
                })
            })
            .map_err(|e| format!("查询脚本授权失败: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("读取脚本授权失败: {e}"))
    }

    /// 执行门禁:当前脚本是否已被授权(供 `run_character_scripts` 调用)。
    ///
    /// 任一环节出错(查库失败)一律返回 `false`——**fail-closed**:
    /// 台账读不到时宁可不执行脚本,也不放行不可信输入。
    pub fn is_authorized(&self, character_id: &str, scripts: &[LoadedScript]) -> bool {
        let current_hash = Self::compute_hash(scripts);
        if current_hash.is_empty() {
            return false;
        }
        match self.get(character_id) {
            Ok(Some(g)) => g.script_hash == current_hash,
            Ok(None) => false,
            Err(e) => {
                tracing::warn!(
                    character_id = character_id,
                    error = e,
                    "脚本授权查询失败,按未授权处理"
                );
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn script(id: &str, name: &str, content: &str) -> LoadedScript {
        LoadedScript {
            id: id.to_string(),
            name: name.to_string(),
            content: content.to_string(),
            data: json!({}),
        }
    }

    /// 哈希口径的关键性质:**内容变则哈希变**(授权随脚本更新自动失效)。
    #[test]
    fn hash_changes_when_content_changes() {
        let a = vec![script("s1", "n", "console.log(1)")];
        let b = vec![script("s1", "n", "console.log(2)")];
        assert_ne!(
            ScriptAuthorizationService::compute_hash(&a),
            ScriptAuthorizationService::compute_hash(&b)
        );
    }

    /// 同一内容多次计算必须稳定(前端回传哈希与后端复算需逐字相等)。
    #[test]
    fn hash_is_stable_for_same_input() {
        let s = vec![script("s1", "n", "x"), script("s2", "m", "y")];
        let h1 = ScriptAuthorizationService::compute_hash(&s);
        let h2 = ScriptAuthorizationService::compute_hash(&s);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64, "SHA-256 hex 应为 64 字符");
        assert!(h1
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    /// 顺序敏感:脚本树顺序不同则哈希不同(树序是内容的一部分)。
    #[test]
    fn hash_is_order_sensitive() {
        let a = vec![script("s1", "n", "x"), script("s2", "m", "y")];
        let b = vec![script("s2", "m", "y"), script("s1", "n", "x")];
        assert_ne!(
            ScriptAuthorizationService::compute_hash(&a),
            ScriptAuthorizationService::compute_hash(&b)
        );
    }

    /// `data` 参与哈希:脚本自身数据变更也应使授权失效。
    #[test]
    fn hash_includes_script_data() {
        let mut a = script("s1", "n", "x");
        a.data = json!({"k": 1});
        let mut b = script("s1", "n", "x");
        b.data = json!({"k": 2});
        assert_ne!(
            ScriptAuthorizationService::compute_hash(&[a]),
            ScriptAuthorizationService::compute_hash(&[b])
        );
    }

    /// 空脚本集 → 空哈希(调用方据此短路,不产生无意义台账行)。
    #[test]
    fn empty_scripts_produce_empty_hash() {
        assert_eq!(ScriptAuthorizationService::compute_hash(&[]), "");
    }
}
