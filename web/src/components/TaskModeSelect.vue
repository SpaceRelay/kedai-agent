<script setup lang="ts">
// 任务执行模式选择器(批次 4 六模式):Sidebar「下达目标」区使用。
// 独立成组件:Sidebar 引用的 /logo.png 静态资源在 SSR 冒烟测试(vitest)下无法解析,
// 抽出后选择器可单独 SSR 测试;选择持久化在 task store(taskRunMode,localStorage)。
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';

const store = useAppStore();
const { taskRunMode } = storeToRefs(store);
</script>

<template>
  <select v-model="taskRunMode" class="sv-select" title="任务执行模式">
    <option value="legacy">模式:三段式(默认)</option>
    <option value="solo">模式:单 Agent</option>
    <option value="multi">模式:多 Agent</option>
    <option value="plan">模式:先规划后批准</option>
    <option value="team">模式:团队协作</option>
    <option value="custom">模式:自定义流程</option>
  </select>
</template>
