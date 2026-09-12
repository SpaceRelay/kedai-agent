// 模型与后端连接 store:连接状态、当前模型、模型列表、连接测试与模型切换。
// 从 store.ts 按领域拆分;被 genSettings(loadSettings/saveSettings 回填 model)在动作运行时引用,
// 本 store 不反向引用其他 store,保持单向依赖。
import { defineStore } from 'pinia';
import { ref } from 'vue';
import * as api from '../api';

export const useModelConnStore = defineStore('app.modelConn', () => {
  // ===== 状态 =====
  const connStatus = ref<'ok' | 'fail' | 'unknown'>('unknown');
  const connMessage = ref('');
  const model = ref('');
  const models = ref<string[]>([]);

  // ===== 动作 =====
  async function testConnection(): Promise<void> {
    try {
      const res = await api.testConnect();
      connStatus.value = res.ok ? 'ok' : 'fail';
      connMessage.value = res.message;
      // 连接测试返回的模型列表顺带填充 models(输入栏模型下拉数据源)
      if (res.models?.length) models.value = res.models;
    } catch (e) {
      connStatus.value = 'fail';
      connMessage.value = (e as Error).message;
    }
  }

  async function loadModel(): Promise<void> {
    try {
      model.value = await api.getModel();
    } catch {
      /* 忽略 */
    }
  }

  async function loadModels(): Promise<void> {
    try {
      models.value = await api.listModels();
    } catch {
      /* 忽略 */
    }
  }

  async function switchModel(m: string): Promise<boolean> {
    const res = await api.switchModel(m);
    model.value = res.model;
    return res.changed;
  }

  return {
    connStatus,
    connMessage,
    model,
    models,
    testConnection,
    loadModel,
    loadModels,
    switchModel,
  };
});
