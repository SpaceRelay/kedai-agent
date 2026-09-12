// 记忆库面板展示纯函数(优化面板「记忆库」分区):
// kind 标签映射与配色、last_usage 格式化、列表行映射排序、本地筛选/高亮分段、
// 蒸馏与清理反馈文案、删除两段式确认状态机。与 cacheHealth.ts 同风格:
// 纯函数供组件渲染与 Vitest 共用。
import type { MemoryEntry } from '../api';

/** kind → 中文标签(未知 kind 原样展示,不吞异常数据) */
export function kindLabel(kind: string): string {
  if (kind === 'distilled') return '蒸馏';
  if (kind === 'tool') return '工具';
  if (kind === 'manual') return '手动';
  return kind;
}

/** kind → 标签配色 class(蓝=蒸馏 / 黄=工具 / 绿=手动;未知中性灰) */
export function kindClass(kind: string): string {
  if (kind === 'distilled' || kind === 'tool' || kind === 'manual') return `kind-${kind}`;
  return 'kind-unknown';
}

/** last_usage 展示:null(从未注入)→ 未使用;ISO → 截断到分(与 cacheHealth 的 formatEntryTime 同口径) */
export function formatMemoryTime(iso: string | null): string {
  if (!iso) return '未使用';
  return iso.replace('T', ' ').slice(0, 16);
}

/** 列表展示行(kind 已映射标签与配色,时间已格式化) */
export interface MemoryRow {
  id: number;
  content: string;
  kind: string;
  kindLabel: string;
  kindClass: string;
  usage: number;
  lastUsage: string;
  selected: boolean;
  /** 分层注入最高优先级(1 = 常驻置顶) */
  pinned: boolean;
}

/** 列表行映射:置顶优先,其次按 id 降序(最新在前,与后端 ORDER BY id DESC 一致;
 *  pinned 排序键与后端 select_for_injection 的 (pinned DESC, ...) 口径一致,幂等保证展示稳定) */
export function memoryRows(entries: MemoryEntry[]): MemoryRow[] {
  return [...entries]
    .sort((a, b) => (Number(b.pinned) - Number(a.pinned)) || (b.id - a.id))
    .map((e) => ({
      id: e.id,
      content: e.content,
      kind: e.kind,
      kindLabel: kindLabel(e.kind),
      kindClass: kindClass(e.kind),
      usage: e.usage_count,
      lastUsage: formatMemoryTime(e.last_usage),
      selected: e.selected,
      pinned: e.pinned,
    }));
}

/** kind 筛选取值(all = 不过滤);与 kindFilterOptions 保持一致 */
export type KindFilter = 'all' | 'distilled' | 'tool' | 'manual';

/** kind 筛选下拉项(label 复用 kindLabel,顺序与后端 kind 枚举一致) */
export const KIND_FILTERS: ReadonlyArray<{ value: KindFilter; label: string }> = [
  { value: 'all', label: '全部类型' },
  { value: 'distilled', label: '蒸馏' },
  { value: 'tool', label: '工具' },
  { value: 'manual', label: '手动' },
];

/**
 * 本地筛选:kind(纯本地)+ query(按内容大小写不敏感子串匹配)。
 * 注意 query 非空时列表数据本身已来自 /memory/search,此处再过滤一次是幂等的
 * 防御(后端短查询走 LIKE,与前端子串匹配语义一致)。
 */
export function filterRows(rows: MemoryRow[], kindFilter: KindFilter, query: string): MemoryRow[] {
  const q = query.trim().toLowerCase();
  return rows.filter(
    (r) =>
      (kindFilter === 'all' || r.kind === kindFilter) &&
      (q === '' || r.content.toLowerCase().includes(q)),
  );
}

/** 高亮分段:一段文本按 query 切成 [{text, hit}] 片段(大小写不敏感,命中段标 hit)。
 *  无 query 时返回单段 hit=false,便于模板统一 v-for 渲染。 */
export interface HighlightSegment {
  text: string;
  hit: boolean;
}

export function highlightSegments(text: string, query: string): HighlightSegment[] {
  const q = query.trim();
  if (!q) return [{ text, hit: false }];
  const haystack = text.toLowerCase();
  const needle = q.toLowerCase();
  const segments: HighlightSegment[] = [];
  let from = 0;
  for (let at = haystack.indexOf(needle); at !== -1; at = haystack.indexOf(needle, from)) {
    if (at > from) segments.push({ text: text.slice(from, at), hit: false });
    segments.push({ text: text.slice(at, at + needle.length), hit: true });
    from = at + needle.length;
  }
  if (from < text.length) segments.push({ text: text.slice(from), hit: false });
  return segments;
}

/** 蒸馏成功文案:inserted>0 → 新增 N 条;0(空历史,后端未调模型)→ 无可提取提示 */
export function distillSuccessText(inserted: number): string {
  return inserted > 0
    ? `蒸馏完成:新增 ${inserted} 条记忆`
    : '蒸馏完成:当前会话没有可提取的记忆';
}

/** 蒸馏失败文案:未开启(后端 400 指引)→ 追加去设置开启的路径;其余错误原样带前缀 */
export function distillErrorText(message: string): string {
  if (message.includes('未开启')) {
    return `蒸馏失败:${message}。可到 设置 → 生成参数 打开「记忆蒸馏」后重试`;
  }
  return `蒸馏失败:${message}`;
}

/** 清理归档成功文案:deleted>0 → 已清理 N 条;0 → 没有可清理条目 */
export function pruneSuccessText(deleted: number): string {
  return deleted > 0 ? `已清理 ${deleted} 条归档记忆` : '没有可清理的归档记忆';
}

/** 删除两段式确认状态:点「删除」→ 该行进入确认态;「确认删除」/「取消」→ 退出 */
export function nextPendingDelete(
  current: number | null,
  action: 'request' | 'confirm' | 'cancel',
  id: number,
): number | null {
  if (action === 'request') return id;
  return null;
}

/** 清理归档两段式确认状态:点「清理已归档」→ 进入确认态;「确认清理」/「取消」→ 退出 */
export function nextPendingPrune(
  current: boolean,
  action: 'request' | 'confirm' | 'cancel',
): boolean {
  return action === 'request';
}
