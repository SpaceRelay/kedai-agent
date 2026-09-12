// cardScripts.ts — 角色卡内嵌酒馆助手「卡级脚本」收集器
//
// 卡级脚本与消息内嵌 regex 脚本不同:它随角色选中常驻运行(监听事件流、读写世界书),
// 数据存于 data_raw.extensions.tavern_helper。ST/酒馆助手导出时数组常被对象化为
// {"0":{"1":{"0":{type:"script",...}}}} 嵌套形态(舰娘卡实测如此),故收集器递归拉平
// 对象/数组,而非只认顶层数组(后端 read_tavern_helper 只认数组,会静默丢弃该形态)。
import type { RegexScript } from './api/types';

/** 卡级脚本节点(酒馆助手 script 库条目) */
export interface CardScript {
  id: string;
  name: string;
  content: string;
  enabled: boolean;
}

/** 判定节点是否像脚本:显式 type:'script',或 name+content 字符串对且带脚本特征键
 *  (enabled/disabled/id)——防 tavern_helper.variables 变量树里的同名键对被误判 */
function isScriptNode(v: unknown): v is Record<string, unknown> {
  if (!v || typeof v !== 'object' || Array.isArray(v)) return false;
  const o = v as Record<string, unknown>;
  if (o.type === 'script' && typeof o.content === 'string') return true;
  const hasScriptKeys = 'enabled' in o || 'disabled' in o || 'id' in o;
  return hasScriptKeys && typeof o.name === 'string' && typeof o.content === 'string' && o.content.length > 0;
}

/** 递归拉平嵌套对象/数组,按键序稳定输出(数字键按数值序,与数组序一致) */
function flatten(node: unknown, out: CardScript[], depth: number): void {
  if (depth > 8 || node === null || node === undefined) return;
  if (isScriptNode(node)) {
    const o = node as Record<string, unknown>;
    out.push({
      id: typeof o.id === 'string' ? o.id : `cs-${out.length}`,
      name: typeof o.name === 'string' ? o.name : '',
      content: String(o.content),
      enabled: o.enabled !== false && (o as { disabled?: unknown }).disabled !== true,
    });
    return;
  }
  if (Array.isArray(node)) {
    for (const item of node) flatten(item, out, depth + 1);
    return;
  }
  if (typeof node === 'object') {
    const entries = Object.entries(node as Record<string, unknown>);
    // 数字键按数值排序(对象化数组的键是 "0","1",...;Object.entries 对整数键已按升序,显式排防御)
    entries.sort(([a], [b]) => {
      const na = Number(a);
      const nb = Number(b);
      if (Number.isInteger(na) && Number.isInteger(nb)) return na - nb;
      return a < b ? -1 : a > b ? 1 : 0;
    });
    for (const [, v] of entries) flatten(v, out, depth + 1);
  }
}

/**
 * 从角色卡 data_raw 收集卡级脚本(启用态优先由调用方过滤,这里全量返回)。
 * 兼容 V2 顶层 extensions 与 V3 data.extensions 两种布局。
 */
export function collectCardScripts(dataRaw: Record<string, unknown> | undefined | null): CardScript[] {
  if (!dataRaw || typeof dataRaw !== 'object') return [];
  const dataObj = dataRaw.data as Record<string, unknown> | undefined;
  const ext =
    (dataRaw.extensions as Record<string, unknown> | undefined) ??
    (dataObj?.extensions as Record<string, unknown> | undefined);
  const th = ext?.tavern_helper;
  if (!th || typeof th !== 'object') return [];
  const out: CardScript[] = [];
  flatten(th, out, 0);
  return out;
}

/** 卡级脚本内容指纹:并入授权哈希的 canonical 形态(字段裁剪与 RegexScript 同款) */
export function cardScriptCanonical(scripts: CardScript[]): Array<Record<string, unknown>> {
  return scripts.map((s) => ({
    kind: 'th-script',
    id: s.id,
    name: s.name,
    content: s.content,
    enabled: s.enabled,
  }));
}

/** 便捷:从角色详情(列表项无 data_raw,详情有)收集启用中的卡级脚本 */
export function enabledCardScriptsOf(detail: { data_raw?: unknown } | null | undefined): CardScript[] {
  const raw = detail?.data_raw as Record<string, unknown> | undefined;
  return collectCardScripts(raw).filter((s) => s.enabled && s.content.trim().length > 0);
}

// 仅 re-export 类型,避免调用方多 import
export type { RegexScript };
