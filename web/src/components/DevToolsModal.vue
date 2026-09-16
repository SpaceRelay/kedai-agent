<script setup lang="ts">
// 事件监控面板(阶段六 6c):SSE 事件日志查看。
// 采集在 store.onSseEvent 一处埋点(100% 事件流经此处,含本地合成事件);
// 本面板仅消费:类型过滤 / 暂停-恢复实时采集 / 清空 / 当前会话过滤。
// 后端 recent API(环形缓冲 + send_event 全链路透传)留扩展位,见 plan6 6c 记录。
import { ref, computed } from 'vue';
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';
import {
  EVENT_TYPES,
  eventTypeLabel,
  filterEventLog,
  truncatePayload,
  type ApiEventLogEntry,
} from '../devTools';

const store = useAppStore();
const { eventLog, currentSessionId } = storeToRefs(store);

/** 类型过滤('all' = 全部) */
const typeFilter = ref('all');
/** 仅显示当前会话的事件 */
const sessionOnly = ref(true);
/** 暂停实时采集(展示冻结;store 采集不中断) */
const paused = ref(false);
/** 暂停时的展示快照 */
const frozen = ref<ApiEventLogEntry[]>([]);
/** 展开完整载荷的条目 ts(折叠状态下点击展开) */
const expandedTs = ref<number | null>(null);

const shown = computed<ApiEventLogEntry[]>(() => {
  const src = paused.value
    ? frozen.value
    : filterEventLog(
        eventLog.value,
        typeFilter.value,
        sessionOnly.value ? (currentSessionId.value ?? '') : '',
      );
  // 新增事件默认折叠(重置展开态,避免长载荷自动铺开)
  expandedTs.value = null;
  return src;
});

function togglePause(): void {
  paused.value = !paused.value;
  if (paused.value) {
    frozen.value = filterEventLog(
      eventLog.value,
      typeFilter.value,
      sessionOnly.value ? (currentSessionId.value ?? '') : '',
    );
  }
}

function clearLog(): void {
  store.clearEventLog();
  frozen.value = [];
  expandedTs.value = null;
}

function toggleExpand(ts: number): void {
  expandedTs.value = expandedTs.value === ts ? null : ts;
}

function payloadText(e: ApiEventLogEntry): string {
  return JSON.stringify(e.event);
}

function displayText(e: ApiEventLogEntry): string {
  const full = payloadText(e);
  return expandedTs.value === e.ts ? full : truncatePayload(full, 300);
}

function timeText(ts: number): string {
  const d = new Date(ts);
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  const ss = String(d.getSeconds()).padStart(2, '0');
  return `${hh}:${mm}:${ss}.${String(d.getMilliseconds()).padStart(3, '0')}`;
}

const close = (): void => {
  store.eventsOpen = false;
};
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal wb-modal">
      <!-- 头部 -->
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" /> 事件监控
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-modal-body">
        <!-- 说明 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme blue" /> 说明</div>
          <p class="sv-note" style="margin: 0; line-height: 1.8">
            SSE 事件日志:全部事件在 <code>store.onSseEvent</code> 一处采集(含本地合成事件,
            如手动中断;任务模式的创建/执行/状态变化由后端 <code>/api/tasks/events</code> 实时推送
            「任务」事件,<b>会话过滤下始终可见</b>),
            <b>cap 500 条丢最旧</b>。<br />
            <b>刷新页面后日志清空</b>(仅当前会话期内有效);后端 recent API 留扩展位。
            长载荷(完成正文/工具输出)默认折叠,点击行展开。
          </p>
        </div>

        <!-- 顶部控件 -->
        <div class="sv-field">
          <div class="sv-stack" style="flex-direction: row; gap: 8px; align-items: center; flex-wrap: wrap">
            <select v-model="typeFilter" class="sv-wb-select" style="width: 150px">
              <option value="all">全部类型</option>
              <option v-for="t in EVENT_TYPES" :key="t" :value="t">{{ eventTypeLabel(t) }}</option>
            </select>
            <label class="sv-note" style="white-space: nowrap; display: flex; align-items: center; gap: 4px">
              <input v-model="sessionOnly" type="checkbox" /> 仅当前会话
            </label>
            <button class="sv-btn ghost sv-btn-sm" @click="togglePause">
              {{ paused ? '▶ 恢复采集' : '❚❚ 暂停' }}
            </button>
            <button class="sv-btn danger sv-btn-sm" @click="clearLog">清空日志</button>
            <span class="sv-note" style="margin: 0">{{ shown.length }} 条(共 {{ eventLog.length }} 条)</span>
          </div>
        </div>

        <!-- 事件列表 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme yellow" /> 事件日志</div>
          <div v-if="shown.length === 0" class="sv-empty" style="padding: 28px 12px">
            <div class="sv-empty-geo mb10">
              <span class="sq black" />
              <span class="sq pink" />
              <span class="sq deep" />
            <i class="diag" />
            </div>
            <p style="font-size: 12px">暂无事件</p>
            <p style="font-size: 11px">发送消息后事件将在此实时出现</p>
          </div>
          <div v-else class="sv-datalist" style="max-height: 420px; overflow-y: auto">
            <div
              v-for="e in shown"
              :key="e.ts"
              class="sv-data-row"
              style="cursor: pointer"
              @click="toggleExpand(e.ts)"
            >
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-2">
                  <span class="sv-tag" style="white-space: nowrap">{{ timeText(e.ts) }}</span>
                  <b style="font-size: 12px">{{ eventTypeLabel(e.event.type) }}</b>
                  <span v-if="e.session_id" class="sv-script-meta" style="font-size: 11px">
                    会话 {{ e.session_id.slice(0, 8) }}
                  </span>
                  <span class="sv-script-meta" style="font-size: 11px; margin-left: auto">
                    {{ expandedTs === e.ts ? '收起' : '展开' }}
                  </span>
                </div>
                <pre
                  class="sv-code"
                  style="white-space: pre-wrap; font-size: 11px; margin: 4px 0 0; max-height: 200px; overflow: auto"
                >{{ displayText(e) }}</pre>
              </div>
            </div>
          </div>
        </div>
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>
