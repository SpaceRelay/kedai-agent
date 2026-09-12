// 记忆库面板交互逻辑(优化面板「记忆库」分区):
// 状态(列表 / 搜索防抖 / kind 筛选 / 蒸馏 / 添加 / 行内编辑 / 注入与置顶开关 /
// 两段式删除与清理归档)与全部操作方法集中在此 composable,MemoryPanel.vue 仅做模板
// 绑定——便于零 DOM 依赖直测交互链路(项目无 jsdom / @vue/test-utils,渲染走
// renderToString,交互走本文件测试)。
// 角色与会话经 getter 传入(组件传 props 的响应式读取),反馈文案复用 memoryPanel.ts 纯函数。
import { computed, ref, watch } from 'vue';
import * as api from '../api';
import {
  distillErrorText,
  distillSuccessText,
  filterRows,
  memoryRows,
  nextPendingDelete,
  nextPendingPrune,
  pruneSuccessText,
  type KindFilter,
  type MemoryRow,
} from '../components/memoryPanel';

/** 面板反馈条(ok / err 两色,sv-feedback 现成样式) */
export interface PanelMessage {
  kind: 'ok' | 'err';
  text: string;
}

/** 搜索防抖延迟(ms):输入停止后触发检索,避免逐字打请求 */
export const SEARCH_DEBOUNCE_MS = 250;

export function useMemoryPanel(getIds: {
  characterId: () => string | null | undefined;
  sessionId: () => string | null | undefined;
}) {
  const loading = ref(false);
  const error = ref('');
  const entries = ref<api.MemoryEntry[]>([]);
  /** 列表展示行(置顶优先,其次 id 降序;映射逻辑在 memoryPanel.ts) */
  const rows = computed(() => memoryRows(entries.value));

  /** 搜索关键词(空 = 全量列表)与 kind 筛选(纯本地) */
  const query = ref('');
  const kindFilter = ref<KindFilter>('all');
  /** 筛选后的展示行(模板只消费此值) */
  const shownRows = computed(() => filterRows(rows.value, kindFilter.value, query.value));
  /** 搜索是否进行中(仅非空查询;用于输入框/空态文案) */
  const searching = ref(false);

  /** 蒸馏反馈(成功新增 N 条 / 400 未开启指引) */
  const distilling = ref(false);
  const distillMsg = ref<PanelMessage | null>(null);

  /** 添加 / 编辑 / 删除 / 置顶 / 清理共用操作反馈 */
  const actionMsg = ref<PanelMessage | null>(null);
  const newContent = ref('');
  const adding = ref(false);

  /** 行内编辑:editingId 非空时该行 content 换成输入框 */
  const editingId = ref<number | null>(null);
  const editContent = ref('');
  const savingEdit = ref(false);

  /** 删除两段式确认:pendingDelete 非空时该行显示「确认删除 / 取消」 */
  const pendingDelete = ref<number | null>(null);
  const deleting = ref(false);

  /** 清理归档两段式确认:true 时「清理已归档」换成「确认清理 / 取消」 */
  const pendingPrune = ref(false);
  const pruning = ref(false);

  /**
   * 拉取当前角色记忆列表;query 非空时走 /memory/search(FTS5),空查询回退全量列表。
   * 无角色时清空并跳过请求。
   */
  async function load(): Promise<void> {
    const characterId = getIds.characterId();
    if (!characterId) {
      entries.value = [];
      return;
    }
    const q = query.value.trim();
    const isSearch = q !== '';
    if (isSearch) searching.value = true;
    else loading.value = true;
    error.value = '';
    try {
      entries.value = isSearch
        ? await api.searchMemories(characterId, q)
        : await api.listMemories(characterId);
    } catch (e) {
      error.value = `${isSearch ? '搜索失败' : '加载失败'}:${(e as Error).message}`;
    } finally {
      loading.value = false;
      searching.value = false;
    }
  }

  /** 搜索防抖:query 变化 250ms 后重新拉取(空查询自动回退全量列表) */
  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  watch(query, () => {
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      searchTimer = null;
      void load();
    }, SEARCH_DEBOUNCE_MS);
  });

  /** 立即触发一次检索(输入框回车/搜索按钮),并取消待执行的防抖 */
  async function searchNow(): Promise<void> {
    if (searchTimer) {
      clearTimeout(searchTimer);
      searchTimer = null;
    }
    await load();
  }

  /** 切换 kind 筛选(纯本地,无需请求) */
  function setKindFilter(kind: KindFilter): void {
    kindFilter.value = kind;
  }

  /** 蒸馏当前会话:成功反馈新增条数并刷新;失败(含未开启 400)走指引文案 */
  async function distillNow(): Promise<void> {
    if (distilling.value) return;
    const sessionId = getIds.sessionId();
    if (!sessionId) {
      distillMsg.value = { kind: 'err', text: '蒸馏失败:当前无会话,无法蒸馏' };
      return;
    }
    distilling.value = true;
    distillMsg.value = null;
    try {
      const res = await api.distillMemory(sessionId);
      distillMsg.value = { kind: 'ok', text: distillSuccessText(res.inserted) };
      await load();
    } catch (e) {
      distillMsg.value = { kind: 'err', text: distillErrorText((e as Error).message) };
    } finally {
      distilling.value = false;
    }
  }

  /** 手动补录一条记忆(kind='manual'),成功后清空输入并刷新 */
  async function addNow(): Promise<void> {
    if (adding.value) return;
    const characterId = getIds.characterId();
    if (!characterId) return;
    const content = newContent.value.trim();
    if (!content) {
      actionMsg.value = { kind: 'err', text: '记忆内容不能为空' };
      return;
    }
    adding.value = true;
    actionMsg.value = null;
    try {
      await api.createMemory(characterId, content);
      newContent.value = '';
      await load();
    } catch (e) {
      actionMsg.value = { kind: 'err', text: `添加失败:${(e as Error).message}` };
    } finally {
      adding.value = false;
    }
  }

  function startEdit(row: MemoryRow): void {
    editingId.value = row.id;
    editContent.value = row.content;
    actionMsg.value = null;
  }

  function cancelEdit(): void {
    editingId.value = null;
    editContent.value = '';
  }

  /** 保存行内编辑:PATCH content,成功后退出编辑态并刷新 */
  async function saveEdit(): Promise<void> {
    if (savingEdit.value || editingId.value === null) return;
    const content = editContent.value.trim();
    if (!content) {
      actionMsg.value = { kind: 'err', text: '记忆内容不能为空' };
      return;
    }
    savingEdit.value = true;
    actionMsg.value = null;
    try {
      await api.updateMemory(editingId.value, { content });
      cancelEdit();
      await load();
    } catch (e) {
      actionMsg.value = { kind: 'err', text: `保存失败:${(e as Error).message}` };
    } finally {
      savingEdit.value = false;
    }
  }

  /** 注入开关(selected):成功本地更新;失败重新拉取列表回滚勾选状态 */
  async function toggleSelected(row: MemoryRow, checked: boolean): Promise<void> {
    actionMsg.value = null;
    try {
      await api.updateMemory(row.id, { selected: checked });
      const target = entries.value.find((e) => e.id === row.id);
      if (target) target.selected = checked;
    } catch (e) {
      actionMsg.value = { kind: 'err', text: `更新失败:${(e as Error).message}` };
      await load();
    }
  }

  /** 置顶开关(pinned):成功本地更新(列表顺序随 pinned 重排);失败重新拉取回滚 */
  async function togglePinned(row: MemoryRow): Promise<void> {
    actionMsg.value = null;
    const next = !row.pinned;
    try {
      await api.updateMemory(row.id, { pinned: next });
      const target = entries.value.find((e) => e.id === row.id);
      if (target) target.pinned = next;
    } catch (e) {
      actionMsg.value = { kind: 'err', text: `更新失败:${(e as Error).message}` };
      await load();
    }
  }

  /** 删除两段式确认:点「删除」进入确认态,「确认删除 / 取消」退出 */
  function requestDelete(row: MemoryRow): void {
    pendingDelete.value = nextPendingDelete(pendingDelete.value, 'request', row.id);
  }

  function cancelDelete(): void {
    pendingDelete.value = nextPendingDelete(pendingDelete.value, 'cancel', 0);
  }

  /** 确认删除:DELETE(204)后清除确认态并刷新;失败保留确认态并反馈 */
  async function confirmDelete(): Promise<void> {
    if (deleting.value || pendingDelete.value === null) return;
    deleting.value = true;
    actionMsg.value = null;
    try {
      await api.deleteMemory(pendingDelete.value);
      pendingDelete.value = null;
      await load();
    } catch (e) {
      actionMsg.value = { kind: 'err', text: `删除失败:${(e as Error).message}` };
    } finally {
      deleting.value = false;
    }
  }

  /** 清理归档两段式确认:点「清理已归档」进入确认态,「确认清理 / 取消」退出 */
  function requestPrune(): void {
    pendingPrune.value = nextPendingPrune(pendingPrune.value, 'request');
  }

  function cancelPrune(): void {
    pendingPrune.value = nextPendingPrune(pendingPrune.value, 'cancel');
  }

  /** 确认清理:POST /memory/prune 硬删除已归档条目,反馈删除条数并刷新 */
  async function pruneNow(): Promise<void> {
    if (pruning.value) return;
    const characterId = getIds.characterId();
    if (!characterId) return;
    pruning.value = true;
    actionMsg.value = null;
    try {
      const res = await api.pruneMemories(characterId);
      pendingPrune.value = nextPendingPrune(pendingPrune.value, 'confirm');
      actionMsg.value = { kind: 'ok', text: pruneSuccessText(res.deleted) };
      await load();
    } catch (e) {
      actionMsg.value = { kind: 'err', text: `清理失败:${(e as Error).message}` };
    } finally {
      pruning.value = false;
    }
  }

  return {
    loading, error, entries, rows,
    query, kindFilter, shownRows, searching,
    distilling, distillMsg,
    actionMsg, newContent, adding,
    editingId, editContent, savingEdit,
    pendingDelete, deleting,
    pendingPrune, pruning,
    load, searchNow, setKindFilter, distillNow, addNow,
    startEdit, cancelEdit, saveEdit, toggleSelected, togglePinned,
    requestDelete, cancelDelete, confirmDelete,
    requestPrune, cancelPrune, pruneNow,
  };
}
