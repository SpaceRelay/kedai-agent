package com.kedai.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat

/**
 * 长任务前台服务:防止 Agent/任务模式的长时间生成在应用切后台或锁屏后被系统冻结/回收。
 *
 * Android 对后台进程有 Doze 与后台限制,不可见的应用进程可能被冻结甚至杀死,导致
 * SSE 长连接断开、任务停在中间态。前台服务(带常驻通知)把进程提升到可见优先级,
 * 是移动端跑长任务的唯一合规手段。
 *
 * 类型用 `dataSync`(targetSdk 34+ 需声明 FOREGROUND_SERVICE_DATA_SYNC 权限,
 * 且必须在清单里写 foregroundServiceType)—— 语义上正是「后台同步/网络数据」。
 *
 * 生命周期由前端驱动:生成/任务开始 → KedaiNative.startKeepAlive();
 * 结束 → stopKeepAlive()。重复调用幂等(startForegroundService 对已运行服务安全,
 * stopService 对未运行服务安全)。
 */
class KeepAliveService : Service() {
    companion object {
        private const val CHANNEL_ID = "kedai.keepalive"
        private const val NOTIFICATION_ID = 0x4B44 // "KD"
        private const val ACTION_START = "com.kedai.app.KEEPALIVE_START"
        private const val ACTION_STOP = "com.kedai.app.KEEPALIVE_STOP"

        /** 启动前台服务(幂等) */
        fun start(context: Context) {
            val intent = Intent(context, KeepAliveService::class.java).setAction(ACTION_START)
            // Android 8+ 用 startForegroundService,服务须在 5 秒内 startForeground(本实现立即调用)
            ContextCompat.startForegroundService(context, intent)
        }

        /** 停止前台服务(幂等) */
        fun stop(context: Context) {
            context.stopService(Intent(context, KeepAliveService::class.java))
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        ensureChannel()
        startForeground(NOTIFICATION_ID, buildNotification())
        // 不被系统自动重启:保活服务只服务于当前生成,进程被杀后由前端下次生成再拉起
        return START_NOT_STICKY
    }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val manager = getSystemService(NotificationManager::class.java) ?: return
        if (manager.getNotificationChannel(CHANNEL_ID) != null) return
        val channel = NotificationChannel(
            CHANNEL_ID,
            "后台任务",
            NotificationManager.IMPORTANCE_LOW,
        ).apply {
            description = "Kedai 生成/任务在后台继续运行时显示"
            setShowBadge(false)
        }
        manager.createNotificationChannel(channel)
    }

    private fun buildNotification(): Notification {
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.mipmap.ic_launcher)
            .setContentTitle("Kedai 正在生成")
            .setContentText("切到后台也会继续,完成后自动停止")
            .setOngoing(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setCategory(NotificationCompat.CATEGORY_PROGRESS)
            .build()
    }
}
