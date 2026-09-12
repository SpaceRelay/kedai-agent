// 主 Agent 提示词(DATA_DIR/AGENTS_RUNTIME.md)编辑与最终提示词预览。
import { ref } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import * as api from '../api';

export function useAgentPromptEditor() {
  const store = useAppStore();
  const { currentSessionId, currentCharacterId, appMode } = storeToRefs(store);

  const agentPromptMd = ref('');
  const promptPreview = ref<api.PromptPreview | null>(null);
  const previewLoading = ref(false);
  const previewError = ref('');

  async function loadPromptPreview(): Promise<void> {
    if (previewLoading.value) return;
    previewLoading.value = true;
    previewError.value = '';
    try {
      promptPreview.value = await api.getPromptPreview(
        currentSessionId.value ?? undefined,
        currentCharacterId.value ?? undefined,
        appMode.value,
      );
    } catch (e) {
      previewError.value = `预览失败:${(e as Error).message}`;
    } finally {
      previewLoading.value = false;
    }
  }

  const mdLoading = ref(false);
  const mdSaving = ref(false);
  const mdMsg = ref<{ kind: 'ok' | 'err'; text: string } | null>(null);

  async function loadAgentPromptMd(): Promise<void> {
    if (mdLoading.value) return;
    mdLoading.value = true;
    try {
      const data = await api.getAgentPromptMd();
      agentPromptMd.value = data.content;
      mdMsg.value = null;
    } catch (e) {
      mdMsg.value = { kind: 'err', text: `读取失败:${(e as Error).message}` };
    } finally {
      mdLoading.value = false;
    }
  }

  async function saveAgentPromptMd(): Promise<void> {
    if (mdSaving.value) return;
    mdSaving.value = true;
    mdMsg.value = null;
    try {
      const data = await api.saveAgentPromptMd(agentPromptMd.value);
      mdMsg.value = { kind: 'ok', text: `已保存到 ${data.path}` };
      setTimeout(() => (mdMsg.value = null), 3000);
    } catch (e) {
      mdMsg.value = { kind: 'err', text: `保存失败:${(e as Error).message}` };
    } finally {
      mdSaving.value = false;
    }
  }

  return {
    agentPromptMd, promptPreview, previewLoading, previewError, mdLoading, mdSaving, mdMsg,
    loadPromptPreview, loadAgentPromptMd, saveAgentPromptMd,
  };
}
