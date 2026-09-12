// 弹窗懒加载包装(2026-09 修复「点了没反应」类失效):
// defineAsyncComponent 的 chunk 加载失败(旧构建缓存/网络抖动)默认静默——面板不出现、
// 界面无任何反馈。这里统一:失败自动重试一次;仍失败则写 uiPrefs.modalLoadError,
// 由 App.vue 全局错误条展示并提供手动重试(重开面板触发重新加载 chunk)。
import { defineAsyncComponent, type AsyncComponentLoader, type Component } from 'vue';
import { useUiPrefsStore } from './stores/uiPrefs';

/**
 * 懒加载包装(泛型保留组件 props 类型,调用方模板传参仍受 vue-tsc 检查)。
 * loader:动态 import;name:面板/分区名(错误条展示);flag:uiPrefs 中重试用的开关 ref 名。
 */
export function lazyModal<T extends Component>(loader: AsyncComponentLoader<T>, name: string, flag: string): T {
  return defineAsyncComponent({
    loader,
    onError(error, retry, fail, attempts) {
      if (attempts <= 1) {
        retry();
        return;
      }
      console.error(`[kedai] 面板「${name}」加载失败`, error);
      // 运行时解析 uiPrefs,避免与 store 初始化环
      useUiPrefsStore().modalLoadError = { name, flag };
      fail();
    },
  }) as T;
}
