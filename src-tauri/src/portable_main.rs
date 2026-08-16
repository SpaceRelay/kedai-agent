// Windows 便携版独立产物入口,运行时与标准桌面二进制完全一致。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kedai_desktop_lib::run()
}
