// 上下文健康度与推荐设置纯函数(阶段六 6d 优化面板):
// 缓存命中率计算(ChatWindow 与 OptimizeModal 共用)+ 推荐设置常量与合并。
import type { TokenUsage, RuntimeSettingsPatch } from './api';

/**
 * 当前请求的 prompt 缓存命中率:缓存命中 token ÷ prompt token(DeepSeek 等提供商);
 * 无缓存字段(prompt_tokens 为 0 或未返回)时为 null(不显示)。
 * 原逻辑位于 ChatWindow.vue:355-360,抽为纯函数供顶栏与优化面板共用。
 *
 * 与 `components/cacheHealth.ts` 的 `entryHitRate(hit, miss)` 口径等价:后端
 * `prompt_tokens = hit + miss`(见 `services/cache_diagnostics.rs`),两者只是分母写法不同;
 * 本函数额外钳制在 100 内以容忍异常数据。
 */
export function computeHitRate(u: TokenUsage | null | undefined): number | null {
  if (!u || !u.prompt_tokens) return null;
  const hit = u.prompt_cache_hit_tokens ?? 0;
  return Math.min(100, Math.round((hit / u.prompt_tokens) * 100));
}

/** 推荐设置项(一键套用:勾选后合并进 RuntimeSettingsPatch) */
export interface RecommendedSetting {
  key: keyof RuntimeSettingsPatch;
  label: string;
  value: RuntimeSettingsPatch[keyof RuntimeSettingsPatch];
  desc: string;
}

/** 推荐设置静态常量(对齐 settings_service.rs 字段;每项带说明,仅勾选项生效) */
export const RECOMMENDED_SETTINGS: RecommendedSetting[] = [
  {
    key: 'mvu_vars_position',
    label: '变量状态注入 system',
    value: 'system',
    desc: 'mvu 状态块并入 system 角色,system + 早期历史前缀保持稳定,前缀缓存友好',
  },
  {
    key: 'render_html',
    label: '开启安全 HTML 渲染',
    value: true,
    desc: '状态栏脚本执行前置条件:开启安全 HTML 渲染(样式作用域化 + 脚本受控执行)',
  },
  {
    key: 'authorization_mode',
    label: '授权模式:宽松',
    value: 'loose',
    desc: '严格=读/写/删文件都需授权;宽松=读/写放行、删需授权;放行=仅系统路径写删需授权',
  },
  {
    key: 'max_tool_rounds',
    label: '工具循环轮次上限 32',
    value: 32,
    desc: 'AGENT/CUSTOM 模式工具循环轮次上限(默认 32,过高可能拖长单轮)',
  },
  {
    key: 'max_context_tokens',
    label: '上下文窗口 64K',
    value: 65536,
    desc: '上下文窗口上限(历史超出后按时间裁剪;最低 64K,上限 1M)',
  },
];

/** 勾选项合并为设置 patch:仅含勾选键,值取推荐值(按数组顺序,重复键后者覆盖) */
export function buildRecommendedPatch(selected: RecommendedSetting[]): RuntimeSettingsPatch {
  const patch: RuntimeSettingsPatch = {};
  for (const s of selected) {
    // key/value 类型在 RecommendedSetting 上是联合(keyof Patch × Patch[union]),
    // TS 无法对「键-值相关联合」的写入收窄,这里按 Record 写入(运行时语义不变)。
    (patch as Record<string, unknown>)[s.key] = s.value;
  }
  return patch;
}

/** 压缩触发判定结果:占比 + 是否需要压缩 */
export interface CompactionNeed {
  /** 当前上下文 token 占窗口上限的比例(0..1;无有效数据时为 null) */
  ratio: number | null;
  /** 是否达到触发阈值(仅 auto 模式有意义) */
  shouldCompress: boolean;
}

/**
 * 计算上下文压缩触发需求:当前 token 数占窗口上限的比例是否达到阈值。
 * 仿 computeHitRate 的纯函数;contextTokens 或 maxContextTokens 缺失/为 0 时返回 null。
 * threshold 越界(0.5..=0.95 之外)时按不触发处理,与后端保守语义一致。
 */
export function computeCompactionNeed(
  contextTokens: number,
  maxContextTokens: number,
  threshold: number,
): CompactionNeed {
  if (!maxContextTokens || !contextTokens || contextTokens < 0) {
    return { ratio: null, shouldCompress: false };
  }
  const ratio = Math.min(1, contextTokens / maxContextTokens);
  const within = threshold >= 0.5 && threshold <= 0.95;
  return { ratio, shouldCompress: within && ratio >= threshold };
}
