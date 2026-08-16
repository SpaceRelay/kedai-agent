// 前端入口
import { createApp } from 'vue';
import { createPinia } from 'pinia';
import App from './App.vue';
// 展示字体(拉丁字标/数字用,本地打包离线可用)
import '@fontsource/archivo-black';
import './style.css';

createApp(App).use(createPinia()).mount('#app');
