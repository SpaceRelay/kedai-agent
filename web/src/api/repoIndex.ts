// 仓库索引 API:读取 .kedai-index 生成的代码索引(server-rs 的 GET /api/repo-index)。
//   GET /api/repo-index?q=   可选关键词过滤;索引未生成时 available=false 并带 reason
// 索引由 node .kedai-index/build.mjs --full 生成,前端只读展示。
import { request } from './client';

/** 索引中的符号条目(函数/结构体/类型等) */
export interface RepoIndexSymbol {
  name: string;
  /** 符号类型(function / struct / const / class / …) */
  kind: string;
  /** 定义所在行号 */
  line: number;
}

/** 索引中的单个文件条目 */
export interface RepoIndexItem {
  /** 相对仓库根的路径 */
  path: string;
  /** 文件类别(通常为扩展名,如 rust / ts / vue / md) */
  kind: string;
  /** 所属模块目录 */
  module: string;
  lines: number;
  bytes: number;
  /** 重要性评分(0-10,越大越核心) */
  importance: number;
  /** 确定性摘要 */
  summary: string;
  /** 深摘要(coreFiles 才有,可能为空) */
  deepSummary: string;
  /** 被引用的使用次数 */
  usageCount: number;
  /** 类别标签 */
  category: string;
  /** 备注 */
  note: string;
  symbols: RepoIndexSymbol[];
}

/** GET /api/repo-index 响应:available=false 时 items 缺省,reason 说明原因 */
export interface RepoIndexResult {
  available: boolean;
  /** 不可用原因(如索引未生成) */
  reason?: string;
  /** 索引生成时间(ISO) */
  generatedAt?: string;
  /** 生成时仓库 git HEAD */
  gitHead?: string;
  /** 索引文件总数 */
  fileCount?: number;
  items?: RepoIndexItem[];
}

/** GET /api/repo-index:读取仓库索引(q 为可选服务端关键词过滤;本地过滤走 repoIndex.ts)。
 *  返回前归一化 items:后端异常响应(缺字段/非数组)不得打崩面板渲染。 */
export async function fetchRepoIndex(q?: string): Promise<RepoIndexResult> {
  const query = q?.trim() ? `?q=${encodeURIComponent(q.trim())}` : '';
  const res = await request<RepoIndexResult>(`/repo-index${query}`);
  const items = Array.isArray(res.items)
    ? res.items.map((raw) => normalizeRepoItem(raw)).filter((it): it is RepoIndexItem => it !== null)
    : [];
  return { ...res, items };
}

/** 把 unknown 索引项归一化为 RepoIndexItem(缺字段补默认值,防旧索引打崩面板) */
export function normalizeRepoItem(raw: unknown): RepoIndexItem | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const it = raw as Record<string, unknown>;
  const num = (v: unknown): number => (typeof v === 'number' && Number.isFinite(v) ? v : 0);
  const str = (v: unknown): string => (typeof v === 'string' ? v : '');
  const symbols = Array.isArray(it.symbols)
    ? it.symbols
        .filter((s): s is Record<string, unknown> => typeof s === 'object' && s !== null)
        .map((s) => ({ name: str(s.name), kind: str(s.kind), line: num(s.line) }))
    : [];
  return {
    path: str(it.path),
    kind: str(it.kind),
    module: str(it.module),
    lines: num(it.lines),
    bytes: num(it.bytes),
    importance: num(it.importance),
    summary: str(it.summary),
    deepSummary: str(it.deepSummary),
    usageCount: num(it.usageCount),
    category: str(it.category),
    note: str(it.note),
    symbols,
  };
}
