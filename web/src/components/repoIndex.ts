// 仓库索引面板纯函数(展示层):本地筛选、kind 分组与选项、重要性/字节格式化。
// 与 devTools.ts 同风格:纯函数供 RepoIndexModal 渲染与 Vitest 共用,零 DOM 依赖。
import type { RepoIndexItem } from '../api';

/** 本地筛选:按路径 / 摘要 / 深摘要 / 符号名做大小写不敏感子串匹配(空查询返回原列表) */
export function filterRepoItems(items: RepoIndexItem[], query: string): RepoIndexItem[] {
  const q = query.trim().toLowerCase();
  if (!q) return items;
  return items.filter(
    (it) =>
      it.path.toLowerCase().includes(q) ||
      it.summary.toLowerCase().includes(q) ||
      it.deepSummary.toLowerCase().includes(q) ||
      it.symbols.some((s) => s.name.toLowerCase().includes(q)),
  );
}

/** 全部 kind 清单(去重,按出现次数降序、同次数按字母序;供筛选下拉用) */
export function repoKindOptions(items: RepoIndexItem[]): string[] {
  const counts = new Map<string, number>();
  for (const it of items) counts.set(it.kind, (counts.get(it.kind) ?? 0) + 1);
  return [...counts.entries()]
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([kind]) => kind);
}

/** 按 kind 过滤('all' = 全部) */
export function filterRepoByKind(items: RepoIndexItem[], kind: string): RepoIndexItem[] {
  return kind === 'all' ? items : items.filter((it) => it.kind === kind);
}

/** kind → 展示标签(保持原样,便于看到新类型而非吞掉) */
export function repoKindLabel(kind: string): string {
  return kind;
}

/** 字节数格式化:1KB = 1024B,保留一位小数(1.5KB / 2.0MB) */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
}

/** 路径 → 末段文件名(展示用;无斜杠时原样返回) */
export function baseName(path: string): string {
  const at = path.lastIndexOf('/');
  return at === -1 ? path : path.slice(at + 1);
}
