<script setup lang="ts">
// 设置区:后端连接(连接器信息 + 测试连接 + 模型计数)。
// 与 ApiSettingsSection 共享壳传入的同一份 useApiSettings 状态。
import type { useApiSettings } from '../../composables/useApiSettings';

const props = withDefaults(defineProps<{
  /** useApiSettings 的返回对象(壳共享实例) */
  state: ReturnType<typeof useApiSettings>;
  /** 是否显示(embedded 模式按 activeSection 切换;standalone 恒 true) */
  show?: boolean;
}>(), {
  show: true,
});

const {
  connInfo, models, connecting, connFeedback, modelFeedback, doConnect,
} = props.state;
</script>

<template>
  <div v-show="props.show" class="sv-field">
    <div class="sv-field-label"><span class="sv-supreme orange" /> 后端连接</div>
    <div class="sv-stack">
      <div class="sv-inp-row">
        <span class="sv-inp-tag">CONN</span>
        <code class="sv-code">{{ connInfo?.connector ?? '...' }}</code>
      </div>
      <button class="sv-btn ghost sv-btn-fill" :disabled="connecting" @click="doConnect">
        {{ connecting ? '连接测试中...' : '测试连接' }}
      </button>
      <div v-if="models.length" class="sv-count">可用 {{ models.length }} 个模型</div>
    </div>
    <div v-if="modelFeedback" class="sv-feedback info">{{ modelFeedback }}</div>
    <div v-if="connFeedback" class="sv-feedback" :class="connFeedback.kind">{{ connFeedback.text }}</div>
  </div>
</template>
