// Kedai 桌面壳(Tauri 2)
// 职责:统一桌面数据目录 → 首次幂等迁移旧数据 → 启动内嵌后端
// → 健康检查通过后再导航并展示窗口。

use std::path::{Path, PathBuf};
use std::time::Duration;
// SystemTime/UNIX_EPOCH 仅用于便携版数据迁移的时间戳标记(桌面专属)
#[cfg(desktop)]
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::Manager;
// Rust→前端下发「用户点了窗口关闭」需要 Emitter trait 在作用域内(emit);
// 该事件仅桌面存在(Android 无 WindowEvent::CloseRequested,见 run() 的 RunEvent 分支)
#[cfg(desktop)]
use tauri::Emitter;

mod native_bridge;

const READY_TIMEOUT: Duration = Duration::from_secs(60);
const READY_INTERVAL: Duration = Duration::from_millis(250);

/// 实际使用的服务端口(setup 时从 config 解析并托管),退出时据此清理端口残留。
/// 仅桌面读取(Android 无端口清理流程),故标注 desktop 以消除移动端 dead_code 警告。
#[cfg(desktop)]
struct ResolvedPort(u16);

/// 关闭确认的二次关闭兜底标记(2026-09-16 批次 5)。
///
/// 为什么需要:关闭确认从「Tauri 原生对话框」改为「前端自绘弹窗」后,壳在
/// CloseRequested 里 prevent_close 并等前端响应。若前端崩溃、页面未加载完或弹窗
/// chunk 加载失败,确认框永远不会出现,窗口就再也关不掉——比没有确认框更糟。
/// 这枚标记让第二次点击关闭直接退出:第一次置位(交给前端弹),已是置位态即强退。
#[cfg(desktop)]
#[derive(Default)]
struct ExitGuard(std::sync::atomic::AtomicBool);

/// 登记一次关闭请求,返回「是否应当强制退出」。
///
/// 首次调用(false→true)返回 false:交给前端弹确认框;再次调用(已是 true)返回 true:
/// 前端没响应,不再阻止关闭。抽成纯函数是为了可单测——窗口关不掉是用户可感的硬故障。
#[cfg(desktop)]
fn register_close_request(flag: &std::sync::atomic::AtomicBool) -> bool {
    flag.swap(true, std::sync::atomic::Ordering::SeqCst)
}

/// 前端取消退出时复位兜底标记:否则「取消一次」之后,下一次单击关闭会因标记仍是
/// 置位态而跳过确认直接退出。
#[cfg(desktop)]
fn reset_close_request(flag: &std::sync::atomic::AtomicBool) {
    flag.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// 结束占用指定端口的**其它**进程(排除自身):关闭 Kedai 时一并清理可能存在的
/// 独立后端残留(例如上次直接运行 dist\kedai-server.exe 未退出、或旧桌面版未回收)。
/// 桌面版的内嵌后端与本进程同 PID,会被排除,不影响正常退出路径。
/// 实跑反馈「点退出后 kedai-server 仍在跑」的兜底修复。
///
/// 平台差异:依赖 netstat/taskkill,仅 Windows 桌面存在;Android 沙箱内既无这些命令,
/// 也不存在「多个 Kedai 实例争抢固定端口」的场景(系统 launcher 保证单实例)。
#[cfg(desktop)]
fn terminate_other_port_owners(port: u16) {
    let self_pid = std::process::id();
    let out = match std::process::Command::new("netstat").arg("-ano").output() {
        Ok(o) => o,
        Err(_) => return,
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let needle = format!(":{port}");
    for line in text.lines() {
        // TCP    127.0.0.1:3001    0.0.0.0:0    LISTENING    8748
        if !line.contains("LISTENING") {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 || !cols[1].ends_with(&needle) {
            continue;
        }
        let pid = match cols[cols.len() - 1].parse::<u32>() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pid == self_pid {
            continue;
        }
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output();
    }
}

/// 应用入口。
///
/// `mobile_entry_point` 在 Android/iOS 上生成 JNI/FFI 入口符号(`Java_<pkg>_<cls>_...`),
/// 供 TauriActivity 在应用启动时回调;桌面端该属性被 cfg 排除,行为不变。
/// 缺此属性时 Android 能编译通过但启动即闪退(找不到原生入口)。
/// app_data_dir() 失败时的回退目录。
/// - 桌面:沿用历史行为 %APPDATA%\com.kedai.app;
/// - Android:无 %APPDATA%,返回错误由启动流程记录并退出(宁可明确报错也不写错位置)。
#[cfg(desktop)]
fn fallback_app_data_dir() -> Result<PathBuf, String> {
    Ok(PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("com.kedai.app"))
}

#[cfg(mobile)]
fn fallback_app_data_dir() -> Result<PathBuf, String> {
    Err("无法解析 Android 应用数据目录(app_data_dir 调用失败)".into())
}

/// JNI 入口:Android 在 `System.loadLibrary("kedai_desktop_lib")` 时调用本函数,
/// 把 JavaVM 指针交给原生库。后端 API Key 加密与原生能力桥(外链/分享/保活)
/// 都依赖它(见 server-rs/src/services/jni_bridge.rs)。
///
/// 定义在此(cdylib 根)是为了保证符号被导出 —— 依赖库内部定义的 `JNI_OnLoad`
/// 可能被链接器丢弃。整个依赖栈中没有其它 `JNI_OnLoad`(已核查 tao/wry/tauri)。
///
/// # Safety
/// 由 JVM 调用,`vm` 为其传入的合法 `JavaVM*`;返回期望的 JNI 版本。
#[cfg(target_os = "android")]
#[no_mangle]
pub unsafe extern "C" fn JNI_OnLoad(vm: *mut std::ffi::c_void, _reserved: *mut std::ffi::c_void) -> i32 {
    kedai_server::services::jni_bridge::on_load(vm)
}

/// 前端请求退出应用的事件名(Android 返回键在无处可退时触发)。
///
/// 为什么用事件而不是自定义命令:
/// Tauri 2 的 IPC 对 **remote origin**(本应用页面来自 http://127.0.0.1:<port>/,
/// 而非 tauri:// 协议)会做 ACL 校验,而框架**不会**为 `#[tauri::command]` 生成权限条目
/// (已确认 gen/schemas 中无对应条目),实测调用自定义命令报
/// `exit_app not allowed. Plugin not found`;
/// `plugin:app|exit` 同样被拒(无 Rust 侧权限声明),
/// `WebviewWindow::close()` 在 Android 上又不结束 Activity(进程仍在且 Promise 悬挂)。
/// 而事件系统(`core:event:allow-emit`)已包含在 core:default 内,无需新增权限。
///
/// 语义与桌面版一致:后端与壳同进程,退出即整体结束,无残留服务。
const EXIT_APP_EVENT: &str = "kedai://exit-app";

/// 壳→前端下发「用户点了窗口关闭,请弹退出确认」(仅桌面)。
///
/// 为什么走事件而不是原生对话框:关闭确认要出前端至上主义样式,而 Tauri 的
/// `window.dialog()` 是 Windows MessageBox,外观不受前端 CSS 控制。
/// 方向与 EXIT_APP_EVENT 相反(Rust→前端),但同样复用事件系统权限
/// (core:default 内含 core:event:default = allow-listen/allow-emit),无需改 capabilities。
#[cfg(desktop)]
const CLOSE_REQUESTED_EVENT: &str = "kedai://close-requested";

/// 前端→壳:用户在确认框点了「取消」,复位二次关闭兜底标记(仅桌面)。
/// 没有它,取消一次之后的下一次关闭会被兜底标记直接放行。
#[cfg(desktop)]
const CLOSE_CANCELLED_EVENT: &str = "kedai://close-cancelled";

/// 退出应用:先清理端口上的其它 Kedai 残留(独立后端/旧实例),再结束本进程。
/// 关闭确认与前端退出事件共用此出口,保证「退出 = 全部结束」。
#[cfg(desktop)]
fn exit_now(app: &tauri::AppHandle) {
    // 先清理端口上的其它 Kedai 残留(独立后端等),再退出本进程
    if let Some(port) = app.try_state::<ResolvedPort>() {
        terminate_other_port_owners(port.0);
    }
    app.exit(0);
}

/// Android:无端口残留清理流程(无 netstat/taskkill,且系统 launcher 保证单实例),
/// 后端与壳同进程,退出即整体结束。
#[cfg(mobile)]
fn exit_now(app: &tauri::AppHandle) {
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 导出保存对话框 + 文件写入(用户自由选择导出位置)
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            // 前端请求退出(Android 返回键在无处可退时发出)。
            // 用事件而非自定义命令的原因见 EXIT_APP_EVENT 的注释。
            {
                use tauri::Listener;
                let handle = app.handle().clone();
                app.listen(EXIT_APP_EVENT, move |_| {
                    tracing::info!("收到前端退出请求,结束进程");
                    // 走统一退出出口:清理端口残留后再退(此前事件路径漏了清理,
                    // 只有原生对话框回调清理,留下「点退出后 kedai-server 还在跑」的缺口)
                    exit_now(&handle);
                });

                // 前端取消退出:复位二次关闭兜底标记,让下一次关闭仍走确认框。
                #[cfg(desktop)]
                {
                    let handle = app.handle().clone();
                    app.listen(CLOSE_CANCELLED_EVENT, move |_| {
                        if let Some(guard) = handle.try_state::<ExitGuard>() {
                            reset_close_request(&guard.0);
                        }
                    });
                }

                // 原生能力事件(外链/分享/保活):同样是「前端 emit → 原生执行」,
                // 绕开 remote origin 下的自定义命令 ACL 限制。
                // 载荷是 JSON 字符串(&str),解析失败仅记 warn 不 panic。
                app.listen(native_bridge::OPEN_EXTERNAL_EVENT, |event| {
                    match serde_json::from_str::<native_bridge::OpenExternalPayload>(event.payload()) {
                        Ok(p) => {
                            if let Err(e) = native_bridge::open_external(&p.url) {
                                tracing::warn!(url = p.url, error = e, "打开外链失败");
                            }
                        }
                        Err(e) => tracing::warn!(error = e.to_string(), "open-external 载荷解析失败"),
                    }
                });
                app.listen(native_bridge::SHARE_FILE_EVENT, |event| {
                    match serde_json::from_str::<native_bridge::ShareFilePayload>(event.payload()) {
                        Ok(p) => {
                            if let Err(e) = native_bridge::share_file(&p.name, &p.content) {
                                tracing::warn!(name = p.name, error = e, "分享导出失败");
                            }
                        }
                        Err(e) => tracing::warn!(error = e.to_string(), "share-file 载荷解析失败"),
                    }
                });
                app.listen(native_bridge::KEEPALIVE_START_EVENT, |_| {
                    if let Err(e) = native_bridge::keepalive_start() {
                        tracing::warn!(error = e, "启动前台服务保活失败");
                    }
                });
                app.listen(native_bridge::KEEPALIVE_STOP_EVENT, |_| {
                    if let Err(e) = native_bridge::keepalive_stop() {
                        tracing::warn!(error = e, "停止前台服务保活失败");
                    }
                });
                // 命令执行层的 Shizuku 授权请求(阶段 E):弹系统授权框
                app.listen(native_bridge::SHIZUKU_REQUEST_EVENT, |_| {
                    if let Err(e) = native_bridge::request_shizuku_permission() {
                        tracing::warn!(error = e, "请求 Shizuku 授权失败");
                    }
                });
            }

            // 数据目录:Android 上 app_data_dir() 映射到应用私有目录(/data/data/<pkg>/files),
            // 无 %APPDATA% 可回退,故回退实现按平台分离(见 fallback_app_data_dir)。
            let app_data = match app.path().app_data_dir() {
                Ok(dir) => dir,
                Err(_) => fallback_app_data_dir()?,
            };
            let data_dir = app_data.join("data");
            let log_dir = app_data.join("logs");

            std::fs::create_dir_all(&log_dir)?;
            // 便携版数据迁移仅桌面存在:靠 exe 同级向上找 data/,APK 内无此目录布局
            #[cfg(desktop)]
            if let Some(project_data) = find_project_data_dir() {
                if let Err(error) = migrate_data_if_needed(&project_data, &data_dir) {
                    write_start_error(&log_dir, &format!("旧数据迁移失败: {error}"));
                }
            }

            std::env::set_var("DATA_DIR", &data_dir);
            std::env::set_var("LOG_DIR", &log_dir);
            eprintln!("[信息] DATA_DIR={}", data_dir.display());

            // Android UI 迭代加速(仅 debug 构建):前端 dist 默认编译期内嵌进二进制
            // (server-rs/api/static_files.rs 的 include_dir),每次改样式都要重编 Rust。
            // 若应用私有目录下存在外置 dist,则经 KEDAI_WEB_DIST 覆盖为磁盘读取,
            // 迭代流程变成「npm build → adb push(tar 经 run-as 解包)→ 重启应用」,
            // 无需重编 Rust。
            //
            // 为什么用应用私有目录而非 /sdcard/Android/data/<pkg>/files:
            // 那个目录由 adb shell 创建时 owner 是 shell,Android 11+ 的 scoped storage
            // 会拒绝应用读取(即使 chmod 777 也不行,权限检查在 FUSE 层)。
            // 私有目录 owner 恒为应用自身,读写无限制;adb 侧用 `run-as` 以应用身份写入。
            #[cfg(all(mobile, debug_assertions))]
            {
                let dev_dist = data_dir
                    .parent()
                    .map(|p| p.join("kedai-dist"))
                    .unwrap_or_else(|| data_dir.join("kedai-dist"));
                if dev_dist.join("index.html").is_file() {
                    std::env::set_var("KEDAI_WEB_DIST", &dev_dist);
                    eprintln!("[信息] 使用设备外置前端目录: {}", dev_dist.display());
                }
            }

            let config = kedai_server::config::AppConfig::from_env();
            // 托管端口供退出清理使用(见 terminate_other_port_owners);仅桌面消费该状态
            #[cfg(desktop)]
            app.manage(ResolvedPort(config.port));
            // 关闭确认的二次关闭兜底标记(仅桌面有 CloseRequested 事件)
            #[cfg(desktop)]
            app.manage(ExitGuard::default());
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = start_and_wait_ready(config, app_handle.clone(), log_dir.clone()).await {
                    write_start_error(&log_dir, &error);
                    eprintln!("[错误] {error}");
                    app_handle.exit(1);
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("构建 Kedai 应用失败")
        .run(|app_handle, event| {
            // Android 不使用 app_handle(退出无需清理端口),显式忽略以避免未使用告警
            #[cfg(mobile)]
            let _ = &app_handle;
            // 关闭确认(实跑问题 8):拦截窗口关闭,下发事件让前端弹「确定退出」确认框;
            // 用户确认后前端发 EXIT_APP_EVENT,才真正退出。
            //
            // 为什么不用原生 window.dialog()(2026-09-16 批次 5 改动):它是 Windows
            // MessageBox,外观不受前端 CSS 影响,做不出至上主义样式;改为前端自绘弹窗。
            //
            // 平台差异:Android 没有 WindowEvent::CloseRequested(Activity 不会被「关闭」),
            // 退出由系统返回键/最近任务驱动(返回键无处可退时前端弹同一个确认框),
            // 故整段确认流程仅桌面编译。
            #[cfg(desktop)]
            if let tauri::RunEvent::WindowEvent {
                event: tauri::WindowEvent::CloseRequested { api, .. },
                ..
            } = event
            {
                api.prevent_close();
                // 二次关闭兜底:前端不响应(页面未加载完/弹窗 chunk 加载失败/脚本异常)
                // 时仍要能关掉窗口,否则比没有确认框更糟。
                let force = app_handle
                    .try_state::<ExitGuard>()
                    .map(|guard| register_close_request(&guard.0))
                    .unwrap_or(false);
                if force {
                    tracing::warn!("前端未响应关闭确认,第二次关闭直接退出");
                    exit_now(&app_handle);
                } else if let Err(e) = app_handle.emit(CLOSE_REQUESTED_EVENT, ()) {
                    // 事件下发失败(无 webview 等)意味着确认框不可能出现:
                    // 用户点了关闭就必须关得掉,直接退出比挂住强。
                    tracing::warn!(error = e.to_string(), "关闭确认事件下发失败,直接退出");
                    exit_now(&app_handle);
                }
            }

            // Android:进程级退出前的收尾(返回键退出/系统回收)——
            // 后端与壳同进程,无需单独 kill;此处仅留观测点便于真机排查。
            #[cfg(mobile)]
            if let tauri::RunEvent::ExitRequested { .. } = event {
                tracing::info!("Kedai Android 进程退出请求");
            }
        });
}

/// 端口当前是否可绑定(用于确认旧实例已真正释放端口)。
/// 直接试绑一次:成功即空闲;失败说明仍被占用。
/// 仅桌面使用(Android 无「接管旧实例」流程)。
#[cfg(desktop)]
async fn port_bindable(config: &kedai_server::config::AppConfig) -> bool {
    let addr = format!("{}:{}", config.host, config.port);
    match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => {
            drop(listener);
            true
        }
        Err(_) => false,
    }
}

async fn start_and_wait_ready(
    config: kedai_server::config::AppConfig,
    app: tauri::AppHandle,
    log_dir: PathBuf,
) -> Result<(), String> {
    let service_url = service_url(&config);

    // 桌面版必须使用本进程内、指向统一 AppData 的后端。端口若已被另一个 Kedai 实例
    // 占用(最常见:上次未正常退出的旧后端/旧桌面版残留),不能让用户对着旧界面以为
    //「新版本没生效」——先结束占用者(仅限端口上健康响应为 Kedai 的进程)再接管。
    //
    // 平台差异:清理手段是 netstat/taskkill,Android 不可用;且 Android 由系统 launcher
    // 保证单实例 + 应用私有目录,不存在跨实例争抢同一端口的场景,故整段仅桌面编译。
    // Android 若端口被占(极少见),走到下方启动逻辑后由绑定错误明确报出。
    #[cfg(desktop)]
    if health_ok(&config).await {
        eprintln!(
            "[信息] 端口 {} 已有旧 Kedai 实例,正在结束它以启动当前版本",
            config.port
        );
        terminate_other_port_owners(config.port);
        // 等端口释放(TCP 表更新有延迟);给 6 秒窗口
        let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
        while tokio::time::Instant::now() < deadline {
            if !health_ok(&config).await && port_bindable(&config).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if health_ok(&config).await {
            return Err(format!(
                "端口 {} 仍被其它 Kedai 实例占用,且无法自动结束。\n\
                 请用任务管理器结束 kedai-server.exe / Kedai.exe 后重启桌面版。",
                config.port
            ));
        }
    }

    let (server_error_tx, mut server_error_rx) = tokio::sync::oneshot::channel();
    let server_config = config.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = kedai_server::run_server(server_config).await {
            let _ = server_error_tx.send(error);
        }
    });

    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let Ok(error) = server_error_rx.try_recv() {
            // 双启动竞态(TOCTOU):health_ok 通过后另一实例抢先绑定端口,本进程 run_server
            // 绑定失败(os error 10048)但服务实际健康 → 复用已有实例照常显示窗口,不退出。
            if health_ok(&config).await {
                // 复用前校验数据目录:运行中的实例若指向另一套库(如浏览器版用的项目目录),
                // 静默复用会把桌面版挂到错误的库上——一边写的聊天另一边不可见。
                // 旧版服务无 data_dir 字段时无法校验,维持复用(竞态的另一方几乎必为桌面版自身)。
                // 该比较按 Windows 路径规则(大小写/分隔符)归一,故仅桌面启用;
                // Android 单应用单实例,复用者必为本进程自身,无需路径比对。
                #[cfg(desktop)]
                if let Some(dir) = health_data_dir(&config).await {
                    if !same_data_dir(&dir, &config.data_dir) {
                        return Err(format!(
                            "已在运行的 Kedai 实例数据目录与桌面版不一致:\n运行中实例: {dir}\n桌面版: {}\n请先关闭该实例再启动桌面版,否则聊天记录会写到另一套数据库",
                            config.data_dir.display()
                        ));
                    }
                }
                tracing::info!(bind_error = error.as_str(), "检测到已有 Kedai 实例,直接复用");
                eprintln!("[信息] 检测到已有 Kedai 实例,直接复用");
                show_main_window(&app, &service_url)?;
                clear_start_error(&log_dir);
                return Ok(());
            }
            return Err(format!("Kedai 后端启动失败: {error}"));
        }
        if health_ok(&config).await {
            show_main_window(&app, &service_url)?;
            // 启动成功:清掉历史失败留下的陈旧错误日志,避免误导排查
            clear_start_error(&log_dir);
            return Ok(());
        }
        tokio::time::sleep(READY_INTERVAL).await;
    }

    Err(format!(
        "Kedai 后端在 {} 秒内未就绪: {service_url}",
        READY_TIMEOUT.as_secs()
    ))
}

/// 导航到服务地址并展示/聚焦主窗口(启动成功与复用已有实例两条路径共用)。
fn show_main_window(app: &tauri::AppHandle, service_url: &str) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "找不到主窗口".to_string())?;
    window
        .navigate(url::Url::parse(service_url).map_err(|e| format!("服务地址非法: {e}"))?)
        .map_err(|e| format!("主窗口导航失败: {e}"))?;
    window.show().map_err(|e| format!("主窗口显示失败: {e}"))?;
    window
        .set_focus()
        .map_err(|e| format!("主窗口聚焦失败: {e}"))?;
    Ok(())
}

/// 删除陈旧的启动错误日志(仅在启动成功/复用成功后调用;失败路径由 write_start_error 重写)。
fn clear_start_error(log_dir: &Path) {
    let _ = std::fs::remove_file(log_dir.join("tauri-start-error.log"));
}

fn service_url(config: &kedai_server::config::AppConfig) -> String {
    let host = match config.host.as_str() {
        "0.0.0.0" | "::" => "127.0.0.1",
        host => host,
    };
    format!("http://{host}:{}/", config.port)
}

async fn health_ok(config: &kedai_server::config::AppConfig) -> bool {
    let url = format!("{}api/health", service_url(config));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(1200))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    match client.get(url).send().await {
        Ok(response) if response.status().is_success() => matches!(
            response.json::<serde_json::Value>().await,
            Ok(body) if body.get("ok").and_then(|value| value.as_bool()).unwrap_or(false)
        ),
        _ => false,
    }
}

/// 取运行中实例的数据目录(健康响应的 data_dir 字段;旧版服务无此字段返回 None)
/// 仅桌面使用:配合同实例数据目录一致性校验。
#[cfg(desktop)]
async fn health_data_dir(config: &kedai_server::config::AppConfig) -> Option<String> {
    let url = format!("{}api/health", service_url(config));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(1200))
        .build()
        .ok()?;
    let body: serde_json::Value = client.get(url).send().await.ok()?.json().await.ok()?;
    body.get("data_dir")?.as_str().map(|s| s.to_string())
}

/// Windows 路径宽松比较:忽略大小写与正反斜杠、尾部分隔符差异
#[cfg(desktop)]
fn same_data_dir(a: &str, b: &Path) -> bool {
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    norm(a) == norm(&b.to_string_lossy())
}

/// 以下便携版数据迁移一组函数仅桌面存在:
/// 依赖 exe 同级向上查找 data/ 与 web/ 的发行目录布局,APK 内无此结构。
/// Android 的数据目录由系统分配,首次安装即为空,无需迁移。
#[cfg(desktop)]
fn find_project_data_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut current = exe.parent()?.to_path_buf();
    for _ in 0..6 {
        let data = current.join("data");
        if data.is_dir() && (current.join("web").is_dir() || current.join("Kedai.exe").is_file()) {
            return Some(data);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// 仅当目标没有任何用户数据时执行复制。源目录始终保留,迁移可重复调用且不会覆盖目标。
#[cfg(desktop)]
fn migrate_data_if_needed(source: &Path, target: &Path) -> Result<usize, String> {
    if source == target || !has_migratable_data(source) || !target_is_pristine(target) {
        return Ok(0);
    }

    std::fs::create_dir_all(target).map_err(|e| format!("创建数据目录失败: {e}"))?;
    let copied = copy_tree(source, target).map_err(|e| format!("复制数据失败: {e}"))?;
    let source_db = source.join("kedai.db");
    if source_db.is_file() {
        kedai_server::migration::snapshot_database(&source_db, &target.join("kedai.db"))?;
    }
    rewrite_avatar_paths(target, source)?;

    let marker = format!(
        "source={}\nmigrated_at={}\nfiles={}\n",
        source.display(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or(0),
        copied
    );
    std::fs::write(target.join(".migration-v1"), marker)
        .map_err(|e| format!("写入迁移标记失败: {e}"))?;
    Ok(copied)
}

#[cfg(desktop)]
fn has_migratable_data(path: &Path) -> bool {
    path.join("kedai.db").is_file()
        || path.join("settings.json").is_file()
        || path.join("characters").is_dir()
        || path.join("avatars").is_dir()
}

#[cfg(desktop)]
fn target_is_pristine(path: &Path) -> bool {
    if !path.exists() {
        return true;
    }
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        if entry.file_name() == "builtin-system.json" {
            continue;
        }
        return false;
    }
    true
}

#[cfg(desktop)]
fn copy_tree(source: &Path, target: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(target)?;
    let mut copied = 0;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if name_text == "kedai.db"
            || name_text.ends_with("-wal")
            || name_text.ends_with("-shm")
            || name_text.ends_with(".log")
            || name_text.contains(".bak-")
            || name_text.starts_with(".verify")
        {
            continue;
        }
        let destination = target.join(name);
        if entry.path().is_dir() {
            copied += copy_tree(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), destination)?;
            copied += 1;
        }
    }
    Ok(copied)
}

#[cfg(desktop)]
fn rewrite_avatar_paths(target: &Path, old_data_dir: &Path) -> Result<(), String> {
    let database = target.join("kedai.db");
    if !database.is_file() {
        return Ok(());
    }
    let connection = rusqlite::Connection::open(&database)
        .map_err(|e| format!("打开迁移后的数据库失败: {e}"))?;
    let has_characters = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='characters')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|e| format!("检查角色表失败: {e}"))?;
    if !has_characters {
        return Ok(());
    }
    let old_prefix = old_data_dir.to_string_lossy();
    let new_prefix = target.to_string_lossy();
    connection
        .execute(
            "UPDATE characters SET avatar_path = replace(avatar_path, ?1, ?2) WHERE avatar_path LIKE ?3",
            rusqlite::params![old_prefix.as_ref(), new_prefix.as_ref(), format!("{}%", old_prefix)],
        )
        .map_err(|e| format!("修正头像路径失败: {e}"))?;
    Ok(())
}

fn write_start_error(log_dir: &Path, message: &str) {
    let _ = std::fs::create_dir_all(log_dir);
    let _ = std::fs::write(
        log_dir.join("tauri-start-error.log"),
        format!("{message}\n"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(desktop)]
    fn temp_dir(tag: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("kedai-desktop-{tag}-{stamp}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn migration_copies_once_and_keeps_source() {
        let source = temp_dir("source");
        let target = temp_dir("target");
        std::fs::write(source.join("settings.json"), "{}").unwrap();
        std::fs::create_dir_all(source.join("characters")).unwrap();
        std::fs::write(source.join("characters/card.json"), "{}").unwrap();
        let source_db = source.join("kedai.db");
        let connection = rusqlite::Connection::open(&source_db).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE snapshot_test(value TEXT); INSERT INTO snapshot_test VALUES('WAL 中的数据');")
            .unwrap();

        assert_eq!(migrate_data_if_needed(&source, &target).unwrap(), 2);
        assert!(source.join("settings.json").is_file(), "迁移不得删除源数据");
        assert!(target.join("settings.json").is_file());
        assert!(target.join("characters/card.json").is_file());
        let migrated = rusqlite::Connection::open(target.join("kedai.db")).unwrap();
        assert_eq!(
            migrated
                .query_row("SELECT COUNT(*) FROM snapshot_test", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1,
            "迁移必须通过 backup API 包含尚在 WAL 中的数据"
        );
        assert!(target.join(".migration-v1").is_file());

        std::fs::write(source.join("new.json"), "new").unwrap();
        assert_eq!(migrate_data_if_needed(&source, &target).unwrap(), 0);
        assert!(
            !target.join("new.json").exists(),
            "重复运行不得覆盖或追加目标数据"
        );

        std::fs::remove_dir_all(source).ok();
        std::fs::remove_dir_all(target).ok();
    }

    #[test]
    fn migration_never_overwrites_existing_target_data() {
        let source = temp_dir("used-source");
        let target = temp_dir("used-target");
        std::fs::write(source.join("settings.json"), "source").unwrap();
        std::fs::write(target.join("settings.json"), "target").unwrap();

        assert_eq!(migrate_data_if_needed(&source, &target).unwrap(), 0);
        assert_eq!(
            std::fs::read_to_string(target.join("settings.json")).unwrap(),
            "target"
        );

        std::fs::remove_dir_all(source).ok();
        std::fs::remove_dir_all(target).ok();
    }

    #[test]
    fn service_url_uses_loopback_for_unspecified_bind_host() {
        let mut config = kedai_server::config::AppConfig::from_env();
        config.host = "0.0.0.0".into();
        config.port = 4321;
        assert_eq!(service_url(&config), "http://127.0.0.1:4321/");
    }

    /// 二次关闭兜底(批次 5):关闭确认改前端弹窗后,前端不响应时窗口也必须关得掉。
    /// 首次关闭交给前端弹确认框;第二次直接强退;用户取消后复位,回到确认流程。
    #[cfg(desktop)]
    #[test]
    fn close_request_guard_forces_exit_only_on_second_request() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let flag = AtomicBool::new(false);
        assert!(
            !register_close_request(&flag),
            "首次关闭应交给前端弹确认框,不得直接退出"
        );
        assert!(
            register_close_request(&flag),
            "前端未响应时,第二次关闭必须强制退出(否则窗口关不掉)"
        );
        // 用户点了取消:复位后应恢复「先弹确认框」的行为,
        // 不得因上一轮的置位标记把下一次关闭直接放行
        reset_close_request(&flag);
        assert!(
            !register_close_request(&flag),
            "取消退出后应回到确认流程,而不是下一次点击就退出"
        );
        assert!(flag.load(Ordering::SeqCst), "复位后应被本次请求重新置位");
    }
}
