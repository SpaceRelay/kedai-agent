<script setup lang="ts">
// 左侧功能区:品牌 + Token 统计 + 搜索 + 角色列表 + 综合设置入口
import { computed, ref } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import type { CharacterRecord } from '../api';

const store = useAppStore();
const {
  filteredCharacters, characters, currentCharacterId, connStatus,
  contextTokens, lastUsage, currentSessionId,
  sessionTotalTokens, globalTotalTokens, cacheZeroStreak,
} = storeToRefs(store);

const fileInput = ref<HTMLInputElement | null>(null);
const uploadError = ref('');

// 头像文件名
function avatarFile(avatarPath: string | null): string {
  if (!avatarPath) return '';
  return avatarPath.split(/[\\/]/).pop() ?? '';
}

// 头像占位颜色
function avatarClass(id: string): string {
  const n = id.charCodeAt(0) || 0;
  if (n % 3 === 1) return 'alt-blue';
  if (n % 3 === 2) return 'alt-red';
  return '';
}

// 连接状态
const connColor = computed(() => store.connStatus);
const connLabel = computed(() => {
  switch (store.connStatus) {
    case 'ok': return '已连接';
    case 'fail': return '未连接';
    default: return '检测中';
  }
});

/** 缓存命中率:连续两次为 0 则不显示 */
const hitRate = computed<number | null>(() => {
  if (cacheZeroStreak.value >= 2) return null;
  const u = lastUsage.value;
  if (!u || !u.prompt_tokens) return null;
  const hit = u.prompt_cache_hit_tokens ?? 0;
  return Math.min(100, Math.round((hit / u.prompt_tokens) * 100));
});

async function onFilePicked(e: Event): Promise<void> {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  if (!file) return;
  uploadError.value = '';
  try {
    await store.uploadCharacter(file);
  } catch (err) {
    uploadError.value = (err as Error).message;
  }
}

async function clearChat(): Promise<void> {
  if (!store.currentSessionId) return;
  if (!confirm('确定清空当前会话?(角色定义保留)')) return;
  try {
    await store.clearCurrentChat();
  } catch (err) {
    alert(`清空失败:${(err as Error).message}`);
  }
}

// ===== 右键菜单(编辑提示词 / 删除) =====
const ctxMenu = ref<{ x: number; y: number; char: CharacterRecord } | null>(null);

function openCtxMenu(e: MouseEvent, c: CharacterRecord): void {
  e.preventDefault();
  const x = Math.min(e.clientX, window.innerWidth - 168);
  const y = Math.min(e.clientY, window.innerHeight - 96);
  ctxMenu.value = { x, y, char: c };
}

function closeCtx(): void {
  ctxMenu.value = null;
}

// ===== 编辑角色提示词 / 开场白 =====
const promptEdit = ref<{ id: string; name: string; text: string; first_mes: string; alts: string[] } | null>(null);
const savingPrompt = ref(false);
const promptMsg = ref('');

function openPromptEditor(c: CharacterRecord): void {
  promptEdit.value = {
    id: c.id,
    name: c.chara_name,
    text: c.description ?? '',
    first_mes: c.first_mes ?? '',
    alts: [...(c.alternate_greetings ?? [])],
  };
  promptMsg.value = '';
  closeCtx();
}

/** 添加一条备用开场(空行保存时忽略) */
function addAlternateGreeting(): void {
  promptEdit.value?.alts.push('');
}

/** 删除指定备用开场 */
function removeAlternateGreeting(i: number): void {
  promptEdit.value?.alts.splice(i, 1);
}

async function savePrompt(): Promise<void> {
  const e = promptEdit.value;
  if (!e || savingPrompt.value) return;
  savingPrompt.value = true;
  promptMsg.value = '';
  try {
    await store.updateCharacterPrompt(e.id, {
      description: e.text,
      first_mes: e.first_mes,
      alternate_greetings: e.alts.map((a) => a.trim()).filter((a) => a.length > 0),
    });
    promptMsg.value = '已更新(开场白已自动识别并保存)';
    setTimeout(() => (promptEdit.value = null), 400);
  } catch (err) {
    promptMsg.value = `保存失败:${(err as Error).message}`;
  } finally {
    savingPrompt.value = false;
  }
}

// ===== 删除角色 =====
function deleteChar(c: CharacterRecord): void {
  closeCtx();
  if (!confirm(`确定删除角色「${c.chara_name}」?其会话与消息将一并删除。`)) return;
  void store.deleteCharacter(c.id);
}
</script>

<template>
  <aside class="sv-sidebar">
    <!-- 顶部:品牌 + Token 状态区 -->
    <div class="sv-side-head">
      <div class="sv-brand">
        <img class="logo" src="/logo.png" alt="Kedai" draggable="false" />
        <h1>KEDAI</h1>
        <span class="sv-supreme" style="width: 10px; height: 10px" aria-hidden="true" />
        <span class="sv-conn" style="margin-left: auto">
          <span class="dot" :class="connColor" />
          {{ connLabel }}
        </span>
      </div>

      <!-- Token 统计区:会话累计 / 全局累计 / 上下文 / 缓存命中率 -->
      <div class="sv-tokens">
        <div class="sv-token-cell">
          <span class="sv-supreme pink" />
          <div>
            <b>{{ sessionTotalTokens.toLocaleString() }}</b>
            <span>本会话累计</span>
          </div>
        </div>
        <div class="sv-token-cell">
          <span class="sv-supreme pink-deep" />
          <div>
            <b>{{ globalTotalTokens.toLocaleString() }}</b>
            <span>全局累计</span>
          </div>
        </div>
        <div class="sv-token-cell">
          <span class="sv-supreme blue" />
          <div>
            <b>{{ contextTokens.toLocaleString() }}</b>
            <span>上下文 TOKEN</span>
          </div>
        </div>
        <div v-if="hitRate !== null" class="sv-token-cell">
          <span class="sv-supreme green" />
          <div>
            <b>{{ hitRate }}%</b>
            <span>缓存命中率</span>
          </div>
        </div>
      </div>
    </div>

    <!-- 中部:角色列表 -->
    <div class="flex min-h-0 flex-1 flex-col px-3 pb-2">
      <input v-model="store.searchQuery" type="text" placeholder="搜索角色..." class="sv-search" />

      <div v-if="characters.length === 0" class="sv-empty" style="padding: 40px 12px">
        <div class="sv-empty-geo" style="margin-bottom: 10px">
          <span class="sq black" style="width: 14px; height: 14px" />
          <span class="sq pink" />
          <span class="sq deep" style="width: 6px; height: 6px" />
        </div>
        <p style="font-size: 12px">暂无角色</p>
        <p style="font-size: 11px">点击下方「综合设置」→「上传角色卡」开始</p>
      </div>

      <div class="min-h-0 flex-1 overflow-y-auto pr-1">
        <button
          v-for="c in filteredCharacters"
          :key="c.id"
          class="sv-char-item"
          :class="{ active: c.id === currentCharacterId }"
          @click="store.selectCharacter(c.id)"
          @contextmenu="openCtxMenu($event, c)"
        >
          <span class="sv-avatar small char" :class="avatarClass(c.id)">
            <img v-if="c.avatar_path" :src="`/api/avatars/${avatarFile(c.avatar_path)}`" alt="" />
            <template v-else>{{ c.chara_name.charAt(0) }}</template>
          </span>
          <span class="min-w-0 flex-1">
            <span class="sv-char-name">{{ c.chara_name }}</span>
            <span class="sv-char-desc">{{ c.description || '无提示词' }}</span>
          </span>
        </button>
      </div>
    </div>

    <!-- 底部:综合设置入口 -->
    <div class="sv-side-foot">
      <div v-if="uploadError" class="sv-feedback err" style="margin: 0 0 8px">{{ uploadError }}</div>
      <button
        class="sv-btn primary"
        style="width: 100%; padding: 12px; font-size: 13px; letter-spacing: 0.1em"
        @click="store.settingsOpen = true"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" style="width: 16px; height: 16px; margin-right: 8px; vertical-align: -3px">
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06-.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
        </svg>
        综合设置
      </button>
      <!-- 脚本管理(阶段三):用户脚本 ScriptTree 管理 -->
      <button
        class="sv-btn ghost"
        style="width: 100%; padding: 10px; font-size: 13px; letter-spacing: 0.1em; margin-top: 8px"
        @click="store.scriptsOpen = true"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" style="width: 16px; height: 16px; margin-right: 8px; vertical-align: -3px">
          <polyline points="16 18 22 12 16 6" />
          <polyline points="8 6 2 12 8 18" />
        </svg>
        脚本管理
      </button>
      <!-- 宏调试(阶段六 6b):模板 {{...}} 展开结果验证 -->
      <button
        class="sv-btn ghost"
        style="width: 100%; padding: 10px; font-size: 13px; letter-spacing: 0.1em; margin-top: 8px"
        @click="store.macrosOpen = true"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" style="width: 16px; height: 16px; margin-right: 8px; vertical-align: -3px">
          <polyline points="4 17 10 11 4 5" />
          <line x1="12" y1="19" x2="20" y2="19" />
        </svg>
        宏调试
      </button>
      <input
        ref="fileInput"
        type="file"
        accept=".png,.json,image/png,application/json"
        class="hidden"
        @change="onFilePicked"
      />
    </div>

    <!-- 右键菜单 -->
    <div v-if="ctxMenu" class="sv-ctx-mask" @click="closeCtx" @contextmenu.prevent="closeCtx">
      <div class="sv-ctx-menu" :style="{ left: `${ctxMenu.x}px`, top: `${ctxMenu.y}px` }" @click.stop>
        <div class="sv-ctx-title">{{ ctxMenu.char.chara_name }}</div>
        <button class="sv-ctx-item" @click="openPromptEditor(ctxMenu.char)">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 20h9" />
            <path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z" />
          </svg>
          编辑提示词
        </button>
        <button class="sv-ctx-item danger" @click="deleteChar(ctxMenu.char)">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <path d="M3 6h18" />
            <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" />
            <path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
            <path d="M10 11v6" />
            <path d="M14 11v6" />
          </svg>
          删除角色
        </button>
      </div>
    </div>

    <!-- 编辑角色提示词模态框 -->
    <div v-if="promptEdit" class="sv-modal-mask" @click.self="promptEdit = null">
      <div class="sv-modal pm-edit">
        <div class="sv-modal-head">
          <h2 class="flex items-center gap-2">
            <span class="sv-supreme pink" style="width: 18px; height: 18px" /> 编辑提示词
          </h2>
          <button class="sv-btn ghost sv-btn-square" @click="promptEdit = null">✕</button>
        </div>
        <div class="sv-modal-body">
          <div class="sv-field">
            <div class="sv-field-label"><span class="sv-supreme blue" /> {{ promptEdit.name }}</div>
            <textarea
              v-model="promptEdit.text"
              rows="16"
              class="sv-input"
              style="resize: vertical; min-height: 320px; line-height: 1.7"
              placeholder="角色提示词(角色设定)。留空表示无提示词,直接以助手身份回复。"
              spellcheck="false"
            ></textarea>
            <div class="sv-field-label" style="margin-top: 14px">
              <span class="sv-supreme pink-deep" /> 开场白(First Message)
            </div>
            <textarea
              v-model="promptEdit.first_mes"
              rows="5"
              class="sv-input"
              style="resize: vertical; min-height: 130px; line-height: 1.7"
              placeholder="主开场:新建会话时作为角色的第一句话(自动发送)。留空表示无主开场。"
              spellcheck="false"
            ></textarea>
            <p class="sv-note" style="margin-top: 6px">
              主开场之外可添加备用开场,新建会话时可在多个开场之间选择。
            </p>

            <!-- 备用开场列表 -->
            <div v-for="(g, i) in promptEdit.alts" :key="i" style="margin-top: 12px">
              <div class="flex items-center gap-2" style="margin-bottom: 6px">
                <span class="sv-tag">备用 {{ i + 1 }}</span>
                <span style="flex: 1" />
                <button class="sv-btn ghost sv-btn-sm" @click="removeAlternateGreeting(i)">删除</button>
              </div>
              <textarea
                v-model="promptEdit.alts[i]"
                rows="4"
                class="sv-input"
                style="resize: vertical; min-height: 96px; line-height: 1.7"
                placeholder="备用开场内容(留空保存时自动忽略)。"
                spellcheck="false"
              ></textarea>
            </div>

            <button class="sv-btn ghost sv-btn-sm" style="margin-top: 12px" @click="addAlternateGreeting">
              ＋ 添加备用开场
            </button>
            <p class="sv-note" style="margin-top: 8px">
              提示词作为角色的系统设定注入每次对话;开场白在新建会话时作为角色的第一句话(自动识别角色卡内嵌开场白)。
            </p>
          </div>
          <div v-if="promptMsg" class="sv-feedback" :class="promptMsg.includes('失败') ? 'err' : 'ok'">
            {{ promptMsg }}
          </div>
        </div>
        <div class="sv-modal-foot">
          <button class="sv-btn ghost" style="margin-right: 8px" @click="promptEdit = null">取消</button>
          <button class="sv-btn primary" :disabled="savingPrompt" @click="savePrompt">
            {{ savingPrompt ? '保存中...' : '保存' }}
          </button>
        </div>
      </div>
    </div>
  </aside>
</template>
