<script setup lang="ts">
// 左侧功能区:品牌 + Token 统计 + 模式切换 + (角色列表 | 任务工作台) + 综合设置入口
import { computed, ref, watch } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import { taskStatusClass as statusClass, taskStatusLabel as statusLabel } from '../taskStatus';
import TaskModeSelect from './TaskModeSelect.vue';
import type { CharacterRecord, TaskRecord } from '../api';

const store = useAppStore();
const {
  filteredCharacters, characters, currentCharacterId, connStatus,
  contextTokens, lastUsage, currentSessionId,
  sessionTotalTokens, globalTotalTokens, cacheZeroStreak, appMode, tasks, currentTaskId,
  currentTaskUsage, globalTaskUsage,
} = storeToRefs(store);

const fileInput = ref<HTMLInputElement | null>(null);
const uploadError = ref('');

// 上传角色卡请求(SettingsHub 快速操作等发起方自增 uiPrefs.characterUploadRequested):
// 在本组件内响应,触发自身隐藏 file input;消除跨组件 document.querySelector 耦合
watch(
  () => store.characterUploadRequested,
  (n, prev) => {
    if (n !== prev) fileInput.value?.click();
  },
);

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

/** 任务模式统计块数据源:当前任务/全部任务累计 token(prompt + completion)。
 *  聊天统计来自 SSE usage 链路,任务模式无此数据源,改由任务详情/全局接口轮询带出。 */
const currentTaskTotalTokens = computed(() => {
  const u = currentTaskUsage.value;
  return u ? u.prompt_tokens + u.completion_tokens : 0;
});
const globalTaskTotalTokens = computed(() => {
  const u = globalTaskUsage.value;
  return u ? u.prompt_tokens + u.completion_tokens : 0;
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

/** 长按菜单的坐标基准:「右键」在触屏上不存在,以长按等价替代。
 *  长按触发后需吞掉随后到来的 click,否则会连带选中该角色(用户本意只是开菜单)。 */
let longPressTimer: ReturnType<typeof setTimeout> | null = null;
let longPressOrigin: { x: number; y: number } | null = null;
let suppressClick = false;

const LONG_PRESS_MS = 500;
/** 手指位移超过此阈值视为滚动/拖拽,取消长按(避免滑动列表时误弹菜单) */
const LONG_PRESS_MOVE_TOLERANCE = 10;

function openCtxMenuAt(x: number, y: number, c: CharacterRecord): void {
  const clampedX = Math.min(x, window.innerWidth - 168);
  const clampedY = Math.min(y, window.innerHeight - 96);
  ctxMenu.value = { x: clampedX, y: clampedY, char: c };
}

function openCtxMenu(e: MouseEvent, c: CharacterRecord): void {
  e.preventDefault();
  openCtxMenuAt(e.clientX, e.clientY, c);
}

function cancelLongPress(): void {
  if (longPressTimer) {
    clearTimeout(longPressTimer);
    longPressTimer = null;
  }
  longPressOrigin = null;
}

function onCharTouchStart(e: TouchEvent, c: CharacterRecord): void {
  const touch = e.touches[0];
  if (!touch) return;
  longPressOrigin = { x: touch.clientX, y: touch.clientY };
  cancelLongPressTimerOnly();
  longPressTimer = setTimeout(() => {
    longPressTimer = null;
    const origin = longPressOrigin;
    longPressOrigin = null;
    if (!origin) return;
    suppressClick = true;
    openCtxMenuAt(origin.x, origin.y, c);
  }, LONG_PRESS_MS);
}

function onCharTouchMove(e: TouchEvent): void {
  if (!longPressTimer || !longPressOrigin) return;
  const touch = e.touches[0];
  if (!touch) return;
  const moved = Math.abs(touch.clientX - longPressOrigin.x) + Math.abs(touch.clientY - longPressOrigin.y);
  if (moved > LONG_PRESS_MOVE_TOLERANCE) cancelLongPressTimerOnly();
}

function cancelLongPressTimerOnly(): void {
  if (longPressTimer) {
    clearTimeout(longPressTimer);
    longPressTimer = null;
  }
}

/** 点击角色:长按已弹菜单时吞掉本次 click(仅吞一次) */
function onCharClick(c: CharacterRecord): void {
  if (suppressClick) {
    suppressClick = false;
    return;
  }
  void store.selectCharacter(c.id);
}

function closeCtx(): void {
  ctxMenu.value = null;
  cancelLongPress();
  suppressClick = false;
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

// ===== 任务模式:下达目标 + 任务历史(原任务工作台左栏移入,单列布局) =====
const taskTitle = ref('');
const personaId = ref('');
const creatingTask = ref(false);

/** 新建并立即执行 */
async function createAndRun(): Promise<void> {
  const t = taskTitle.value.trim();
  if (!t || creatingTask.value) return;
  creatingTask.value = true;
  try {
    const task = await store.createTask(t, personaId.value || undefined);
    taskTitle.value = '';
    await store.runTask(task.id);
  } catch (err) {
    alert(`创建任务失败:${(err as Error).message}`);
  } finally {
    creatingTask.value = false;
  }
}

/** 删除任务 */
async function removeTask(task: TaskRecord): Promise<void> {
  if (!confirm(`确定删除任务「${task.title}」?其子任务将一并删除。`)) return;
  try {
    await store.deleteTask(task.id);
  } catch (err) {
    alert(`删除失败:${(err as Error).message}`);
  }
}
</script>

<template>
  <!-- open 类仅移动端生效(窄屏下 Sidebar 为覆盖式抽屉,见 style.css 移动端区块);
       桌面端该断点不匹配,布局与此前完全一致。 -->
  <aside class="sv-sidebar" :class="{ wide: appMode === 'task', open: store.sidebarOpen }">
    <!-- 顶部:品牌 + Token 状态区 -->
    <div class="sv-side-head">
      <div class="sv-brand">
        <img class="logo" src="/logo.png" alt="Kedai" draggable="false" />
        <h1>KEDAI</h1>
        <span class="sv-supreme sm" aria-hidden="true" />
        <span class="sv-conn sv-ml-auto">
          <span class="dot" :class="connColor" />
          {{ connLabel }}
        </span>
        <!-- 抽屉关闭键:仅移动端显示(桌面端由 CSS 隐藏) -->
        <button
          class="sv-sidebar-close"
          title="收起角色库"
          aria-label="收起角色库"
          @click="store.sidebarOpen = false"
        >✕</button>
      </div>

      <!-- Token 统计区:角色扮演 = 会话累计/全局累计/上下文/缓存命中率;
           任务模式 = 当前任务累计/全部任务累计(无上下文与命中率数据源,两格隐藏) -->
      <div class="sv-tokens">
        <template v-if="appMode === 'task'">
          <div class="sv-token-cell">
            <span class="sv-supreme pink" />
            <div>
              <b class="sv-tnum">{{ currentTaskTotalTokens.toLocaleString() }}</b>
              <span>当前任务累计</span>
            </div>
          </div>
          <div class="sv-token-cell">
            <span class="sv-supreme pink-deep" />
            <div>
              <b class="sv-tnum">{{ globalTaskTotalTokens.toLocaleString() }}</b>
              <span>全部任务累计</span>
            </div>
          </div>
        </template>
        <template v-else>
          <div class="sv-token-cell">
            <span class="sv-supreme pink" />
            <div>
              <b class="sv-tnum">{{ sessionTotalTokens.toLocaleString() }}</b>
              <span>本会话累计</span>
            </div>
          </div>
          <div class="sv-token-cell">
            <span class="sv-supreme pink-deep" />
            <div>
              <b class="sv-tnum">{{ globalTotalTokens.toLocaleString() }}</b>
              <span>全局累计</span>
            </div>
          </div>
          <div class="sv-token-cell">
            <span class="sv-supreme blue" />
            <div>
              <b class="sv-tnum">{{ contextTokens.toLocaleString() }}</b>
              <span>上下文 TOKEN</span>
            </div>
          </div>
          <div v-if="hitRate !== null" class="sv-token-cell">
            <span class="sv-supreme green" />
            <div>
              <b class="sv-tnum">{{ hitRate }}%</b>
              <span>缓存命中率</span>
            </div>
          </div>
        </template>
      </div>
    </div>

    <!-- 顶层模式切换:角色扮演 / 任务 -->
    <div class="sv-appmode">
      <button
        :class="{ active: appMode === 'roleplay' }"
        title="角色扮演模式:与角色进行沉浸式对话与文学创作"
        @click="store.setAppMode('roleplay')"
      >
        角色扮演
      </button>
      <button
        :class="{ active: appMode === 'task' }"
        title="任务模式:下达目标,自动拆解计划并派子智能体执行"
        @click="store.setAppMode('task')"
      >
        任务
      </button>
    </div>

    <!-- 中部:角色扮演模式 = 搜索 + 角色列表;任务模式 = 下达目标 + 任务历史 -->
    <div class="flex min-h-0 flex-1 flex-col px-3 pb-2">
      <Transition name="sv-fade" mode="out-in">
        <div v-if="appMode === 'roleplay'" key="roleplay" class="min-h-0 flex-1 flex flex-col">
          <input
            v-model="store.searchQuery"
            type="text"
            placeholder="搜索角色..."
            class="sv-search"
          />

          <div v-if="characters.length === 0" class="sv-empty compact">
            <div class="sv-empty-geo mb10">
              <span class="sq black" />
              <span class="sq pink" />
              <span class="sq deep" />
            <i class="diag" />
            </div>
            <p class="sub">暂无角色</p>
            <p class="hint">点击下方「综合设置」→「上传角色卡」开始</p>
          </div>

          <div class="min-h-0 flex-1 overflow-y-auto pr-1">
            <button
              v-for="c in filteredCharacters"
              :key="c.id"
              class="sv-char-item"
              :class="{ active: c.id === currentCharacterId }"
              @click="onCharClick(c)"
              @contextmenu="openCtxMenu($event, c)"
              @touchstart.passive="onCharTouchStart($event, c)"
              @touchmove.passive="onCharTouchMove($event)"
              @touchend="cancelLongPress"
              @touchcancel="cancelLongPress"
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

        <!-- 任务模式:下达目标 + 任务历史(承接任务工作台左栏,单列布局) -->
        <div v-else key="task" class="flex min-h-0 flex-1 flex-col">
          <!-- 下达目标 -->
          <div class="sv-task-new">
            <div class="sv-task-new-title">下达目标</div>
            <textarea
              v-model="taskTitle"
              class="sv-input"
              rows="3"
              placeholder="描述要完成的任务,例如:帮我写一篇 2000 字的科幻短篇并列出大纲…"
              spellcheck="false"
            />
            <!-- 执行模式(批次 4 六模式;独立子组件,选择持久化在 task store) -->
            <TaskModeSelect />
            <!-- 执行者人设(可选):默认通用执行者 -->
            <select v-model="personaId" class="sv-select">
              <option value="">执行者:通用执行者</option>
              <option v-for="c in characters" :key="c.id" :value="c.id">
                执行者:{{ c.chara_name }}
              </option>
            </select>
            <button
              class="sv-btn primary"
              :disabled="creatingTask || !taskTitle.trim()"
              @click="createAndRun"
            >
              {{ creatingTask ? '创建中…' : '▶ 创建并执行' }}
            </button>
          </div>

          <!-- 任务历史 -->
          <div class="sv-task-list-head">任务历史</div>
          <div class="sv-task-list">
            <button
              v-for="t in tasks"
              :key="t.id"
              class="sv-task-item"
              :class="{ active: t.id === currentTaskId }"
              @click="store.selectTask(t.id)"
            >
              <span class="sv-supreme xs" :class="statusClass(t.status)" />
              <span class="sv-task-item-title">{{ t.title }}</span>
              <span class="sv-task-item-meta">{{ statusLabel(t.status) }}</span>
              <span class="sv-task-item-del" title="删除任务" @click.stop="removeTask(t)">✕</span>
            </button>
            <div v-if="tasks.length === 0" class="sv-empty-geo sm mb10">
              <span class="sq black" />
              <span class="sq pink" />
              <span class="sq deep" />
              <span class="diag" />
            </div>
            <p v-if="tasks.length === 0" class="sv-task-list-empty">
              暂无任务,输入目标创建第一个任务
            </p>
          </div>
        </div>
      </Transition>
    </div>

    <!-- 底部:仅保留综合设置入口(2026-09 入口整合:工具面板统一收进综合设置的二级分类,
         原脚本管理/宏调试/记忆库/仓库索引 4 个按钮已移除,避免与 Hub 内入口重复) -->
    <div class="sv-side-foot">
      <div v-if="uploadError" class="sv-feedback err">{{ uploadError }}</div>
      <button
        class="sv-btn primary sv-side-btn"
        @click="store.settingsOpen = true"
      >
        <span class="sv-side-btn-ico">
          <svg viewBox="0 0 24 24">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06-.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </span>
        综合设置
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
    <!-- 角色操作菜单(编辑提示词 / 删除)。
         Teleport 到 body 是必需的:移动端侧栏抽屉带 transform + overflow-y:auto,
         而 transform 祖先会成为 position:fixed 后代的包含块,导致菜单定位偏移、
         遮罩只盖住侧栏、且被 overflow 裁切。挂到 body 后按视口坐标正常定位。 -->
    <Teleport to="body">
      <div v-if="ctxMenu" class="sv-ctx-mask" @click="closeCtx" @contextmenu.prevent="closeCtx">
        <div class="sv-ctx-menu" :style="{ left: `${ctxMenu.x}px`, top: `${ctxMenu.y}px` }" @click.stop>
        <div class="sv-ctx-title">{{ ctxMenu.char.chara_name }}</div>
        <button class="sv-ctx-item" @click="openPromptEditor(ctxMenu.char)">
          <svg viewBox="0 0 24 24">
            <path d="M12 20h9" />
            <path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z" />
          </svg>
          编辑提示词
        </button>
        <button class="sv-ctx-item danger" @click="deleteChar(ctxMenu.char)">
          <svg viewBox="0 0 24 24">
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
    </Teleport>

    <!-- 编辑角色提示词模态框(同样 Teleport:见上,侧栏 transform 会破坏 fixed 定位) -->
    <Teleport to="body">
      <div v-if="promptEdit" class="sv-modal-mask" @click.self="promptEdit = null">
      <div class="sv-modal pm-edit">
        <div class="sv-modal-head">
          <h2 class="flex items-center gap-2">
            <span class="sv-supreme pink" /> 编辑提示词
          </h2>
          <button class="sv-btn ghost sv-btn-square" @click="promptEdit = null">✕</button>
        </div>
        <div class="sv-modal-body">
          <div class="sv-field">
            <div class="sv-field-label"><span class="sv-supreme blue" /> {{ promptEdit.name }}</div>
            <textarea
              v-model="promptEdit.text"
              rows="16"
              class="sv-input sv-area xl"
              placeholder="角色提示词(角色设定)。留空表示无提示词,直接以助手身份回复。"
              spellcheck="false"
            ></textarea>
            <div class="sv-field-label sv-mt14">
              <span class="sv-supreme pink-deep" /> 开场白(First Message)
            </div>
            <textarea
              v-model="promptEdit.first_mes"
              rows="5"
              class="sv-input sv-area md"
              placeholder="主开场:新建会话时作为角色的第一句话(自动发送)。留空表示无主开场。"
              spellcheck="false"
            ></textarea>
            <p class="sv-note sv-mt6">
              主开场之外可添加备用开场,新建会话时可在多个开场之间选择。
            </p>

            <!-- 备用开场列表 -->
            <div v-for="(g, i) in promptEdit.alts" :key="i" class="sv-mt12">
              <div class="flex items-center gap-2 sv-mb6">
                <span class="sv-tag">备用 {{ i + 1 }}</span>
                <span class="sv-flex-1" />
                <button class="sv-btn ghost sv-btn-sm" @click="removeAlternateGreeting(i)">删除</button>
              </div>
              <textarea
                v-model="promptEdit.alts[i]"
                rows="4"
                class="sv-input sv-area sm"
                placeholder="备用开场内容(留空保存时自动忽略)。"
                spellcheck="false"
              ></textarea>
            </div>

            <button class="sv-btn ghost sv-btn-sm sv-mt12" @click="addAlternateGreeting">
              ＋ 添加备用开场
            </button>
            <p class="sv-note sv-mt8">
              提示词作为角色的系统设定注入每次对话;开场白在新建会话时作为角色的第一句话(自动识别角色卡内嵌开场白)。
            </p>
          </div>
          <div v-if="promptMsg" class="sv-feedback" :class="promptMsg.includes('失败') ? 'err' : 'ok'">
            {{ promptMsg }}
          </div>
        </div>
        <div class="sv-modal-foot">
          <button class="sv-btn ghost sv-mr8" @click="promptEdit = null">取消</button>
          <button class="sv-btn primary" :disabled="savingPrompt" @click="savePrompt">
            {{ savingPrompt ? '保存中...' : '保存' }}
          </button>
        </div>
      </div>
      </div>
    </Teleport>
  </aside>
</template>
