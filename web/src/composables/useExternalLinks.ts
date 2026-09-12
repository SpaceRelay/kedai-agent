// 消息正文/资源卡外链的打开方式修正。
//
// 问题:render.ts 给所有正文链接与资源卡外链加了 target="_blank"(render.ts:218/608),
// 但 Android WebView 未处理新窗口请求(Tauri 生成层没有 onCreateWindow),_blank 常被
// 当作当前 WebView 导航 —— 页面来自 loopback 的 SPA 被替换后,返回键无处可退直接退出应用。
// 桌面 WebView2 同样会「在应用内开新窗口」,行为不一致。
//
// 做法:捕获阶段委托监听 document 上所有 a[href] 的 http(s) 链接:
//   - Android Tauri → emit 'kedai://open-external',由 Kotlin 侧 Intent.ACTION_VIEW 交给系统浏览器;
//   - 桌面 Tauri   → 同一事件,由 Rust 侧用系统 opener(cmd start / open / xdg-open)打开;
//   - 浏览器       → 不拦截,保持原生行为(用户可在浏览器里正常使用)。
//
// 事件而非自定义命令:remote origin 下 Tauri IPC 会对 #[tauri::command] 做 ACL 校验并拒绝,
// 而 core:event:allow-emit 已在 core:default 中(与 App.vue 的返回键退出同一模式)。
import { onMounted, onUnmounted } from 'vue';
import { inTauri, isAndroidTauri } from '../platform';

const OPEN_EXTERNAL_EVENT = 'kedai://open-external';

/** 该 URL 是否应交给系统浏览器(只处理 http/https,忽略 hash/mailto 等) */
function isExternalHttp(url: string): boolean {
  try {
    const parsed = new URL(url, window.location.href);
    return parsed.protocol === 'http:' || parsed.protocol === 'https:';
  } catch {
    return false;
  }
}

/** 排除应用自身同源地址(loopback SPA),避免把内部导航也甩给浏览器 */
function isSameOrigin(url: string): boolean {
  try {
    return new URL(url, window.location.href).origin === window.location.origin;
  } catch {
    return false;
  }
}

let installed = false;

function onDocumentClick(ev: MouseEvent): void {
  if (!inTauri) return;
  const target = ev.target as Element | null;
  const anchor = target?.closest?.('a[href]') as HTMLAnchorElement | null;
  if (!anchor) return;
  const href = anchor.getAttribute('href');
  if (!href || !isExternalHttp(href) || isSameOrigin(href)) return;

  ev.preventDefault();
  void emitOpenExternal(new URL(href, window.location.href).toString());
}

/** 请求原生侧用系统浏览器打开外链(Android Intent / 桌面 opener) */
export async function emitOpenExternal(url: string): Promise<void> {
  if (!inTauri) {
    window.open(url, '_blank', 'noopener,noreferrer');
    return;
  }
  try {
    const { emit } = await import('@tauri-apps/api/event');
    await emit(OPEN_EXTERNAL_EVENT, { url });
  } catch (e) {
    console.warn('[kedai] 交给系统浏览器打开失败,回退 window.open', e);
    window.open(url, '_blank', 'noopener,noreferrer');
  }
}

/** 挂载外链委托(在 App.vue setup 中调用一次) */
export function useExternalLinks(): void {
  onMounted(() => {
    if (installed) return;
    document.addEventListener('click', onDocumentClick, true);
    installed = true;
    // Android 原生壳已消费系统栏 insets,Web 侧归零 safe-area 兜底避免双重留白
    if (isAndroidTauri) document.documentElement.classList.add('kd-native-insets');
  });
  onUnmounted(() => {
    if (!installed) return;
    document.removeEventListener('click', onDocumentClick, true);
    installed = false;
  });
}
