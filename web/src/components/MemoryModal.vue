<script setup lang="ts">
// 记忆库弹窗(升级工作流 C 入口优化):把原本埋在「优化面板 → 记忆库」的跨会话记忆面板
// 提到侧边栏一级入口。内容与优化面板内的 MemoryPanel 完全一致(同一组件、同一数据源
// /api/memory),此处只负责弹窗外壳 + 按当前角色/会话传参。
// 无角色时面板自身显示「请先选择角色」提示,不在此处重复判断。
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';
import MemoryPanel from './MemoryPanel.vue';

const store = useAppStore();
// 记忆按角色维度共享(跨会话),会话 id 仅用于「蒸馏当前会话」
const { currentCharacterId, currentSessionId } = storeToRefs(store);

function close(): void {
  store.memoryOpen = false;
}
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal memory-modal">
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" /> 记忆库
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>
      <div class="sv-modal-body">
        <MemoryPanel :character-id="currentCharacterId" :session-id="currentSessionId" />
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 宽度容纳搜索框 + 行内编辑 + 操作按钮;高度留出长列表滚动空间 */
.memory-modal {
  width: min(720px, calc(100vw - 48px));
  max-height: min(86vh, 720px);
}
</style>
