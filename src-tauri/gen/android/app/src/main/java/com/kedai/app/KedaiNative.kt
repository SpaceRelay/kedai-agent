package com.kedai.app

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.annotation.Keep
import androidx.core.content.FileProvider
import java.io.File

/**
 * Kedai 原生能力桥接(供 Rust 侧经 JNI 调用)。
 *
 * 与 [KeystoreBridge] 同一模式:Rust 只做字符串进出,Kotlin 侧封装 Android API。
 * 四项能力:
 *   - openExternal(url):用系统浏览器打开外链。WebView 里 `target=_blank` 在
 *     Android 上没有新窗口处理,会被当作当前页导航,把 SPA 界面顶掉且返回键无处可退;
 *     改为交给系统浏览器,行为与桌面一致。
 *   - shareFile(payload):把导出内容写入缓存目录后经 FileProvider 弹系统分享选单。
 *     Android 的文件保存是 SAF(content:// URI)语义,插件 fs 的路径写入不适用;
 *     「写缓存 + 分享」是移动端导出 JSON 的标准做法。
 *   - startKeepAlive() / stopKeepAlive():启停前台服务,避免长任务在切后台/锁屏后
 *     被系统冻结或回收。
 *
 * 入参约定(与 server-rs/src/services/native_bridge_android.rs 对齐):
 *   - openExternal:入参即 URL;
 *   - shareFile:入参为 "文件名\u{1f}内容"(US 单元分隔符);
 *   - keepalive*:入参为空串。
 * 全部返回 "" 表成功;异常抛出由 Rust 侧转为错误。
 *
 * ⚠️ 混淆约束:本类被 native 按名调用(FindClass / CallStaticMethod),
 * release 的 R8 若重命名会导致运行时失败。双保险:`@Keep` 注解 +
 * app/proguard-rules.pro 里 `-keep class com.kedai.app.KedaiNative { *; }`。
 */
@Keep
object KedaiNative {
    private const val SEP = '\u001f'

    /** 应用上下文(在 MainActivity.onCreate 中注入) */
    @Volatile
    private var appContext: Context? = null

    /** 由 MainActivity 在启动时注入应用上下文(未注入时抛异常,由 Rust 侧转成可见错误) */
    fun attach(context: Context) {
        appContext = context.applicationContext
    }

    private fun requireContext(): Context =
        appContext ?: throw IllegalStateException("KedaiNative 未初始化:MainActivity 未注入 Context")

    /**
     * 取应用上下文(未注入时返回 null)。供 [ShellExecutorBridge] 等兄弟桥类做
     * 「可用性探测」——探测路径不应因上下文缺失而抛异常(抛了会让整段探测失败)。
     */
    @JvmStatic
    @Keep
    fun appContextOrNull(): Context? = appContext

    /** 用系统浏览器打开外链 */
    @JvmStatic
    @Keep
    fun openExternal(url: String): String {
        val trimmed = url.trim()
        if (trimmed.isEmpty()) throw IllegalArgumentException("外链为空")
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(trimmed)).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        requireContext().startActivity(intent)
        return ""
    }

    /** 分享文本为文件:写缓存目录 → FileProvider URI → 系统分享选单 */
    @JvmStatic
    @Keep
    fun shareFile(payload: String): String {
        val idx = payload.indexOf(SEP)
        val name = if (idx >= 0) payload.substring(0, idx) else payload
        val content = if (idx >= 0) payload.substring(idx + 1) else ""
        if (name.isBlank()) throw IllegalArgumentException("导出文件名为空")

        val context = requireContext()
        val dir = File(context.cacheDir, "exports").apply { mkdirs() }
        // 文件名去掉路径分隔符,避免目录穿越
        val safeName = name.replace('/', '_').replace('\\', '_')
        val file = File(dir, safeName)
        file.writeText(content, Charsets.UTF_8)

        val uri: Uri = FileProvider.getUriForFile(
            context,
            context.packageName + ".fileprovider",
            file,
        )
        val intent = Intent(Intent.ACTION_SEND).apply {
            type = "application/json"
            putExtra(Intent.EXTRA_STREAM, uri)
            putExtra(Intent.EXTRA_SUBJECT, safeName)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        val chooser = Intent.createChooser(intent, "导出到").apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        context.startActivity(chooser)
        return ""
    }

    /** 启动前台服务(长任务保活);重复调用幂等 */
    @JvmStatic
    @Keep
    fun startKeepAlive(): String {
        KeepAliveService.start(requireContext())
        return ""
    }

    /** 停止前台服务保活;重复调用幂等 */
    @JvmStatic
    @Keep
    fun stopKeepAlive(): String {
        KeepAliveService.stop(requireContext())
        return ""
    }
}
