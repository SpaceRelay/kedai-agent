<script setup lang="ts">
// 启动动画:淡粉红至上主义几何方块 + Logo + 文字逐字淡入
import { onMounted, ref } from 'vue';
import { useAppStore } from '../store';

const store = useAppStore();
const fading = ref(false);

onMounted(() => {
  // 1.2s 后开始淡出,0.4s 淡出动画,总计 1.6s
  setTimeout(() => {
    fading.value = true;
    setTimeout(() => {
      store.splashDone = true;
    }, 400);
  }, 1200);
});

const letters = 'KEDAI'.split('');
</script>

<template>
  <div class="sv-splash" :class="{ 'fade-out': fading }">
    <!-- 几何装饰 -->
    <div class="sv-splash-geo pink f1" />
    <div class="sv-splash-geo deep f2" />
    <div class="sv-splash-geo soft f3" />

    <div class="sv-splash-content">
      <img class="sv-splash-logo" src="/logo.png" alt="Kedai" draggable="false" />
      <h1 class="sv-splash-title">
        <span
          v-for="(letter, i) in letters"
          :key="i"
          :style="{ animationDelay: `${0.3 + i * 0.08}s` }"
        >{{ letter }}</span>
      </h1>
    </div>
  </div>
</template>
