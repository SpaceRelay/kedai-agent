import { defineConfig } from 'vitest/config';
import vue from '@vitejs/plugin-vue';

export default defineConfig({
  // vue 插件:组件渲染测试(CacheHealthPanel.test.ts 经 vue/server-renderer 导入 .vue)所必需;
  // 对既有纯逻辑测试无影响。
  plugins: [vue()],
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
});
