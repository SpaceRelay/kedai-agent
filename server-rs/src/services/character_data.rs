// characters.data_raw 的**读改写单一入口**(防丢更新)。
//
// 背景:`characters.data_raw` 是角色卡原始 JSON 的权威副本,世界书编辑、脚本树、
// 契约内嵌、角色字段更新四处都会「读整行 → 改 JSON → UPDATE 回写」。原实现把读
// (`db.read()`)与写(`db.write()`)分成两步,两步之间不持锁——并发编辑同一张卡时
// 后写者会用「基于旧快照的 JSON」覆盖先写者的修改,即经典的丢更新(lost update)。
//
// 修法:把 SELECT → 闭包改写 → UPDATE 放进**同一把写锁 + 同一事务**。本项目是单进程
// 架构(`Db{writer: Mutex<Connection>}` 是唯一写通道,桌面壳也复用同一进程内 server),
// 因此写锁内的读改写即可完全消除该竞态;事务保证中途出错时不留半写状态。
//
// 何时才需要引入 `version` 乐观锁列:出现**多进程同时写同一 DB** 的场景(例如两个
// 客户端进程共享一个 DATA_DIR)。当前架构不存在该场景;若将来出现,应在建表 DDL、
// 跨库合并与迁移注册三处同步加列,并在本函数内校验版本后返回冲突。
use crate::models::db::Db;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

/// 在写锁事务内完成 `characters.data_raw` 的读改写。
///
/// - `Ok(true)`:已找到角色并提交更新;
/// - `Ok(false)`:角色不存在(无写入,事务回滚);
/// - `Err`:读取/解析/闭包拒绝/序列化/SQL/提交任一失败(事务回滚,原值不变)。
pub(crate) fn update_data_raw<F>(db: &Db, character_id: &str, mutate: F) -> Result<bool, String>
where
    F: FnOnce(&mut Value) -> Result<(), String>,
{
    let mut conn = db.write();
    let tx = conn
        .transaction()
        .map_err(|e| format!("开启角色卡写事务失败: {e}"))?;
    let raw: Option<String> = tx
        .query_row(
            "SELECT data_raw FROM characters WHERE id = ?1",
            params![character_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("读取角色卡失败: {e}"))?;
    let Some(raw) = raw else {
        // 事务未写入任何内容,Drop 即回滚
        return Ok(false);
    };
    let mut value: Value =
        serde_json::from_str(&raw).map_err(|e| format!("解析角色卡失败: {e}"))?;
    // 闭包拒绝(如角色卡结构不符合预期)时事务回滚,不留下半写状态
    mutate(&mut value)?;
    let serialized = serde_json::to_string(&value).map_err(|e| format!("序列化角色卡失败: {e}"))?;
    tx.execute(
        "UPDATE characters SET data_raw = ?1 WHERE id = ?2",
        params![serialized, character_id],
    )
    .map_err(|e| format!("回写角色卡失败: {e}"))?;
    tx.commit()
        .map_err(|e| format!("提交角色卡写事务失败: {e}"))?;
    Ok(true)
}
