<script setup lang="ts">
// 提示词顺序管理面板:按 6 大层规范(位置5→0)查看/调整全部提示词的注入位置与顺序。
//   位置5 头部 = 系统提示词/主 agent 提示词
//   位置4 = 简单模式注入 / 复杂模式预设(楼层) / agent 执行流程提示词
//   位置3 = 角色卡提示词 / 世界书常态(常驻)
//   位置2 = 历史上下文(固定块)
//   位置1 = 世界书激发(触发)
//   位置0 尾部 = Dvar 变量(留位,即将上线) / 反思失败建议(自动生成,激发之后、预设尾部之前) / 复杂模式预设尾部
// 楼层与世界书条目支持 ↑↓ 排序(楼层另支持拖拽),角色可自由选择;保存写回服务端
// (prompt_floors.json / settings.json / 世界书条目 order)。
import { ref, onMounted, computed } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import * as api from '../api';

const store = useAppStore();
const {
  agentSystemPrompt,
  presetTailPrompt,
  presetTailRole,
  reflectAdvicePrompt,
  reflectAdviceRole,
  promptInject,
  worldBooks,
} = storeToRefs(store);

const close = (): void => {
  store.promptsOpen = false;
};

const saving = ref(false);
const msg = ref('');
/** 展开的世界书 id(懒加载条目) */
const expandedBook = ref<string | null>(null);
/** 各世界书条目缓存:bookId -> entries */
const bookEntries = ref<Record<string, api.WorldBookEntry[]>>({});
/** 世界书条目发生过移动的书 id(保存时统一写回) */
const dirtyBooks = ref<Set<string>>(new Set());

const FLOOR_ROLE_LABELS: Record<api.FloorRole, string> = {
  system: '系统提示词',
  user: '用户',
  assistant: '角色',
};

/** 世界书条目角色标签(空 = 自动) */
const ENTRY_ROLE_LABELS: Record<string, string> = {
  '': '自动',
  system: '系统提示词',
  user: '用户',
  assistant: '角色',
};

/** 文本截断(卡片摘要用) */
function truncate(text: string, n: number): string {
  const t = text.trim();
  return t.length > n ? `${t.slice(0, n)}…` : t;
}

// ===== 位置4 楼层草稿(克隆自 store,保存时才提交) =====
const injectMode = ref<api.InjectMode>('simple');
const floorsDraft = ref<api.PromptFloor[]>([]);
const SIMPLE_ORDER_DEFAULT = ['word_count', 'paraphrase', 'dialogue', 'perspective'];
const SIMPLE_ITEM_LABELS: Record<string, string> = {
  word_count: '字数',
  paraphrase: '转述',
  dialogue: '对话',
  perspective: '视角',
};
const simpleDraft = ref<api.SimplePromptConfig>({
  word_count_enabled: false,
  word_count: 200,
  paraphrase_enabled: false,
  dialogue_enabled: false,
  perspective_enabled: false,
  perspective: '第三人称',
  order: [...SIMPLE_ORDER_DEFAULT],
  banned_words_enabled: false,
  banned_prompt: '',
});
/** 简单模式四项按 order 排序的键(旧配置无 order 时用默认顺序) */
const orderedSimpleKeys = computed(() => {
  const order = (simpleDraft.value.order?.length ? simpleDraft.value.order : SIMPLE_ORDER_DEFAULT).filter((k) => k in SIMPLE_ITEM_LABELS);
  // 兜底补全未出现的启用项(保证四项都在)
  for (const k of SIMPLE_ORDER_DEFAULT) {
    if (!order.includes(k)) order.push(k);
  }
  return order;
});
/** 简单模式四项 ↑↓ 重排(同步 order) */
function moveSimple(key: string, dir: -1 | 1): void {
  const order = simpleDraft.value.order ?? [...SIMPLE_ORDER_DEFAULT];
  const idx = order.indexOf(key);
  const to = idx + dir;
  if (idx < 0 || to < 0 || to >= order.length) return;
  [order[idx], order[to]] = [order[to], order[idx]];
  simpleDraft.value.order = [...order];
}
const editingFloorId = ref<string | null>(null);
const dragFloorId = ref<string | null>(null);

/** 位置4 agent 执行流程摘要(只读,编辑在设置面板) */
const currentFlow = computed(() => {
  const lib = store.agentFlowLibrary;
  if (!lib) return null;
  return lib.flows.find((f) => f.id === lib.current_flow_id) ?? lib.flows[0] ?? null;
});

// ===== 位置3/位置1 世界书条目分组 =====
/** 当前角色卡内嵌世界书(character_book)条目 */
const charEntries = ref<api.WorldBookEntry[]>([]);
const charEntriesLoaded = ref(false);
/** 角色卡内嵌条目是否发生过修改(保存时写回) */
const charEntriesDirty = ref(false);
/** 角色卡内嵌世界书区块展开开关 */
const charBookExpanded = ref(false);

function constantEntries(bookId: string): api.WorldBookEntry[] {
  return (bookEntries.value[bookId] ?? []).filter((e) => e.constant);
}
function triggeredEntries(bookId: string): api.WorldBookEntry[] {
  return (bookEntries.value[bookId] ?? []).filter((e) => !e.constant);
}
/** 角色卡内嵌条目:常态 / 激发 */
function charConstantEntries(): api.WorldBookEntry[] {
  return charEntries.value.filter((e) => e.constant);
}
function charTriggeredEntries(): api.WorldBookEntry[] {
  return charEntries.value.filter((e) => !e.constant);
}

async function toggleBook(id: string): Promise<void> {
  expandedBook.value = expandedBook.value === id ? null : id;
  if (expandedBook.value === id && !bookEntries.value[id]) {
    try {
      bookEntries.value[id] = await api.getWorldBookEntries(id);
    } catch (e) {
      msg.value = `加载世界书条目失败:${(e as Error).message}`;
    }
  }
}

/** 角色卡内嵌世界书:组内上移/下移(常态/激发各一组,不跨组) */
function moveCharEntry(entryId: number, dir: -1 | 1, constant: boolean): void {
  const group = charEntries.value.filter((e) => e.constant === constant);
  const idx = group.findIndex((e) => e.id === entryId);
  const to = idx + dir;
  if (idx < 0 || to < 0 || to >= group.length) return;
  [group[idx], group[to]] = [group[to], group[idx]];
  group.forEach((e, i) => (e.order = i));
  charEntries.value = [...charEntries.value];
  charEntriesDirty.value = true;
}

/** 角色卡内嵌世界书:设置注入角色 */
function setCharEntryRole(e: api.WorldBookEntry, val: string): void {
  e.role = (val === '' ? null : val) as api.WorldBookEntry['role'];
  charEntriesDirty.value = true;
}

/** 组内上移/下移:仅重排该组(常态/激发)条目的 order;组间相对位置由 constant 决定,不跨组 */
function moveWorldEntry(bookId: string, entryId: number, dir: -1 | 1, constant: boolean): void {
  const list = bookEntries.value[bookId] ?? [];
  const group = list.filter((e) => e.constant === constant);
  const idx = group.findIndex((e) => e.id === entryId);
  const to = idx + dir;
  if (idx < 0 || to < 0 || to >= group.length) return;
  [group[idx], group[to]] = [group[to], group[idx]];
  group.forEach((e, i) => (e.order = i));
  bookEntries.value[bookId] = [...list];
  dirtyBooks.value.add(bookId);
}

/** 设置世界书条目注入角色(空 = 自动:常驻→系统提示词,激发→用户) */
function setEntryRole(bookId: string, e: api.WorldBookEntry, val: string): void {
  e.role = (val === '' ? null : val) as api.WorldBookEntry['role'];
  dirtyBooks.value.add(bookId);
}

// ===== 位置4 楼层排序 =====
function moveFloor(id: string, dir: -1 | 1): void {
  const fs = floorsDraft.value;
  const idx = fs.findIndex((f) => f.id === id);
  const to = idx + dir;
  if (idx < 0 || to < 0 || to >= fs.length) return;
  [fs[idx], fs[to]] = [fs[to], fs[idx]];
  fs.forEach((f, i) => (f.order = i));
}

function addFloor(): void {
  const id = (crypto.randomUUID?.() ?? `f-${Date.now()}`) as string;
  floorsDraft.value.push({
    id,
    name: '新楼层',
    content: '',
    role: 'user',
    position: 'system',
    depth: 0,
    enabled: true,
    order: floorsDraft.value.length,
  });
  editingFloorId.value = id;
}

function removeFloor(id: string): void {
  floorsDraft.value = floorsDraft.value.filter((f) => f.id !== id);
  floorsDraft.value.forEach((f, i) => (f.order = i));
  if (editingFloorId.value === id) editingFloorId.value = null;
}

function onFloorDragStart(e: DragEvent, id: string): void {
  dragFloorId.value = id;
  if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
}
function onFloorDragOver(e: DragEvent, id: string): void {
  e.preventDefault();
  if (!dragFloorId.value || dragFloorId.value === id) return;
  const fs = floorsDraft.value;
  const from = fs.findIndex((f) => f.id === dragFloorId.value);
  const to = fs.findIndex((f) => f.id === id);
  if (from < 0 || to < 0) return;
  fs.splice(to, 0, fs.splice(from, 1)[0]);
  fs.forEach((f, i) => (f.order = i));
}
function onFloorDrop(e: DragEvent): void {
  e.preventDefault();
  dragFloorId.value = null;
}
function onFloorDragEnd(): void {
  dragFloorId.value = null;
}

// ===== 加载 =====
onMounted(async () => {
  await store.loadPromptInject();
  const cfg = store.promptInject;
  if (cfg) {
    injectMode.value = cfg.mode;
    floorsDraft.value = JSON.parse(JSON.stringify(cfg.floors)) as api.PromptFloor[];
    simpleDraft.value = JSON.parse(JSON.stringify(cfg.simple)) as api.SimplePromptConfig;
  }
  await store.loadWorldBooks();
  // 当前角色卡内嵌世界书(character_book)条目
  const cid = store.currentCharacterId;
  if (cid) {
    try {
      charEntries.value = await api.getCharacterWorldEntries(cid);
    } catch (e) {
      msg.value = `加载角色卡世界书失败:${(e as Error).message}`;
    }
  }
  charEntriesLoaded.value = true;
});

/** 跳转设置面板编辑主提示词 / 执行流程 */
function openSettings(): void {
  close();
  store.settingsOpen = true;
}

// ===== 保存 =====
async function saveAll(): Promise<void> {
  if (saving.value) return;
  saving.value = true;
  msg.value = '';
  try {
    // 1. 位置4:简单模式 + 楼层(全量写回 prompt_floors.json)
    if (store.promptInject) {
      const cfg: api.PromptInjectConfig = {
        mode: injectMode.value,
        simple: simpleDraft.value,
        floors: floorsDraft.value,
      };
      await store.savePromptInjectConfig(cfg);
    }
    // 2. 位置0:预设尾部 + 反思建议(含注入角色,写回 settings.json)
    await store.saveSettings({
      preset_tail_prompt: presetTailPrompt.value,
      preset_tail_role: presetTailRole.value,
      reflect_advice_prompt: reflectAdvicePrompt.value,
      reflect_advice_role: reflectAdviceRole.value,
    });
    // 3. 位置3/位置1:世界书条目顺序(逐本写回)
    for (const bookId of dirtyBooks.value) {
      const entries = bookEntries.value[bookId];
      if (entries) await api.saveWorldBookEntries(bookId, entries);
    }
    dirtyBooks.value.clear();
    // 4. 位置3/位置1:角色卡内嵌世界书(顺序/角色改动写回)
    const cid = store.currentCharacterId;
    if (charEntriesDirty.value && cid) {
      charEntries.value = await api.saveCharacterWorldEntries(cid, charEntries.value);
      charEntriesDirty.value = false;
    }
    msg.value = '提示词顺序已保存';
    setTimeout(() => (msg.value = ''), 2500);
  } catch (e) {
    msg.value = `保存失败:${(e as Error).message}`;
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal sv-modal-wide">
      <!-- 头部 -->
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" /> 提示词顺序管理
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-modal-body">
        <p class="sv-note">
          提示词按 6 大层注入,自上而下 位置5 → 位置0。各层内可自由调整顺序与消息角色;
          位置2 为历史上下文固定块,不可调整。保存后对所有会话生效。
        </p>

        <!-- 位置5 头部 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme red" /> 位置5 · 头部</div>
          <div class="sv-stack">
            <div class="sv-inp-row">
              <label class="sv-inp-tag">主 Agent 提示词</label>
              <span class="sv-note">角色:系统提示词(固定)</span>
            </div>
            <div class="pm-summary">{{ agentSystemPrompt ? truncate(agentSystemPrompt, 140) : '未自定义,使用内置默认模板(角色名 + 角色设定 + 世界书 + 回复要求)' }}</div>
            <div class="sv-btn-row">
              <button class="sv-btn ghost" @click="openSettings">去设置中编辑</button>
            </div>
          </div>
        </div>

        <!-- 位置4 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme orange" /> 位置4 · 模式注入与预设</div>
          <div class="sv-stack">
            <div class="sv-inp-row">
              <label class="sv-inp-tag">注入模式</label>
              <div class="sv-mode">
                <button :class="{ active: injectMode === 'simple' }" @click="injectMode = 'simple'">简单</button>
                <button :class="{ active: injectMode === 'complex' }" @click="injectMode = 'complex'">复杂</button>
              </div>
            </div>

            <!-- 禁词库:输出含禁用词时注入此提示词(所有模式);deep/agent/custom 另由引擎收尾工具同义替换 -->
            <div class="sv-inp-row" style="align-items: flex-start">
              <label class="sv-inp-tag">禁词库</label>
              <button
                class="sv-btn ghost"
                :class="{ 'sv-btn-on': simpleDraft.banned_words_enabled }"
                @click="simpleDraft.banned_words_enabled = !simpleDraft.banned_words_enabled"
              >
                {{ simpleDraft.banned_words_enabled ? '已启用' : '已关闭' }}
              </button>
              <textarea
                v-model="simpleDraft.banned_prompt"
                rows="2"
                class="sv-input"
                placeholder="输出中禁止出现以下词语,如:笨蛋、脏话、滚"
                :disabled="!simpleDraft.banned_words_enabled"
                spellcheck="false"
              />
            </div>
            <p class="sv-note">输出含禁用词时注入此提示词,对所有模式生效;deep/agent/custom 模式另由引擎收尾做同义替换。</p>

            <!-- 简单模式四项(可 ↑↓ 调整注入顺序) -->
            <template v-if="injectMode === 'simple'">
              <div v-for="(key, i) in orderedSimpleKeys" :key="key" class="sv-inp-row">
                <button class="sv-btn ghost sv-btn-square" title="上移" :disabled="i === 0" @click="moveSimple(key, -1)">↑</button>
                <button class="sv-btn ghost sv-btn-square" title="下移" :disabled="i === orderedSimpleKeys.length - 1" @click="moveSimple(key, 1)">↓</button>
                <label class="sv-inp-tag">{{ SIMPLE_ITEM_LABELS[key] }}</label>
                <template v-if="key === 'word_count'">
                  <button
                    class="sv-btn ghost"
                    :class="{ 'sv-btn-on': simpleDraft.word_count_enabled }"
                    @click="simpleDraft.word_count_enabled = !simpleDraft.word_count_enabled"
                  >
                    {{ simpleDraft.word_count_enabled ? '已启用' : '已关闭' }}
                  </button>
                  <input v-model.number="simpleDraft.word_count" type="number" min="1" max="100000" class="sv-input inject-num" :disabled="!simpleDraft.word_count_enabled" />
                </template>
                <template v-else-if="key === 'paraphrase'">
                  <button
                    class="sv-btn ghost"
                    :class="{ 'sv-btn-on': simpleDraft.paraphrase_enabled }"
                    @click="simpleDraft.paraphrase_enabled = !simpleDraft.paraphrase_enabled"
                  >
                    {{ simpleDraft.paraphrase_enabled ? '已启用' : '已关闭' }}
                  </button>
                  <span class="sv-note">用自己的话复述内容,不直接照抄原文</span>
                </template>
                <template v-else-if="key === 'dialogue'">
                  <button
                    class="sv-btn ghost"
                    :class="{ 'sv-btn-on': simpleDraft.dialogue_enabled }"
                    @click="simpleDraft.dialogue_enabled = !simpleDraft.dialogue_enabled"
                  >
                    {{ simpleDraft.dialogue_enabled ? '已启用' : '已关闭' }}
                  </button>
                  <span class="sv-note">只输出台词与必要动作描写,不输出旁白段落</span>
                </template>
                <template v-else>
                  <button
                    class="sv-btn ghost"
                    :class="{ 'sv-btn-on': simpleDraft.perspective_enabled }"
                    @click="simpleDraft.perspective_enabled = !simpleDraft.perspective_enabled"
                  >
                    {{ simpleDraft.perspective_enabled ? '已启用' : '已关闭' }}
                  </button>
                  <select v-model="simpleDraft.perspective" class="sv-select" :disabled="!simpleDraft.perspective_enabled">
                    <option>第一人称</option>
                    <option>第二人称</option>
                    <option>第三人称</option>
                  </select>
                </template>
              </div>
              <p class="sv-note">启用项合成一条注入提示词拼入系统提示词末尾;↑↓ 可调整各项注入顺序。</p>
            </template>

            <!-- 位置4 预设楼层(始终显示,可拖拽/↑↓ 排序;简单模式下不注入,切到复杂模式生效) -->
            <p class="sv-note">
              楼层按列表顺序注入(系统角色进系统提示词,用户/角色紧随其后);可拖拽或 ↑↓ 排序,消息角色可自由选择。
              <template v-if="injectMode === 'simple'"><b>当前为简单模式,楼层不注入</b>——切到「复杂」即生效。</template>
              <code v-pre>{{setvar::键::值}}</code> 持久化到会话变量,后续楼层可
              <code v-pre>{{getvar::键}}</code> 读取。
            </p>
            <div v-if="!floorsDraft.length" class="sv-note inject-empty">尚未添加楼层,点击下方「新增楼层」。</div>
            <div
              v-for="floor in floorsDraft"
              :key="floor.id"
              class="floor-row"
              :class="{ dragging: dragFloorId === floor.id }"
              draggable="true"
              @dragstart="onFloorDragStart($event, floor.id)"
              @dragover="onFloorDragOver($event, floor.id)"
              @drop="onFloorDrop"
              @dragend="onFloorDragEnd"
            >
              <span class="floor-grip" title="拖拽排序">⋮⋮</span>
              <button
                class="sv-btn ghost"
                :class="{ 'sv-btn-on': floor.enabled }"
                :title="floor.enabled ? '点击停用' : '点击启用'"
                @click="floor.enabled = !floor.enabled"
              >
                {{ floor.enabled ? '开' : '关' }}
              </button>
              <input v-model="floor.name" type="text" class="sv-input floor-name" placeholder="楼层名称" spellcheck="false" />
              <select v-model="floor.role" class="sv-select floor-select" title="消息角色">
                <option v-for="(label, val) in FLOOR_ROLE_LABELS" :key="val" :value="val">{{ label }}</option>
              </select>
              <div class="floor-actions">
                <button class="sv-btn ghost sv-btn-square" title="上移" @click="moveFloor(floor.id, -1)">↑</button>
                <button class="sv-btn ghost sv-btn-square" title="下移" @click="moveFloor(floor.id, 1)">↓</button>
                <button
                  class="sv-btn ghost sv-btn-square"
                  :title="editingFloorId === floor.id ? '收起编辑' : '编辑内容'"
                  @click="editingFloorId = editingFloorId === floor.id ? null : floor.id"
                >
                  ✎
                </button>
                <button class="sv-btn danger sv-btn-square" title="删除楼层" @click="removeFloor(floor.id)">✕</button>
              </div>
              <textarea
                v-if="editingFloorId === floor.id"
                v-model="floor.content"
                rows="5"
                class="sv-input floor-content"
                placeholder="楼层内容,支持酒馆宏。例如:{{char}}内心提醒:{{setvar::场景::酒馆}}今晚在{{getvar::场景}}见面。"
                spellcheck="false"
              />
            </div>
            <div class="sv-btn-row">
              <button class="sv-btn ghost sv-btn-fill" @click="addFloor">＋ 新增楼层</button>
            </div>

            <!-- agent 执行流程摘要 -->
            <div class="sv-inp-row">
              <label class="sv-inp-tag">执行流程</label>
              <span v-if="currentFlow" class="sv-note">{{ currentFlow.name }}({{ currentFlow.steps.length }} 步)按步骤注入,编辑在设置面板</span>
              <span v-else class="sv-note">未配置执行流程</span>
            </div>
          </div>
        </div>

        <!-- 位置3 角色卡 + 世界书常态 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme yellow" /> 位置3 · 角色卡与世界书常态</div>
          <div class="sv-stack">
            <div class="pm-summary">
              当前角色:{{ store.currentCharacterName }} — {{ truncate(store.currentCharacter?.description ?? '无角色描述', 100) }}
            </div>
            <!-- 角色卡内嵌世界书(character_book)常驻条目 -->
            <div class="pm-book">
              <div class="pm-book-head" @click="charBookExpanded = !charBookExpanded">
                <span class="pm-book-name">角色卡内嵌世界书</span>
                <span class="sv-note">{{ store.currentCharacterName }} · 常驻 {{ charConstantEntries().length }}</span>
                <span class="pm-book-caret">{{ charBookExpanded ? '▾' : '▸' }}</span>
              </div>
              <div v-if="charBookExpanded" class="pm-book-body">
                <div v-if="!charEntriesLoaded" class="sv-note">加载中…</div>
                <template v-else>
                  <div v-if="!charConstantEntries().length" class="sv-note">该角色卡未内嵌常驻世界书条目</div>
                  <div v-for="(e, i) in charConstantEntries()" :key="e.id" class="pm-entry-row">
                    <span class="pm-entry-name">{{ e.comment || `条目 ${e.id}` }}</span>
                    <select
                      class="sv-select pm-entry-role"
                      title="注入角色:自动 = 常驻→系统提示词"
                      :value="e.role ?? ''"
                      @change="setCharEntryRole(e, ($event.target as HTMLSelectElement).value)"
                    >
                      <option v-for="(label, val) in ENTRY_ROLE_LABELS" :key="val" :value="val">{{ label }}</option>
                    </select>
                    <span class="pm-entry-order">{{ i + 1 }}</span>
                    <button class="sv-btn ghost sv-btn-square" title="上移" :disabled="i === 0" @click="moveCharEntry(e.id, -1, true)">↑</button>
                    <button class="sv-btn ghost sv-btn-square" title="下移" :disabled="i === charConstantEntries().length - 1" @click="moveCharEntry(e.id, 1, true)">↓</button>
                  </div>
                </template>
              </div>
            </div>
            <div v-if="!worldBooks.length" class="sv-note inject-empty">
              暂无世界书,常驻条目为空 —
              <button class="sv-btn ghost" @click="store.worldBooksOpen = true; close()">去创建世界书</button>
            </div>
            <div v-for="book in worldBooks" :key="`c-${book.id}`" class="pm-book">
              <div class="pm-book-head" @click="toggleBook(book.id)">
                <span class="pm-book-name">{{ book.name }}</span>
                <span class="sv-note">{{ book.character_name ? `绑定:${book.character_name}` : '全局' }} · 常态 {{ constantEntries(book.id).length }}</span>
                <span class="pm-book-caret">{{ expandedBook === book.id ? '▾' : '▸' }}</span>
              </div>
              <div v-if="expandedBook === book.id" class="pm-book-body">
                <div v-if="!bookEntries[book.id]" class="sv-note">加载中…</div>
                <template v-else>
                  <div v-if="!constantEntries(book.id).length" class="sv-note">无常驻条目</div>
                  <div v-for="(e, i) in constantEntries(book.id)" :key="e.id" class="pm-entry-row">
                    <span class="pm-entry-name">{{ e.comment || `条目 ${e.id}` }}</span>
                    <select
                      class="sv-select pm-entry-role"
                      title="注入角色:自动 = 常驻→系统提示词"
                      :value="e.role ?? ''"
                      @change="setEntryRole(book.id, e, ($event.target as HTMLSelectElement).value)"
                    >
                      <option v-for="(label, val) in ENTRY_ROLE_LABELS" :key="val" :value="val">{{ label }}</option>
                    </select>
                    <span class="pm-entry-order">{{ i + 1 }}</span>
                    <button class="sv-btn ghost sv-btn-square" title="上移" :disabled="i === 0" @click="moveWorldEntry(book.id, e.id, -1, true)">↑</button>
                    <button class="sv-btn ghost sv-btn-square" title="下移" :disabled="i === constantEntries(book.id).length - 1" @click="moveWorldEntry(book.id, e.id, 1, true)">↓</button>
                  </div>
                </template>
              </div>
            </div>
          </div>
        </div>

        <!-- 位置2 历史上下文 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme blue" /> 位置2 · 历史上下文</div>
          <div class="pm-summary pm-fixed">对话历史原样保留,不可调整;超长时由服务端按上下文窗口从最旧消息起裁剪。</div>
        </div>

        <!-- 位置1 世界书激发 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme purple" /> 位置1 · 世界书激发</div>
          <div class="sv-stack">
            <p class="sv-note">关键词/正则命中后,条目内容追加到最新用户消息尾部(位置3 常态之后、位置0 之前)。</p>
            <!-- 角色卡内嵌世界书(character_book)激发条目 -->
            <div class="pm-book">
              <div class="pm-book-head" @click="charBookExpanded = !charBookExpanded">
                <span class="pm-book-name">角色卡内嵌世界书</span>
                <span class="sv-note">{{ store.currentCharacterName }} · 激发 {{ charTriggeredEntries().length }}</span>
                <span class="pm-book-caret">{{ charBookExpanded ? '▾' : '▸' }}</span>
              </div>
              <div v-if="charBookExpanded" class="pm-book-body">
                <div v-if="!charEntriesLoaded" class="sv-note">加载中…</div>
                <template v-else>
                  <div v-if="!charTriggeredEntries().length" class="sv-note">该角色卡未内嵌激发世界书条目</div>
                  <div v-for="(e, i) in charTriggeredEntries()" :key="e.id" class="pm-entry-row">
                    <span class="pm-entry-name">{{ e.comment || `条目 ${e.id}` }}</span>
                    <select
                      class="sv-select pm-entry-role"
                      title="注入角色:自动 = 激发→用户(追加最新用户消息尾部)"
                      :value="e.role ?? ''"
                      @change="setCharEntryRole(e, ($event.target as HTMLSelectElement).value)"
                    >
                      <option v-for="(label, val) in ENTRY_ROLE_LABELS" :key="val" :value="val">{{ label }}</option>
                    </select>
                    <span class="pm-entry-order">{{ i + 1 }}</span>
                    <button class="sv-btn ghost sv-btn-square" title="上移" :disabled="i === 0" @click="moveCharEntry(e.id, -1, false)">↑</button>
                    <button class="sv-btn ghost sv-btn-square" title="下移" :disabled="i === charTriggeredEntries().length - 1" @click="moveCharEntry(e.id, 1, false)">↓</button>
                  </div>
                  <div class="sv-btn-row">
                    <button class="sv-btn ghost" @click="store.worldBooksOpen = true; close()">在世界书中编辑条目</button>
                  </div>
                </template>
              </div>
            </div>
            <div v-if="!worldBooks.length" class="sv-note inject-empty">
              暂无世界书,激发条目为空 —
              <button class="sv-btn ghost" @click="store.worldBooksOpen = true; close()">去创建世界书</button>
            </div>
            <div v-for="book in worldBooks" :key="`t-${book.id}`" class="pm-book">
              <div class="pm-book-head" @click="toggleBook(book.id)">
                <span class="pm-book-name">{{ book.name }}</span>
                <span class="sv-note">{{ book.character_name ? `绑定:${book.character_name}` : '全局' }} · 激发 {{ triggeredEntries(book.id).length }}</span>
                <span class="pm-book-caret">{{ expandedBook === book.id ? '▾' : '▸' }}</span>
              </div>
              <div v-if="expandedBook === book.id" class="pm-book-body">
                <div v-if="!bookEntries[book.id]" class="sv-note">加载中…</div>
                <template v-else>
                  <div v-if="!triggeredEntries(book.id).length" class="sv-note">无激发条目</div>
                  <div v-for="(e, i) in triggeredEntries(book.id)" :key="e.id" class="pm-entry-row">
                    <span class="pm-entry-name">{{ e.comment || `条目 ${e.id}` }}</span>
                    <select
                      class="sv-select pm-entry-role"
                      title="注入角色:自动 = 激发→用户(追加最新用户消息尾部)"
                      :value="e.role ?? ''"
                      @change="setEntryRole(book.id, e, ($event.target as HTMLSelectElement).value)"
                    >
                      <option v-for="(label, val) in ENTRY_ROLE_LABELS" :key="val" :value="val">{{ label }}</option>
                    </select>
                    <span class="pm-entry-order">{{ i + 1 }}</span>
                    <button class="sv-btn ghost sv-btn-square" title="上移" :disabled="i === 0" @click="moveWorldEntry(book.id, e.id, -1, false)">↑</button>
                    <button class="sv-btn ghost sv-btn-square" title="下移" :disabled="i === triggeredEntries(book.id).length - 1" @click="moveWorldEntry(book.id, e.id, 1, false)">↓</button>
                  </div>
                  <div class="sv-btn-row">
                    <button class="sv-btn ghost" @click="store.worldBooksOpen = true; close()">在世界书中编辑条目</button>
                  </div>
                </template>
              </div>
            </div>
          </div>
        </div>

        <!-- 位置0 尾部 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme green" /> 位置0 · 尾部</div>
          <div class="sv-stack">
            <div class="pm-summary pm-disabled">
              Dvar 变量提示词 — 即将上线(本次仅留位;届时变量状态将以尾部块注入)
            </div>
            <div class="sv-inp-row">
              <label class="sv-inp-tag">预设尾部</label>
              <button
                class="sv-btn ghost"
                :class="{ 'sv-btn-on': presetTailPrompt.trim() !== '' }"
                @click="presetTailPrompt = presetTailPrompt.trim() ? '' : '以上是当前世界的补充设定,请结合以上全部内容继续。'"
              >
                {{ presetTailPrompt.trim() ? '已启用' : '已关闭' }}
              </button>
              <select v-model="presetTailRole" class="sv-select" title="注入角色:角色 = 追加为独立角色消息,用户 = 并入最新用户消息尾部">
                <option value="user">用户</option>
                <option value="assistant">角色</option>
              </select>
            </div>
            <textarea
              v-model="presetTailPrompt"
              rows="3"
              class="sv-input"
              placeholder="复杂模式预设尾部提示词:追加到最新用户消息尾部(位置1 激发之后)。支持酒馆宏。"
              spellcheck="false"
            />
            <div class="sv-inp-row">
              <label class="sv-inp-tag">反思建议</label>
              <button
                class="sv-btn ghost"
                :class="{ 'sv-btn-on': reflectAdvicePrompt.trim() !== '' }"
                @click="reflectAdvicePrompt = reflectAdvicePrompt.trim() ? '' : '修正后请继续保持角色设定与世界观一致。'"
              >
                {{ reflectAdvicePrompt.trim() ? '含补充' : '无补充' }}
              </button>
              <select v-model="reflectAdviceRole" class="sv-select" title="注入角色:角色 = 追加为独立角色消息,用户 = 并入最新用户消息尾部">
                <option value="user">用户</option>
                <option value="assistant">角色</option>
              </select>
            </div>
            <div class="pm-summary pm-disabled">
              反思未通过且放弃重试时,引擎自动生成本次改进建议(≤200 token,内容随失败原因变化),
              注入在位置1 激发之后、预设尾部之前 —— 不再位于最末尾,主体建议无需手填。
            </div>
            <textarea
              v-model="reflectAdvicePrompt"
              rows="2"
              class="sv-input"
              placeholder="可选补充说明:附加在自动生成的改进建议之后(留空 = 仅自动建议)。"
              spellcheck="false"
            />
            <p class="sv-note">自上而下依次注入:位置1 激发 → 反思反馈(仅反思失败时自动注入)→ 预设尾部。</p>
          </div>
        </div>
      </div>

      <!-- 底部 -->
      <div class="sv-modal-foot">
        <button class="sv-btn primary sv-btn-fill" :disabled="saving" @click="saveAll">
          {{ saving ? '保存中…' : '保存提示词顺序' }}
        </button>
        <div v-if="msg" class="sv-feedback-flex" :class="msg.includes('失败') ? 'err' : 'ok'">{{ msg }}</div>
      </div>
    </div>
  </div>
</template>
