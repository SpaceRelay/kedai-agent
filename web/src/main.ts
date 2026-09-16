// 前端入口
import { createApp } from 'vue';
import { createPinia } from 'pinia';
import App from './App.vue';
import { installGlobalErrorHandlers } from './globalErrorHandlers';
import { useUiPrefsStore } from './stores/uiPrefs';
// 展示字体(拉丁字标/数字用,本地打包离线可用)
import '@fontsource/archivo-black';
import './style.css';

const app = createApp(App);
const pinia = createPinia();
app.use(pinia);

// 全局错误兜底(计划批次 5.1):必须在 mount 之前注册,否则首屏渲染期的异常会漏掉。
// 经 useUiPrefsStore(pinia) 显式取 store——main.ts 里还没有活跃的 pinia 实例上下文,
// 不能直接调 useUiPrefsStore()。异常只写内存态横幅,不落库、不含聊天正文。
const uiPrefs = useUiPrefsStore(pinia);
installGlobalErrorHandlers({
  app,
  onError: (message) => {
    uiPrefs.globalError = message;
  },
});

app.mount('#app');
