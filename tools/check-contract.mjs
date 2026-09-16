#!/usr/bin/env node
/**
 * Kedai 前后端契约快照校验(Node 零依赖)。
 *
 * 背景:前端 `web/src/api/types.ts` / `web/src/api/memory.ts` 的类型是手写的,
 * 与 Rust 后端契约逐字对齐靠人。历史漂移样例:`DistillResult.skipped` 后端已下发、
 * 前端漏字段,消费方读到 undefined。本脚本把「已知高变更面」的字段集合做差集比对,
 * 漂移即非零退出,纳入 `tools/check-all.ps1` 门禁。
 *
 * 用法:
 *   node tools/check-contract.mjs
 *   node tools/check-contract.mjs --verbose   # 打印对齐通过的条目
 *
 * 判定规则(刻意宽严有别,避免噪声淹没真问题):
 *   - 后端有字段、前端缺     → FAIL(真漂移,前端会读到 undefined)
 *   - 前端有字段、后端无     → FAIL(前端在臆造契约)
 *   - 后端可选、前端非可选   → WARN(后端可能省略该字段)
 *   - 前端可选、后端非可选   → 容忍(前端放宽类型是安全的)
 *
 * 新增契约类型时,在下方 MAPPINGS 登记即可;未登记的条目不做校验。
 */

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const VERBOSE = process.argv.includes('--verbose');

/** Rust 结构体/枚举 与 前端类型 的对应登记表。 */
const MAPPINGS = [
  {
    label: '跨会话记忆条目',
    rust: { file: 'server-rs/src/services/memory_service.rs', name: 'MemoryEntry' },
    ts: { file: 'web/src/api/memory.ts', name: 'MemoryEntry', kind: 'interface' },
  },
  {
    label: '记忆蒸馏结果',
    rust: { file: 'server-rs/src/services/memory_service.rs', name: 'DistillOutcome' },
    ts: { file: 'web/src/api/memory.ts', name: 'DistillResult', kind: 'interface' },
    // 线格式由 api/memory.rs 手工拼 JSON,与结构体不完全同形:
    // new_ids 仅后端内部用于补向量,不下发;ok 是该 handler 固定附加的成功标记。
    rustIgnore: ['new_ids'],
    tsIgnore: ['ok'],
  },
  {
    label: '任务记录',
    rust: { file: 'server-rs/src/models/types.rs', name: 'TaskRecord' },
    ts: { file: 'web/src/api/types.ts', name: 'TaskRecord', kind: 'interface' },
  },
  {
    label: '任务步骤',
    rust: { file: 'server-rs/src/models/types.rs', name: 'TaskStep' },
    ts: { file: 'web/src/api/types.ts', name: 'TaskStep', kind: 'interface' },
  },
  {
    label: '任务子任务',
    rust: { file: 'server-rs/src/models/types.rs', name: 'TaskSubtaskRecord' },
    ts: { file: 'web/src/api/types.ts', name: 'TaskSubtask', kind: 'interface' },
  },
  {
    label: '任务 LLM 调用记录',
    rust: { file: 'server-rs/src/models/types.rs', name: 'TaskLlmCallRecord' },
    ts: { file: 'web/src/api/types.ts', name: 'TaskLlmCall', kind: 'interface' },
  },
  {
    label: '命令执行审计行',
    rust: { file: 'server-rs/src/services/exec/audit.rs', name: 'ExecAuditEntry' },
    ts: { file: 'web/src/api/exec.ts', name: 'ExecAuditEntry', kind: 'interface' },
  },
  {
    label: '任务事件分类枚举',
    rust: { file: 'server-rs/src/models/types.rs', name: 'TaskEventKind' },
    ts: { file: 'web/src/api/types.ts', name: 'TaskEventKind', kind: 'union' },
  },
  {
    label: '任务执行模式枚举',
    rust: { file: 'server-rs/src/models/types.rs', name: 'TaskRunMode' },
    ts: { file: 'web/src/api/types.ts', name: 'TaskRunMode', kind: 'union' },
  },
  {
    label: '任务状态枚举',
    rust: { file: 'server-rs/src/models/types.rs', name: 'TaskStatus' },
    ts: { file: 'web/src/api/types.ts', name: 'TaskStatus', kind: 'union' },
  },
  {
    // SSE 顶层事件判别式联合(Rust `#[serde(tag="type")]` ↔ TS 判别式联合)。
    // 此前**未登记**:新增一个顶层事件要手改 Rust 枚举 + TS 手写 union + 前端两个 switch
    // (sseReducer.ts / stores/task.ts)共 3 处,其中 Rust→TS 这一步完全靠人,漏改不报错。
    // 登记后漏改即 FAIL;switch 侧的漏接由 sseReducer.ts / task.ts 的 never 穷尽断言在
    // 类型检查阶段拦住(两处配合才闭环)。
    label: 'SSE 顶层事件枚举',
    rust: { file: 'server-rs/src/models/types.rs', name: 'SseEvent' },
    ts: { file: 'web/src/api/types.ts', name: 'SseEvent', kind: 'tagged-union' },
  },
];

const read = (rel) => readFileSync(join(ROOT, rel), 'utf8');

/** 取 `pub struct Name {` 的花括号体内文本(Rust;按大括号配平)。 */
function rustBlock(src, name, keyword) {
  const head = new RegExp(`pub\\s+${keyword}\\s+${name}\\b[^{]*\\{`);
  const m = head.exec(src);
  if (!m) return null;
  let depth = 1;
  let i = m.index + m[0].length;
  const start = i;
  for (; i < src.length && depth > 0; i++) {
    if (src[i] === '{') depth++;
    else if (src[i] === '}') depth--;
  }
  return src.slice(start, i - 1);
}

/**
 * 解析 Rust 结构体字段。
 * 返回 Map<线格式字段名, { optional: boolean }>。
 * 处理:多行类型、泛型内逗号、serde(rename)、serde(skip)、Option<T> 与 skip_serializing_if 视为可选。
 */
function rustFields(src, name) {
  const body = rustBlock(src, name, 'struct');
  if (body === null) return null;
  // 按顶层逗号切分字段声明(<> () [] 内逗号不算分隔符);
  // Rust 结构体字段以逗号收尾,类型可跨行,故不能按行解析。
  const segments = [];
  let buf = '';
  let depth = 0;
  for (const ch of body) {
    if (ch === '<' || ch === '(' || ch === '[') depth++;
    else if (ch === '>' || ch === ')' || ch === ']') depth = Math.max(0, depth - 1);
    if (ch === ',' && depth === 0) {
      segments.push(buf);
      buf = '';
      continue;
    }
    buf += ch;
  }
  if (buf.trim()) segments.push(buf);

  const fields = new Map();
  for (const seg of segments) {
    // 去掉注释行,保留属性行
    const cleaned = seg
      .split('\n')
      .filter((l) => !/^\s*\/\//.test(l))
      .join('\n');
    const attrs = [...cleaned.matchAll(/#\[[^\]]*\]/g)].map((m) => m[0]).join(' ');
    const decl = cleaned.replace(/#\[[^\]]*\]/g, '');
    const m = /pub\s+([A-Za-z_][A-Za-z0-9_]*)\s*:\s*([\s\S]*)/.exec(decl);
    if (!m) continue;
    if (/#\[serde\(skip\)\]/.test(attrs)) continue;
    const rename = /serde\([^)]*rename\s*=\s*"([^"]+)"/.exec(attrs);
    const wire = rename ? rename[1] : m[1];
    // Option<T> 本身不代表字段会消失(序列化为 null);只有 skip_serializing_if
    // 才让字段在 None 时整体省略,前端此时应标为可选。
    const optional = /skip_serializing_if/.test(attrs);
    fields.set(wire, { optional });
  }
  return fields;
}

/** 解析 Rust 枚举变体名,按 serde rename_all=snake_case 折算为线格式文本。 */
function rustVariants(src, name) {
  const body = rustBlock(src, name, 'enum');
  if (body === null) return null;
  const variants = new Set();
  for (const m of body.matchAll(/(?:^|\n)\s*([A-Z][A-Za-z0-9_]*)\s*(?:\{|\(|,)/g)) {
    variants.add(m[1].replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase());
  }
  return variants;
}

/** 取 TS `export interface Name {` 的体内文本(按大括号配平,字符串内大括号不敏感于此用途)。 */
function tsBlock(src, name, kind) {
  const head =
    kind === 'union'
      ? new RegExp(`export\\s+type\\s+${name}\\b[^=]*=`)
      : new RegExp(`export\\s+interface\\s+${name}\\b[^{]*\\{`);
  const m = head.exec(src);
  if (!m) return null;
  let i = m.index + m[0].length;
  const start = i;
  if (kind === 'union') {
    // union 以分号收尾
    const end = src.indexOf(';', i);
    return end < 0 ? null : src.slice(start, end);
  }
  let depth = 1;
  for (; i < src.length && depth > 0; i++) {
    if (src[i] === '{') depth++;
    else if (src[i] === '}') depth--;
  }
  return src.slice(start, i - 1);
}

/**
 * 解析 TS interface 字段。返回 Map<字段名, { optional: boolean }>。
 * 忽略注释;`name?: T` 为可选。
 */
function tsFields(src, name, kind) {
  const body = tsBlock(src, name, kind);
  if (body === null) return null;
  const stripped = body.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/[^\n]*/g, '');
  const fields = new Map();
  for (const m of stripped.matchAll(/(?:^|[\n;])\s*([A-Za-z_][A-Za-z0-9_]*)\s*(\??)\s*:/g)) {
    fields.set(m[1], { optional: m[2] === '?' });
  }
  return fields;
}

/** 解析 TS string-literal union 的成员。 */
function tsUnion(src, name) {
  const body = tsBlock(src, name, 'union');
  if (body === null) return null;
  const values = new Set();
  for (const m of body.matchAll(/'([^']+)'|"([^"]+)"/g)) {
    values.add(m[1] ?? m[2]);
  }
  return values;
}

/**
 * 取 `export type Name = ...;` 的完整声明体(括号配平,收尾于深度 0 的分号)。
 *
 * 为什么不能复用 `tsBlock(kind:'union')`:后者以**第一个分号**收尾,对
 * `{ type: 'token'; text: string } | ...` 这种判别式联合会在第一个成员内部即截断,
 * 得到残缺变体集并**静默通过**——比不检查更危险。
 */
function tsTypeBlock(src, name) {
  const head = new RegExp(`export\\s+type\\s+${name}\\b[^=]*=`);
  const m = head.exec(src);
  if (!m) return null;
  let depth = 0;
  let i = m.index + m[0].length;
  const start = i;
  for (; i < src.length; i++) {
    const ch = src[i];
    if (ch === '{' || ch === '(' || ch === '[') depth++;
    else if (ch === '}' || ch === ')' || ch === ']') depth--;
    else if (ch === ';' && depth === 0) return src.slice(start, i);
  }
  return null;
}

/**
 * 提取 TS 判别式联合的变体集合(`type: '<字面量>'`)。
 *
 * 联合里可能引用同文件的类型别名(如 `SseEvent` 的 `| TaskEvent`),故对被引用的
 * 别名递归展开——否则会误报「TS 缺少取值 task」。
 * 递归带 visited 集合防自引用死循环。
 */
function tsTaggedUnionVariants(src, name, seen = new Set()) {
  if (seen.has(name)) return new Set();
  seen.add(name);
  const body = tsTypeBlock(src, name);
  if (body === null) return null;
  const out = new Set();
  for (const m of body.matchAll(/\btype\s*:\s*'([^']+)'/g)) out.add(m[1]);
  // 联合成员里引用的类型别名(位于行首或 `|` 之后,且不是内联对象/字面量)
  for (const m of body.matchAll(/(?:^|\|)\s*([A-Z][A-Za-z0-9_]*)\s*(?=\||;|$)/gm)) {
    const sub = tsTaggedUnionVariants(src, m[1], seen);
    if (sub) for (const v of sub) out.add(v);
  }
  return out;
}

const failures = [];
const warnings = [];
let checked = 0;

function fail(msg) {
  failures.push(msg);
}
function warn(msg) {
  warnings.push(msg);
}

for (const map of MAPPINGS) {
  const rustSrc = read(map.rust.file);
  const tsSrc = read(map.ts.file);
  checked++;

  if (map.ts.kind === 'tagged-union') {
    const want = rustVariants(rustSrc, map.rust.name);
    const got = tsTaggedUnionVariants(tsSrc, map.ts.name);
    if (!want || !got) {
      fail(`${map.label}: ${!want ? `Rust 未找到枚举 ${map.rust.name}` : ''}${!got ? ` TS 未找到判别式联合 ${map.ts.name}` : ''}`);
      continue;
    }
    const missing = [...want].filter((v) => !got.has(v));
    const extra = [...got].filter((v) => !want.has(v));
    if (missing.length) fail(`${map.label}(${map.ts.name}):TS 缺少事件类型 ${missing.map((v) => `'${v}'`).join(', ')}`);
    if (extra.length) fail(`${map.label}(${map.ts.name}):TS 多出事件类型 ${extra.map((v) => `'${v}'`).join(', ')}`);
    if (!missing.length && !extra.length && VERBOSE) {
      console.log(`  [OK] ${map.label}:${want.size} 个事件类型对齐`);
    }
    continue;
  }

  if (map.ts.kind === 'union') {
    const want = rustVariants(rustSrc, map.rust.name);
    const got = tsUnion(tsSrc, map.ts.name);
    if (!want || !got) {
      fail(`${map.label}: ${!want ? `Rust 未找到枚举 ${map.rust.name}` : ''}${!got ? ` TS 未找到 union ${map.ts.name}` : ''}`);
      continue;
    }
    const missing = [...want].filter((v) => !got.has(v));
    const extra = [...got].filter((v) => !want.has(v));
    if (missing.length) fail(`${map.label}(${map.ts.name}):TS 缺少取值 ${missing.map((v) => `'${v}'`).join(', ')}`);
    if (extra.length) fail(`${map.label}(${map.ts.name}):TS 多出取值 ${extra.map((v) => `'${v}'`).join(', ')}`);
    if (!missing.length && !extra.length && VERBOSE) {
      console.log(`  [OK] ${map.label}:${want.size} 个取值对齐`);
    }
    continue;
  }

  const rust = rustFields(rustSrc, map.rust.name);
  const ts = tsFields(tsSrc, map.ts.name, map.ts.kind);
  if (!rust) {
    fail(`${map.label}:Rust 未找到结构体 ${map.rust.name}(${map.rust.file})`);
    continue;
  }
  if (!ts) {
    fail(`${map.label}:TS 未找到类型 ${map.ts.name}(${map.ts.file})`);
    continue;
  }
  const ignoreRust = new Set(map.rustIgnore ?? []);
  const ignoreTs = new Set(map.tsIgnore ?? []);
  const missingInTs = [...rust.keys()].filter((k) => !ts.has(k) && !ignoreRust.has(k));
  const extraInTs = [...ts.keys()].filter((k) => !rust.has(k) && !ignoreTs.has(k));
  if (missingInTs.length) {
    fail(`${map.label}(${map.ts.name}):TS 缺少后端字段 ${missingInTs.join(', ')}`);
  }
  if (extraInTs.length) {
    fail(`${map.label}(${map.ts.name}):TS 多出后端不存在的字段 ${extraInTs.join(', ')}`);
  }
  for (const [k, info] of rust) {
    const t = ts.get(k);
    if (t && info.optional && !t.optional) {
      warn(`${map.label}(${map.ts.name}):后端字段 ${k} 可省略,建议 TS 标为可选`);
    }
  }
  if (!missingInTs.length && !extraInTs.length && VERBOSE) {
    console.log(`  [OK] ${map.label}:${rust.size} 个字段对齐`);
  }
}

console.log('========== Kedai 契约快照校验 ==========');
console.log(`已校验 ${checked} 组映射(${MAPPINGS.length} 条登记)`);
if (warnings.length) {
  console.log(`\n[WARN] ${warnings.length} 条建议:`);
  for (const w of warnings) console.log(`  - ${w}`);
}
if (failures.length) {
  console.log(`\n[FAIL] ${failures.length} 处契约漂移:`);
  for (const f of failures) console.log(`  - ${f}`);
  console.log('\n修法:以后端类型为准修正 TS 手写类型(或反向,若后端才是漏改方)。');
  process.exit(1);
}
console.log('\n[ OK ] 契约一致(手写类型与后端字段集合对齐)');
