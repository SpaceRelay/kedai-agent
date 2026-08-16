// HTML 渲染开关按角色卡记忆:localStorage 存储层(v1)。
// 仅保存 角色卡 id -> boolean,不保存正文/敏感数据;损坏数据回退空表。
// 供 store 使用(每次切换角色卡时应用记忆,无记忆回退全局默认)与单测。
const STORAGE_KEY = 'kedai.character-render-html.v1';

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** 解析存储原文:非对象/数组/字段类型错误一律丢弃,单条损坏不影响其他条目 */
export function parseRenderHtmlOverrides(raw: string | null): Record<string, boolean> {
  if (!raw) return {};
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {};
    const out: Record<string, boolean> = {};
    for (const [id, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (id && typeof v === 'boolean') out[id] = v;
    }
    return out;
  } catch {
    return {};
  }
}

export class LocalRenderHtmlPreferenceStore {
  constructor(private readonly storage: StorageLike) {}

  read(): Record<string, boolean> {
    return parseRenderHtmlOverrides(this.storage.getItem(STORAGE_KEY));
  }

  write(map: Record<string, boolean>): void {
    try {
      this.storage.setItem(STORAGE_KEY, JSON.stringify(map));
    } catch {
      /* localStorage 不可用/超限:忽略,仅本次会话内生效 */
    }
  }
}
