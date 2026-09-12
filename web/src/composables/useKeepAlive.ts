// 长任务前台服务保活接线(仅 Android)。
//
// 问题:Android 对不可见的后台进程有冻结/回收策略,Agent/任务模式的长时间生成在
// 用户切后台或锁屏后可能被切断(SSE 断开、任务停在中间态)。前台服务是唯一合规解法。
//
// 做法:监听「是否有进行中的长任务」这一合成条件(generating 或任务 running/planning),
// 状态翻转时才 emit 一次 kedai://keepalive-start / stop,避免每帧抖动。
// 非 Android 环境完全不注册监听,零开销。
import { onUnmounted, watch } from 'vue';
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';
import { isAndroidTauri } from '../platform';

const START_EVENT = 'kedai://keepalive-start';
const STOP_EVENT = 'kedai://keepalive-stop';

/** 任务是否处于执行中(与 AgentPanel 的 taskActive 判定一致) */
function isTaskActive(status: string | undefined): boolean {
  return status === 'planning' || status === 'running';
}

export function useKeepAlive(): void {
  if (!isAndroidTauri) return;

  const store = useAppStore();
  const { generating, currentTask } = storeToRefs(store);
  let active = false;

  async function emit(name: string): Promise<void> {
    try {
      const { emit: tauriEmit } = await import('@tauri-apps/api/event');
      await tauriEmit(name);
    } catch (e) {
      console.warn('[kedai] 前台服务保活事件发送失败', e);
    }
  }

  // setup 期创建:自动绑定组件作用域,随组件卸载一并停止
  watch(
    [generating, () => currentTask.value?.task.status],
    ([isGenerating, status]) => {
      const shouldBeActive = isGenerating || isTaskActive(status);
      if (shouldBeActive === active) return;
      active = shouldBeActive;
      void emit(active ? START_EVENT : STOP_EVENT);
    },
    { immediate: true },
  );

  onUnmounted(() => {
    // 应用关闭/整页刷新时确保服务停止,避免残留常驻通知
    if (active) void emit(STOP_EVENT);
  });
}
