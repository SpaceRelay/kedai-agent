// 测试用临时数据目录守卫(2026-09-15,测试基建批次 R5)。
//
// 背景:各处测试辅助普遍 `std::env::temp_dir().join(format!("kedai-..."))` 建目录却从不清理,
// 实测(%TEMP%)累计 14015 个 `kedai-*` 目录 / 11.22 GB,是压满 C 盘的主力之一。
// 本模块把「建唯一临时目录 + 作用域结束即递归删除」收敛成一个 RAII 守卫,
// 新建测试一律用它,不再手写 remove_dir_all(手写清理在断言失败/panic 时会被跳过)。
//
// 硬约束(改动前先读,踩过才知道疼):
//   - 目录名固定 `kedai-<tag>-<uuid-v4>`:tag 保留人工辨识度,uuid 保证唯一。
//     不按 `std::process::id()` 命名——同二进制内多个测试并发跑时 pid 相同,
//     共用一个目录会让「测试 A 结束时删掉测试 B 正在用的目录」变成随机失败。
//   - Drop 必须 best-effort 且**绝不 panic**:Windows 上仍有句柄未释放时
//     `remove_dir_all` 会失败,Drop 内 panic 会 abort 整个测试进程。
//   - 守卫必须活到句柄之后析构,顺序反了就是「句柄仍打开 → 删除失败 → 静默残留」,
//     不报错但没修好。**析构顺序是逆序**(2026-09-15 rustc 1.97 实测):
//       局部变量      → 按声明逆序析构,故守卫要声明在它保护的 Db/服务**之前**;
//       解构绑定      → 按绑定逆序析构,故 `let (guard, svc) = service();` 才让守卫最后析构
//                       (`let (svc, guard) = ...` 是**错的**:guard 先析构、句柄还开着);
//       整个元组不解构 → 才按字段声明顺序析构;
//       结构体字段    → 按**声明顺序**析构,故守卫字段必须放在**最后**
//                       (`struct Fixture { _dir, svc, .. }` 会让守卫先析构)。
//   - 禁止临时值形态:`Db::open(&TempDataDir::new("x").path().join("kedai.db"), ..)` 会
//     在语句末析构守卫,库还开着就删目录。必须先 `let dir = TempDataDir::new("x");`。
//
// 门控:单元测试靠 `cfg(test)`;`tests/**` 集成测试是独立 crate,看不到本 crate 的 cfg(test),
// 故另开 `test-support` feature(由 Cargo.toml 的自引用 dev-dependency 打开)。
use std::ops::Deref;
use std::path::{Path, PathBuf};

/// 临时数据目录守卫:作用域结束(含 panic 展开)即递归删除目录。
pub struct TempDataDir {
    path: PathBuf,
    /// `keep()` 逃逸阀置位后 Drop 不再清理
    keep: bool,
}

impl TempDataDir {
    /// 建唯一临时数据目录:tag 用于人工辨识,自动追加 uuid 保证并发唯一
    pub fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("kedai-{tag}-{}", uuid::Uuid::new_v4()));
        // 建目录失败不 panic:调用方随后的建库/写文件会给出更具体的错误,
        // 此处 panic 只会掩盖真实原因(且 cfg(not(test)) 形态下 clippy::unwrap_used 告警)。
        let _ = std::fs::create_dir_all(&path);
        Self { path, keep: false }
    }

    /// 目录路径
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 逃逸阀:某些测试需要在 Drop 后仍检查目录(极少用),调用后不再自动清理
    pub fn keep(mut self) -> PathBuf {
        self.keep = true;
        self.path.clone()
    }
}

impl Deref for TempDataDir {
    type Target = Path;

    /// 既有 `dir.join(...)` / `&dir` 代码零改动:解引用即 Path
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDataDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDataDir {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        if std::fs::remove_dir_all(&self.path).is_ok() {
            return;
        }
        // Windows 上句柄释放有延迟(SQLite 连接/杀软临时占用),全量并发下偶发失败。
        // 有界重试 3 次 × 10ms(最坏 30ms,且只在失败路径付出)——不无限重试:
        // Drop 里卡死测试比留一个临时目录更糟。
        // 注意:实测有 3 个 agentgo 用例无论如何都清不掉(重试到 500ms 仍失败):
        // 它们 `tokio::spawn` 的后台子任务持有 `Arc<ToolDeps>`(内含 `Arc<Db>`),
        // 在测试函数返回后仍存活,故目录被占用。属已知有界残留,见 MAINTENANCE §10-32。
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            if std::fs::remove_dir_all(&self.path).is_ok() {
                return;
            }
        }
    }
}
