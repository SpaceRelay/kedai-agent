<script setup lang="ts">
// 聊天记录管理面板:列出全部会话(含角色名/消息数),支持选择切换、新建、删除、导出/导入 JSON。
import { ref, onMounted } from 'vue';
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';
import * as api from '../api';
import { saveExportFile } from '../exportFile';

const store = useAppStore();
const { characters, currentSessionId, currentCharacterId } = storeToRefs(store);

const sessions = ref<api.SessionWithCharacter[]>([]);
const loading = ref(false);
const feedback = ref<{ kind: 'ok' | 'err'; text: string } | null>(null);
const busy = ref(false);

const close = (): void => {
  store.chatRecordsOpen = false;
};

function charName(s: api.SessionWithCharacter): string {
  return s.character_name ?? '未知角色';
}

function fmtTime(iso: string): string {
  try {
    return new Date(iso).toLocaleString('zh-CN', { hour12: false });
  } catch {
    return iso;
  }
}

async function load(): Promise<void> {
  loading.value = true;
  try {
    sessions.value = await api.listAllSessions();
  } catch (e) {
    feedback.value = { kind: 'err', text: `加载失败:${(e as Error).message}` };
  } finally {
    loading.value = false;
  }
}

/** 切换到某会话:若属于当前角色则走 switchSession(保留聊天区),否则切换角色后再切会话 */
async function openSession(s: api.SessionWithCharacter): Promise<void> {
  if (busy.value) return;
  busy.value = true;
  try {
    if (currentCharacterId.value !== s.character_id) {
      await store.selectCharacter(s.character_id);
    }
    if (currentSessionId.value !== s.id) {
      await store.switchSession(s.id);
    }
    close();
  } catch (e) {
    feedback.value = { kind: 'err', text: `切换失败:${(e as Error).message}` };
  } finally {
    busy.value = false;
  }
}

/** 当前角色新建会话并切换 */
async function newSession(): Promise<void> {
  if (busy.value) return;
  if (!currentCharacterId.value) {
    feedback.value = { kind: 'err', text: '请先选择一个角色,再新建会话' };
    return;
  }
  busy.value = true;
  try {
    await store.newSession();
    await load();
    close();
  } catch (e) {
    feedback.value = { kind: 'err', text: `新建失败:${(e as Error).message}` };
  } finally {
    busy.value = false;
  }
}

async function removeSession(s: api.SessionWithCharacter): Promise<void> {
  if (busy.value) return;
  if (!confirm(`确定删除会话「${s.title}」?其全部消息将一并删除,不可恢复。`)) return;
  busy.value = true;
  try {
    await api.deleteSession(s.id);
    // 当前会话被删:回到该角色下最新会话(selectCharacter 内部会刷新会话与消息)
    if (currentSessionId.value === s.id) {
      if (characters.value.some((c) => c.id === s.character_id)) {
        await store.selectCharacter(s.character_id);
      } else {
        await store.loadCharacters();
      }
    }
    await load();
  } catch (e) {
    feedback.value = { kind: 'err', text: `删除失败:${(e as Error).message}` };
  } finally {
    busy.value = false;
  }
}

/** 导出单个会话(SillyTavern 兼容);桌面版弹保存对话框选择位置,浏览器回退下载 */
async function exportSession(s: api.SessionWithCharacter): Promise<void> {
  try {
    const data = await api.exportChat(s.id);
    const fileName = `kedai-${charName(s)}-${s.title.replace(/[\\/:*?"<>|]/g, '_')}-${new Date().toISOString().replace(/[:.]/g, '-')}.json`;
    const saved = await saveExportFile(fileName, JSON.stringify(data, null, 2));
    if (!saved) return; // 用户取消
    feedback.value = { kind: 'ok', text: `已导出:${fileName}` };
  } catch (e) {
    feedback.value = { kind: 'err', text: `导出失败:${(e as Error).message}` };
  }
}

/** 导入 JSON 替换指定会话内容 */
const importInput = ref<HTMLInputElement | null>(null);
const importTarget = ref<api.SessionWithCharacter | null>(null);
const importError = ref('');

async function onImportFile(e: Event): Promise<void> {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  if (!file || !importTarget.value) return;
  importError.value = '';
  try {
    const messages = JSON.parse(await file.text());
    const arr = Array.isArray(messages) ? messages : (messages as { messages?: unknown[] }).messages;
    if (!Array.isArray(arr)) throw new Error('文件须为 SillyTavern 消息数组或 {messages:[...]}');
    const target = importTarget.value;
    await api.importChat(target.id, arr as api.StMessage[]);
    feedback.value = { kind: 'ok', text: `已导入 ${arr.length} 条消息到「${target.title}」` };
    importTarget.value = null;
  } catch (err) {
    importError.value = (err as Error).message;
  }
}

onMounted(() => {
  void load();
});
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal wb-modal">
      <!-- 头部 -->
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="logo" /> 聊天记录
        </h2>
        <div class="flex items-center gap-2">
          <button class="sv-btn primary sv-btn-sm" :disabled="busy" @click="newSession">＋ 新建</button>
          <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
        </div>
      </div>

      <div class="sv-modal-body">
        <div v-if="feedback" class="sv-feedback" :class="feedback.kind" style="margin-bottom: 10px">{{ feedback.text }}</div>
        <p class="sv-note" style="margin-top: 0">
          全部会话列表(按最近活动排序)。选择切换会话、新建当前角色会话、删除会话,或对单个会话导出 / 导入 JSON(导入将替换该会话全部内容)。
        </p>

        <div v-if="loading" class="sv-empty" style="padding: 30px 12px">加载中…</div>
        <div v-else-if="sessions.length === 0" class="sv-empty" style="padding: 30px 12px">
          <p style="font-size: 12px">暂无会话记录。选择角色后发送第一条消息,或点击「新建」。</p>
        </div>
        <div v-else class="sv-datalist">
          <div
            v-for="s in sessions"
            :key="s.id"
            class="sv-data-row sv-wb-row"
            :class="{ 'sv-session-current': s.id === currentSessionId }"
          >
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-2">
                <b>{{ s.title }}</b>
                <span class="sv-tag">{{ charName(s) }}</span>
                <span v-if="s.id === currentSessionId" class="sv-tag sv-tag-on">当前</span>
              </div>
              <div class="sv-wb-sub">
                <span class="sv-wb-sub-item">{{ s.message_count ?? 0 }} 条消息</span>
                <span class="sv-wb-sub-item">{{ fmtTime(s.updated_at) }}</span>
              </div>
            </div>
            <div class="sv-wb-ops">
              <button class="sv-btn ghost sv-btn-sm" :disabled="busy" @click="openSession(s)">打开</button>
              <button class="sv-btn ghost sv-btn-sm" @click="exportSession(s)">导出</button>
              <button class="sv-btn ghost sv-btn-sm" @click="importTarget = s; importInput?.click()">导入</button>
              <button class="sv-btn danger sv-btn-sm" :disabled="busy" @click="removeSession(s)">删除</button>
            </div>
          </div>
        </div>
        <div v-if="importError" class="sv-feedback err" style="margin-top: 10px">{{ importError }}</div>
        <input ref="importInput" type="file" accept=".json,application/json" class="hidden" @change="onImportFile" />
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>
