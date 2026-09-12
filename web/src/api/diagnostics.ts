// 缓存诊断 API:GET /api/diagnostics/cache(server-rs/src/api/diagnostics.rs)。
// 可选 query:session_id(会话过滤,缺省 = 全部会话合并)、window(统计窗口条数,
// 默认 20,1..=500);entries 按时间正序返回便于前端画趋势。
import { request } from './client';

/** 单条请求的缓存统计(llm_requests 行,时间正序) */
export interface CacheUsageEntry {
  session_id: string;
  created_at: string;
  prompt_tokens: number;
  completion_tokens: number;
  /** 缓存命中 token */
  hit: number;
  /** 未命中 token */
  miss: number;
}

/** 窗口汇总:hit_rate 为 token 加权口径 total_hit/(total_hit+total_miss),
 *  提供商不回报缓存字段时为 null(不得显示为 0%) */
export interface CacheTotals {
  count: number;
  total_prompt: number;
  total_completion: number;
  total_hit: number;
  total_miss: number;
  hit_rate: number | null;
}

/** 每百万 token 单价(元;DeepSeek 参考价 0.27/2/8) */
export interface CachePricing {
  cache_hit_per_m: number;
  input_per_m: number;
  output_per_m: number;
  currency_hint: string;
}

/** 上下文四级水位:level ok/soft/snip/compact/force/unknown(未配置上限),
 *  ratio = input_tokens / max_context_tokens(unknown 时 null) */
export interface CacheWatermark {
  level: string;
  ratio: number | null;
  input_tokens: number;
  max_context_tokens: number;
  next_level: string | null;
  gap_tokens: number | null;
}

/** 诊断响应:命中率汇总 + 逐条明细 + 费用/节省估算(后端已算好,元)+ 水位报告 */
export interface CacheDiagnostics {
  window: number;
  session_id: string | null;
  totals: CacheTotals;
  entries: CacheUsageEntry[];
  pricing: CachePricing;
  cost: number;
  saved: number;
  watermark: CacheWatermark;
}

/** GET /api/diagnostics/cache:读取近 N 轮缓存命中率、费用估算与上下文水位 */
export async function getCacheDiagnostics(
  opts: { sessionId?: string; window?: number } = {},
): Promise<CacheDiagnostics> {
  const query = new URLSearchParams();
  if (opts.sessionId) query.set('session_id', opts.sessionId);
  if (opts.window !== undefined) query.set('window', String(opts.window));
  return request(`/diagnostics/cache${query.size ? `?${query}` : ''}`);
}
