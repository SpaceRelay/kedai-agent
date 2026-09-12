<script setup lang="ts">
// 设置区:API 设置(Base URL / Key / 模型选择 + 从 API 加载模型列表)。
// 从 SettingsModal.vue 双模板合并而来:两分支内容一致,统一为 embedded 版本。
// 状态由壳(SettingsModal)创建一次后经 prop 传入,保证与 ConnectionSection 共享同一份连接状态。
import { onMounted } from 'vue';
import { useAppStore } from '../../store';
import { storeToRefs } from 'pinia';
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
  apiBaseUrl, apiKey, apiKeyMasked, hasApiKey, savingApi, apiFeedback,
  connInfo, models, switchingModel, refreshing,
  loadApiSettings, onBaseUrlBlur, saveApi, refreshModelList, onModelChange,
} = props.state;

// 当前模型(修复原模板引用了未定义变量 model 的隐患:原实现运行时回退 connInfo?.model)
const { model } = storeToRefs(useAppStore());

onMounted(() => {
  void loadApiSettings();
});
</script>

<template>
  <div v-show="props.show" class="sv-field">
    <div class="sv-field-label"><span class="sv-supreme red" /> API 设置</div>
    <div class="sv-stack">
      <div class="sv-inp-row">
        <label class="sv-inp-tag">BASE URL</label>
        <input
          v-model="apiBaseUrl"
          type="text"
          class="sv-input"
          placeholder="https://api.openai.com/v1"
          spellcheck="false"
          @blur="onBaseUrlBlur"
        />
      </div>
      <div class="sv-inp-row">
        <label class="sv-inp-tag">API KEY</label>
        <input
          v-model="apiKey"
          type="password"
          class="sv-input"
          :placeholder="hasApiKey ? `已配置 ${apiKeyMasked} · 留空保持现有` : '粘贴 API Key'"
          autocomplete="off"
          spellcheck="false"
        />
      </div>

      <!-- 模型选择 + 从 API 加载 -->
      <div class="sv-inp-row">
        <label class="sv-inp-tag">MODEL</label>
        <select
          class="sv-select"
          :value="model || connInfo?.model || ''"
          :disabled="switchingModel"
          @change="onModelChange"
        >
          <option value="" disabled>— 选择模型 —</option>
          <option v-for="m in [...new Set([model, ...models])].filter(Boolean)" :key="m" :value="m">
            {{ m }}
          </option>
        </select>
      </div>

      <div class="sv-btn-row">
        <button class="sv-btn primary sv-btn-fill" :disabled="savingApi" @click="saveApi">
          {{ savingApi ? '保存中...' : '保存 API 设置' }}
        </button>
        <button class="sv-btn ghost sv-btn-fill" :disabled="refreshing" @click="refreshModelList">
          {{ refreshing ? '加载中...' : '从 API 加载模型列表' }}
        </button>
      </div>
      <p class="sv-note">
        保存后立即生效并写入 data/settings.json;Key 仅存本地服务端,不回显明文。地址会自动补全
        协议与 /v1。
      </p>
    </div>
    <div v-if="apiFeedback" class="sv-feedback" :class="apiFeedback.kind">{{ apiFeedback.text }}</div>
  </div>
</template>
