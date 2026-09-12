// mvu(MagVarUpdate 兼容)解析器
// 协议与存储约定源自 MagVarUpdate(原作者:MagicalAstrogy,
// github.com/MagicalAstrogy/MagVarUpdate,MIT)。Kedai 为独立兼容实现。
// AI 输出 `<UpdateVariable>` XML 块,内嵌 `_.set('path', old, new);//原因` 语句。
// 本模块负责:① 从消息文本中剥离该块(显示与 prompt 均不可见);
//           ② 解析块内语句为结构化命令。

export type JsonValue = string | number | boolean | null | JsonValue[] | { [k: string]: JsonValue };

/** 一条变量更新命令:设置 path 处的值为 newValue,原值为 oldValue */
export interface UpdateCommand {
  /** set=赋值(默认)/ delta=数值加 / remove=置空 / move=移动 */
  op?: 'set' | 'delta' | 'remove' | 'move';
  path: string;
  /** move 命令的源路径 */
  from?: string;
  oldValue: JsonValue;
  newValue: JsonValue;
  reason: string;
}

/** 解析结果:cleaned 为剥离块后的原文,commands 为该消息的全部更新命令 */
export interface UpdateVariableParse {
  cleaned: string;
  commands: UpdateCommand[];
  /**
   * 块内 `<initvar>` 初始化段原文(YAML 缩进键值/JSON/_.set 混合,由调用方解析)。
   * 开场白常用它播种初始变量(如 wuwa 14 个备用开场);无该段为 undefined。
   */
  initvar?: string;
}

/** 提取全部 `<UpdateVariable>` 块(不区分大小写,容忍属性/空白) */
const BLOCK_RE = /<\s*UpdateVariable\b[^>]*>([\s\S]*?)<\/\s*UpdateVariable\s*>/gi;

/** 匹配 `_.set(` 开头到语句结束(含 `)` 与 `;//原因` 注释)的调用片段 */
function findSetCalls(content: string): string[] {
  const calls: string[] = [];
  let i = 0;
  while (i < content.length) {
    // 定位 `_.set(`(允许空白)
    const m = /[_.]\s*set\s*\(/g;
    m.lastIndex = i;
    const hit = m.exec(content);
    if (!hit) break;
    const openIdx = m.lastIndex - 1; // '(' 位置
    const closeIdx = scanBalanced(content, openIdx);
    if (closeIdx <= openIdx) {
      i = hit.index + 1;
      continue;
    }
    // 语句结束:')' 后直到 ';' 或行尾;若 ';' 后紧跟 `//注释` 则一并包含
    let stmtEnd = closeIdx + 1;
    while (stmtEnd < content.length && content[stmtEnd] !== ';' && content[stmtEnd] !== '\n' && content[stmtEnd] !== '\r') {
      stmtEnd++;
    }
    if (stmtEnd < content.length && content[stmtEnd] === ';') {
      // 检查分号后是否为 // 注释
      let k = stmtEnd + 1;
      while (k < content.length && /\s/.test(content[k])) k++;
      if (content[k] === '/' && content[k + 1] === '/') {
        const nl = content.indexOf('\n', stmtEnd);
        stmtEnd = nl < 0 ? content.length : nl;
      }
    }
    calls.push(content.slice(openIdx, stmtEnd));
    i = stmtEnd + 1;
  }
  return calls;
}

/** 从 start(指向 '(')开始扫描到配对 ')' 的索引;未配对返回 start */
function scanBalanced(s: string, start: number): number {
  let depth = 0;
  let quote: string | null = null;
  let esc = false;
  for (let i = start; i < s.length; i++) {
    const ch = s[i];
    if (quote) {
      if (esc) {
        esc = false;
      } else if (ch === '\\') {
        esc = true;
      } else if (ch === quote) {
        quote = null;
      }
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
    } else if (ch === '(' || ch === '[' || ch === '{') {
      depth++;
    } else if (ch === ')' || ch === ']' || ch === '}') {
      depth--;
      if (depth === 0) return i;
    }
  }
  return start;
}

/** 在顶层逗号处切分参数(忽略引号与嵌套结构) */
function splitTopLevel(s: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let quote: string | null = null;
  let esc = false;
  let cur = '';
  for (let i = 0; i < s.length; i++) {
    const ch = s[i];
    if (quote) {
      cur += ch;
      if (esc) {
        esc = false;
      } else if (ch === '\\') {
        esc = true;
      } else if (ch === quote) {
        quote = null;
      }
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      cur += ch;
    } else if (ch === '(' || ch === '[' || ch === '{') {
      depth++;
      cur += ch;
    } else if (ch === ')' || ch === ']' || ch === '}') {
      depth--;
      cur += ch;
    } else if (ch === ',' && depth === 0) {
      parts.push(cur);
      cur = '';
    } else {
      cur += ch;
    }
  }
  if (cur.trim()) parts.push(cur);
  return parts;
}

/** 解析任意 JS 风格字面量:字符串(单/双引号)、数字、布尔、null、对象、数组 */
export function parseJsValue(raw: string): JsonValue {
  const t = raw.trim();
  if (t.startsWith("'") || t.startsWith('"')) {
    return parseJsString(t);
  }
  try {
    // 其余按 JSON 解析(对象/数组/数字/布尔/null)
    return JSON.parse(t) as JsonValue;
  } catch {
    // 兜底:视作字符串
    return stripQuotes(t);
  }
}

function parseJsString(t: string): string {
  let s = t;
  if ((s.startsWith("'") && s.endsWith("'")) || (s.startsWith('"') && s.endsWith('"'))) {
    s = s.slice(1, -1);
  }
  return s
    .replace(/\\'/g, "'")
    .replace(/\\"/g, '"')
    .replace(/\\\\/g, '\\')
    .replace(/\\n/g, '\n')
    .replace(/\\r/g, '\r')
    .replace(/\\t/g, '\t');
}

function stripQuotes(t: string): string {
  return t.replace(/^['"]|['"]$/g, '');
}

/** 从语句片段(含 `(` 起、含 `);//原因` 或 `;//原因`)解析命令;解析失败返回 null */
export function parseSetStatement(body: string): UpdateCommand | null {
  // 去掉行尾注释 `//...`
  let code = body;
  const commentIdx = findLineComment(code);
  const reason = commentIdx >= 0 ? code.slice(commentIdx + 2).trim().replace(/;*$/, '').trim() : findBlockReason(code);
  if (commentIdx >= 0) code = code.slice(0, commentIdx);
  code = code.trim();
  // 剥尾部 ';'
  if (code.endsWith(';')) code = code.slice(0, -1).trim();
  // 剥最外层 ( ... )
  if (code.startsWith('(') && code.endsWith(')')) code = code.slice(1, -1).trim();
  const parts = splitTopLevel(code);
  if (parts.length < 2) return null;
  const path = stripQuotes(parts[0].trim()).trim();
  if (!path) return null;
  const oldValue = parts.length >= 3 ? parseJsValue(parts[1]) : null;
  const newValue = parts.length >= 3 ? parseJsValue(parts[2]) : parseJsValue(parts[1]);
  return { path, oldValue, newValue, reason };
}

/** 行注释起点(忽略引号内) */
function findLineComment(s: string): number {
  let quote: string | null = null;
  let esc = false;
  for (let i = 0; i < s.length; i++) {
    const ch = s[i];
    if (quote) {
      if (esc) {
        esc = false;
      } else if (ch === '\\') {
        esc = true;
      } else if (ch === quote) {
        quote = null;
      }
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
    } else if (ch === '/' && s[i + 1] === '/') {
      return i;
    }
  }
  return -1;
}

/** 提取块级注释 /* ... *` `/ 中的原因文本 */
function findBlockReason(s: string): string {
  const block = /\/\*([\s\S]*?)\*\//.exec(s);
  return block ? block[1].trim() : '';
}

/**
 * 主入口:从 assistant 消息原文提取并剥离 UpdateVariable 块,同时解析全部更新命令。
 * 兼容两种块内格式:
 *   1. 酒馆助手格式:<Analysis>…</Analysis><JSONPatch>[{op,path,value},…]</JSONPatch>
 *   2. MagVarUpdate 格式:_.set('path', old, new);//原因
 */
export function parseUpdateVariable(text: string): UpdateVariableParse {
  const commands: UpdateCommand[] = [];
  let cleaned = text;
  const blocks: string[] = [];
  BLOCK_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = BLOCK_RE.exec(text)) !== null) {
    blocks.push(m[1]);
  }
  let initvar: string | undefined;
  if (blocks.length > 0) {
    cleaned = text.replace(BLOCK_RE, '');
    for (const block of blocks) {
      // <initvar> 初始化段(开场白播种初始变量;多段拼接)
      const iv = extractInitVar(block);
      if (iv) initvar = initvar ? `${initvar}\n${iv}` : iv;
      // 优先酒馆助手 JSONPatch 格式
      const jsonPatch = parseJsonPatchBlock(block);
      if (jsonPatch) {
        commands.push(...jsonPatch);
        continue;
      }
      // 回落 MagVarUpdate _.set(...) 语句
      for (const body of findSetCalls(block)) {
        const cmd = parseSetStatement(body);
        if (cmd) commands.push(cmd);
      }
    }
  }
  return initvar !== undefined ? { cleaned, commands, initvar } : { cleaned, commands };
}

/** 提取块内 `<initvar>` 段原文(去标签;空段返回 undefined) */
function extractInitVar(block: string): string | undefined {
  const m = /<\s*initvar\b[^>]*>([\s\S]*?)<\/\s*initvar\s*>/i.exec(block);
  const body = m?.[1].trim();
  return body ? body : undefined;
}

/** 解析块内 <JSONPatch> 数组为更新命令;无该标签或解析失败返回 null */
function parseJsonPatchBlock(block: string): UpdateCommand[] | null {
  const m = /<\s*JSONPatch\b[^>]*>([\s\S]*?)<\/\s*JSONPatch\s*>/i.exec(block);
  if (!m) return null;
  let arr: Array<Record<string, unknown>>;
  try {
    arr = JSON.parse(m[1]) as Array<Record<string, unknown>>;
  } catch {
    return null;
  }
  const cmds: UpdateCommand[] = [];
  for (const item of arr) {
    const cmd = jsonPatchToCommand(item);
    if (cmd) cmds.push(cmd);
  }
  return cmds.length > 0 ? cmds : null;
}

/** 单条 JSON Patch 操作 → UpdateCommand(/心之所向/好感度 → 心之所向.好感度) */
function jsonPatchToCommand(item: Record<string, unknown>): UpdateCommand | null {
  const op = String(item.op ?? '');
  // move 用 from/to;move 外操作用 path(与后端 extract_json_patch 一致)
  const rawPath = op === 'move' ? item.to ?? item.path ?? item.from ?? '' : item.path ?? item.from ?? '';
  const path = pathToDots(String(rawPath));
  if (!path) return null;
  const reason = '更新';
  switch (op) {
    case 'replace':
    case 'set':
    case 'insert':
      return { op: 'set', path, oldValue: null, newValue: item.value as JsonValue, reason };
    case 'delta':
    case 'add':
      return { op: 'delta', path, oldValue: null, newValue: item.value as JsonValue, reason };
    case 'remove':
      return { op: 'remove', path, oldValue: null, newValue: null, reason };
    case 'move': {
      const from = pathToDots(String(item.from ?? ''));
      if (!from) return null;
      return { op: 'move', path, from, oldValue: null, newValue: null, reason };
    }
    default:
      return null;
  }
}

/** /a/b/c → a.b.c */
function pathToDots(p: string): string {
  return p
    .replace(/^\//, '')
    .split('/')
    .filter((s) => s.length > 0)
    .join('.');
}

/** 是否包含 UpdateVariable 块(快速判断) */
export function hasUpdateVariable(text: string): boolean {
  BLOCK_RE.lastIndex = 0;
  return BLOCK_RE.test(text);
}
