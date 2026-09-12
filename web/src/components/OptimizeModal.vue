<script setup lang="ts">
// 优化面板(阶段六 6d):提示词检查 + 一键推荐设置 + 上下文健康度。
//  - 提示词检查:复用 useAgentPromptEditor 的 promptPreview/previewLoading/loadPromptPreview,
//    layers 渲染模板搬自 SettingsModal.vue:388-396;每层内容调 countTokens 显示 token 估算
//    (懒加载:预览加载后逐层计数;失败静默不阻塞展示)。
//  - 一键推荐设置:静态常量 RECOMMENDED_SETTINGS,仅勾选项经 store.queueSettingsSave 串行套用。
//  - 上下文健康度:缓存命中率(computeHitRate 与顶栏共用)+ 最近一次请求 token 用量。
import { ref, computed } from 'vue';
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';
import * as api from '../api';
import { useAgentPromptEditor } from '../composables/useAgentPromptEditor';
import { computeHitRate, computeCompactionNeed, RECOMMENDED_SETTINGS, buildRecommendedPatch, type RecommendedSetting } from '../contextStats';
import CacheHealthPanel from './CacheHealthPanel.vue';
import MemoryPanel from './MemoryPanel.vue';

const store = useAppStore();
const { lastUsage } = storeToRefs(store);
// 记忆库按当前角色/会话拉取(store 字段:currentCharacterId / currentSessionId)
const { currentCharacterId, currentSessionId } = storeToRefs(store);
const { promptPreview, previewLoading, previewError, loadPromptPreview } = useAgentPromptEditor();

/** 每层 token 估算(懒加载:预览就绪后逐层计数;失败保持 undefined 静默) */
const layerTokens = ref<Array<number | undefined>>([]);
const tokenCounting = ref(false);

async function refreshPreview(): Promise<void> {
  await loadPromptPreview();
  layerTokens.value = [];
  if (!promptPreview.value) return;
  tokenCounting.value = true;
  try {
    layerTokens.value = await Promise.all(
      promptPreview.value.layers.map(async (l) => {
        try {
          return await api.countTokens([{ role: l.role, content: l.content }]);
        } catch {
          return undefined; // 失败静默,不阻塞整体展示
        }
      }),
    );
  } finally {
    tokenCounting.value = false;
  }
}

// ===== 一键推荐设置 =====
const checked = ref<Record<string, boolean>>({});
for (const s of RECOMMENDED_SETTINGS) checked.value[s.key] = false;
const applying = ref(false);
const applyMsg = ref<{ kind: 'ok' | 'err'; text: string } | null>(null);

const selectedSettings = computed<RecommendedSetting[]>(() =>
  RECOMMENDED_SETTINGS.filter((s) => checked.value[s.key]),
);

async function applyRecommended(): Promise<void> {
  if (applying.value) return;
  const selected = selectedSettings.value;
  if (selected.length === 0) {
    applyMsg.value = { kind: 'ok', text: '未勾选任何推荐项' };
    return;
  }
  applying.value = true;
  applyMsg.value = null;
  try {
    await store.queueSettingsSave(buildRecommendedPatch(selected));
    applyMsg.value = { kind: 'ok', text: `已应用 ${selected.length} 项推荐设置(串行保存)` };
    setTimeout(() => (applyMsg.value = null), 3000);
  } catch (e) {
    applyMsg.value = { kind: 'err', text: `应用失败:${(e as Error).message}` };
  } finally {
    applying.value = false;
  }
}

// ===== 上下文健康度 =====
const hitRate = computed<number | null>(() => computeHitRate(lastUsage.value));
const usageRows = computed(() => {
  const u = lastUsage.value;
  if (!u) return [];
  return [
    { label: 'prompt token', value: u.prompt_tokens },
    { label: 'completion token', value: u.completion_tokens },
    { label: 'total token', value: u.total_tokens },
    { label: 'context token', value: u.context_tokens },
  ];
});

// ===== 上下文压缩(阶段借鉴 harness) =====
const compacting = ref(false);
const compactMsg = ref<{ kind: 'ok' | 'err'; text: string } | null>(null);
const compactionNeed = computed(() =>
  computeCompactionNeed(store.contextTokens, store.maxContextTokens, store.compactionThreshold),
);

async function compactNow(): Promise<void> {
  if (compacting.value) return;
  compacting.value = true;
  compactMsg.value = null;
  try {
    const res = await store.compactCurrentChat();
    compactMsg.value = res.compacted
      ? { kind: 'ok', text: '已压缩:较早对话已生成摘要并注入,原文仍保留可恢复' }
      : { kind: 'ok', text: '历史较短,暂无需要压缩的内容' };
    setTimeout(() => (compactMsg.value = null), 4000);
  } catch (e) {
    compactMsg.value = { kind: 'err', text: `压缩失败:${(e as Error).message}` };
  } finally {
    compacting.value = false;
  }
}

async function clearCompactionNow(): Promise<void> {
  if (compacting.value) return;
  compacting.value = true;
  compactMsg.value = null;
  try {
    const res = await store.clearCurrentCompaction();
    compactMsg.value = res.cleared
      ? { kind: 'ok', text: '已恢复完整原文历史(摘要已清除)' }
      : { kind: 'ok', text: '当前无压缩摘要,无需恢复' };
    setTimeout(() => (compactMsg.value = null), 4000);
  } catch (e) {
    compactMsg.value = { kind: 'err', text: `恢复失败:${(e as Error).message}` };
  } finally {
    compacting.value = false;
  }
}

const close = (): void => {
  store.optimizeOpen = false;
};
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal wb-modal">
      <!-- 头部 -->
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" /> 优化面板
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-modal-body">
        <!-- 提示词检查 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme blue" /> 提示词检查</div>
          <p class="sv-note" style="margin: 0 0 8px; line-height: 1.8">
            按 source / role / layer / order 展示最终提示词(历史已脱敏,不返回聊天正文或
            API Key);每层附 token 估算。
          </p>
          <button class="sv-btn ghost sv-btn-sm" :disabled="previewLoading || tokenCounting" @click="refreshPreview">
            {{ previewLoading ? '加载中…' : tokenCounting ? '估算中…' : '刷新最终提示词预览' }}
          </button>
          <div v-if="previewError" class="sv-feedback err" style="margin-top: 6px">{{ previewError }}</div>
          <div v-if="promptPreview" class="sv-stack" style="margin-top: 10px">
            <div v-for="(layer, i) in promptPreview.layers" :key="`${layer.order}-${layer.source}`" class="sv-data-row">
              <div class="info">
                <div class="flex items-center gap-2">
                  <b style="font-size: 12px">#{{ layer.order }} · L{{ layer.layer }} · {{ layer.role }} · {{ layer.source }}</b>
                  <span class="sv-script-meta" style="font-size: 11px">
                    {{ layerTokens[i] !== undefined ? `≈ ${layerTokens[i]} token` : '' }}
                  </span>
                </div>
                <pre class="sv-code" style="white-space: pre-wrap; max-height: 180px; overflow: auto">{{ layer.content }}</pre>
              </div>
            </div>
            <p class="sv-note">{{ promptPreview.note }}</p>
          </div>
        </div>

        <!-- 一键推荐设置 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme yellow" /> 一键推荐设置</div>
          <p class="sv-note" style="margin: 0 0 8px; line-height: 1.8">
            勾选后「一键套用」:仅应用勾选项(不自动改其它配置),走既有串行保存队列。
          </p>
          <div class="sv-stack" style="gap: 6px">
            <label
              v-for="s in RECOMMENDED_SETTINGS"
              :key="s.key"
              class="sv-data-row"
              style="display: flex; gap: 8px; align-items: flex-start; justify-content: flex-start; cursor: pointer"
            >
              <input v-model="checked[s.key]" type="checkbox" style="margin-top: 3px; flex-shrink: 0" />
              <div style="flex: 1; min-width: 0">
                <b style="font-size: 12px">{{ s.label }}</b>
                <p class="sv-note" style="margin: 2px 0 0">{{ s.desc }}</p>
              </div>
            </label>
          </div>
          <div class="sv-wb-edit-foot">
            <button class="sv-btn primary sv-btn-sm" :disabled="applying" @click="applyRecommended">
              {{ applying ? '应用成功…' : `一键套用(${selectedSettings.length} 项)` }}
            </button>
          </div>
          <div v-if="applyMsg" class="sv-feedback" :class="applyMsg.kind" style="margin-top: 6px">{{ applyMsg.text }}</div>
        </div>

        <!-- 上下文健康度 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme green" /> 上下文健康度</div>
          <p class="sv-note" style="margin: 0 0 8px; line-height: 1.8">
            缓存命中率 = 缓存命中 token ÷ prompt token(DeepSeek 等提供商);无缓存字段时不显示。
          </p>
          <div class="sv-stack" style="gap: 6px">
            <div class="sv-data-row" style="display: flex; gap: 12px; align-items: center">
              <b style="font-size: 12px; width: 120px">缓存命中率</b>
              <span v-if="hitRate !== null" class="sv-tag">{{ hitRate }}%</span>
              <span v-else class="sv-script-meta" style="font-size: 11px">暂无数据</span>
            </div>
            <div v-for="r in usageRows" :key="r.label" class="sv-data-row" style="display: flex; gap: 12px; align-items: center">
              <b style="font-size: 12px; width: 120px">{{ r.label }}</b>
              <span class="sv-script-meta" style="font-size: 12px">{{ r.value }}</span>
            </div>
            <div v-if="usageRows.length === 0" class="sv-empty" style="padding: 14px 0 4px">
              <div class="sv-empty-geo mb8">
                <span class="sq black" />
                <span class="sq pink" />
                <span class="sq deep" />
                <i class="diag" />
              </div>
              <p style="font-size: 12px">暂无用量数据</p>
              <p style="font-size: 11px">最近一次请求的 token 用量将在发送消息后显示</p>
            </div>
          </div>
        </div>

        <!-- 缓存健康(近 N 轮加权命中率 / 费用估算 / 四级水位,数据来自 /api/diagnostics/cache) -->
        <CacheHealthPanel />

        <!-- 记忆库(当前角色跨会话记忆:蒸馏 / 补录 / 注入开关,数据来自 /api/memory) -->
        <MemoryPanel :character-id="currentCharacterId" :session-id="currentSessionId" />

        <!-- 上下文压缩 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme purple" /> 上下文压缩</div>
          <p class="sv-note" style="margin: 0 0 8px; line-height: 1.8">
            把较早对话压成一条摘要注入给模型,原文不删除、可随时恢复。当前占比
            <b v-if="compactionNeed.ratio !== null" class="sv-tag">{{ Math.round(compactionNeed.ratio * 100) }}%</b>
            <span v-else>暂无数据</span>
            (模式 {{ store.compactionMode }},阈值 {{ Math.round(store.compactionThreshold * 100) }}%)。
          </p>
          <div class="sv-wb-edit-foot">
            <button class="sv-btn primary sv-btn-sm" :disabled="compacting" @click="compactNow">
              {{ compacting ? '压缩中…' : '立即压缩当前会话' }}
            </button>
            <button class="sv-btn ghost sv-btn-sm" :disabled="compacting" @click="clearCompactionNow">
              恢复完整历史
            </button>
          </div>
          <div v-if="compactMsg" class="sv-feedback" :class="compactMsg.kind" style="margin-top: 6px">{{ compactMsg.text }}</div>
        </div>
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>
