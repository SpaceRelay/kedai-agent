import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';
import tailwindcss from '@tailwindcss/vite';

// 前端开发服务器:API 请求代理到 Rust 后端(端口 3001 为硬契约)
export default defineConfig({
  plugins: [vue(), tailwindcss()],
  server: {
    port: 5173,
    proxy: {
      '/api': { target: 'http://127.0.0.1:3001', changeOrigin: true }
    }
  },
  build: {
    outDir: 'dist',
    // 生产构建禁 sourcemap:防前端源码反编译(§8.3.2-T3);Vite 默认 false,此处显式固化防回归。
    sourcemap: false,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes('node_modules')) {
            if (id.includes('markdown-it') || id.includes('sanitize-html')) return 'content-rendering';
            if (id.includes('vue') || id.includes('pinia')) return 'vue-vendor';
            return 'vendor';
          }
          // 应用代码分组(配合 App.vue defineAsyncComponent 懒加载):大弹窗各自/分组拆 chunk,
          // 首屏 index.js 不再内联弹窗实现。仅归组「仅被懒加载入口引用」的组件文件,
          // 避免把首屏共享模块误划入异步 chunk 造成反向耦合。
          if (id.includes('/components/SettingsHub') || id.includes('/components/SettingsModal')) return 'modal-settings';
          if (id.includes('/components/PromptManager')) return 'modal-prompt';
          if (id.includes('/components/ContractsModal')) return 'modal-contracts';
          if (id.includes('/components/DevToolsModal')) return 'modal-devtools';
          if (id.includes('/components/WorldBooksModal')) return 'modal-worldbooks';
          return undefined;
        }
      }
    }
  }
});
