// 同步阻塞段让出 tokio worker(2026-09-16 性能批次 P-6)。
//
// 背景:任务引擎的执行器经 `tokio::spawn` 跑在 tokio 的 async worker 线程上
// (`task_engine/mod.rs:146`),但落库调用(`set_status`/`set_plan`/`record_usage`/
// `record_llm_call`/`finalize_terminal` 等)是同步 rusqlite 写。写在 async worker 上
// 直接执行,一旦撞上写锁竞争(SQLite 唯一写连接 + busy_timeout 最长 5s)或 fsync
// 延迟,就会**卡住整个 worker**,连带该 worker 上排队的其他请求一起停摆 ——
// 这正是 `docs/功能-变更史.md` 记录的「队在阻塞 tokio worker」问题。
// chat 路径的同类写入早已用 `spawn_blocking` 隔离(`engine/run_finish.rs:94`),
// 任务路径此前漏改,属纪律不统一。
//
// 为什么用 `block_in_place` 而不是 `spawn_blocking`:
//   `spawn_blocking` 要求调用点变成 async 并 await,而落库调用散布在 6 个执行器的
//   40+ 处(且部分在同步辅助函数里),改造面大、易引入生命周期与顺序错误。
//   `block_in_place` 语义是「我要在 async 上下文里跑阻塞代码」:tokio 会把当前
//   worker 的调度核心(及其排队任务)移交给另一个线程,本线程则专心跑这段阻塞代码。
//   效果与「不阻塞事件循环」等价,而调用点保持同步签名不变。
//
// 四种运行环境下都必须安全(helper 内部已分派,见下):
//   1. multi-thread worker(生产)      → `block_in_place`,让出调度核心 ✅
//   2. `spawn_blocking` 线程(嵌套调用) → tokio 直接返回 Ok 空操作,本就在阻塞线程上 ✅
//   3. current_thread runtime(多数测试)→ **必须不走 block_in_place**,否则 tokio
//      会 panic「can call blocking only when running on the multi-threaded runtime」;
//      故检测到 CurrentThread 时直接执行 ✅
//   4. 完全不在 runtime 内(纯 #[test]) → 直接执行 ✅

use tokio::runtime::{Handle, RuntimeFlavor};

/// 在需要时让出 async worker 执行同步阻塞代码。
///
/// 判定依据是当前 runtime 的 flavor,而不是「猜」:
/// 只有 multi-thread runtime 的 worker 线程才需要(也才可以)hand off 调度核心;
/// current_thread 与阻塞线程、以及无 runtime 的场景一律直接执行。
#[inline]
pub fn park_worker<T, F>(f: F) -> T
where
    F: FnOnce() -> T,
{
    let on_multi_thread = matches!(
        Handle::try_current().map(|h| h.runtime_flavor()),
        Ok(RuntimeFlavor::MultiThread)
    );
    if on_multi_thread {
        // 在 spawn_blocking 线程上调用时,tokio 内部判定为「已在阻塞区域」直接返回,
        // 故此处不必额外区分;嵌套调用同样安全。
        tokio::task::block_in_place(f)
    } else {
        f()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 纯 #[test](无 runtime)下必须直接执行:此时没有 worker 可让,
    /// 走 `block_in_place` 会因拿不到 runtime 上下文而 panic。
    #[test]
    fn works_without_runtime() {
        assert_eq!(park_worker(|| 42), 42);
    }

    /// current_thread runtime:这是**最容易踩的坑**——`block_in_place` 在该
    /// flavor 下会 panic(「can call blocking only when running on the
    /// multi-threaded runtime」),而本仓大量单测用 #[tokio::test] 默认就是它。
    /// helper 必须先探测 flavor 再决定,不能无条件调用。
    #[tokio::test]
    async fn works_on_current_thread_runtime() {
        assert_eq!(park_worker(|| "ok"), "ok");
    }

    /// multi_thread runtime 的 async worker 上:必须正常执行(实际走 block_in_place)。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn works_on_multi_thread_worker() {
        assert_eq!(park_worker(|| 7), 7);
    }

    /// 嵌套调用(在 spawn_blocking 线程内再 park_worker):tokio 在阻塞区域内
    /// 视 block_in_place 为空操作,必须不 panic、结果正确。真实链路里
    /// `AppState::db_call` 就是 spawn_blocking,其内部再调 TaskService 落库方法
    /// 便会形成这种嵌套,故这条必须成立。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nested_inside_spawn_blocking_is_safe() {
        let out = tokio::task::spawn_blocking(|| park_worker(|| "nested")).await;
        assert_eq!(out.unwrap(), "nested");
    }

    /// 核心断言:park_worker 的阻塞段**不会卡住同 worker 上的其他任务**。
    ///
    /// 判定方式必须是「阻塞段进行期间」观察到其他任务被推进,而不是「事后它最终跑完」——
    /// 后者两种实现都会通过,无法区分。
    ///
    /// 关键细节:`#[tokio::test]` 的测试体跑在 `block_on` 自己的线程上,**不在 worker 池里**。
    /// 若直接在测试体里调 park_worker,被观测任务会由空闲 worker 跑掉,测不出任何差异
    /// (这一版最初就踩了这个坑)。故必须把**阻塞代码本身**放进 `tokio::spawn`,
    /// 让它真正占用 worker,再用另一个 spawn 的任务作为观测者:
    ///   - 让出调度核心时:核心被移交到另一线程,观测者得以在阻塞期间执行 → 提前观察成功;
    ///   - 不让出时:唯一 worker 卡在阻塞闭包里,观测者永远排不上 → 直到截止时间都观察不到。
    /// 用 `worker_threads = 1` 是刻意的:多 worker 下别的 worker 会兜住,同样测不出差异。
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn blocking_section_does_not_stall_other_tasks() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let flag = Arc::new(AtomicBool::new(false));

        // 观测者:占住 worker 执行阻塞段,并在段内轮询标志位(带截止时间)
        let observer_flag = flag.clone();
        let observer = tokio::spawn(async move {
            park_worker(|| {
                let deadline = Instant::now() + Duration::from_millis(500);
                while Instant::now() < deadline {
                    if observer_flag.load(Ordering::SeqCst) {
                        return true;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                false
            })
        });

        // 被观测者:必须能在观测者阻塞期间被调度执行(给一点时间让观测者先占住 worker)
        let setter_flag = flag.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            setter_flag.store(true, Ordering::SeqCst);
        });

        let observed = observer.await.unwrap();
        assert!(
            observed,
            "阻塞段进行期间其他任务应能被推进(说明调度核心已让出);失败说明 park_worker 退化为直接执行"
        );
    }
}
