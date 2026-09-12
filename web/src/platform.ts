// 运行平台判定:区分「浏览器 / 桌面 Tauri 壳 / Android Tauri 壳」。
//
// 为什么用 UA 判 Android 而不是插件:@tauri-apps/plugin-os 未安装(本机网络拉不到
// 新依赖),而 Android WebView 的 UA 必含 "Android",对本应用自身足够可靠。
// 桌面端 UA 不含 Android,判定为 false,行为与改动前完全一致。
//
// 用途:
//  - exportFile:Android 走「写缓存 + 分享 Intent」,桌面走保存对话框;
//  - 消息正文外链:Android/桌面都交给系统浏览器打开,避免 WebView 导航走 SPA;
//  - 长任务保活:仅 Android 需要前台服务;
//  - 原生已处理 insets,Web 侧据此归零 safe-area 兜底。

/** 是否运行在 Tauri 壳内(桌面或 Android) */
export const inTauri: boolean =
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

/** 是否 Android 平台(UA 判定;浏览器/桌面均为 false) */
export const isAndroid: boolean =
  typeof navigator !== 'undefined' && /Android/i.test(navigator.userAgent);

/** 是否 Android Tauri 壳(需要走原生桥的场景) */
export const isAndroidTauri: boolean = inTauri && isAndroid;
