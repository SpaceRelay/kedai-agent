//! 原生能力桥的前端事件入口。
//!
//! 前端通过 Tauri 事件(而非自定义命令,原因见 lib.rs 中 EXIT_APP_EVENT 的注释)
//! 请求原生能力,这里把事件分发到平台实现:
//!   - Android:经 JNI 调 Kotlin(server-rs/services/native_bridge_android.rs);
//!   - 桌面:外链用系统 opener 打开,分享/保活不适用(导出走保存对话框)。
//!
//! 事件名与前端 web/src/composables/useExternalLinks.ts、web/src/exportFile.ts 对齐。

use serde::Deserialize;

/// 用系统浏览器打开外链
pub const OPEN_EXTERNAL_EVENT: &str = "kedai://open-external";
/// 分享文本为文件(Android:写缓存 + 分享选单)
pub const SHARE_FILE_EVENT: &str = "kedai://share-file";
/// 启动长任务前台服务保活(仅 Android 有效)
pub const KEEPALIVE_START_EVENT: &str = "kedai://keepalive-start";
/// 停止长任务前台服务保活(仅 Android 有效)
pub const KEEPALIVE_STOP_EVENT: &str = "kedai://keepalive-stop";
/// 请求 Shizuku 权限(阶段 E;仅 Android 有效)。首次触发系统授权弹窗。
pub const SHIZUKU_REQUEST_EVENT: &str = "kedai://shizuku-request";

#[derive(Debug, Deserialize)]
pub struct OpenExternalPayload {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct ShareFilePayload {
    pub name: String,
    pub content: String,
}

/// 用系统浏览器打开外链。
///
/// 桌面侧顺带修好同一问题:此前后端正文明文链接在 WebView2 里也会「在应用内开窗」,
/// 行为与浏览器不一致;现在统一交给系统默认浏览器。
#[cfg(not(target_os = "android"))]
pub fn open_external(url: &str) -> Result<(), String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("拒绝打开非 http(s) 链接: {url}"));
    }
    let result = if cfg!(target_os = "windows") {
        // cmd /C start 的第一个参数在部分情况下会被当作窗口标题,故先给一个空标题占位
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    result
        .map(|_| ())
        .map_err(|e| format!("调用系统浏览器失败: {e}"))
}

#[cfg(target_os = "android")]
pub fn open_external(url: &str) -> Result<(), String> {
    kedai_server::services::native_bridge_android::open_external(url)
}

/// 分享文本为文件。桌面端不使用(导出走保存对话框),仅记录提示。
#[cfg(not(target_os = "android"))]
pub fn share_file(_name: &str, _content: &str) -> Result<(), String> {
    Err("分享导出仅在 Android 上可用(桌面端请使用保存对话框)".into())
}

#[cfg(target_os = "android")]
pub fn share_file(name: &str, content: &str) -> Result<(), String> {
    kedai_server::services::native_bridge_android::share_file(name, content)
}

/// 启动长任务保活前台服务(仅 Android 有意义;桌面端 no-op)
#[cfg(not(target_os = "android"))]
pub fn keepalive_start() -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "android")]
pub fn keepalive_start() -> Result<(), String> {
    kedai_server::services::native_bridge_android::keepalive_start()
}

/// 停止长任务保活前台服务(仅 Android 有意义;桌面端 no-op)
#[cfg(not(target_os = "android"))]
pub fn keepalive_stop() -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "android")]
pub fn keepalive_stop() -> Result<(), String> {
    kedai_server::services::native_bridge_android::keepalive_stop()
}

/// 请求 Shizuku 授权(仅 Android 有意义;桌面端明确报错,便于前端提示)。
#[cfg(not(target_os = "android"))]
pub fn request_shizuku_permission() -> Result<(), String> {
    Err("Shizuku 仅在 Android 上可用".into())
}

#[cfg(target_os = "android")]
pub fn request_shizuku_permission() -> Result<(), String> {
    kedai_server::services::exec::android::request_shizuku_permission()
}
