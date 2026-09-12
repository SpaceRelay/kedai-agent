<script setup lang="ts">
// 设置区:预设导入 / 导出(SillyTavern 预设 ↔ 注入配置)。
// 原仅存在于 embedded 分支(standalone 缺失,属模板漂移);合并后两种模式均展示。
// 与 PromptInjectSection 共享壳传入的同一份 usePromptInject 状态;
// 文件选择输入用本组件本地 ref(原实现与注入区共享 injectImportInput,存在重复 ref 覆盖的隐患)。
import { ref } from 'vue';
import type { usePromptInject } from '../../composables/usePromptInject';

const props = withDefaults(defineProps<{
  /** usePromptInject 的返回对象(壳共享实例) */
  state: ReturnType<typeof usePromptInject>;
  /** 是否显示(embedded 模式按 activeSection 切换;standalone 恒 true) */
  show?: boolean;
}>(), {
  show: true,
});

const { injectDraft, injectImporting, injectMsg, onImportPreset, exportInjectConfig } = props.state;

/** 本区独立的文件选择输入(避免与提示词注入区的 injectImportInput 互相覆盖) */
const presetImportInput = ref<HTMLInputElement | null>(null);
</script>

<template>
  <div v-show="props.show" class="sv-field">
    <div class="sv-field-label"><span class="sv-supreme yellow" /> 预设导入 / 导出</div>
    <div class="sv-stack">
      <div class="sv-data-row">
        <div class="info">
          <b>导入酒馆预设</b>
          <span>导入 SillyTavern 预设 JSON(顶层 prompts + prompt_order),替换当前全部楼层并自动切换复杂模式</span>
        </div>
        <button class="sv-btn ghost" :disabled="injectImporting" @click="presetImportInput?.click()">
          {{ injectImporting ? '导入中...' : '导入' }}
        </button>
      </div>
      <div class="sv-data-row">
        <div class="info">
          <b>导出注入配置</b>
          <span>当前模式 + 简单项 + 全部楼层导出为 JSON,可在本机保存或共享</span>
        </div>
        <button class="sv-btn ghost" :disabled="!injectDraft" @click="exportInjectConfig">导出</button>
      </div>
      <input
        ref="presetImportInput"
        type="file"
        accept=".json,application/json"
        class="hidden"
        @change="onImportPreset"
      />
      <div v-if="injectMsg" class="sv-feedback" :class="injectMsg.startsWith('导入失败') ? 'err' : 'ok'">{{ injectMsg }}</div>
      <p class="sv-note">
        导入格式:SillyTavern 预设 JSON(含 <code>prompts</code> 数组与 <code>prompt_order</code>);
        导出的注入配置 JSON 亦可用「提示词注入 → 导入酒馆预设」回导。
      </p>
    </div>
  </div>
</template>
