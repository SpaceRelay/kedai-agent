// 取消信号与执行 token:NEXT_RUN_TOKEN 自增分配,register_cancel/signal_cancel/
// is_current_run/remove_cancel_if/remove_cancel_entry 管理「任务 id → (取消通道, token)」表,
// 供 stop 取消与「stop 后立即重跑」时区分新旧执行。
// 自 task_service.rs 拆分迁入,纯代码移动,逻辑不变;依赖经 `use super::*` 取自 mod.rs。
use super::*;

/// 每次 run 分配自增 token,识别「同一任务的当前执行」。
static NEXT_RUN_TOKEN: AtomicU64 = AtomicU64::new(1);

impl TaskService {
    // ===== 取消信号 =====

    /// 注册本次执行:总是新建 watch 通道并用新 token 覆盖旧条目(重跑语义),
    /// 返回取消接收端与本次 token。旧执行持有的 Sender 随 drop 失效,不影响新通道。
    pub(super) fn register_cancel(&self, id: &str) -> (watch::Receiver<bool>, u64) {
        let token = NEXT_RUN_TOKEN.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = watch::channel(false);
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), (tx, token));
        (rx, token)
    }

    pub(super) fn signal_cancel(&self, id: &str) {
        if let Some((tx, _)) = self
            .cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
        {
            let _ = tx.send(true);
        }
    }

    /// 指定 token 是否仍是该任务当前的执行(旧执行在重跑后返回 false)。
    pub(super) fn is_current_run(&self, id: &str, token: u64) -> bool {
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|(_, t)| *t == token)
            .unwrap_or(false)
    }

    /// 仅当条目仍归属本次 token 时移除(旧执行不得删掉新执行刚注册的条目)。
    pub(super) fn remove_cancel_if(&self, id: &str, token: u64) {
        let mut cancels = self.cancels.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, t)) = cancels.get(id) {
            if *t == token {
                cancels.remove(id);
            }
        }
    }

    /// 无条件移除条目(仅任务已删除等终态路径使用)。
    pub(super) fn remove_cancel_entry(&self, id: &str) {
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }
}
