<script setup lang="ts">
// 任务执行模式选择器(批次 4 六模式):Sidebar「下达目标」区使用。
// 独立成组件:Sidebar 引用的 /logo.png 静态资源在 SSR 冒烟测试(vitest)下无法解析,
// 抽出后选择器可单独 SSR 测试;选择持久化在 task store(taskRunMode,localStorage)。
// 选项文案统一取自 api/labels.ts(唯一源,穷尽校验),避免与 TaskBoard 两处手写漂移。
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';
import { MODE_OPTION_LABELS, MODE_ORDER } from '../api/labels';

const store = useAppStore();
const { taskRunMode } = storeToRefs(store);
</script>

<template>
  <select v-model="taskRunMode" class="sv-select" title="任务执行模式">
    <option v-for="m in MODE_ORDER" :key="m" :value="m">{{ MODE_OPTION_LABELS[m] }}</option>
  </select>
</template>
