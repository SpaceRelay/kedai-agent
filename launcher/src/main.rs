// Kedai 图形启动器
//
// 存在意义:替代 start.ps1。本机 .ps1 无文件关联,双击不执行;且 PowerShell 启动器
// 用 Start-Process 后立即打印 [OK] 就返回,从不校验进程是否真的活着——失败时窗口
// 一闪而过,用户看不到任何原因。
//
// 本启动器的职责(顺序即优先级):
//   1) 从 exe 位置定位项目根(不依赖工作目录,双击时 cwd 不可靠)
//   2) 校验构建产物存在,缺失时给出「该跑哪条命令」的明确指引
//   3) 迁移旧数据:桌面版数据目录在 %APPDATA%,与命令行版的 <root>\data 是两套,
//      项目本身无迁移逻辑,直接启动会得到一个空库(角色全没、API Key 丢失)
//   4) 探测 3001 端口:区分「Kedai 已在运行」与「被别的程序占用」
//   5) 拉起桌面应用,并确认它真的活着 + 服务真的就绪,失败则弹窗报出可读原因
//
// 所有失败路径都以 MessageBox 呈现,不依赖控制台。

// 测试构建需要控制台输出,故仅在非 test 时隐藏控制台窗口
#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------- Win32 绑定

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, utype: u32) -> i32;
}

const MB_OK: u32 = 0x0000;
const MB_YESNO: u32 = 0x0004;
const MB_ICONERROR: u32 = 0x0010;
const MB_ICONWARNING: u32 = 0x0030;
const MB_ICONINFO: u32 = 0x0040;
const IDYES: i32 = 6;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn msgbox(text: &str, caption: &str, flags: u32) -> i32 {
    unsafe { MessageBoxW(0, wide(text).as_ptr(), wide(caption).as_ptr(), flags) }
}

fn fail(text: &str) -> ! {
    msgbox(text, "Kedai 启动失败", MB_OK | MB_ICONERROR);
    std::process::exit(1);
}

fn warn(text: &str) {
    msgbox(text, "Kedai 提示", MB_OK | MB_ICONWARNING);
}

fn confirm(text: &str) -> bool {
    msgbox(text, "Kedai", MB_YESNO | MB_ICONINFO) == IDYES
}

// ---------------------------------------------------------------- 项目根定位

/// 从 exe 所在目录向上找项目根。判据与 server-rs 的 project_root() 保持一致:
/// 同时存在 web\ 且存在 server-rs\ 或 data\。
///
/// 双击启动时 cwd 是桌面或 System32,绝不能用 current_dir() 推断。
fn locate_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    for _ in 0..6 {
        let looks_like_root = dir.join("web").is_dir()
            && (dir.join("server-rs").is_dir() || dir.join("data").is_dir());
        if looks_like_root {
            return Some(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

// ---------------------------------------------------------------- 服务健康探测

#[derive(PartialEq, Eq, Debug)]
enum PortState {
    /// 端口空闲,可以启动
    Free,
    /// 端口上是健康的 Kedai 服务
    KedaiRunning,
    /// 端口被别的程序占了(连得上但不是 Kedai)
    Occupied,
}

/// 直接走裸 TCP 发一个 HTTP 请求,避免为了一次健康检查引入 http 客户端依赖。
/// 服务端契约:GET /api/health 返回 200 且响应体含 "ok":true。
fn probe_port(addr: SocketAddr) -> PortState {
    let mut stream = match TcpStream::connect_timeout(&addr, Duration::from_millis(800)) {
        Ok(s) => s,
        Err(_) => return PortState::Free,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(1500)));

    let req = format!(
        "GET /api/health HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        addr
    );
    if stream.write_all(req.as_bytes()).is_err() {
        return PortState::Occupied;
    }
    let _ = stream.flush();

    let mut buf = Vec::new();
    // 健康响应很小;限个上限,免得对面是个会狂吐数据的程序
    let mut chunk = [0u8; 2048];
    while buf.len() < 16 * 1024 {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    let _ = stream.shutdown(Shutdown::Both);

    let text = String::from_utf8_lossy(&buf);
    let ok = text.starts_with("HTTP/1.1 200") && text.contains("\"ok\"") && text.contains("true");
    if ok {
        PortState::KedaiRunning
    } else {
        PortState::Occupied
    }
}

/// 读 .env 里的 PORT(若存在)。启动器必须和服务端看同一个端口,
/// 否则用户改了端口后启动器会一直探测 3001 而误报。
fn resolve_port(root: &Path) -> u16 {
    // 环境变量优先级最高,与 AppConfig::from_env 一致
    if let Ok(v) = std::env::var("PORT") {
        if let Ok(p) = v.trim().parse::<u16>() {
            return p;
        }
    }
    match std::fs::read_to_string(root.join(".env")) {
        Ok(content) => parse_port(&content).unwrap_or(3001),
        Err(_) => 3001,
    }
}

/// 从 .env 文本中取 PORT。独立成函数以便测试。
/// 注意必须精确匹配键名:WEB_DEV_PORT 也以 PORT 结尾,不能误命中。
fn parse_port(content: &str) -> Option<u16> {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let (key, val) = match line.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        if key.trim() != "PORT" {
            continue;
        }
        // 去掉行尾注释与引号
        let val = val.split('#').next().unwrap_or("").trim().trim_matches('"');
        if let Ok(p) = val.parse::<u16>() {
            return Some(p);
        }
    }
    None
}

// ---------------------------------------------------------------- 旧实例清理

/// 列出监听指定端口的进程 PID(Windows:解析 `netstat -ano`)。
/// 用于结束上一次未正常退出的 Kedai 实例——它占着端口会让新版无法启动,
/// 用户只看到旧界面,误以为「修复没生效」(实跑反馈)。
fn pids_listening_on(port: u16) -> Vec<u32> {
    let out = match Command::new("netstat").arg("-ano").output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let needle = format!(":{port}");
    let mut pids = Vec::new();
    for line in text.lines() {
        // TCP    127.0.0.1:3001    0.0.0.0:0    LISTENING    8748
        if !line.contains("LISTENING") {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        // 本地地址列须以 :PORT 结尾(避免 :13001 之类的误命中)
        if !cols[1].ends_with(&needle) {
            continue;
        }
        if let Ok(pid) = cols[cols.len() - 1].parse::<u32>() {
            if !pids.contains(&pid) {
                pids.push(pid);
            }
        }
    }
    pids
}

/// 结束占用指定端口的进程(排除自身,避免启动器在自清理时把自己杀掉)。
/// 返回是否至少成功结束一个进程。
fn terminate_port_owner(port: u16) -> bool {
    let self_pid = std::process::id();
    let mut killed = false;
    for pid in pids_listening_on(port) {
        if pid == self_pid {
            continue;
        }
        let ok = Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            killed = true;
        }
    }
    killed
}

/// 等待端口释放(结束旧进程后 TCP 表更新有延迟)
fn wait_port_free(addr: SocketAddr, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if probe_port(addr) == PortState::Free {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ---------------------------------------------------------------- 数据迁移

/// 桌面应用把 DATA_DIR 强制指向 %APPDATA%\com.kedai.app\data(见 src-tauri/src/lib.rs),
/// 而命令行版用 <root>\data。项目里没有任何跨目录迁移逻辑,于是第一次改用桌面版的人
/// 会看到一个全新空库:角色卡全空、settings.json 里的 API Key 丢失、历史会话不见。
///
/// 这里只在目标目录「明显是全新的」时迁移,且迁移前把被覆盖的文件改名备份,可回滚。
fn migrate_data_if_needed(root: &Path) {
    let appdata = match std::env::var("APPDATA") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => return,
    };
    let dst = appdata.join("com.kedai.app").join("data");
    let src = root.join("data");

    if !src.is_dir() {
        return;
    }
    // 源目录得真的有东西可搬
    let src_db = src.join("kedai.db");
    let src_settings = src.join("settings.json");
    if !src_db.is_file() && !src_settings.is_file() {
        return;
    }
    // 目标不存在 = 桌面版还没跑过,首次启动时由服务自己建即可,但我们提前搬更省事
    if !dst.exists() {
        if std::fs::create_dir_all(&dst).is_err() {
            return;
        }
    }

    if !looks_pristine(&dst) {
        // 目标已有真实使用痕迹,绝不碰——用户可能已经在桌面版里积累了新数据
        return;
    }

    let prompt = format!(
        "检测到桌面版数据目录是空的,而命令行版有既有数据。\n\n\
         桌面版和命令行版使用两个不同的数据目录,项目本身不会自动搬迁,\n\
         直接启动会得到一个空白的 Kedai(角色卡为空、API Key 丢失)。\n\n\
         来源:{}\n目标:{}\n\n是否现在迁移?(被覆盖的文件会先备份,可回滚)",
        src.display(),
        dst.display()
    );
    if !confirm(&prompt) {
        return;
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // 目标可能已被空库跑过一次,留下属于那个空库的 WAL/SHM。
    // 新主库覆盖上去后,这对陈旧的 WAL 会被回放到不匹配的库上导致损坏,必须先清掉。
    for suffix in ["kedai.db-wal", "kedai.db-shm"] {
        let _ = std::fs::remove_file(dst.join(suffix));
    }

    match copy_tree(&src, &dst, stamp) {
        Ok(n) => {
            // 数据库里的 avatar_path 存的是绝对路径,指向旧目录。
            // 文件搬了但路径没改的话,一旦旧目录被删/移动,头像就全丢。
            let note = match rewrite_avatar_paths(&dst, &src) {
                Ok(0) => String::new(),
                Ok(k) => format!("\n已修正 {k} 条头像路径。"),
                Err(e) => format!("\n注意:头像路径未能自动修正({e}),头像可能显示不出来。"),
            };
            msgbox(
                &format!(
                    "已迁移 {n} 个文件到桌面版数据目录。{note}\n\n{}",
                    dst.display()
                ),
                "Kedai 数据迁移完成",
                MB_OK | MB_ICONINFO,
            );
        }
        Err(e) => {
            warn(&format!(
                "数据迁移未能完成:{e}\n\n启动会继续,但桌面版可能显示为空白数据。"
            ));
        }
    }
}

/// 把 characters.avatar_path 里的旧目录前缀改写为新目录。
///
/// 数据库存的是绝对路径,只搬文件不改库,头像会在旧目录消失后集体失效。
/// 启动器刻意零依赖,不内嵌 sqlite,这里借用系统 sqlite3;没有它就如实报告,
/// 让用户知道头像可能显示不出来,而不是留下一个沉默的坏状态。
fn rewrite_avatar_paths(dst: &Path, old_root: &Path) -> Result<usize, String> {
    let db = dst.join("kedai.db");
    if !db.is_file() {
        return Ok(0);
    }
    let old = old_root.to_string_lossy().replace('\'', "''");
    let new = dst.to_string_lossy().replace('\'', "''");
    if old == new {
        return Ok(0);
    }

    let sql = format!(
        "UPDATE characters SET avatar_path = replace(avatar_path, '{old}', '{new}') \
         WHERE avatar_path LIKE '{old}%'; SELECT changes();"
    );

    let out = Command::new("sqlite3")
        .arg(&db)
        .arg(&sql)
        .output()
        .map_err(|_| "系统未提供 sqlite3 命令".to_string())?;

    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let n = String::from_utf8_lossy(&out.stdout)
        .trim()
        .lines()
        .last()
        .and_then(|l| l.trim().parse::<usize>().ok())
        .unwrap_or(0);
    Ok(n)
}

/// 目标目录是否「还没被真正用过」。
/// 判据:没有 settings.json(用户一存设置就会生成),且 characters 下除内置角色外没有别的卡。
fn looks_pristine(dst: &Path) -> bool {
    if dst.join("settings.json").is_file() {
        return false;
    }
    let chars = dst.join("characters");
    if let Ok(entries) = std::fs::read_dir(&chars) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name != "builtin-system.json" {
                return false;
            }
        }
    }
    true
}

/// 递归复制。已存在的目标文件先改名为 .bak-<stamp> 再覆盖,保证可回滚。
/// 跳过日志与备份类文件,避免把陈年垃圾也搬过去。
fn copy_tree(src: &Path, dst: &Path, stamp: u64) -> std::io::Result<usize> {
    let mut count = 0usize;
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();

        // 这些不该跨目录搬:日志属于各自实例,.bak/.verify 是开发残留。
        //
        // -wal/-shm 尤其危险:它们与特定的 kedai.db 强绑定。若把源库的 WAL 搬到
        // 目标、或让目标残留的旧 WAL 遇上新主库,SQLite 会把不匹配的 WAL 回放到
        // 主库上,直接损坏数据。只搬主库文件,让 SQLite 自己重建 WAL 才安全。
        if name_str.ends_with(".log")
            || name_str.starts_with(".verify")
            || name_str.contains(".bak-")
            || name_str.ends_with("-wal")
            || name_str.ends_with("-shm")
        {
            continue;
        }

        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            count += copy_tree(&from, &to, stamp)?;
        } else {
            if to.exists() {
                let backup = dst.join(format!("{name_str}.bak-{stamp}"));
                // 备份失败就跳过这个文件,宁可不迁也不做不可回滚的覆盖
                if std::fs::rename(&to, &backup).is_err() {
                    continue;
                }
            }
            std::fs::copy(&from, &to)?;
            count += 1;
        }
    }
    Ok(count)
}

// ---------------------------------------------------------------- 产物定位

struct Artifacts {
    desktop: Option<PathBuf>,
    server: Option<PathBuf>,
    web_dist: bool,
}

fn find_artifacts(root: &Path) -> Artifacts {
    let first_existing =
        |cands: &[PathBuf]| -> Option<PathBuf> { cands.iter().find(|p| p.is_file()).cloned() };

    // 便携目录产物(build-portable.ps1 的输出)优先:构建脚本结束时会删除
    // src-tauri\target,那里的 exe 只是构建中间态,常态下不存在。
    // Tauri 原始产物名取决于 [[bin]]/package name,作为回退保留。
    let desktop = first_existing(&[
        root.join("dist/Kedai-portable/Kedai.exe"),
        root.join("src-tauri/target/release/kedai-desktop.exe"),
        root.join("src-tauri/target/release/kedai.exe"),
    ]);
    let server = first_existing(&[
        root.join("dist/kedai-server.exe"),
        root.join("server-rs/target/release/kedai-server.exe"),
        root.join("src-tauri/target/release/kedai-server.exe"),
        root.join("server-rs/target/debug/kedai-server.exe"),
    ]);
    let web_dist = root.join("web/dist/index.html").is_file();

    Artifacts {
        desktop,
        server,
        web_dist,
    }
}

// ---------------------------------------------------------------- 新鲜度检测

/// 递归取目录(含子文件)与单文件的最新修改时间;路径不存在跳过。
fn latest_mtime(path: &Path, latest: &mut SystemTime) {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return,
    };
    if meta.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for e in entries.flatten() {
                latest_mtime(&e.path(), latest);
            }
        }
        return;
    }
    if let Ok(t) = meta.modified() {
        if t > *latest {
            *latest = t;
        }
    }
}

/// 产品源码(web/src、server-rs/src、src-tauri/src、构建脚本与配置)的最新修改时间。
/// 与 start.ps1 的 Get-LatestSourceTime 扫描表保持一致。
fn latest_source_mtime(root: &Path) -> SystemTime {
    let mut latest = UNIX_EPOCH;
    let paths: [PathBuf; 17] = [
        root.join("web/src"),
        root.join("web/public"),
        root.join("web/index.html"),
        root.join("web/vite.config.ts"),
        root.join("web/tsconfig.json"),
        root.join("web/package.json"),
        root.join("server-rs/src"),
        root.join("server-rs/build.rs"),
        root.join("server-rs/Cargo.toml"),
        root.join("server-rs/Cargo.lock"),
        root.join("src-tauri/src"),
        root.join("src-tauri/build.rs"),
        root.join("src-tauri/tauri.conf.json"),
        root.join("src-tauri/Cargo.toml"),
        root.join("src-tauri/Cargo.lock"),
        root.join("tools/build-portable.ps1"),
        root.join("build.ps1"),
    ];
    for p in &paths {
        latest_mtime(p, &mut latest);
    }
    latest
}

/// 便携版产物是否比源码旧(超过 2 分钟宽容窗口,与 start.ps1 一致)。
fn artifact_stale(root: &Path, exe: &Path) -> bool {
    let src = latest_source_mtime(root);
    let art = match std::fs::metadata(exe).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return true,
    };
    let grace = Duration::from_secs(120);
    src.duration_since(art).map(|d| d > grace).unwrap_or(false)
}

// ---------------------------------------------------------------- 两版漂移检测

/// 测试版与便携版的构建指纹是否不一致。
/// mtime 检测只能发现「产物比源码旧」,发现不了「两版各自从不同的 web/dist 编译」
/// (旧构建流程默认只刷新测试版,便携版会整体落后)。sidecar 指纹比对补上这个盲区。
/// 任一 sidecar 缺失视为无法判定(不误报),仍由 mtime 检测兜底。
fn versions_drifted(root: &Path, desktop: &Path) -> bool {
    let test_exe = root.join("dist/kedai-server.exe");
    if !test_exe.is_file() || !desktop.is_file() {
        return false;
    }
    match (sidecar_dist_hash(&test_exe), sidecar_dist_hash(desktop)) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

/// 读取 <exe>.build.json 的 dist_hash 字段。sidecar 由 tools/Write-BuildStamp.ps1
/// 以紧凑扁平 JSON 写出;零依赖启动器不引入 JSON 库,按固定模式做子串提取。
fn sidecar_dist_hash(exe: &Path) -> Option<String> {
    let mut name = exe.file_name()?.to_os_string();
    name.push(".build.json");
    let sidecar = exe.with_file_name(name);
    let content = std::fs::read_to_string(sidecar).ok()?;
    extract_json_string(&content, "dist_hash")
}

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = json.find(&pat)? + pat.len();
    let rest = &json[start..];
    let end = rest.find('"')?;
    let val = &rest[..end];
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}

// ---------------------------------------------------------------- 启动

fn main() {
    let root = match locate_root() {
        Some(r) => r,
        None => fail(
            "找不到 Kedai 项目根目录。\n\n\
             启动器需要放在项目根目录下(与 web\\、server-rs\\ 同级),\n\
             或其 target\\release\\ 子目录中。\n\n\
             请把 Kedai.exe 移回项目根目录后重试。",
        ),
    };

    let data_dir = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.clone())
        .join("com.kedai.app")
        .join("data");
    eprintln!("[信息] DATA_DIR={}", data_dir.display());

    let port = resolve_port(&root);
    let addr: SocketAddr = match format!("127.0.0.1:{port}").parse() {
        Ok(a) => a,
        Err(_) => fail(&format!("端口配置非法:{port}")),
    };

    // 先看端口,再谈启动:被别的程序占着的话,启动多少次都是白屏
    match probe_port(addr) {
        PortState::KedaiRunning => {
            // 端口上已有 Kedai 服务:绝大多数情况是上次未正常退出的旧实例
            // (直接跑过 kedai-server.exe、或旧桌面版未回收内嵌后端)。它占着端口,
            // 新版根本起不来,用户看到的始终是旧界面——误以为「修复没生效」。
            // 这里提供一键结束旧实例并继续启动(对齐 start.ps1 的交互;不再只报错劝退)。
            if confirm(&format!(
                "端口 {port} 上已有 Kedai 服务在运行。\n\n\
                 这通常是上一次未正常退出的旧实例,它占着端口会让新版本无法启动\n\
                 (你看到的会一直是旧界面)。\n\n\
                 是否结束该旧实例,并启动最新版?"
            )) {
                terminate_port_owner(port);
                if !wait_port_free(addr, Duration::from_secs(5)) {
                    fail(&format!(
                        "已尝试结束旧实例,但端口 {port} 仍被占用。\n\n\
                         请在任务管理器中结束 kedai-server.exe / Kedai.exe 后重试,\n\
                         或把 .env 里的 PORT 改成别的端口(如 3002)。"
                    ));
                }
            } else {
                fail(&format!(
                    "已取消启动:端口 {port} 仍被旧 Kedai 实例占用,新版本不会启动。\n\n\
                     你看到的仍是旧界面。如需使用新版,请先结束旧实例后重新双击本启动器。"
                ));
            }
        }
        PortState::Occupied => fail(&format!(
            "端口 {port} 已被其它程序占用,Kedai 无法启动。\n\n\
             该端口上有程序在响应,但它不是 Kedai 服务。\n\n\
             解决办法(任选其一):\n\
             1. 关掉占用该端口的程序;\n\
             2. 在项目根目录的 .env 中改用别的端口,例如 PORT=3002\n\
                (没有 .env 就复制 .env.example 为 .env)。\n\n\
             查占用者:在 PowerShell 执行\n\
             Get-Process -Id (Get-NetTCPConnection -LocalPort {port}).OwningProcess",
        )),
        PortState::Free => {}
    }

    let art = find_artifacts(&root);

    let desktop = match art.desktop {
        Some(d) => d,
        None => {
            let hint = if art.server.is_some() && art.web_dist {
                "后端和前端产物都在,只差桌面应用本身。"
            } else {
                "构建产物不完整。"
            };
            fail(&format!(
                "未找到桌面应用产物 Kedai.exe(期望位于 dist\\Kedai-portable\\)。\n\n{hint}\n\n\
                 请在项目根目录 {} 打开 PowerShell 执行:\n\n\
                 npm install\n\
                 .\\build.ps1\n\n\
                 首次构建 Tauri 需要较长时间,请耐心等待完成。",
                root.display()
            ));
        }
    };

    // 触发重建的两类原因:① 源码比产物新(mtime);② 两版构建指纹不一致(sidecar)。
    // 保证双击启动器拿到的始终是最新且与测试版同步的版本
    // (与 start.ps1 的自动重建语义一致;直接双击 dist 内 exe 无此保障)。
    let drifted = versions_drifted(&root, &desktop);
    if drifted || artifact_stale(&root, &desktop) {
        let prompt = if drifted {
            "检测到测试版与便携版的构建指纹不一致(两版曾分开构建)。\n\n\
             是否现在自动重建并同步两版?(前端 + Rust 编译,首次或全量需数分钟)\n\n\
             选「是」开始重建并自动启动新版;选「否」直接启动当前便携版。"
        } else {
            "检测到源码比当前程序新。\n\n\
             是否现在自动重建?(前端 + Rust 编译,首次或全量需数分钟)\n\n\
             选「是」开始重建并自动启动新版;选「否」直接启动当前版本。"
        };
        if confirm(prompt) {
            // build.ps1 默认双端同步产出(测试版 + 便携版),重建即对齐指纹
            let script = root.join("build.ps1");
            #[cfg(windows)]
            let status = {
                use std::os::windows::process::CommandExt;
                // 新控制台窗口呈现构建进度(启动器自身无控制台)
                Command::new("powershell")
                    .args([
                        "-NoProfile",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                        script.to_str().unwrap_or_default(),
                    ])
                    .current_dir(&root)
                    .creation_flags(0x0000_0010) // CREATE_NEW_CONSOLE
                    .status()
            };
            #[cfg(not(windows))]
            let status = Command::new("powershell")
                .arg(script.to_str().unwrap_or_default())
                .current_dir(&root)
                .status();
            match status {
                Ok(s) if s.success() => {}
                Ok(s) => fail(&format!(
                    "自动重建失败(退出码 {:?}),请在上面的构建窗口中查看错误。\n\
                     修复后重新双击本启动器,或手动执行 .\\build.ps1。",
                    s.code()
                )),
                Err(e) => fail(&format!("无法启动构建脚本:{e}\n请手动执行 .\\build.ps1。")),
            }
            // 重建后产物路径不变(build.ps1 原地更新 dist\Kedai-portable\Kedai.exe)
        }
    }

    // 数据迁移必须发生在启动之前:服务一旦起来就会在空目录里建库并写入,
    // 之后再迁就要面对「两边都有数据」的合并问题。
    migrate_data_if_needed(&root);

    // 工作目录设为项目根,让服务端的 dotenvy 能找到 .env
    // (dotenvy 从 cwd 起向上搜,双击时 cwd 是桌面,.env 会被静默忽略)
    let mut child = match Command::new(&desktop).current_dir(&root).spawn() {
        Ok(c) => c,
        Err(e) => fail(&format!(
            "无法启动桌面应用:{e}\n\n路径:{}\n\n\
             若提示被拒绝访问,请检查杀毒软件是否拦截,或以管理员身份重试。",
            desktop.display()
        )),
    };

    // 核心:确认它真的活着。这正是 start.ps1 缺失的一步——
    // Start-Process 成功不代表进程没有秒退。
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut service_ready = false;

    while Instant::now() < deadline {
        // 进程提前退出 = 启动失败,立刻报出退出码和日志位置
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "未知".into());
                fail(&format!(
                    "桌面应用启动后立即退出(退出码 {code})。\n\n\
                     常见原因:\n\
                     1. 缺少 WebView2 运行时——请安装 Microsoft Edge WebView2 Runtime;\n\
                     2. 服务启动失败——查看日志:\n\
                        %APPDATA%\\com.kedai.app\\logs\\tauri-start-error.log\n\
                     3. 数据库损坏——检查 %APPDATA%\\com.kedai.app\\data\\kedai.db"
                ));
            }
            Ok(None) => {}
            Err(_) => break,
        }

        if probe_port(addr) == PortState::KedaiRunning {
            service_ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }

    if !service_ready {
        // 进程还活着但服务没起来:窗口大概率是白屏,给出可查证的下一步
        warn(&format!(
            "桌面应用已启动,但 60 秒内后端服务仍未就绪。\n\n\
             窗口可能显示为空白。请查看日志:\n\
             %APPDATA%\\com.kedai.app\\logs\\\n\n\
             其中 tauri-start-error.log 若存在,会记录服务启动失败的原因。\n\n\
             服务地址:http://127.0.0.1:{port}/"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("kedai-launcher-test-{tag}-{stamp}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn extract_json_string_reads_compact_json() {
        let json = r#"{"version":"0.2.0","build_time":"2026-08-28T12:00:00Z","dist_hash":"abc123"}"#;
        assert_eq!(
            extract_json_string(json, "dist_hash"),
            Some("abc123".to_string())
        );
        assert_eq!(
            extract_json_string(json, "version"),
            Some("0.2.0".to_string())
        );
        assert_eq!(extract_json_string(json, "missing"), None);
        // 空值与无引号值都不该误判
        assert_eq!(extract_json_string(r#"{"dist_hash":""}"#, "dist_hash"), None);
        assert_eq!(extract_json_string(r#"{"dist_hash":123}"#, "dist_hash"), None);
    }

    #[test]
    fn drift_detects_mismatch_and_tolerates_missing_sidecar() {
        let root = tmpdir("drift");
        let dist = root.join("dist");
        std::fs::create_dir_all(&dist).unwrap();
        let test_exe = dist.join("kedai-server.exe");
        let desktop = dist.join("Kedai.exe");
        std::fs::write(&test_exe, b"a").unwrap();
        std::fs::write(&desktop, b"b").unwrap();

        // sidecar 缺失(旧产物)→ 无法判定,绝不能误报漂移
        assert!(!versions_drifted(&root, &desktop));

        let stamp = |h: &str| {
            format!(r#"{{"version":"0.2.0","build_time":"t","dist_hash":"{h}"}}"#)
        };
        std::fs::write(dist.join("kedai-server.exe.build.json"), stamp("aaa")).unwrap();
        std::fs::write(dist.join("Kedai.exe.build.json"), stamp("aaa")).unwrap();
        assert!(!versions_drifted(&root, &desktop), "指纹一致不算漂移");

        std::fs::write(dist.join("Kedai.exe.build.json"), stamp("bbb")).unwrap();
        assert!(versions_drifted(&root, &desktop), "指纹不一致必须判漂移");

        // 测试版产物缺失(未构建过)→ 不误报
        std::fs::remove_file(&test_exe).unwrap();
        assert!(!versions_drifted(&root, &desktop));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn parse_port_reads_plain_value() {
        assert_eq!(parse_port("PORT=3002"), Some(3002));
        assert_eq!(parse_port("HOST=127.0.0.1\nPORT=8080\n"), Some(8080));
    }

    #[test]
    fn parse_port_ignores_comments_and_quotes() {
        assert_eq!(parse_port("#PORT=9999\nPORT=3005"), Some(3005));
        assert_eq!(parse_port("PORT=\"3006\""), Some(3006));
        assert_eq!(parse_port("PORT=3007  # 端口"), Some(3007));
    }

    /// 回归:.env.example 里有 WEB_DEV_PORT=5173,决不能被当成 PORT
    #[test]
    fn parse_port_does_not_match_web_dev_port() {
        assert_eq!(parse_port("WEB_DEV_PORT=5173"), None);
        assert_eq!(parse_port("WEB_DEV_PORT=5173\nPORT=3001"), Some(3001));
    }

    #[test]
    fn parse_port_absent_yields_none() {
        assert_eq!(parse_port("HOST=127.0.0.1\nCONNECTOR=mock"), None);
    }

    #[test]
    fn pristine_detects_untouched_dir() {
        let d = tmpdir("pristine");
        assert!(looks_pristine(&d), "空目录应判为未使用");

        std::fs::create_dir_all(d.join("characters")).unwrap();
        std::fs::write(d.join("characters/builtin-system.json"), "{}").unwrap();
        assert!(looks_pristine(&d), "只有内置角色仍应判为未使用");

        std::fs::remove_dir_all(&d).ok();
    }

    /// 关键安全性:目标目录一旦有真实数据就绝不能被迁移覆盖
    #[test]
    fn pristine_rejects_used_dir() {
        let d = tmpdir("used-settings");
        std::fs::write(d.join("settings.json"), "{}").unwrap();
        assert!(!looks_pristine(&d), "有 settings.json 说明已被使用");
        std::fs::remove_dir_all(&d).ok();

        let d2 = tmpdir("used-chars");
        std::fs::create_dir_all(d2.join("characters")).unwrap();
        std::fs::write(d2.join("characters/my-card.json"), "{}").unwrap();
        assert!(!looks_pristine(&d2), "有自建角色卡说明已被使用");
        std::fs::remove_dir_all(&d2).ok();
    }

    #[test]
    fn copy_tree_copies_recursively_and_skips_noise() {
        let src = tmpdir("copy-src");
        let dst = tmpdir("copy-dst");

        std::fs::write(src.join("kedai.db"), b"DB").unwrap();
        std::fs::write(src.join("settings.json"), b"{}").unwrap();
        std::fs::write(src.join("server.log"), b"noise").unwrap();
        std::fs::write(src.join(".verify-wb.mjs"), b"noise").unwrap();
        std::fs::write(src.join("settings.json.bak-before-x"), b"noise").unwrap();
        std::fs::write(src.join("kedai.db-wal"), b"WAL").unwrap();
        std::fs::write(src.join("kedai.db-shm"), b"SHM").unwrap();
        std::fs::create_dir_all(src.join("characters")).unwrap();
        std::fs::write(src.join("characters/card.json"), b"card").unwrap();

        let n = copy_tree(&src, &dst, 42).unwrap();

        assert!(dst.join("kedai.db").is_file());
        assert!(dst.join("settings.json").is_file());
        assert!(dst.join("characters/card.json").is_file());
        assert!(!dst.join("server.log").exists(), "日志不该迁移");
        assert!(!dst.join(".verify-wb.mjs").exists(), "开发残留不该迁移");
        assert!(
            !dst.join("settings.json.bak-before-x").exists(),
            "旧备份不该迁移"
        );
        // WAL/SHM 与特定主库强绑定,搬过去会导致 SQLite 回放不匹配的 WAL 而损坏数据
        assert!(!dst.join("kedai.db-wal").exists(), "WAL 绝不能迁移");
        assert!(!dst.join("kedai.db-shm").exists(), "SHM 绝不能迁移");
        assert_eq!(n, 3, "应只迁移 3 个有效文件");

        std::fs::remove_dir_all(&src).ok();
        std::fs::remove_dir_all(&dst).ok();
    }

    /// 覆盖前必须留下可回滚的备份
    #[test]
    fn copy_tree_backs_up_before_overwrite() {
        let src = tmpdir("bak-src");
        let dst = tmpdir("bak-dst");
        std::fs::write(src.join("settings.json"), b"NEW").unwrap();
        std::fs::write(dst.join("settings.json"), b"OLD").unwrap();

        copy_tree(&src, &dst, 42).unwrap();

        assert_eq!(std::fs::read(dst.join("settings.json")).unwrap(), b"NEW");
        assert_eq!(
            std::fs::read(dst.join("settings.json.bak-42")).unwrap(),
            b"OLD",
            "原文件必须完整保留以便回滚"
        );

        std::fs::remove_dir_all(&src).ok();
        std::fs::remove_dir_all(&dst).ok();
    }

    /// 端口空闲时不得误报为被占用(否则启动器会拒绝启动)
    #[test]
    fn probe_reports_free_for_closed_port() {
        // 绑定后立即释放,得到一个大概率空闲的端口
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(matches!(probe_port(addr), PortState::Free));
    }

    /// 端口上是非 Kedai 程序时必须判为 Occupied,而不是 Free
    #[test]
    fn probe_reports_occupied_for_foreign_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            }
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(matches!(probe_port(addr), PortState::Occupied));
    }

    /// 健康的 Kedai 响应必须被识别,否则会误判端口被占用而拒绝启动
    #[test]
    fn probe_recognizes_healthy_kedai() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let body = br#"{"ok":true,"ts":1786206964852}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
                let _ = s.write_all(body);
            }
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(matches!(probe_port(addr), PortState::KedaiRunning));
    }
}
