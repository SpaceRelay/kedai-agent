package com.kedai.app

import android.content.pm.PackageManager
import androidx.annotation.Keep
import java.io.File
import java.util.concurrent.TimeUnit

/**
 * 命令执行桥(阶段 D):供 Rust 侧经 JNI 调用。
 *
 * 与 [KedaiNative] / [KeystoreBridge] 同一模式:Rust 只做字符串进出,Kotlin 侧封装
 * Android 进程与权限 API。三档执行器按 ROOT → Shizuku → Sandbox 逐级探测:
 *   - ROOT:`su -c <cmd>`(兼容 Magisk / KernelSU);
 *   - Shizuku:以 ADB shell(UID 2000)权限执行,无需 root;
 *   - Sandbox:应用自身 UID 的 `sh -c`,能力等于本进程。
 *
 * 入参/出参编码(与 server-rs/src/services/exec/android.rs 严格对齐,US 分隔符):
 *   exec:                command ␟ cwd ␟ timeoutMs   →  exitCode ␟ stdout ␟ stderr
 *   detectTier:          ""                          →  root|shizuku|sandbox|disabled
 *   requestShizukuPermission: ""                     →  ""(成功)或抛异常
 *
 * ⚠️ 混淆约束:本类被 native 按名调用(FindClass / CallStaticMethod)。release 的 R8
 * 若重命名会导致运行时失败(KeystoreBridge 已有实测踩坑)。双保险:`@Keep` 注解 +
 * app/proguard-rules.pro 里 `-keep class com.kedai.app.ShellExecutorBridge { *; }`。
 *
 * ⚠️ Shizuku 依赖是编译期 optional:用反射访问,未装 Shizuku 或依赖缺失时静默降级,
 * 不因类加载失败而崩溃(NoClassDefFoundError 在 Dalvik 上是 Error,不 catch 会崩进程)。
 */
@Keep
object ShellExecutorBridge {
    private const val SEP = '\u001f'

    /** 执行器等级常量(与 Rust 侧 ShellTier 对齐) */
    private const val TIER_ROOT = "root"
    private const val TIER_SHIZUKU = "shizuku"
    private const val TIER_SANDBOX = "sandbox"
    private const val TIER_DISABLED = "disabled"

    /** Shizuku 包名(用于探测是否安装) */
    private const val SHIZUKU_PACKAGE = "moe.shizuku.privileged.api"

    /** 最近一次探测结果缓存(避免每次 exec 都跑一次 su 探测) */
    @Volatile
    private var cachedTier: String? = null

    // ==================== 探测 ====================

    /**
     * 探测当前可用等级。顺序 ROOT → Shizuku → Sandbox。
     * 结果缓存;权限状态变化(用户新装/授权 Shizuku)后由前端点「刷新」调 refreshTier。
     */
    @JvmStatic
    @Keep
    fun detectTier(): String {
        cachedTier?.let { return it }
        val t = probeTier()
        cachedTier = t
        return t
    }

    /** 强制重新探测(前端「刷新等级」按钮用) */
    @JvmStatic
    @Keep
    fun refreshTier(): String {
        cachedTier = null
        return detectTier()
    }

    private fun probeTier(): String {
        if (canUseRoot()) return TIER_ROOT
        if (isShizukuAvailable()) return TIER_SHIZUKU
        // Android 必带 /system/bin/sh,沙箱档恒可用
        return TIER_SANDBOX
    }

    /** 探测 su 是否可用:短超时执行 `su -c id` 并看返回码。 */
    private fun canUseRoot(): Boolean {
        return try {
            val p = ProcessBuilder("su", "-c", "id")
                .redirectErrorStream(true)
                .start()
            val ok = p.waitFor(3, TimeUnit.SECONDS)
            if (!ok) {
                p.destroyForcibly()
                return false
            }
            p.exitValue() == 0
        } catch (_: Exception) {
            false
        }
    }

    // ==================== Shizuku(反射,可选依赖) ====================

    /** Shizuku 是否已安装且服务可用(binder 已启动)。 */
    private fun isShizukuAvailable(): Boolean {
        if (!isShizukuInstalled()) return false
        return try {
            val cls = Class.forName("rikka.shizuku.Shizuku")
            val binderReceived = cls.getMethod("pingBinder").invoke(null) as? Boolean ?: false
            if (!binderReceived) return false
            val version = cls.getMethod("getVersion").invoke(null) as? Int ?: 0
            version > 0
        } catch (_: Throwable) {
            // NoClassDefFoundError 也是 Throwable:依赖缺失时静默降级
            false
        }
    }

    private fun isShizukuInstalled(): Boolean {
        val ctx = KedaiNative.appContextOrNull() ?: return false
        return try {
            ctx.packageManager.getPackageInfo(SHIZUKU_PACKAGE, 0)
            true
        } catch (_: PackageManager.NameNotFoundException) {
            false
        } catch (_: Exception) {
            false
        }
    }

    /**
     * 请求 Shizuku 权限(首次弹系统授权框)。需在 Activity 上下文可用时调用。
     * 未安装 Shizuku / 服务未运行 → 抛带说明的异常,由 Rust 侧转成前端可读提示。
     */
    @JvmStatic
    @Keep
    fun requestShizukuPermission(): String {
        if (!isShizukuInstalled()) {
            throw IllegalStateException("未安装 Shizuku。请先安装 Shizuku 应用并通过 ADB 启动其服务后重试。")
        }
        return try {
            val cls = Class.forName("rikka.shizuku.Shizuku")
            val has = cls.getMethod("checkSelfPermission").invoke(null) as? Boolean ?: false
            if (has) return ""
            // Shizuku 的 requestPermission(code, listener) 不需要 Context 参数,
            // 但系统弹窗要求当前有 Activity 在前台(否则用户看不到授权框)。
            // 这里只做可用性检查,弹窗时机由前端在设置页(Activity 可见时)触发。
            if (KedaiNative.appContextOrNull() == null) {
                throw IllegalStateException("应用上下文未就绪,请在设置页重试")
            }
            // 用动态代理实现监听接口(避免编译期依赖 Shizuku 类型)
            val listenerCls = Class.forName("rikka.shizuku.Shizuku\$OnRequestPermissionResultListener")
            val listenerProxy = java.lang.reflect.Proxy.newProxyInstance(
                listenerCls.classLoader,
                arrayOf(listenerCls),
            ) { _, method, _ ->
                // onRequestPermissionResult(requestCode, grantResult):授权后清缓存,
                // 下次 detectTier 重新探测(具体结果以 checkSelfPermission 复核为准)
                if (method.name == "onRequestPermissionResult") {
                    cachedTier = null
                }
                null
            }
            val requestMethod = cls.getMethod(
                "requestPermission",
                Int::class.javaPrimitiveType,
                listenerCls,
            )
            requestMethod.invoke(null, 1, listenerProxy)
            ""
        } catch (e: Exception) {
            throw IllegalStateException("请求 Shizuku 授权失败:${e.message ?: e.javaClass.simpleName}")
        }
    }

    // ==================== 执行 ====================

    /**
     * 执行命令。入参 `command␟cwd␟timeoutMs`,返回 `exitCode␟stdout␟stderr`。
     *
     * 说明:授权与风险确认已在 Rust 侧(bash 工具 + 权限矩阵)完成,本函数只负责执行。
     * 超时到达即强杀进程,避免命令挂死拖垮应用。
     */
    @JvmStatic
    @Keep
    fun exec(payload: String): String {
        val parts = payload.split(SEP)
        val command = parts.getOrNull(0) ?: ""
        if (command.isBlank()) throw IllegalArgumentException("命令为空")
        val cwd = parts.getOrNull(1)?.takeIf { it.isNotBlank() }
        val timeoutMs = parts.getOrNull(2)?.toLongOrNull() ?: 60_000L

        val tier = detectTier()
        if (tier == TIER_DISABLED) {
            throw IllegalStateException("执行器不可用")
        }

        val dir = cwd?.let { File(it) }?.takeIf { it.isDirectory }

        // 三档各自返回已启动的 Process(Shizuku 走 binder,不经 ProcessBuilder)
        val proc: Process = when (tier) {
            TIER_ROOT -> startProcessBuilder(arrayOf("su", "-c", command), dir)
            TIER_SHIZUKU -> try {
                newShizukuProcess(command, dir)
            } catch (_: Throwable) {
                // Shizuku 中途失效(服务被杀 / 依赖缺失):回退沙箱档,
                // 不让整条命令失败(NoClassDefFoundError 也是 Throwable)
                startProcessBuilder(arrayOf("sh", "-c", command), dir)
            }
            else -> startProcessBuilder(arrayOf("sh", "-c", command), dir)
        }

        // 非交互:关掉 stdin,避免命令等待输入挂死
        try {
            proc.outputStream.close()
        } catch (_: Exception) {
        }

        val finished = proc.waitFor(timeoutMs, TimeUnit.MILLISECONDS)
        if (!finished) {
            proc.destroyForcibly()
            throw IllegalStateException(
                "命令超时(${timeoutMs}ms)已终止。建议:拆小步骤或调高 timeout_ms。"
            )
        }

        val stdout = proc.inputStream.bufferedReader().use { it.readText() }
        val stderr = proc.errorStream.bufferedReader().use { it.readText() }
        // 格式:exitCode␟stdout␟stderr(US 分隔)
        return "${proc.exitValue()}$SEP$stdout$SEP$stderr"
    }

    /** 用 ProcessBuilder 启动并返回已开始的 Process(stdout/stderr 分管道)。 */
    private fun startProcessBuilder(cmd: Array<String>, dir: File?): Process {
        val pb = ProcessBuilder(*cmd)
        pb.redirectErrorStream(false)
        dir?.let { pb.directory(it) }
        return pb.start()
    }

    /**
     * 经 Shizuku 以 ADB shell 权限启动进程(反射,避免编译期硬依赖)。
     * Shizuku.newProcess(String[] cmd, String[] env, String dir) 返回 Process。
     * 注意:Shizuku 的 env 数组首个元素须为 PATH 或 "--",否则参数会被拒。
     */
    private fun newShizukuProcess(command: String, dir: File?): Process {
        val cls = Class.forName("rikka.shizuku.Shizuku")
        val method = cls.getMethod(
            "newProcess",
            Array<String>::class.java,
            Array<String>::class.java,
            String::class.java,
        )
        val cmd = arrayOf("sh", "-c", command)
        val env = arrayOf("PATH=/sbin:/system/sbin:/system/bin:/system/xbin")
        return method.invoke(null, cmd, env, dir?.absolutePath) as Process
    }
}
