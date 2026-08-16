// mvu 变量树:stat_data(逻辑值,叶子为 [新值, 更新条件])与 display_data(展示值,叶子为 "老->新(原因)")
// 与 MagVarUpdate 的存储约定一致(原作者:MagicalAstrogy,github.com/MagicalAstrogy/MagVarUpdate,
// MIT),支持嵌套点路径与 _.set 更新。Kedai 为独立兼容实现。
import type { JsonValue, UpdateCommand } from './parser';
import { parseSetStatement } from './parser';

export interface MvuVariables {
  stat_data: Record<string, unknown>;
  display_data: Record<string, unknown>;
}

export function emptyVariables(): MvuVariables {
  return { stat_data: {}, display_data: {} };
}

/** 危险段:LLM 输出不可信,禁止经点路径访问/写入这些键,防原型污染 */
const FORBIDDEN_SEGMENTS = new Set(['__proto__', 'constructor', 'prototype']);

/** 路径是否含危险段(任意一段命中即拒绝) */
function hasForbiddenSegment(segs: string[]): boolean {
  return segs.some((s) => FORBIDDEN_SEGMENTS.has(s));
}

/** lodash 子集:按点路径取值(危险段返回 undefined) */
export function pathGet(obj: unknown, path: string): unknown {
  if (path === '') return obj;
  const segs = path.split('.');
  if (hasForbiddenSegment(segs)) return undefined;
  let cur: unknown = obj;
  for (const seg of segs) {
    if (cur === null || cur === undefined) return undefined;
    if (typeof cur !== 'object') return undefined;
    cur = (cur as Record<string, unknown>)[seg];
  }
  return cur;
}

/** lodash 子集:按点路径设值(自动创建中间对象;数组下标支持 `a.0.b`;危险段静默跳过) */
export function pathSet(obj: Record<string, unknown>, path: string, value: unknown): void {
  if (path === '') {
    if (typeof value === 'object' && value !== null) {
      Object.assign(obj, value);
    }
    return;
  }
  const segs = path.split('.');
  if (hasForbiddenSegment(segs)) return;
  let cur: Record<string, unknown> = obj;
  for (let i = 0; i < segs.length - 1; i++) {
    const seg = segs[i];
    const next = cur[seg];
    const isIndex = /^\d+$/.test(segs[i + 1]);
    if (typeof next !== 'object' || next === null) {
      const created: unknown = isIndex ? [] : {};
      cur[seg] = created;
      cur = created as Record<string, unknown>;
    } else {
      cur = next as Record<string, unknown>;
    }
  }
  cur[segs[segs.length - 1]] = value;
}

/** stat_data 叶子是 [新值, 原因] 数组 → 取其 [0] 作为实际值 */
export function statValue(stat: unknown): JsonValue | undefined {
  if (Array.isArray(stat) && stat.length >= 1) {
    return stat[0] as JsonValue;
  }
  return stat as JsonValue | undefined;
}

/** 把普通值包装为 [value, reason] 存储对 */
export function statPair(value: JsonValue, reason: string): [JsonValue, string] {
  return [value, reason];
}

/** 按点路径删除键(与后端 remove_path 语义一致;危险段静默跳过) */
export function pathDelete(obj: Record<string, unknown>, path: string): void {
  if (path === '') return;
  const segs = path.split('.');
  if (hasForbiddenSegment(segs)) return;
  let cur: unknown = obj;
  for (let i = 0; i < segs.length - 1; i++) {
    if (cur === null || cur === undefined || typeof cur !== 'object') return;
    cur = (cur as Record<string, unknown>)[segs[i]];
  }
  if (cur === null || cur === undefined || typeof cur !== 'object') return;
  delete (cur as Record<string, unknown>)[segs[segs.length - 1]];
}

/** 应用一条更新命令到变量树(容错:old 不匹配仍按 new 生效) */
export function applyUpdate(vars: MvuVariables, cmd: UpdateCommand): void {
  if (cmd.op === 'delta') {
    // 数值加:当前值 + delta
    const oldActual = pathGet(vars.stat_data, cmd.path);
    const oldVal = statValue(oldActual);
    const base = typeof oldVal === 'number' ? oldVal : Number(oldVal ?? 0);
    const delta = typeof cmd.newValue === 'number' ? cmd.newValue : Number(cmd.newValue ?? 0);
    const newVal = base + delta;
    pathSet(vars.stat_data, cmd.path, statPair(newVal, cmd.reason));
    pathSet(vars.display_data, cmd.path, `${String(base)}->${String(newVal)}(${cmd.reason})`);
    return;
  }
  if (cmd.op === 'remove') {
    // 与后端 remove_path 一致:stat_data 删键(逻辑值消失);
    // display_data 保留展示记录(回放时展示"曾删除"),缺失时回放镜像 stat_data
    pathDelete(vars.stat_data, cmd.path);
    pathSet(vars.display_data, cmd.path, `->null(${cmd.reason})`);
    return;
  }
  if (cmd.op === 'move' && cmd.from) {
    // 与后端 apply_patches 一致:源缺失则整条跳过(不置 null、不设目标)
    const fromVal = statValue(pathGet(vars.stat_data, cmd.from));
    if (fromVal !== undefined) {
      pathSet(vars.stat_data, cmd.path, statPair(fromVal, cmd.reason));
      pathSet(vars.display_data, cmd.path, `${String(fromVal)}->${String(fromVal)}(${cmd.reason})`);
      pathDelete(vars.stat_data, cmd.from);
    }
    return;
  }
  const oldActual = pathGet(vars.stat_data, cmd.path);
  const oldVal = statValue(oldActual) ?? cmd.oldValue;

  pathSet(vars.stat_data, cmd.path, statPair(cmd.newValue, cmd.reason));
  pathSet(
    vars.display_data,
    cmd.path,
    `${String(oldVal ?? '')}->${String(cmd.newValue)}(${cmd.reason})`,
  );
}

/** 批量应用命令 */
export function applyCommands(vars: MvuVariables, cmds: UpdateCommand[]): void {
  for (const c of cmds) applyUpdate(vars, c);
}

/** 深度合并(对象递归,标量覆盖),用于快照回放与初始化叠加 */
export function deepMerge(target: Record<string, unknown>, patch: unknown): void {
  if (typeof patch !== 'object' || patch === null || Array.isArray(patch)) {
    return;
  }
  for (const [k, v] of Object.entries(patch as Record<string, unknown>)) {
    const tv = target[k];
    if (
      typeof v === 'object' &&
      v !== null &&
      !Array.isArray(v) &&
      typeof tv === 'object' &&
      tv !== null &&
      !Array.isArray(tv)
    ) {
      deepMerge(tv as Record<string, unknown>, v);
    } else {
      target[k] = v;
    }
  }
}

/** 从 [InitVar] 世界书条目文本提取初始变量:
 *  优先解析 `_.set(...)` 语句(可带 "初始"/"初始化" 原因);
 *  否则尝试整体 JSON 对象,叶子包装为 [value, '初始']。 */
export function collectFromInitVarContent(content: string): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  const setRe = /[_.]\s*set\s*\(/g;
  const bodies: string[] = [];
  let m: RegExpExecArray | null;
  while ((m = setRe.exec(content)) !== null) {
    // 注意:lastIndex 在正则对象上,不在 exec 返回的数组上
    const openIdx = setRe.lastIndex - 1;
    const depth = scanParen(content, openIdx);
    if (depth > openIdx) {
      bodies.push(content.slice(openIdx + 1, depth));
    }
  }
  if (bodies.length > 0) {
    // 逐条应用
    for (const body of bodies) {
      const cmd = parseSetStatement(body);
      if (cmd) {
        pathSet(out, cmd.path, statPair(cmd.newValue, cmd.reason || '初始'));
      }
    }
    return out;
  }
  // JSON 对象整体
  try {
    const obj = JSON.parse(content) as unknown;
    if (typeof obj === 'object' && obj !== null && !Array.isArray(obj)) {
      wrapLeaves(out, obj as Record<string, unknown>);
      return out;
    }
  } catch {
    /* 非 JSON 跳过 */
  }
  // YAML 风格缩进键值对(社区 [InitVar] 常见格式):
  //   世界:
  //     年分: 2024
  //     时间: "14:30"
  const indented = parseIndentedYaml(content);
  if (Object.keys(indented).length > 0) {
    wrapLeaves(out, indented);
  }
  return out;
}

/** 解析 YAML 风格缩进键值对(只支持字符串/数字/布尔/null,忽略注释与列表) */
function parseIndentedYaml(content: string): Record<string, unknown> {
  const lines = content
    .split(/\r?\n/)
    .map((l) => l.replace(/\t/g, '  '))
    .map((l) => ({ indent: (l.match(/^ */)?.[0].length ?? 0), raw: l.trim() }))
    .filter((l) => l.raw && !l.raw.startsWith('#'));
  if (lines.length === 0) return {};
  const root: Record<string, unknown> = {};
  const stack: Array<{ indent: number; obj: Record<string, unknown> }> = [{ indent: -1, obj: root }];
  for (const line of lines) {
    const colon = line.raw.indexOf(':');
    if (colon <= 0) continue;
    const key = line.raw.slice(0, colon).trim().replace(/^['"]|['"]$/g, '');
    const valueRaw = line.raw.slice(colon + 1).trim();
    // 弹出缩进更深的祖先
    while (stack.length > 1 && line.indent <= stack[stack.length - 1].indent) {
      stack.pop();
    }
    const parent = stack[stack.length - 1].obj;
    if (valueRaw === '') {
      // 子节点
      const child: Record<string, unknown> = {};
      parent[key] = child;
      stack.push({ indent: line.indent, obj: child });
    } else {
      parent[key] = parseYamlScalar(valueRaw);
    }
  }
  return root;
}

/** 解析 YAML 标量:去引号、数字、布尔、null */
function parseYamlScalar(raw: string): JsonValue {
  const t = raw.trim();
  if ((t.startsWith('"') && t.endsWith('"')) || (t.startsWith("'") && t.endsWith("'"))) {
    return t.slice(1, -1);
  }
  if (t === 'true') return true;
  if (t === 'false') return false;
  if (t === 'null' || t === '~') return null;
  if (/^-?\d+$/.test(t)) return parseInt(t, 10);
  if (/^-?\d*\.\d+$/.test(t)) return parseFloat(t);
  return t;
}

function wrapLeaves(target: Record<string, unknown>, src: Record<string, unknown>): void {
  for (const [k, v] of Object.entries(src)) {
    if (typeof v === 'object' && v !== null && !Array.isArray(v)) {
      const next: Record<string, unknown> = {};
      target[k] = next;
      wrapLeaves(next, v as Record<string, unknown>);
    } else {
      target[k] = statPair(v as JsonValue, '初始');
    }
  }
}

function scanParen(s: string, start: number): number {
  let depth = 0;
  let quote: string | null = null;
  for (let i = start; i < s.length; i++) {
    const ch = s[i];
    if (quote) {
      if (ch === quote) quote = null;
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
    } else if (ch === '(') {
      depth++;
    } else if (ch === ')') {
      depth--;
      if (depth === 0) return i;
    }
  }
  return start;
}
