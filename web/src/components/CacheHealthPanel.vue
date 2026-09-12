<script setup lang="ts">
// 缓存健康分区(优化面板):近 N 轮 token 加权缓存命中率 + 趋势条形 + 费用/节省估算
// + 四级水位横条(soft/snip/compact/force)+ 可折叠明细表。
// 数据来自 GET /api/diagnostics/cache(api/diagnostics.ts);展示逻辑纯函数在 cacheHealth.ts。
// onServerPrefetch:服务端渲染测试通道(renderToString 会等待预取完成)。
import { computed, onMounted, onServerPrefetch, ref } from 'vue';
import * as api from '../api';
import {
  WATERMARK_STEPS,
  entryHitRate,
  formatCny,
  formatEntryTime,
  formatHitRate,
  trendBars,
  watermarkGapText,
  watermarkLitCount,
} from './cacheHealth';

const props = defineProps<{ sessionId?: string }>();

/** 统计窗口(近 N 轮请求),切换后立即重新请求 */
const WINDOW_OPTIONS = [20, 50, 100] as const;
const windowSize = ref<number>(20);

const loading = ref(false);
const error = ref('');
const data = ref<api.CacheDiagnostics | null>(null);

async function load(): Promise<void> {
  loading.value = true;
  error.value = '';
  try {
    data.value = await api.getCacheDiagnostics({ sessionId: props.sessionId, window: windowSize.value });
  } catch (e) {
    error.value = `加载失败:${(e as Error).message}`;
  } finally {
    loading.value = false;
  }
}

onMounted(load);
onServerPrefetch(load);

const hitRateText = computed(() => formatHitRate(data.value?.totals.hit_rate ?? null));
const bars = computed(() => trendBars(data.value?.entries ?? []));
const litCount = computed(() => watermarkLitCount(data.value?.watermark.level ?? 'unknown'));
const gapText = computed(() => (data.value ? watermarkGapText(data.value.watermark) : ''));
/** 水位占比展示(unknown 时 ratio 为 null 不渲染) */
const ratioText = computed(() => {
  const wm = data.value?.watermark;
  if (!wm || wm.ratio === null) return '';
  return `${Math.round(wm.ratio * 100)}%`;
});
/** 明细行(时间正序,最近几条) */
const entryRows = computed(() =>
  (data.value?.entries ?? []).map((e) => ({
    time: formatEntryTime(e.created_at),
    hit: e.hit,
    miss: e.miss,
    rate: entryHitRate(e.hit, e.miss),
  })),
);
</script>

<template>
  <div class="sv-field">
    <div class="sv-field-label"><span class="sv-supreme orange" /> 缓存健康</div>
    <p class="sv-note" style="margin: 0 0 8px; line-height: 1.8">
      近 N 轮请求的 token 加权缓存命中率、费用估算与上下文水位(DeepSeek 等提供商的
      prompt_cache 字段口径;命中率按 token 加权,非按请求条数)。
    </p>

    <!-- 窗口切换 + 刷新 -->
    <div style="display: flex; gap: 8px; align-items: center; margin-bottom: 8px">
      <select
        v-model.number="windowSize"
        class="sv-select"
        style="width: auto"
        title="统计窗口:近 N 轮请求(切换后重新请求)"
        @change="load"
      >
        <option v-for="w in WINDOW_OPTIONS" :key="w" :value="w">近 {{ w }} 轮</option>
      </select>
      <button class="sv-btn ghost sv-btn-sm" :disabled="loading" @click="load">
        {{ loading ? '加载中…' : '刷新' }}
      </button>
    </div>

    <div v-if="error" class="sv-feedback err">{{ error }}</div>

    <div v-if="data" class="sv-stack" style="gap: 6px">
      <!-- 加权命中率与 token 汇总 -->
      <div class="sv-data-row" style="display: flex; gap: 12px; align-items: center">
        <b style="font-size: 12px; width: 120px">加权命中率</b>
        <span v-if="data.totals.hit_rate !== null" class="sv-tag">{{ hitRateText }}</span>
        <span v-else class="sv-script-meta" style="font-size: 11px">{{ hitRateText }}</span>
      </div>
      <div class="sv-data-row" style="display: flex; gap: 12px; align-items: center">
        <b style="font-size: 12px; width: 120px">窗口 token</b>
        <span class="sv-script-meta" style="font-size: 12px">
          命中 {{ data.totals.total_hit }} · 未命中 {{ data.totals.total_miss }} · 输出 {{ data.totals.total_completion }}
        </span>
      </div>

      <!-- 趋势条形图:每条请求一根竖条,高度 = 单条命中率(无缓存数据灰色占位) -->
      <div v-if="bars.length" class="cache-trend" title="逐条命中率趋势(左旧右新)">
        <div
          v-for="(b, i) in bars"
          :key="i"
          class="cache-trend-bar"
          :class="{ empty: b.rate === null }"
          :style="{ height: `${Math.max(b.heightPct, 4)}%` }"
          :title="b.rate === null ? '无缓存数据' : `${formatHitRate(b.rate)}`"
        />
      </div>

      <!-- 费用估算(后端已按 0.27/2/8 元每百万算好) -->
      <div class="sv-data-row" style="display: flex; gap: 12px; align-items: center">
        <b style="font-size: 12px; width: 120px">费用估算</b>
        <span class="sv-script-meta" style="font-size: 12px">
          窗口花费 {{ formatCny(data.cost) }} · 缓存节省 {{ formatCny(data.saved) }}
          (单价 {{ data.pricing.currency_hint }})
        </span>
      </div>

      <!-- 四级水位横条:soft 0.5 / snip 0.6 / compact 0.8 / force 0.9 -->
      <div class="cache-wm">
        <div class="cache-wm-track">
          <div
            v-for="(step, i) in WATERMARK_STEPS"
            :key="step"
            class="cache-wm-step"
            :class="[`wm-${step}`, { active: i < litCount }]"
          >
            {{ step }}
          </div>
        </div>
        <p class="sv-script-meta" style="font-size: 11px; margin: 4px 0 0">
          输入 {{ data.watermark.input_tokens }} / {{ data.watermark.max_context_tokens }} token
          <template v-if="ratioText">({{ ratioText }})</template>
          · {{ gapText }}
        </p>
      </div>

      <!-- 明细表(默认折叠) -->
      <details v-if="entryRows.length" class="cache-entries">
        <summary class="sv-script-meta" style="font-size: 11px; cursor: pointer">
          明细(最近 {{ entryRows.length }} 条,左旧右新)
        </summary>
        <div class="cache-entry-table">
          <div class="cache-entry-head">
            <span>时间</span>
            <span>命中</span>
            <span>未命中</span>
            <span>命中率</span>
          </div>
          <div v-for="(r, i) in entryRows" :key="i" class="cache-entry-row">
            <span>{{ r.time }}</span>
            <span>{{ r.hit }}</span>
            <span>{{ r.miss }}</span>
            <span>{{ r.rate === null ? '-' : formatHitRate(r.rate) }}</span>
          </div>
        </div>
      </details>
    </div>
    <p v-else-if="!error && !loading" class="sv-script-meta" style="font-size: 11px">
      发送消息后将展示缓存命中情况。
    </p>
  </div>
</template>

<style scoped>
/* 趋势条形图:容器定高,内部竖条按内联 height 百分比撑起(无新图表依赖) */
.cache-trend {
  display: flex;
  align-items: flex-end;
  gap: 2px;
  height: 48px;
  padding: 2px 4px;
  border: 1px solid var(--sv-line);
  overflow: hidden;
}
.cache-trend-bar {
  flex: 1 1 0;
  min-width: 3px;
  max-width: 14px;
  background: var(--sv-green);
}
.cache-trend-bar.empty {
  background: var(--sv-ink-faint);
}

/* 四级水位横条:soft/snip/compact/force 各占一段,达标段点亮为档位色 */
.cache-wm-track {
  display: flex;
  gap: 4px;
}
.cache-wm-step {
  flex: 1 1 0;
  text-align: center;
  font-size: 10px;
  padding: 3px 0;
  border: 1px solid var(--sv-line);
  color: var(--sv-ink-faint);
}
.cache-wm-step.wm-soft.active {
  background: var(--sv-green);
  border-color: var(--sv-green);
  color: var(--sv-white);
}
.cache-wm-step.wm-snip.active {
  background: var(--sv-yellow);
  border-color: var(--sv-yellow);
  color: var(--sv-white);
}
.cache-wm-step.wm-compact.active {
  background: var(--sv-orange);
  border-color: var(--sv-orange);
  color: var(--sv-white);
}
.cache-wm-step.wm-force.active {
  background: var(--sv-red);
  border-color: var(--sv-red);
  color: var(--sv-white);
}

/* 明细表:四列等宽栅格(时间/命中/未命中/命中率) */
.cache-entry-table {
  margin-top: 6px;
  font-size: 11px;
  font-family: var(--font-mono);
}
.cache-entry-head,
.cache-entry-row {
  display: grid;
  grid-template-columns: 1.6fr 1fr 1fr 1fr;
  gap: 4px;
  padding: 2px 4px;
}
.cache-entry-head {
  color: var(--sv-ink-faint);
  border-bottom: 1px solid var(--sv-line);
}
</style>
