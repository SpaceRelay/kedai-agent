// 向量化模型设置(Phase 3):Base URL / Key / 模型 / 维度 的编辑、保存、连接测试与索引重建。
// 与聊天 API 设置(useApiSettings)完全解耦:embedding 可指向不同厂商,凭据独立存储;
// 后端对 embedding_api_key 走与聊天 Key 同一套 DPAPI 加密 + 后 4 位脱敏回显。
import { ref } from 'vue';
import { useAppStore } from '../store';
import * as api from '../api';
import { normalizeUrl } from './useApiSettings';

export function useEmbeddingSettings() {
  const store = useAppStore();

  const enabled = ref(false);
  const baseUrl = ref('');
  const apiKey = ref('');
  const apiKeyMasked = ref('');
  const hasApiKey = ref(false);
  const model = ref('');
  const dim = ref(0);

  const loading = ref(false);
  const saving = ref(false);
  const testing = ref(false);
  const rebuilding = ref(false);
  /** 已嵌入 / 总条数(索引重建进度) */
  const status = ref<api.EmbeddingStatus | null>(null);
  const feedback = ref<{ kind: 'ok' | 'err' | 'info'; text: string } | null>(null);

  /** 回填服务端配置(组件挂载时调用) */
  async function load(): Promise<void> {
    loading.value = true;
    try {
      const s = await api.getSettings(store.appMode);
      enabled.value = s.embedding_enabled;
      baseUrl.value = s.embedding_base_url;
      apiKeyMasked.value = s.embedding_api_key_masked;
      hasApiKey.value = s.has_embedding_api_key;
      model.value = s.embedding_model;
      dim.value = s.embedding_dim;
      apiKey.value = '';
    } catch {
      /* 忽略:加载失败保持空表单,用户仍可填写后保存 */
    } finally {
      loading.value = false;
    }
    await refreshStatus();
  }

  /** 拉取向量索引状态(总数/已嵌入/维度漂移) */
  async function refreshStatus(): Promise<void> {
    try {
      status.value = await api.getEmbeddingStatus();
    } catch {
      status.value = null;
    }
  }

  /** Base URL 失焦自动补全(复用聊天侧的同一规则) */
  function onBaseUrlBlur(): void {
    const normalized = normalizeUrl(baseUrl.value);
    if (normalized && normalized !== baseUrl.value) {
      baseUrl.value = normalized;
      feedback.value = { kind: 'info', text: '已自动补全地址(协议与 /v1)' };
    }
  }

  /** 从聊天 API 配置一键填入(地址与 Key 复用;模型仍需用户填 embedding 专用模型) */
  function fillFromChat(): void {
    const chat = store as unknown as { openai_base_url?: string };
    // 聊天 Base URL 由后端 settings 提供,这里走一次读取避免 store 字段依赖
    void (async () => {
      try {
        const s = await api.getSettings(store.appMode);
        baseUrl.value = s.openai_base_url || chat.openai_base_url || '';
        feedback.value = {
          kind: 'info',
          text: '已填入聊天 API 地址。若向量化服务与聊天同源,API Key 请重新填写(出于安全不回显明文)。',
        };
      } catch (e) {
        feedback.value = { kind: 'err', text: `读取聊天配置失败:${(e as Error).message}` };
      }
    })();
  }

  /** 保存配置 */
  async function save(): Promise<void> {
    if (saving.value) return;
    saving.value = true;
    feedback.value = null;
    try {
      const patch: api.RuntimeSettingsPatch = {
        embedding_enabled: enabled.value,
        embedding_base_url: baseUrl.value ? normalizeUrl(baseUrl.value) : '',
        embedding_model: model.value.trim(),
        embedding_dim: dim.value,
      };
      // 留空 = 保持现有 Key 不变更(与聊天 API 设置同语义)
      if (apiKey.value.trim()) patch.embedding_api_key = apiKey.value.trim();
      await store.saveSettings(patch);
      apiKey.value = '';
      feedback.value = { kind: 'ok', text: '向量化设置已保存' };
      await load();
    } catch (e) {
      feedback.value = { kind: 'err', text: `保存失败:${(e as Error).message}` };
    } finally {
      saving.value = false;
    }
  }

  /** 测试连接:嵌入一条固定文本,回传实际维度并回填 */
  async function test(): Promise<void> {
    if (testing.value) return;
    testing.value = true;
    feedback.value = null;
    try {
      const res = await api.testEmbedding();
      if (res.ok) {
        if (res.dim) dim.value = res.dim;
        feedback.value = { kind: 'ok', text: res.message };
      } else {
        feedback.value = { kind: 'err', text: res.message };
      }
    } catch (e) {
      feedback.value = { kind: 'err', text: `测试失败:${(e as Error).message}` };
    } finally {
      testing.value = false;
    }
  }

  /** 手动重建向量索引(清表 + 全量回填);完成后刷新状态 */
  async function rebuild(): Promise<void> {
    if (rebuilding.value) return;
    rebuilding.value = true;
    feedback.value = null;
    try {
      const res = await api.rebuildEmbeddings();
      feedback.value = {
        kind: 'ok',
        text: `重建完成:已为 ${res.embedded} 条记忆生成向量(维度 ${res.dim})`,
      };
    } catch (e) {
      feedback.value = { kind: 'err', text: `重建失败:${(e as Error).message}` };
    } finally {
      rebuilding.value = false;
      await refreshStatus();
    }
  }

  return {
    enabled, baseUrl, apiKey, apiKeyMasked, hasApiKey, model, dim,
    loading, saving, testing, rebuilding, status, feedback,
    load, refreshStatus, onBaseUrlBlur, fillFromChat, save, test, rebuild,
  };
}
