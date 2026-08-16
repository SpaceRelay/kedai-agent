// slash 输入联想纯函数(阶段四 4c):与 Vue 解耦,便于单测
import type { SlashCommandMeta } from '../api';

/** 按前缀过滤命令清单(input 为去掉前导 / 后的输入;前缀匹配 name,保持清单顺序) */
export function filterCommands(input: string, list: SlashCommandMeta[]): SlashCommandMeta[] {
  const q = input.trim().toLowerCase();
  if (!q) return [];
  return list.filter((c) => c.name.toLowerCase().startsWith(q));
}

/** 取光标位置之前的「单词」(最后一个空白之后的连续非空白串);无则返回空串 */
export function wordBeforeCursor(text: string, cursor: number): string {
  const head = text.slice(0, Math.max(0, Math.min(cursor, text.length)));
  const m = head.match(/(\S+)$/);
  return m ? m[1] : '';
}
