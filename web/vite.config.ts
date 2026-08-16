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
          if (!id.includes('node_modules')) return undefined;
          if (id.includes('markdown-it') || id.includes('sanitize-html')) return 'content-rendering';
          if (id.includes('vue') || id.includes('pinia')) return 'vue-vendor';
          return 'vendor';
        }
      }
    }
  }
});
