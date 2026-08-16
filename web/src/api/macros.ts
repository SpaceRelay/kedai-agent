// 宏调试 API(阶段六 6b):POST /api/macros/expand + 面板变量区解析纯函数。
// 封装风格对齐 scripts.ts(request 直调 + 类型具名)。
import { request } from './client';
import type { MacroExpandCtx, MacroExpandResult } from './types';

/** 展开模板中的 {{...}} 宏;ctx 全可选(缺省即空/默认) */
export async function expandMacros(text: string, ctx?: MacroExpandCtx): Promise<string> {
  const data = await request<MacroExpandResult>('/macros/expand', {
    method: 'POST',
    body: JSON.stringify({ text, ctx: ctx ?? {} }),
  });
  return data.expanded;
}

/** 面板变量值类型(与服务端 vars 一致:字符串/数字/布尔) */
export type MacroVarsValue = string | number | boolean;

/**
 * 解析「宏调试」面板的变量文本区(每行 key::value)为扁平 map。
 * 空行、无 :: 分隔、key 为空的行忽略(便于整段粘贴、渐进调试);
 * 值类型推断:true/false → boolean,纯整数 → number,其余 → string。
 */
export function parseVarsLines(lines: string): Record<string, MacroVarsValue> {
  const vars: Record<string, MacroVarsValue> = {};
  for (const raw of lines.split('\n')) {
    const line = raw.trim();
    if (!line) continue;
    const sep = line.indexOf('::');
    if (sep <= 0) continue; // 无 :: 或 key 为空 → 忽略
    const key = line.slice(0, sep).trim();
    const value = line.slice(sep + 2).trim();
    if (!key) continue;
    vars[key] = inferVarsValue(value);
  }
  return vars;
}

/** 值类型推断:true/false → boolean;纯整数 → number;其余原样字符串 */
function inferVarsValue(v: string): MacroVarsValue {
  if (v === 'true') return true;
  if (v === 'false') return false;
  if (/^-?\d+$/.test(v)) return Number(v);
  return v;
}
