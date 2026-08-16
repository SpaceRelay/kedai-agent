// 数据管理:聊天导入/导出/清空 + 界面项(Agent 面板开关、安全 HTML 渲染、脚本授权)。
import { computed, ref } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import * as api from '../api';

export function useDataManager() {
  const store = useAppStore();
  const { currentSessionId } = storeToRefs(store);

  const importInput = ref<HTMLInputElement | null>(null);
  const importError = ref('');
  const clearMsg = ref('');
  /** 导出成功提示(成功 3 秒后自动清除) */
  const exportMsg = ref('');

  function characterLabel(id: string): string {
    const character = store.characters.find((item) => item.id === id);
    return character?.chara_name ?? character?.name ?? `角色 ${id.slice(0, 8)}`;
  }

  function confirmRevokeScriptAuthorization(characterId: string): void {
    if (confirm(`撤销“${characterLabel(characterId)}”的角色卡 JavaScript 授权？\n\n安全 HTML 渲染不受影响。`)) {
      store.revokeCharacterScripts(characterId);
    }
  }

  async function onImportFile(e: Event): Promise<void> {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (!file) return;
    importError.value = '';
    try {
      const messages = JSON.parse(await file.text());
      if (!Array.isArray(messages)) throw new Error('文件须为 SillyTavern 消息数组');
      const sid = currentSessionId.value;
      if (!sid) {
        importError.value = '请先选择一个角色会话';
        return;
      }
      await api.importChat(sid, messages);
      await store.loadHistory(sid);
      alert(`导入成功,共 ${messages.length} 条消息`);
    } catch (err) {
      importError.value = (err as Error).message;
    }
  }

  async function clearAllData(): Promise<void> {
    if (!currentSessionId.value) return;
    if (!confirm('确定清空当前会话全部消息?(角色定义保留)')) return;
    try {
      await store.clearCurrentChat();
      clearMsg.value = '已清空当前会话';
      setTimeout(() => (clearMsg.value = ''), 2500);
    } catch (err) {
      importError.value = (err as Error).message;
    }
  }

  async function exportChat(): Promise<void> {
    if (!currentSessionId.value) {
      importError.value = '请先选择一个角色会话再导出';
      return;
    }
    importError.value = '';
    try {
      const fileName = await store.exportCurrentChat();
      if (fileName === null) return; // 用户在保存对话框中取消
      exportMsg.value = `已导出:${fileName}`;
      setTimeout(() => (exportMsg.value = ''), 3000);
    } catch (e) {
      importError.value = `导出失败:${(e as Error).message}`;
    }
  }

  return {
    importInput, importError, exportMsg, clearMsg, characterLabel, confirmRevokeScriptAuthorization,
    onImportFile, clearAllData, exportChat,
  };
}

export function useGenerationParams() {
  const store = useAppStore();
  const { temperature, topP, maxTokens, maxContextTokens, maxToolRounds, compactionMode, compactionThreshold } = storeToRefs(store);

  const tempLabel = computed(() => `${Math.round(temperature.value * 100)}%`);
  const topPLabel = computed(() => `${Math.round(topP.value * 100)}%`);
  const ctxLabel = computed(() =>
    maxContextTokens.value >= 1000 ? `${(maxContextTokens.value / 1000).toFixed(1)}k` : String(maxContextTokens.value),
  );

  const saveParams = ref(false);
  const paramsMsg = ref('');

  async function saveParamsNow(): Promise<void> {
    saveParams.value = true;
    paramsMsg.value = '';
    try {
      await store.saveSettings({
        default_temperature: temperature.value,
        default_top_p: topP.value,
        default_max_tokens: maxTokens.value,
        max_context_tokens: maxContextTokens.value,
        max_tool_rounds: maxToolRounds.value,
        compaction_mode: compactionMode.value,
        compaction_threshold: compactionThreshold.value,
      });
      paramsMsg.value = '已保存为默认生成参数';
      setTimeout(() => (paramsMsg.value = ''), 2500);
    } catch (e) {
      paramsMsg.value = `保存失败:${(e as Error).message}`;
    } finally {
      saveParams.value = false;
    }
  }

  return { tempLabel, topPLabel, ctxLabel, saveParams, paramsMsg, saveParamsNow };
}
