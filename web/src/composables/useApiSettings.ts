// API 连接设置:Base URL / Key / 模型 的编辑、保存与连接测试。
// 从 SettingsModal.vue 拆分:逻辑集中于 composable,组件模板零改动(同名 ref 解构可用)。
import { ref } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import * as api from '../api';

/** 规范化 OpenAI 兼容 API 地址(与后端 normalize_base_url 规则一致):
 * 无协议补协议(本机补 http,其余补 https)、无路径补 /v1。 */
export function normalizeUrl(input: string): string {
  let s = input.trim();
  if (!s) return s;
  while (s.endsWith('/')) s = s.slice(0, -1);
  if (!/^https?:\/\//i.test(s)) {
    s = /^(localhost|127\.|0\.0\.0\.0|\[::1\])/i.test(s) ? `http://${s}` : `https://${s}`;
  }
  const afterScheme = s.replace(/^https?:\/\//i, '');
  const slashIdx = afterScheme.indexOf('/');
  const hasPath = slashIdx !== -1 && afterScheme.slice(slashIdx).replace(/\//g, '').length > 0;
  if (!hasPath) s += '/v1';
  return s;
}

/** 模型列表加载失败的友好提示(区分服务版本过旧 / 网络 / 接口不支持) */
export function modelLoadHint(err: Error): string {
  const msg = err.message || '';
  if (/Not Found|404|Failed to fetch|网络|加载失败/.test(msg)) {
    return '无法加载模型列表:可能服务端版本过旧或未保存 API 设置。请先「保存 API 设置」;若仍失败,请重新构建并重启 kedai 服务。';
  }
  return `加载模型列表失败:${msg}`;
}

export function useApiSettings() {
  const store = useAppStore();
  const { model } = storeToRefs(store);

  // ===== API 设置(前端直接编辑;保存到服务端) =====
  const apiBaseUrl = ref('');
  const apiKey = ref('');
  const apiKeyMasked = ref('');
  const hasApiKey = ref(false);
  const savingApi = ref(false);
  const apiFeedback = ref<{ kind: 'ok' | 'err' | 'info'; text: string } | null>(null);

  // ===== 连接 =====
  const connInfo = ref<{ connector: string; model: string } | null>(null);
  const models = ref<string[]>([]);
  const connecting = ref(false);
  const connFeedback = ref<{ kind: 'ok' | 'err' | 'info'; text: string } | null>(null);
  const switchingModel = ref(false);
  const modelFeedback = ref('');
  const refreshing = ref(false);

  /** 回填服务端运行期设置(API 连接 + Key 脱敏);组件挂载时调用 */
  async function loadApiSettings(): Promise<void> {
    try {
      const data = await api.settingsInfo();
      connInfo.value = { connector: data.connector, model: data.model };
      if (data.models?.length) models.value = data.models;
      if (!model.value) model.value = data.model;
    } catch {
      /* 忽略 */
    }
    try {
      // 按当前顶层模式回填:任务模式读 task 覆盖层,否则读 roleplay(扁平字段)。
      const s = await api.getSettings(store.appMode);
      apiBaseUrl.value = s.openai_base_url;
      apiKeyMasked.value = s.api_key_masked;
      hasApiKey.value = s.has_api_key;
      apiKey.value = '';
      if (!model.value) model.value = s.model;
    } catch {
      /* 忽略 */
    }
  }

  /** Base URL 失焦时自动补全格式 */
  function onBaseUrlBlur(): void {
    const normalized = normalizeUrl(apiBaseUrl.value);
    if (normalized && normalized !== apiBaseUrl.value) {
      apiBaseUrl.value = normalized;
      apiFeedback.value = { kind: 'info', text: '已自动补全 API 地址(协议与 /v1)' };
    }
  }

  async function doConnect(): Promise<void> {
    connecting.value = true;
    connFeedback.value = null;
    try {
      const res = await api.testConnect();
      models.value = res.models;
      connFeedback.value = { kind: res.ok ? 'ok' : 'err', text: res.message };
    } catch (e) {
      connFeedback.value = { kind: 'err', text: `连接失败:${(e as Error).message}` };
    } finally {
      connecting.value = false;
    }
  }

  /** 保存 API 设置:Base URL / Key / 模型(立即重建连接器,并顺带刷新模型列表) */
  async function saveApi(): Promise<void> {
    if (savingApi.value) return;
    savingApi.value = true;
    apiFeedback.value = null;
    try {
      const patch: api.RuntimeSettingsPatch = {
        openai_base_url: normalizeUrl(apiBaseUrl.value),
        model: model.value || undefined,
      };
      if (apiKey.value.trim()) patch.openai_api_key = apiKey.value.trim();
      await store.saveSettings(patch);
      apiKey.value = '';
      apiFeedback.value = { kind: 'ok', text: 'API 设置已保存并生效' };
      // 保存后立即向新 API 拉取模型列表,避免模型下拉仍是旧的
      try {
        const res = await api.refreshModels();
        models.value = res.models;
        if (res.message) {
          apiFeedback.value = { kind: 'info', text: res.message };
        }
      } catch (err) {
        connFeedback.value = { kind: 'info', text: modelLoadHint(err as Error) };
      }
      try {
        const data = await api.settingsInfo();
        connInfo.value = { connector: data.connector, model: data.model };
      } catch {
        /* 忽略 */
      }
    } catch (e) {
      apiFeedback.value = { kind: 'err', text: `保存失败:${(e as Error).message}` };
    } finally {
      savingApi.value = false;
    }
  }

  /** 从已保存的 API 请求模型列表(不保存,立即生效) */
  async function refreshModelList(): Promise<void> {
    if (refreshing.value) return;
    refreshing.value = true;
    apiFeedback.value = null;
    try {
      const res = await api.refreshModels();
      models.value = res.models;
      if (res.message) {
        apiFeedback.value = { kind: 'info', text: res.message };
      } else {
        apiFeedback.value = { kind: 'ok', text: `已加载 ${models.value.length} 个模型` };
      }
    } catch (e) {
      apiFeedback.value = { kind: 'err', text: modelLoadHint(e as Error) };
    } finally {
      refreshing.value = false;
    }
  }

  /** 运行时切换模型(立即生效) */
  async function onModelChange(e: Event): Promise<void> {
    const value = (e.target as HTMLSelectElement).value;
    if (!value || value === model.value) return;
    switchingModel.value = true;
    modelFeedback.value = '';
    try {
      await store.switchModel(value);
      modelFeedback.value = `已切换到 ${value}`;
    } catch (err) {
      modelFeedback.value = (err as Error).message;
    } finally {
      switchingModel.value = false;
    }
  }

  return {
    apiBaseUrl, apiKey, apiKeyMasked, hasApiKey, savingApi, apiFeedback,
    connInfo, models, connecting, connFeedback, switchingModel, modelFeedback, refreshing,
    loadApiSettings, onBaseUrlBlur, doConnect, saveApi, refreshModelList, onModelChange,
  };
}
