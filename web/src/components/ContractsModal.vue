<script setup lang="ts">
// 契约编辑弹窗(P6 面板):查看/编辑角色卡内嵌契约(extensions.nlkaleido)。
// 左侧 JSON 编辑器(带格式化),右侧保存前 diff 预览;保存走后端 parse_contract 校验,
// 422 错误原样回显。无契约时提供「从模板新建」。
import { computed, onMounted, ref, watch } from 'vue';
import { useAppStore } from '../store';
import {
  deleteCharacterContract,
  getCharacterContract,
  getContractHistory,
  putCharacterContract,
  rollbackContract,
  type CharacterContractStatus,
  type ContractChangeEntry,
} from '../api/contracts';
import { diffContracts, formatDiffItem } from '../contracts/contractDiff';

const store = useAppStore();

const close = (): void => {
  store.contractsOpen = false;
};

const characterId = computed(() => store.currentCharacterId);
const msg = ref<{ kind: 'ok' | 'err' | 'info'; text: string } | null>(null);
const busy = ref(false);
/** 服务端返回的原始契约(保存成功的基准) */
const saved = ref<Record<string, unknown> | null>(null);
/** 存量契约已损坏(解析失败)警示 */
const savedInvalid = ref(false);
const editorText = ref('');
/** 编辑器是否偏离已格式化状态(控制 diff 基准提示) */
const loaded = ref(false);

/** 当前激活的 tab:edit=编辑 / history=变更历史 */
const activeTab = ref<'edit' | 'history'>('edit');
/** 历史是否已加载过(懒加载标记,切换角色后失效) */
const historyLoaded = ref(false);
const historyLoading = ref(false);
const historyEntries = ref<ContractChangeEntry[]>([]);

/** 变更来源徽标中文映射(与后端 ChangelogSource 对齐) */
const SOURCE_LABELS: Record<ContractChangeEntry['source'], string> = {
  agent: 'Agent 自动',
  manual: '手动保存',
  contract_init: '首次挂载',
  rollback: '回滚',
  import: '导入',
  repair: '修复',
};

/** 模板契约:最小可运行示例(与后端测试契约同构) */
function template(): Record<string, unknown> {
  return {
    version: 1,
    id: `${characterId.value ?? 'card'}-contract`,
    schema: { properties: {} },
    updateRules: {
      好感度: { path: '好感度', type: 'number', default: 0, updateMode: 'every_turn', display: true },
    },
    displayRules: [{ path: '好感度', render: 'value' }],
    guardrails: {},
  };
}

function stringify(v: unknown): string {
  return JSON.stringify(v, null, 2);
}

async function reload(): Promise<void> {
  if (!characterId.value) return;
  busy.value = true;
  try {
    const status: CharacterContractStatus = await getCharacterContract(characterId.value);
    saved.value = status.contract;
    savedInvalid.value = !status.contract ? false : !status.valid;
    editorText.value = status.contract ? stringify(status.contract) : '';
    loaded.value = true;
    if (!status.contract) {
      msg.value = { kind: 'info', text: '当前角色卡未内嵌契约(世界书来源契约不在此编辑)。可从模板新建。' };
    } else if (!status.valid) {
      msg.value = { kind: 'err', text: '已存契约无法通过服务端校验,保存前请修正。' };
    } else {
      msg.value = null;
    }
  } catch (err) {
    msg.value = { kind: 'err', text: `读取失败:${(err as Error).message}` };
  } finally {
    busy.value = false;
  }
}

/** 编辑器当前 JSON(解析失败返回 null) */
const editing = computed<Record<string, unknown> | null>(() => {
  if (!editorText.value.trim()) return null;
  try {
    const v = JSON.parse(editorText.value);
    return typeof v === 'object' && v !== null && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
});

const jsonError = computed(() => {
  if (!editorText.value.trim() || editing.value) return '';
  return 'JSON 语法错误';
});

const diffItems = computed(() => {
  if (!editing.value) return [];
  return diffContracts(saved.value, editing.value);
});

async function onSave(): Promise<void> {
  if (!characterId.value || !editing.value) return;
  busy.value = true;
  msg.value = null;
  try {
    const res = await putCharacterContract(characterId.value, editing.value);
    saved.value = editing.value;
    savedInvalid.value = false;
    msg.value = res.warning
      ? { kind: 'err', text: res.warning }
      : { kind: 'ok', text: '契约已保存(缓存已失效,下一轮生成即生效)' };
  } catch (err) {
    msg.value = { kind: 'err', text: `保存失败:${(err as Error).message}` };
  } finally {
    busy.value = false;
  }
}

function onFormat(): void {
  if (!editing.value) return;
  editorText.value = stringify(editing.value);
}

function onTemplate(): void {
  editorText.value = stringify(template());
  msg.value = { kind: 'info', text: '已填入模板契约,按需修改后保存。' };
}

async function onRemove(): Promise<void> {
  if (!characterId.value) return;
  if (!confirm('确定移除角色卡内嵌契约?角色卡其余内容保留。')) return;
  busy.value = true;
  msg.value = null;
  try {
    const res = await deleteCharacterContract(characterId.value);
    saved.value = null;
    savedInvalid.value = false;
    editorText.value = '';
    msg.value = res?.warning
      ? { kind: 'err', text: res.warning }
      : { kind: 'ok', text: '已移除内嵌契约' };
  } catch (err) {
    msg.value = { kind: 'err', text: `移除失败:${(err as Error).message}` };
  } finally {
    busy.value = false;
  }
}

onMounted(reload);
watch(characterId, () => {
  // 切换角色后历史缓存失效:重置懒加载标记;若正停在历史 tab 则直接重拉
  historyLoaded.value = false;
  historyEntries.value = [];
  void reload();
  if (activeTab.value === 'history') void loadHistory();
});
watch(activeTab, (tab) => {
  // 进入历史 tab 时懒加载(仅首次,之后靠「刷新」按钮重拉)
  if (tab === 'history' && !historyLoaded.value) void loadHistory();
});

/** 拉取契约变更历史(失败走 msg 反馈,风格与 onSave 一致) */
async function loadHistory(): Promise<void> {
  if (!characterId.value) return;
  historyLoading.value = true;
  try {
    const res = await getContractHistory(characterId.value);
    historyEntries.value = res.entries;
    historyLoaded.value = true;
    msg.value = null;
  } catch (err) {
    msg.value = { kind: 'err', text: `历史加载失败:${(err as Error).message}` };
  } finally {
    historyLoading.value = false;
  }
}

/** 单条历史记录的变更摘要(复用 diffContracts 计数) */
function historySummary(entry: ContractChangeEntry): string {
  if (!entry.after) return '移除契约';
  if (!entry.before) return '首次挂载契约';
  const n = diffContracts(entry.before, entry.after).length;
  return n > 0 ? `保存契约(${n} 处变更)` : '保存契约(无内容差异)';
}

function formatTime(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

/** 回滚到指定历史版本:确认后调后端,成功后刷新历史与编辑基准 */
async function onRollback(entry: ContractChangeEntry): Promise<void> {
  if (!characterId.value || !entry.after) return;
  if (!confirm('恢复后当前契约将被该版本覆盖,可在历史中回滚')) return;
  // 回滚期间同时锁编辑 tab 的保存/移除,避免与写卡交错
  historyLoading.value = true;
  busy.value = true;
  msg.value = null;
  try {
    const res = await rollbackContract(characterId.value, entry.seq);
    await loadHistory();
    await reload();
    msg.value = res.warning
      ? { kind: 'err', text: res.warning }
      : { kind: 'ok', text: `已恢复到 seq ${entry.seq} 的契约版本(生成新记录)` };
  } catch (err) {
    msg.value = { kind: 'err', text: `回滚失败:${(err as Error).message}` };
  } finally {
    historyLoading.value = false;
    busy.value = false;
  }
}
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal lg">
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" /> 契约编辑
          <span v-if="characterId" class="sv-char-desc" style="font-size: 12px">
            {{ store.currentCharacterName }}
          </span>
          <span v-if="savedInvalid" class="sv-badge-off">已存契约校验失败</span>
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-modal-body flex flex-col" style="gap: 10px; min-height: 0">
        <p v-if="!characterId" class="sv-note">请先在左侧选择角色。</p>
        <template v-else>
          <div class="flex items-center gap-2" style="flex: none">
            <button class="sv-btn ghost sv-btn-sm" :class="{ 'sv-btn-on': activeTab === 'edit' }" @click="activeTab = 'edit'">
              编辑
            </button>
            <button
              class="sv-btn ghost sv-btn-sm"
              :class="{ 'sv-btn-on': activeTab === 'history' }"
              @click="activeTab = 'history'"
            >
              历史
            </button>
          </div>

          <div v-if="msg" class="sv-feedback" :class="msg.kind === 'err' ? 'err' : msg.kind === 'ok' ? 'ok' : ''" style="flex: none">
            {{ msg.text }}
          </div>

          <template v-if="activeTab === 'edit'">
            <div class="flex items-center gap-2" style="flex: none">
              <button class="sv-btn primary sv-btn-sm" :disabled="busy || !editing || !diffItems.length" @click="onSave">
                保存{{ diffItems.length ? `(${diffItems.length} 处变更)` : '' }}
              </button>
              <button class="sv-btn ghost sv-btn-sm" :disabled="!editing" @click="onFormat">格式化</button>
              <button v-if="!editing && !editorText.trim()" class="sv-btn ghost sv-btn-sm" @click="onTemplate">
                从模板新建
              </button>
              <button v-if="saved" class="sv-btn danger sv-btn-sm" :disabled="busy" @click="onRemove">移除契约</button>
            </div>

            <div class="flex" style="gap: 10px; flex: 1; min-height: 0">
              <!-- JSON 编辑器 -->
              <div class="flex flex-col" style="flex: 1; min-width: 0; gap: 4px">
                <textarea
                  v-model="editorText"
                  class="sv-contract-editor"
                  spellcheck="false"
                  placeholder='{"version":1,"id":"…","schema":{"properties":{}},"updateRules":{…}}'
                />
                <div v-if="jsonError" class="sv-feedback err" style="flex: none">{{ jsonError }}</div>
              </div>

              <!-- diff 预览 -->
              <div class="sv-contract-diff" style="flex: none">
                <div style="font-size: 11px; color: var(--sv-ink-faint); letter-spacing: 0.1em; margin-bottom: 6px">
                  变更预览
                </div>
                <div v-if="!diffItems.length" class="sv-note" style="font-size: 12px">无变更</div>
                <div v-for="(item, i) in diffItems" :key="i" class="sv-contract-diff-item" :class="item.kind">
                  {{ formatDiffItem(item) }}
                </div>
              </div>
            </div>

            <p class="sv-note" style="flex: none; margin: 0; font-size: 11px">
              契约保存在角色卡 extensions.nlkaleido,随卡导出;保存后服务端契约缓存立即失效,下一轮生成即用新契约。
              结构说明:version/id/schema/updateRules(字段定义)/displayRules/guardrails/invariants。
            </p>
          </template>

          <!-- 历史 tab:变更记录列表 + 恢复入口 -->
          <div v-else class="flex flex-col" style="flex: 1; min-height: 0; gap: 10px">
            <div class="flex items-center gap-2" style="flex: none">
              <button class="sv-btn ghost sv-btn-sm" :disabled="historyLoading" @click="loadHistory">刷新</button>
              <span v-if="historyLoading" style="font-size: 12px; color: var(--sv-ink-faint)">加载中…</span>
            </div>

            <div v-if="historyLoaded && !historyEntries.length" class="sv-note" style="font-size: 12px">
              暂无变更历史
            </div>
            <div v-else-if="historyLoaded" class="sv-history-list">
              <div v-for="entry in historyEntries" :key="entry.seq" class="sv-history-item">
                <div class="sv-history-head">
                  <span class="sv-history-time">{{ formatTime(entry.createdAt) }}</span>
                  <span class="sv-history-source">{{ SOURCE_LABELS[entry.source] }}</span>
                </div>
                <div class="sv-history-summary" :class="{ removed: !entry.after }">{{ historySummary(entry) }}</div>
                <div v-if="entry.rationale" class="sv-history-rationale">说明:{{ entry.rationale }}</div>
                <div v-if="entry.after" style="display: flex; justify-content: flex-end">
                  <button class="sv-btn ghost sv-btn-sm" :disabled="historyLoading" @click="onRollback(entry)">
                    恢复此版本
                  </button>
                </div>
              </div>
            </div>

            <p class="sv-note" style="flex: none; margin: 0; font-size: 11px">
              此处显示该角色契约的每次变更记录(最新在前);「恢复此版本」会以该快照覆盖当前契约,
              并生成一条新的「回滚」记录。移除类记录(after 为空)不可恢复。
            </p>
          </div>
        </template>
      </div>
    </div>
  </div>
</template>

<style scoped>
.sv-contract-editor {
  width: 100%;
  height: 100%;
  min-height: 280px;
  resize: none;
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.5;
  padding: 10px;
  border: 2px solid var(--sv-ink);
  background: var(--sv-white);
  color: var(--sv-ink);
  outline: none;
}
.sv-contract-editor:focus {
  border-color: var(--sv-pink-deep);
  box-shadow: 3px 3px 0 rgba(var(--sv-pink-deep-rgb), 0.2);
}

.sv-contract-diff {
  width: 300px;
  max-height: 100%;
  overflow-y: auto;
  padding: 10px;
  border: 2px solid var(--sv-line-strong);
  background: var(--sv-surface-elevated);
}

.sv-contract-diff-item {
  font-family: var(--font-mono);
  font-size: 11px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
  color: var(--sv-ink-faint);
}

.sv-contract-diff-item.added {
  color: var(--sv-green);
}

.sv-contract-diff-item.removed {
  color: var(--sv-red-deep);
  text-decoration: line-through;
}

.sv-contract-diff-item.changed {
  color: var(--sv-yellow-deep);
}

/* 历史 tab:变更记录列表 */
.sv-history-list {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.sv-history-item {
  flex: none;
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 10px;
  border: 2px solid var(--sv-line-strong);
  background: var(--sv-white);
}

.sv-history-head {
  display: flex;
  align-items: center;
  gap: 8px;
}

.sv-history-time {
  font-size: 11px;
  color: var(--sv-ink-faint);
}

/* 变更来源徽章(中性小徽章,风格对齐 sv-tag) */
.sv-history-source {
  flex: none;
  font-size: 11px;
  padding: 1px 8px;
  border: 2px solid var(--sv-line);
  color: var(--sv-ink-dim);
  background: var(--sv-white);
}

.sv-history-summary {
  font-size: 12px;
  color: var(--sv-ink);
}

.sv-history-summary.removed {
  color: var(--sv-red);
  text-decoration: line-through;
}

.sv-history-rationale {
  font-size: 11px;
  color: var(--sv-ink-faint);
  font-style: italic;
}
</style>
