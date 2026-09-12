import java.io.File
import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.logging.LogLevel
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.TaskAction

/**
 * Kedai 修改版(Gradle 侧 Rust 构建任务)。
 *
 * 原始 Tauri 模板把可执行文件设为 `...\node` 并把 `"tauri"` 当作第一个参数传给 node;
 * 但 node.exe 只会把第一个参数当**脚本文件**解析,不存在名为 `tauri` 的脚本,
 * 于是报 "A problem occurred starting process 'command '...\node.bat''" 而构建中断。
 *
 * 这里改为:node.exe + `node_modules/@tauri-apps/cli/tauri.js` 的绝对路径,
 * 并把子命令参数直接跟在脚本之后(不再传多余的 "tauri")。
 * 路径由 rootDirRel 相对推导,仓库换位置也可用。
 */
open class BuildTask : DefaultTask() {
    @Input
    var rootDirRel: String? = null
    @Input
    var target: String? = null
    @Input
    var release: Boolean? = null

    @TaskAction
    fun assemble() {
        runTauriCli(resolveNodeExecutable())
    }

    /** 依次尝试常见 node 安装位置;都找不到时回退到 PATH 上的 node。 */
    private fun resolveNodeExecutable(): String {
        val candidates = listOf(
            "C:\\Program Files\\nodejs\\node.exe",
            "C:\\Program Files (x86)\\nodejs\\node.exe",
            System.getenv("NODE_EXE") ?: "",
        )
        for (candidate in candidates) {
            if (candidate.isNotBlank() && File(candidate).isFile) return candidate
        }
        return "node"
    }

    fun runTauriCli(executable: String) {
        val rootDirRel = rootDirRel ?: throw GradleException("rootDirRel cannot be null")
        val target = target ?: throw GradleException("target cannot be null")
        val release = release ?: throw GradleException("release cannot be null")

        // 工作目录 = rootDirRel 指向的目录(即 src-tauri,tauri.conf.json 所在处)。
        val workDir = File(project.projectDir, rootDirRel).canonicalFile
        // node_modules 可能位于仓库根(workspaces 布局),从工作目录逐级向上查找,
        // 避免硬编码层级(gen/android/app → android → gen → src-tauri → 仓库根)。
        val tauriJs = findUpwards(workDir, "node_modules/@tauri-apps/cli/tauri.js")
            ?: throw GradleException(
                "找不到 Tauri CLI 入口(已从 ${workDir.absolutePath} 逐级向上查找 " +
                    "node_modules/@tauri-apps/cli/tauri.js);请先在仓库根执行 npm install"
            )

        val args = mutableListOf(tauriJs.absolutePath, "android", "android-studio-script")
        if (project.logger.isEnabled(LogLevel.DEBUG)) {
            args.add("-vv")
        } else if (project.logger.isEnabled(LogLevel.INFO)) {
            args.add("-v")
        }
        if (release) {
            args.add("--release")
        }
        args.add("--target")
        args.add(target)

        logger.info("运行 Tauri Android 构建: $executable ${args.joinToString(" ")}")
        project.exec {
            workingDir(workDir)
            executable(executable)
            args(args)
        }.assertNormalExitValue()
    }

    /** 从 start 起逐级向上查找相对路径 rel,找到返回文件,否则 null。 */
    private fun findUpwards(start: File, rel: String): File? {
        var dir: File? = start
        while (dir != null) {
            val candidate = File(dir, rel)
            if (candidate.isFile) return candidate
            dir = dir.parentFile
        }
        return null
    }
}
