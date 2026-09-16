import { defineConfig } from 'vitest/config';
import vue from '@vitejs/plugin-vue';

export default defineConfig({
  // vue 插件:组件渲染测试(CacheHealthPanel.test.ts 经 vue/server-renderer 导入 .vue)所必需;
  // 对既有纯逻辑测试无影响。
  // transformAssetUrls 关闭:默认会把模板里的 <img src="/logo.png"> 转成资源 import,
  // 测试环境无 dev server/public 目录可解析(报 file:///logo.png 非合法路径);
  // 测试从不断言哈希后的资源 URL,保持字面量即可。
  plugins: [vue({ template: { transformAssetUrls: false } })],
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
    environmentOptions: {
      // 组件里出现公共资源(src="/logo.png" 等)时,jsdom 需要一个 http 基址才能解析;
      // 给一个不实际发起请求的基址(与上面 transformAssetUrls 双保险)。
      jsdom: { url: 'http://localhost/' },
    },
  },
});
