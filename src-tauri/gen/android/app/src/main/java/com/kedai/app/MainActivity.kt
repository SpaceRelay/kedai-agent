package com.kedai.app

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.view.View
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat

/**
 * Kedai 主 Activity(修改自 Tauri 模板)。
 *
 * ## 为什么要在原生侧处理 insets
 *
 * targetSdk 36 起,Android 15+ 对应用**强制 edge-to-edge**:内容必然延伸到状态栏与
 * 导航栏之下,`fitsSystemWindows` / `windowOptOutEdgeToEdgeEnforcement` 都不再恢复旧行为。
 * 而 WebView 的 `env(safe-area-inset-*)` 只反映**刘海(display cutout)**,在无刘海设备上
 * 恒为 0 —— 于是顶栏被状态栏的时间/信号压住、底部导航被手势条盖住(真机确认)。
 *
 * 因此这里主动消费 insets:把 systemBars 与 IME(软键盘)的 insets 作为 padding
 * 施加到窗口内容根视图上。一次解决三个问题:
 *   1) 顶部让开状态栏(顶栏不再被压);
 *   2) 底部让开导航栏/手势条(底部导航完整可见);
 *   3) 软键盘弹出时用 IME 高度顶起布局,输入框不被遮挡(配合清单 adjustResize)。
 *
 * 前端侧的 `env(safe-area-inset-*)` 兜底仍保留,但原生已让位,故 Web 侧在 Android
 * Tauri 环境把 --safe-top/--safe-bottom 置 0 避免重复留白(见 App.vue / style.css)。
 * insets 在此被标记为 CONSUMED,不向下传递,避免 WebView 再算一遍。
 */
class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)

    // 注入应用上下文,供 KedaiNative(jni_bridge 反射调用)启动服务/发起 Intent
    KedaiNative.attach(this)

    // 长任务前台服务需要通知渠道可见;Android 13+ 需运行时 POST_NOTIFICATIONS。
    // 被拒绝也不影响功能(前台服务仍可运行,只是通知不可见),故仅在启动时静默申请一次。
    requestNotificationPermissionIfNeeded()

    // 显式声明由本 Activity 处理 insets 适配(Android 15+ 本就强制 edge-to-edge,
    // 这里统一为同一套逻辑,低版本也走一致路径)。
    WindowCompat.setDecorFitsSystemWindows(window, false)

    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      // 状态栏/导航栏(含手势条)
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars())
      // 刘海屏切口:无刘海时为 0;与状态栏取较大值避免重复叠加
      val cutout = insets.getInsets(WindowInsetsCompat.Type.displayCutout())
      // 软键盘:弹出时顶起底部,收起时回落为 0
      val ime = insets.getInsets(WindowInsetsCompat.Type.ime())

      view.setPadding(
        maxOf(bars.left, cutout.left),
        maxOf(bars.top, cutout.top),
        maxOf(bars.right, cutout.right),
        maxOf(bars.bottom, cutout.bottom, ime.bottom),
      )
      WindowInsetsCompat.CONSUMED
    }
    ViewCompat.requestApplyInsets(content)
  }

  /** Android 13+ 申请通知权限(前台服务通知可见所需);拒绝不影响保活功能本身 */
  private fun requestNotificationPermissionIfNeeded() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
    val granted = ContextCompat.checkSelfPermission(
      this,
      Manifest.permission.POST_NOTIFICATIONS,
    ) == PackageManager.PERMISSION_GRANTED
    if (!granted) {
      requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIFICATIONS)
    }
  }

  companion object {
    private const val REQ_NOTIFICATIONS = 0x4B01
  }
}
