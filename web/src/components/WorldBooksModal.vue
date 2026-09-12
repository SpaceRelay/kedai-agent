<script setup lang="ts">
// 世界书管理模态框:
//   - 角色卡内嵌世界书(character_book):当前角色内嵌条目展示 + 编辑(位置/关键词/常驻/激活)
//   - 独立世界书:上传/绑定/启用/删除 + 条目编辑(同酒馆 World Info 编辑器)
import { ref, computed, onMounted } from 'vue';
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';
import * as api from '../api';

const store = useAppStore();
const { worldBooks, characters, currentCharacter, currentCharacterId } = storeToRefs(store);

const fileInput = ref<HTMLInputElement | null>(null);
const uploadError = ref('');
const uploadMsg = ref('');
const busy = ref(false);
/** 上传时是否绑定当前选中角色 */
const bindCurrent = ref(true);

// ===== 角色卡内嵌世界书(当前角色) =====
const charEntries = ref<api.WorldBookEntry[]>([]);
const charEntriesLoaded = ref(false);
const charSaving = ref(false);
const charMsg = ref<{ kind: 'ok' | 'err'; text: string } | null>(null);

/** 归一注入角色:后端 role=None 序列化时省略(undefined),下拉「自动」选项值是 null,需对齐 */
function normalizeRoles(entries: api.WorldBookEntry[]): void {
  for (const e of entries) e.role = e.role ?? null;
}

async function loadCharacterEntries(): Promise<void> {
  const cid = currentCharacterId.value;
  if (!cid) return;
  charEntriesLoaded.value = false;
  charMsg.value = null;
  try {
    charEntries.value = await api.getCharacterWorldEntries(cid);
    normalizeRoles(charEntries.value);
  } catch (err) {
    charMsg.value = { kind: 'err', text: `加载角色卡世界书失败:${(err as Error).message}` };
  } finally {
    charEntriesLoaded.value = true;
  }
}

async function saveCharacterEntries(): Promise<void> {
  const cid = currentCharacterId.value;
  if (!cid || charSaving.value) return;
  charSaving.value = true;
  charMsg.value = null;
  try {
    charEntries.value = await api.saveCharacterWorldEntries(cid, charEntries.value);
    normalizeRoles(charEntries.value);
    charMsg.value = { kind: 'ok', text: '角色卡世界书已保存(下次发送生效)' };
    setTimeout(() => (charMsg.value = null), 2500);
  } catch (err) {
    charMsg.value = { kind: 'err', text: `保存失败:${(err as Error).message}` };
  } finally {
    charSaving.value = false;
  }
}

// ===== 独立世界书条目编辑 =====
/** 正在编辑条目的世界书 id */
const editingBookId = ref<string | null>(null);
/** 条目编辑草稿:bookId → entries */
const entryDrafts = ref<Record<string, api.WorldBookEntry[]>>({});
const entrySaving = ref<Record<string, boolean>>({});
const entryMsg = ref<{ kind: 'ok' | 'err'; text: string } | null>(null);

const editCount = computed(() => (editingBookId.value && entryDrafts.value[editingBookId.value] ? entryDrafts.value[editingBookId.value].length : 0));

async function openEntries(b: api.WorldBookRecord): Promise<void> {
  if (editingBookId.value === b.id) {
    editingBookId.value = null;
    return;
  }
  entryMsg.value = null;
  try {
    const entries = await api.getWorldBookEntries(b.id);
    normalizeRoles(entries);
    entryDrafts.value[b.id] = entries;
    editingBookId.value = b.id;
  } catch (err) {
    listOpError.value = `加载条目失败:${(err as Error).message}`;
  }
}

function addEntryRow(): void {
  const id = editingBookId.value;
  const draft = id && entryDrafts.value[id];
  if (!draft) return;
  const maxId = draft.reduce((m, e) => Math.max(m, e.id), 0);
  draft.push({ id: maxId + 1, comment: '新条目', keys: [], regex: null, use_regex: false, constant: false, enabled: true, content: '', position: 1 });
}

function addCharEntryRow(): void {
  const maxId = charEntries.value.reduce((m, e) => Math.max(m, e.id), 0);
  charEntries.value.push({ id: maxId + 1, comment: '新条目', keys: [], regex: null, use_regex: false, constant: false, enabled: true, content: '', position: 1 });
}

function removeEntryRow(draft: api.WorldBookEntry[], i: number): void {
  draft.splice(i, 1);
}

/** 常驻/触发切换并联动注入位置:常驻 → 位置3(常态),触发 → 位置1(激发),与 6 层规范对齐 */
function toggleConstant(en: api.WorldBookEntry): void {
  en.constant = !en.constant;
  en.position = en.constant ? 3 : 1;
}

/** 组内上移/下移(常态/激发各一组,不跨组);移动后重排 order */
function moveEntryRow(draft: api.WorldBookEntry[], i: number, dir: -1 | 1): void {
  const to = i + dir;
  if (to < 0 || to >= draft.length) return;
  [draft[i], draft[to]] = [draft[to], draft[i]];
  draft.forEach((e, idx) => (e.order = idx));
}

async function saveEntries(b: api.WorldBookRecord): Promise<void> {
  const id = editingBookId.value;
  const draft = id && entryDrafts.value[id];
  if (!id || !draft || entrySaving.value[id]) return;
  entrySaving.value[id] = true;
  entryMsg.value = null;
  try {
    const saved = await api.saveWorldBookEntries(id, draft);
    normalizeRoles(saved);
    entryDrafts.value[id] = saved;
    entryMsg.value = { kind: 'ok', text: '条目已保存(下次发送生效)' };
    setTimeout(() => (entryMsg.value = null), 2500);
  } catch (err) {
    entryMsg.value = { kind: 'err', text: `保存失败:${(err as Error).message}` };
  } finally {
    delete entrySaving.value[id];
  }
}

// ===== 通用:关键词输入(逗号分隔) ↔ keys 数组 =====
function keysToText(keys: string[]): string {
  return (keys ?? []).join(', ');
}
function textToKeys(text: string): string[] {
  return text
    .split(/[,，、\n]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

const close = (): void => {
  store.worldBooksOpen = false;
};

function sourceLabel(source: string): string {
  return source === 'character_card' ? '角色卡内嵌' : '独立';
}

async function onFilePicked(e: Event): Promise<void> {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  if (!file || busy.value) return;
  busy.value = true;
  uploadError.value = '';
  uploadMsg.value = '';
  try {
    const cid = bindCurrent.value ? currentCharacterId.value ?? undefined : undefined;
    const record = await store.uploadWorldBook(file, cid);
    const conv = record.conversion;
    const convText =
      conv && (conv.converted_count > 0 || conv.constant_auto_count > 0)
        ? `｜自动转换 ${conv.converted_count} 条(常态自动判定 ${conv.constant_auto_count} 条、关键词归一 ${conv.key_normalized_count} 条)`
        : '';
    uploadMsg.value = `已上传「${record.name}」,${record.entry_count} 条条目生效${convText}`;
  } catch (err) {
    uploadError.value = (err as Error).message;
  } finally {
    busy.value = false;
  }
}

// ===== 自动分配属性机制自检 =====
const autoCheck = ref<{ running: boolean; result: api.AutoAssignCheckResult | null; error: string | null }>({
  running: false,
  result: null,
  error: null,
});

async function runAutoAssignCheck(): Promise<void> {
  if (autoCheck.value.running) return;
  autoCheck.value = { running: true, result: null, error: null };
  try {
    autoCheck.value.result = await api.checkWorldBookAutoAssign(currentCharacterId.value ?? undefined);
  } catch (err) {
    autoCheck.value.error = `自检失败:${(err as Error).message}`;
  } finally {
    autoCheck.value.running = false;
  }
}

// ===== 独立世界书列表加载错误 =====
const listLoadError = ref<string>('');
/** 列表区操作错误(打开条目/启停/绑定/删除),就近显示在列表上方 */
const listOpError = ref<string>('');

async function toggleEnabled(b: api.WorldBookRecord): Promise<void> {
  try {
    listOpError.value = '';
    await store.updateWorldBook(b.id, { enabled: !b.enabled });
  } catch (err) {
    listOpError.value = `操作失败:${(err as Error).message}`;
  }
}

async function onBindChange(e: Event, b: api.WorldBookRecord): Promise<void> {
  const val = (e.target as HTMLSelectElement).value;
  if (val === (b.character_id ?? '')) return;
  try {
    listOpError.value = '';
    await store.updateWorldBook(b.id, { character_id: val });
  } catch (err) {
    listOpError.value = `绑定失败:${(err as Error).message}`;
  }
}

async function removeBook(b: api.WorldBookRecord): Promise<void> {
  if (!confirm(`确定删除世界书「${b.name}」?删除后不再注入。`)) return;
  try {
    listOpError.value = '';
    await store.deleteWorldBook(b.id);
    if (editingBookId.value === b.id) {
      editingBookId.value = null;
      delete entryDrafts.value[b.id];
    }
  } catch (err) {
    listOpError.value = `删除失败:${(err as Error).message}`;
  }
}

onMounted(() => {
  void loadWorldBooksChecked();
  void loadCharacterEntries();
});

/** 加载独立世界书列表并捕获失败(避免误显示「暂无独立世界书」) */
async function loadWorldBooksChecked(): Promise<void> {
  listLoadError.value = '';
  try {
    await store.loadWorldBooks();
  } catch (err) {
    listLoadError.value = `加载独立世界书失败:${(err as Error).message}`;
  }
}
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal wb-modal">
      <!-- 头部 -->
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" /> 世界书
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-modal-body">
        <!-- 说明 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme blue" /> 说明</div>
          <p class="sv-note" style="margin: 0; line-height: 1.8">
            世界书(World Info)在对话时按关键词命中注入设定。<br />
            <b>内嵌世界书</b>:角色卡内
            <code>character_book</code>
            条目随角色卡自动生效(常驻条目始终注入,其余按关键词匹配),可在此直接编辑。<br />
            <b>独立世界书</b>:上传 SillyTavern 世界书 JSON 后,默认全局启用,也可绑定到指定角色;<br />
            条目编辑完整支持酒馆惯例:<code>位置</code>/<code>注入顺序</code>(排序)、
            <code>关键词</code>/<code>副关键词</code>(子串匹配)、<code>正则</code>、
            <code>扫描深度</code>(扫最近 N 条)、<code>常驻</code>(始终注入)、
            <code>命中概率</code>、<code>大小写敏感</code>与<code>激活状态</code>(总开关)。<br />
            <b>状态定义</b>:<code>激活</code> = 该条目参与注入的总开关(停用 = 完全不注入);
            <code>触发</code> = 激活且非常驻的条目,按关键词/正则命中最近对话才注入;
            <code>常驻</code> = 始终注入,无需关键词命中。
          </p>
        </div>

        <!-- 自动分配机制自检 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme green" /> 自动分配机制</div>
          <p class="sv-note" style="margin: 0 0 8px; line-height: 1.8">
            条目「注入角色」选<code>自动</code>时按 常驻→系统提示词、激发→用户 自动分配;
            上传时自动转换关键词拆分、常态/激发判定与属性默认自动。可一键自检链路是否可用。
          </p>
          <div class="sv-stack">
            <button class="sv-btn ghost sv-btn-sm" :disabled="autoCheck.running" @click="runAutoAssignCheck">
              {{ autoCheck.running ? '检查中…' : '检查自动分配机制' }}
            </button>
            <div v-if="autoCheck.error" class="sv-feedback err" style="margin-top: 6px">{{ autoCheck.error }}</div>
            <div v-else-if="autoCheck.result" class="sv-wb-check">
              <div class="sv-feedback" :class="autoCheck.result.ok ? 'ok' : 'err'" style="margin: 6px 0">{{ autoCheck.result.summary }}</div>
              <div v-for="c in autoCheck.result.checks" :key="c.name" class="sv-wb-check-item">
                <span class="sv-tag" :class="c.status === 'ok' ? 'sv-tag-on' : ''">{{ c.status === 'ok' ? '正常' : '异常' }}</span>
                <span class="sv-wb-check-name">{{ c.label }}</span>
                <span class="sv-wb-check-detail">{{ c.detail }}</span>
              </div>
            </div>
          </div>
        </div>

        <!-- 角色卡内嵌世界书(当前角色) -->
        <div class="sv-field">
          <div class="sv-field-label">
            <span class="sv-supreme red" /> 角色卡内嵌世界书
            <span class="sv-wb-count">{{ currentCharacter?.chara_name ?? '未选择角色' }}</span>
          </div>
          <p v-if="currentCharacterId && !charEntriesLoaded" class="sv-note">加载中…</p>
          <div v-else-if="currentCharacterId && charMsg?.kind === 'err'" class="sv-empty" style="padding: 16px 12px">
            <div class="sv-empty-geo mb8">
              <span class="sq black" />
              <span class="sq pink" />
              <span class="sq deep" />
              <i class="diag" />
            </div>
            <p class="sv-note-mini err">{{ charMsg.text }}</p>
          </div>
          <div v-else-if="currentCharacterId && charEntries.length === 0" class="sv-empty" style="padding: 20px 12px">
            <div class="sv-empty-geo mb8">
              <span class="sq black" />
              <span class="sq pink" />
              <span class="sq deep" />
            <i class="diag" />
            </div>
            <p style="font-size: 12px">该角色卡未内嵌世界书</p>
            <p style="font-size: 11px">在角色卡 JSON 中提供 character_book 后即可在此编辑</p>
          </div>
          <div v-else-if="!currentCharacterId" class="sv-empty" style="padding: 20px 12px">
            <div class="sv-empty-geo mb8">
              <span class="sq black" />
              <span class="sq pink" />
              <span class="sq deep" />
              <i class="diag" />
            </div>
            <p style="font-size: 12px">请在左侧选择一个角色</p>
            <p style="font-size: 11px">选中后可查看/编辑其内嵌世界书</p>
          </div>
          <template v-else>
            <div class="sv-wb-edit">
              <div v-for="(en, i) in charEntries" :key="`c-${en.id}-${i}`" class="sv-wb-entry">
                <div class="sv-wb-entry-head">
                  <input v-model="en.comment" type="text" class="sv-input sv-wb-comment" placeholder="条目名称" spellcheck="false" />
                  <select v-model.number="en.position" class="sv-wb-select" title="注入位置(酒馆 position):常驻→位置3(并入系统提示词),激发→位置1(追加最新用户消息尾部)">
                    <option :value="0">0(before_char)</option>
                    <option :value="1">1(激发/追加用户消息)</option>
                    <option :value="2">2(normal)</option>
                    <option :value="3">3(常态/系统提示词)</option>
                    <option :value="4">4(after_char)</option>
                  </select>
                  <select
                    v-model="en.role"
                    class="sv-wb-select"
                    title="注入角色:自动 = 常驻→系统提示词,激发→用户"
                  >
                    <option :value="null">自动</option>
                    <option value="system">系统提示词</option>
                    <option value="user">用户</option>
                    <option value="assistant">角色</option>
                  </select>
                  <button
                    class="sv-btn ghost sv-btn-sm"
                    :class="{ 'sv-btn-on': en.constant }"
                    title="常驻:始终注入,位置3(并入系统提示词);触发:按关键词/正则命中,位置1(追加最新用户消息尾部)"
                    @click="toggleConstant(en)"
                  >
                    {{ en.constant ? '常驻' : '触发' }}
                  </button>
                  <button
                    class="sv-btn ghost sv-btn-sm"
                    :class="{ 'sv-btn-on': en.enabled }"
                    title="激活:该条目参与注入的总开关(停用 = 完全不注入)"
                    @click="en.enabled = !en.enabled"
                  >
                    {{ en.enabled ? '激活' : '停用' }}
                  </button>
                  <button
                    class="sv-btn ghost sv-btn-sm"
                    :class="{ 'sv-btn-on': en.case_sensitive }"
                    title="大小写敏感(酒馆 caseSensitive)"
                    @click="en.case_sensitive = !en.case_sensitive"
                  >
                    {{ en.case_sensitive ? 'Aa敏感' : 'Aa不敏' }}
                  </button>
                  <button class="sv-btn ghost sv-btn-sm" title="上移(组内排序)" :disabled="i === 0" @click="moveEntryRow(charEntries, i, -1)">↑</button>
                  <button class="sv-btn ghost sv-btn-sm" title="下移(组内排序)" :disabled="i === charEntries.length - 1" @click="moveEntryRow(charEntries, i, 1)">↓</button>
                  <button class="sv-btn danger sv-btn-sm" title="删除条目" @click="removeEntryRow(charEntries, i)">✕</button>
                </div>
                <div class="sv-wb-entry-fields">
                  <label class="sv-wb-field-tag">关键词</label>
                  <input :value="keysToText(en.keys)" type="text" class="sv-input" placeholder="逗号分隔;命中最近对话中的任意词即注入" spellcheck="false" @input="en.keys = textToKeys(($event.target as HTMLInputElement).value)" />
                </div>
                <div class="sv-wb-entry-fields">
                  <label class="sv-wb-field-tag">副关键词</label>
                  <input :value="keysToText(en.keys_secondary ?? [])" type="text" class="sv-input" placeholder="可选;与主关键词并列,命中其一即注入(酒馆 keysecondary)" spellcheck="false" @input="en.keys_secondary = textToKeys(($event.target as HTMLInputElement).value)" />
                </div>
                <div class="sv-wb-entry-fields">
                  <label class="sv-wb-field-tag">正则</label>
                  <input v-model="en.regex" type="text" class="sv-input" placeholder="可选;填写并勾选正则后优先于关键词匹配" spellcheck="false" />
                  <button
                    class="sv-btn ghost sv-btn-sm"
                    :class="{ 'sv-btn-on': en.use_regex }"
                    title="启用正则匹配"
                    :disabled="!en.regex"
                    @click="en.use_regex = !en.use_regex"
                  >
                    {{ en.use_regex ? '正则开' : '正则关' }}
                  </button>
                </div>
                <div class="sv-wb-entry-fields">
                  <label class="sv-wb-field-tag">扫描深度</label>
                  <input v-model.number="en.depth" type="number" min="0" class="sv-input sv-wb-num" title="从最新消息往前扫最近 N 条用户消息(0 = 全部历史)" placeholder="0=全部" />
                  <label class="sv-wb-field-tag">注入顺序</label>
                  <input v-model.number="en.order" type="number" min="0" class="sv-input sv-wb-num" title="同位置内按此值从小到大注入(酒馆 order)" placeholder="100" />
                  <label class="sv-wb-field-tag">命中概率</label>
                  <input v-model.number="en.probability" type="number" min="0" max="100" class="sv-input sv-wb-num" title="命中后按此百分比决定是否注入(酒馆 probability)" placeholder="100" />
                  <button
                    class="sv-btn ghost sv-btn-sm"
                    :class="{ 'sv-btn-on': en.use_probability }"
                    title="启用命中概率(100 = 恒注入)"
                    @click="en.use_probability = !en.use_probability"
                  >
                    {{ en.use_probability ? '概率开' : '概率关' }}
                  </button>
                </div>
                <div class="sv-wb-entry-fields">
                  <label class="sv-wb-field-tag">粘性</label>
                  <input v-model.number="en.sticky" type="number" min="0" class="sv-input sv-wb-num" title="命中后接下来 N 条消息持续注入(酒馆 sticky)" placeholder="0" />
                  <label class="sv-wb-field-tag">冷却</label>
                  <input v-model.number="en.cooldown" type="number" min="0" class="sv-input sv-wb-num" title="命中后 N 条消息内不再触发(酒馆 cooldown)" placeholder="0" />
                </div>
                <textarea v-model="en.content" rows="6" class="sv-input sv-wb-entry-content" placeholder="注入内容(留空则该条不注入)" spellcheck="false" />
              </div>
              <div class="sv-wb-edit-foot">
                <button class="sv-btn ghost sv-btn-sm" @click="addCharEntryRow">＋ 新增条目</button>
                <button class="sv-btn primary sv-btn-sm" :disabled="charSaving" @click="saveCharacterEntries">
                  {{ charSaving ? '保存中…' : '保存角色卡世界书' }}
                </button>
              </div>
            </div>
            <div v-if="charMsg" class="sv-feedback" :class="charMsg.kind">{{ charMsg.text }}</div>
          </template>
        </div>

        <!-- 上传 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme" /> 上传独立世界书</div>
          <div class="sv-stack">
            <label class="sv-wb-bind">
              <input v-model="bindCurrent" type="checkbox" />
              绑定到当前角色「{{ currentCharacter?.chara_name ?? '未选择角色' }}」
              <span class="sv-wb-bind-note">(不勾选则为全局世界书)</span>
            </label>
            <button class="sv-btn primary sv-btn-fill" :disabled="busy" @click="fileInput?.click()">
              {{ busy ? '上传中…' : '选择 JSON 文件上传' }}
            </button>
            <input ref="fileInput" type="file" accept=".json,application/json" class="hidden" @change="onFilePicked" />
            <p class="sv-note" style="margin-top: 6px">支持 SillyTavern 世界书导出格式(顶层 <code>entries</code> 对象/数组)与角色卡 <code>character_book</code> 结构。</p>
          </div>
          <div v-if="uploadError" class="sv-feedback err">{{ uploadError }}</div>
          <div v-if="uploadMsg" class="sv-feedback ok">{{ uploadMsg }}</div>
        </div>

        <!-- 独立世界书列表 -->
        <div class="sv-field">
          <div class="sv-field-label">
            <span class="sv-supreme yellow" /> 独立世界书列表
            <span class="sv-wb-count">{{ worldBooks.length }} 本</span>
          </div>
          <div v-if="listLoadError" class="sv-feedback err" style="margin: 8px 0">{{ listLoadError }}</div>
          <div v-else-if="listOpError" class="sv-feedback err" style="margin: 8px 0">{{ listOpError }}</div>
          <div v-else-if="worldBooks.length === 0" class="sv-empty" style="padding: 28px 12px">
            <div class="sv-empty-geo mb10">
              <span class="sq black" />
              <span class="sq pink" />
              <span class="sq deep" />
            <i class="diag" />
            </div>
            <p style="font-size: 12px">暂无独立世界书</p>
            <p style="font-size: 11px">上传 JSON 后即在此列出</p>
          </div>
          <div v-else class="sv-datalist">
            <div v-for="b in worldBooks" :key="b.id" class="sv-data-row sv-wb-row">
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-2">
                  <b>{{ b.name }}</b>
                  <span class="sv-tag">{{ sourceLabel(b.source) }}</span>
                  <span class="sv-tag sv-tag-muted">{{ b.entry_count }} 条</span>
                  <span v-if="b.enabled" class="sv-tag sv-tag-on">启用</span>
                </div>
                <div class="sv-wb-sub">
                  <span v-if="b.character_id" class="sv-wb-sub-item">绑定:{{ b.character_name ?? '角色' }}</span>
                  <span v-else class="sv-wb-sub-item">全局</span>
                  <span class="sv-wb-sub-item">{{ new Date(b.created_at).toLocaleString('zh-CN', { hour12: false }) }}</span>
                </div>
              </div>

              <div class="sv-wb-ops">
                <select
                  class="sv-wb-select"
                  :value="b.character_id ?? ''"
                  title="绑定角色(空=全局)"
                  @change="onBindChange($event, b)"
                >
                  <option value="">全局</option>
                  <option v-for="c in characters" :key="c.id" :value="c.id">{{ c.chara_name }}</option>
                </select>
                <button class="sv-btn ghost sv-btn-sm" @click="openEntries(b)">
                  {{ editingBookId === b.id ? '收起' : '条目' }}
                </button>
                <button class="sv-btn ghost sv-btn-sm" :class="{ 'sv-btn-on': b.enabled }" @click="toggleEnabled(b)">
                  {{ b.enabled ? '停用' : '启用' }}
                </button>
                <button class="sv-btn danger sv-btn-sm" @click="removeBook(b)">删除</button>
              </div>
            </div>
          </div>
        </div>

        <!-- 独立世界书条目编辑区 -->
        <div v-if="editingBookId && entryDrafts[editingBookId]" class="sv-field">
          <div class="sv-field-label">
            <span class="sv-supreme red" /> 编辑条目
            <span class="sv-wb-count">{{ worldBooks.find((b) => b.id === editingBookId)?.name ?? editingBookId }} · {{ editCount }} 条</span>
          </div>
          <p class="sv-note" style="margin-top: 0">
            <b>状态定义</b>:<code>激活</code> = 条目参与注入的总开关(停用则不注入);
            <code>常驻</code> = 始终注入,归位「位置3(常态)」并入系统提示词;<code>触发</code> = 激活且非常驻时按关键词/正则命中,归位「位置1(激发)」追加到最新用户消息尾部。<br />
            注入顺序:同位置内按列表顺序注入(可 ↑↓ 排序);关键词/副关键词:逗号分隔,命中最近对话即注入;
            扫描深度:只扫最近 N 条用户消息(0 = 全部);
            命中概率:按百分比随机决定是否注入;粘性/冷却:命中后 N 条内持续注入 / 不触发。
          </p>
          <div class="sv-wb-edit">
            <div v-for="(en, i) in entryDrafts[editingBookId]" :key="`e-${editingBookId}-${en.id}-${i}`" class="sv-wb-entry">
              <div class="sv-wb-entry-head">
                <input v-model="en.comment" type="text" class="sv-input sv-wb-comment" placeholder="条目名称" spellcheck="false" />
                <select v-model.number="en.position" class="sv-wb-select" title="注入位置(酒馆 position):常驻→位置3(并入系统提示词),激发→位置1(追加最新用户消息尾部)">
                  <option :value="0">0(before_char)</option>
                  <option :value="1">1(激发/追加用户消息)</option>
                  <option :value="2">2(normal)</option>
                  <option :value="3">3(常态/系统提示词)</option>
                  <option :value="4">4(after_char)</option>
                </select>
                <select
                  v-model="en.role"
                  class="sv-wb-select"
                  title="注入角色:自动 = 常驻→系统提示词,激发→用户"
                >
                  <option :value="null">自动</option>
                  <option value="system">系统提示词</option>
                  <option value="user">用户</option>
                  <option value="assistant">角色</option>
                </select>
                <button
                  class="sv-btn ghost sv-btn-sm"
                  :class="{ 'sv-btn-on': en.constant }"
                  title="常驻:始终注入,位置3(并入系统提示词);触发:按关键词/正则命中,位置1(追加最新用户消息尾部)"
                  @click="toggleConstant(en)"
                >
                  {{ en.constant ? '常驻' : '触发' }}
                </button>
                <button
                  class="sv-btn ghost sv-btn-sm"
                  :class="{ 'sv-btn-on': en.enabled }"
                  title="激活:该条目参与注入的总开关(停用 = 完全不注入)"
                  @click="en.enabled = !en.enabled"
                >
                  {{ en.enabled ? '激活' : '停用' }}
                </button>
                <button
                  class="sv-btn ghost sv-btn-sm"
                  :class="{ 'sv-btn-on': en.case_sensitive }"
                  title="大小写敏感(酒馆 caseSensitive)"
                  @click="en.case_sensitive = !en.case_sensitive"
                >
                  {{ en.case_sensitive ? 'Aa敏感' : 'Aa不敏' }}
                </button>
                <button class="sv-btn ghost sv-btn-sm" title="上移(组内排序)" :disabled="i === 0" @click="moveEntryRow(entryDrafts[editingBookId], i, -1)">↑</button>
                <button class="sv-btn ghost sv-btn-sm" title="下移(组内排序)" :disabled="i === entryDrafts[editingBookId].length - 1" @click="moveEntryRow(entryDrafts[editingBookId], i, 1)">↓</button>
                <button class="sv-btn danger sv-btn-sm" title="删除条目" @click="removeEntryRow(entryDrafts[editingBookId], i)">✕</button>
              </div>
              <div class="sv-wb-entry-fields">
                <label class="sv-wb-field-tag">关键词</label>
                <input :value="keysToText(en.keys)" type="text" class="sv-input" placeholder="逗号分隔;留空且非常驻则需正则命中" spellcheck="false" @input="en.keys = textToKeys(($event.target as HTMLInputElement).value)" />
              </div>
              <div class="sv-wb-entry-fields">
                <label class="sv-wb-field-tag">副关键词</label>
                <input :value="keysToText(en.keys_secondary ?? [])" type="text" class="sv-input" placeholder="可选;与主关键词并列,命中其一即注入(酒馆 keysecondary)" spellcheck="false" @input="en.keys_secondary = textToKeys(($event.target as HTMLInputElement).value)" />
              </div>
              <div class="sv-wb-entry-fields">
                <label class="sv-wb-field-tag">正则</label>
                <input v-model="en.regex" type="text" class="sv-input" placeholder="可选;填写并开启后优先于关键词" spellcheck="false" />
                <button
                  class="sv-btn ghost sv-btn-sm"
                  :class="{ 'sv-btn-on': en.use_regex }"
                  title="启用正则匹配"
                  :disabled="!en.regex"
                  @click="en.use_regex = !en.use_regex"
                >
                  {{ en.use_regex ? '正则开' : '正则关' }}
                </button>
              </div>
              <div class="sv-wb-entry-fields">
                <label class="sv-wb-field-tag">扫描深度</label>
                <input v-model.number="en.depth" type="number" min="0" class="sv-input sv-wb-num" title="从最新消息往前扫最近 N 条用户消息(0 = 全部历史)" placeholder="0=全部" />
                <label class="sv-wb-field-tag">注入顺序</label>
                <input v-model.number="en.order" type="number" min="0" class="sv-input sv-wb-num" title="同位置内按此值从小到大注入(酒馆 order)" placeholder="100" />
                <label class="sv-wb-field-tag">命中概率</label>
                <input v-model.number="en.probability" type="number" min="0" max="100" class="sv-input sv-wb-num" title="命中后按此百分比决定是否注入(酒馆 probability)" placeholder="100" />
                <button
                  class="sv-btn ghost sv-btn-sm"
                  :class="{ 'sv-btn-on': en.use_probability }"
                  title="启用命中概率(100 = 恒注入)"
                  @click="en.use_probability = !en.use_probability"
                >
                  {{ en.use_probability ? '概率开' : '概率关' }}
                </button>
              </div>
              <div class="sv-wb-entry-fields">
                <label class="sv-wb-field-tag">粘性</label>
                <input v-model.number="en.sticky" type="number" min="0" class="sv-input sv-wb-num" title="命中后接下来 N 条消息持续注入(酒馆 sticky)" placeholder="0" />
                <label class="sv-wb-field-tag">冷却</label>
                <input v-model.number="en.cooldown" type="number" min="0" class="sv-input sv-wb-num" title="命中后 N 条消息内不再触发(酒馆 cooldown)" placeholder="0" />
              </div>
              <textarea v-model="en.content" rows="6" class="sv-input sv-wb-entry-content" placeholder="注入内容(留空则该条不注入)" spellcheck="false" />
            </div>
            <div class="sv-wb-edit-foot">
              <button class="sv-btn ghost sv-btn-sm" @click="addEntryRow">＋ 新增条目</button>
              <button
                class="sv-btn primary sv-btn-sm"
                :disabled="!!entrySaving[editingBookId]"
                @click="saveEntries(worldBooks.find((b) => b.id === editingBookId)!)"
              >
                {{ entrySaving[editingBookId] ? '保存中…' : '保存条目' }}
              </button>
            </div>
          </div>
          <div v-if="entryMsg" class="sv-feedback" :class="entryMsg.kind">{{ entryMsg.text }}</div>
        </div>
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>
