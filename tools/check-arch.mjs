#!/usr/bin/env node
/**
 * Kedai 架构护栏(Node 零依赖)。
 *
 * 前端两项检查:
 *   A. store 循环依赖 —— store 间顶层 import 构成的环。Pinia setup store 允许在
 *      action 内运行期 `useXStore()`,但顶层 import 成环会让初始化顺序变成隐式契约
 *      (谁先被 import 影响 setup 期行为),且无法单独替换任一 store。检出即 FAIL。
 *   B. 组件直改 store state —— 组件里 `store.field = ...` 绕过 action 直接赋值,
 *      使状态变更路径分散、无法在 action 里统一做副作用与校验。仅报「疑似绕过」
 *      (字段写了 action 才会被拦),存量无害赋值经 BASELINE 白名单豁免。
 *
 * 后端分层与冻结检查(2026-09-13 起,落实三结合 L1/L2 边界):
 *   C. `services/task_engine/**` 不得 import `task_service`(任务引擎依赖倒置)。
 *   D. L1(parsing/models/contracts)不得 `use crate::services::`(老层不被上层渗透)。
 *   E. `services/**` 生产代码不得出现 `.expect(`(连接池异常不得 panic 掉请求线程)。
 *   G. `api/**` 不得新增「裸 {error}」响应(错误形状 ratchet,数量不得超基线)。
 *   H. EJS 自研解释器能力面(builtin_global / get_prop / set_prop 的字面量条数)不得增长
 *      (威胁模型 D5;冻结纪律的机器门禁)。
 *   I. 代际归属完整性 —— server-rs/src 顶层模块/根文件、web/src 顶层目录/根文件
 *      必须全部登记在 `tools/arch-layers.json`(三结合分层的单一事实源)。未登记即 FAIL:
 *      没有代际归属的模块无从按分层纪律评审,是分层叙事的静默失效点。
 *   J. 跨代依赖方向 —— 按 arch-layers.json 的 L1/L2/L3 归属与允许方向校验实测 import 图。
 *      新增越代边 FAIL;存量越代边须在 registeredEdges 中登记(只报债务不阻塞,可还债不可增债)。
 *
 * 用法:
 *   node tools/check-arch.mjs
 *   node tools/check-arch.mjs --verbose   # 打印通过的检查项与白名单命中
 *
 * 新增白名单须写明理由:白名单条目代表「已确认无害、暂不修」,不是「检测不到」。
 */

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const SRC = join(ROOT, 'web', 'src');
const VERBOSE = process.argv.includes('--verbose');

/**
 * 已知且暂未偿还的 store 循环依赖(基线快照)。脚本对**新增**环 FAIL;
 * 基线内的环打印为待偿还债务,不阻塞。修完一对就删一行;全绿时应为空数组。
 *
 * 2026-09-12(M5 断环后):五个 store 原构成一个强连通分量
 * (character/chat/genSettings/uiPrefs/task 互相可达),已通过
 * sharedState.ts(共享状态外提)+ storeBridge.ts(动作回调注册)拆解为单向 DAG,
 * 故此处为空。新增环会立即被检出并判 FAIL——不要为了「过检查」往这里加条目,
 * 那等同于关掉护栏。
 */
const KNOWN_CYCLES = [];

const failures = [];
/**
 * 前端「代际归属」类违规(规则 I 的前端分支)独立分桶。
 *
 * 为什么必须分桶:规则 I 的前端检查与后端检查此前共用 backendFailures,而汇总段
 * 在后端违规时**立即 process.exit(1)**(位于前端报告段之前),后果是「只要有一个
 * 前端根文件未登记,前端规则 J 的全部结论就永不打印」——护栏静默失效。
 * 现在两个桶在汇总处一并打印后再统一退出,任一桶非空都判 FAIL(不降低门禁强度)。
 */
const frontendFailures = [];
const notes = [];

/**
 * 弹窗 flag 白名单**单点派生**(批次 5.2):读取 web/src/modals.ts 的弹窗注册表,
 * 不再在此手抄 flag 名(此前含僵尸项 devToolsOpen,真名是 eventsOpen)。
 *
 * 解析契约:modals.ts 内每个弹窗必须是单独一行
 *   `modal('flag', '中文 label', () => import('./components/X.vue')),`
 * 解析失败(文件缺失/结构改写)时**显式报错并判 FAIL**——绝不静默返回空列表:
 * 白名单静默变空会让「组件直改弹窗开关」从合规变违规(或反之),护栏即失效。
 */
function readModalFlags() {
  const file = join(SRC, 'modals.ts');
  let src;
  try {
    src = readFileSync(file, 'utf8');
  } catch (e) {
    failures.push(`弹窗白名单派生失败:无法读取 web/src/modals.ts(${e.message})`);
    return [];
  }
  const start = src.indexOf('export const MODALS');
  const end = start === -1 ? -1 : src.indexOf('] as const;', start);
  if (start === -1 || end === -1) {
    failures.push(
      '弹窗白名单派生失败:web/src/modals.ts 的 `export const MODALS = [...] as const;` 结构未识别(改了结构请同步 check-arch.mjs 的解析契约)',
    );
    return [];
  }
  const body = src.slice(start, end);
  // flag 引号放宽到单/双引号,避免「有人用双引号写新条目」时静默少算一项。
  const entry = /^\s*modal\(\s*['"]([A-Za-z_$][\w$]*)['"]\s*,\s*['"][^'"]*['"]\s*,\s*\(\)\s*=>\s*import\(/gm;
  const flags = [...body.matchAll(entry)].map((m) => m[1]);
  if (flags.length === 0) {
    failures.push(
      '弹窗白名单派生失败:web/src/modals.ts 的 MODALS 注册表解析出 0 条(解析契约已失效,请同步 check-arch.mjs)',
    );
    return [];
  }
  // 交叉校验:注册表里 `modal(` 的出现次数必须与解析出的条数一致,
  // 否则说明有条目写法未匹配(单条漏算会让白名单静默少项,违背本函数「绝不静默」纪律)。
  const modalCalls = [...body.matchAll(/\bmodal\s*\(/g)].length;
  if (modalCalls !== flags.length) {
    failures.push(
      `弹窗白名单派生失败:web/src/modals.ts 的 MODALS 中 \`modal(\` 出现 ${modalCalls} 次,但仅解析出 ${flags.length} 条` +
        `(有条目写法未匹配解析契约,请对齐 check-arch.mjs 的正则或统一写法)`,
    );
  }
  const dup = [...new Set(flags.filter((f, i) => flags.indexOf(f) !== i))];
  if (dup.length) {
    failures.push(`弹窗白名单派生失败:web/src/modals.ts 弹窗 flag 重复:${dup.join(', ')}`);
  }
  if (VERBOSE) {
    console.log(`  [B] 弹窗白名单由 web/src/modals.ts 派生:${flags.length} 项(${flags.join(', ')})`);
  }
  return flags;
}

/**
 * 组件直改 store state 的白名单:字段名 → 理由。
 * 这些字段是纯 UI 开关(弹窗显隐 / 抽屉开合 / 面板展开 / 加载错误提示),
 * 变更无副作用、无校验、无持久化需求,直改不构成风险;一旦为其补了 action
 * (如 renderHtml 已改走 setRenderHtml/toggleRenderHtml)应从白名单移出。
 * 注意:不在白名单的字段会被判为违规——这正是护栏生效的地方,新增 action
 * 后请同步把字段名从本集合删除。
 */
const UI_FLAG_WHITELIST = new Set([
  // 弹窗/面板显隐:由 web/src/modals.ts 弹窗注册表派生(新增弹窗本脚本零改动)
  ...readModalFlags(),
  // 抽屉/面板开合(sidebarOpen 无副作用;agentPanelOpen 的收合语义走 collapseAgentPanel)
  'sidebarOpen',
  'audioOpen',
  'callTraceOpen',
  'taskResultSummaryOpen',
  // 启动与加载状态提示
  'splashDone',
  'renderHintDismissed',
  'modalLoadError',
  'dataLoadError',
  // 全局未捕获异常横幅(批次 5.1):纯内存态展示开关,与 dataLoadError 同类——
  // 由 main.ts 的注入回调写、App.vue 点「关闭」清 null,无业务副作用,不值得包 action。
  'globalError',
]);

// ---------- 工具 ----------

const isTest = (f) => /\.test\.ts$/.test(f);

/** 归一化为「文件名」键(仅比较同目录内 store 文件名)。 */
const baseName = (rel) => rel.split(sep).pop();

// ---------- A. store 循环依赖 ----------

function collectStores() {
  const dir = join(SRC, 'stores');
  const present = new Set(
    readdirSync(dir).filter((n) => n.endsWith('.ts') && !isTest(n)),
  );
  const out = new Map();
  for (const name of present) {
    const src = readFileSync(join(dir, name), 'utf8');
    // 只认顶层 import(行首 import);TS 项目里 specifier 通常省略 .ts 后缀
    const deps = [];
    for (const m of src.matchAll(/^import\s[^\n]*from\s+'\.\/([^']+)'/gm)) {
      const raw = baseName(m[1]);
      const target = raw.endsWith('.ts') ? raw : `${raw}.ts`;
      if (present.has(target) && target !== name) deps.push(target);
    }
    out.set(name, deps);
  }
  return out;
}

/** DFS 找有向环,返回规范化后的环列表(每环以字典序最小的节点开头去重)。 */
function findCycles(graph) {
  const cycles = new Map();
  const state = new Map(); // 0=未访问 1=在栈 2=完成
  const stack = [];
  const visit = (node) => {
    state.set(node, 1);
    stack.push(node);
    for (const next of graph.get(node) ?? []) {
      const s = state.get(next) ?? 0;
      if (s === 0) {
        visit(next);
      } else if (s === 1) {
        // 找到环:截取栈中 next..末尾
        const from = stack.indexOf(next);
        const ring = stack.slice(from).filter((x) => x !== 'storeFacade');
        if (ring.length > 1) {
          // 规范化:旋转到字典序最小开头,便于与基线比较
          let min = 0;
          for (let i = 1; i < ring.length; i++) if (ring[i] < ring[min]) min = i;
          const norm = ring.slice(min).concat(ring.slice(0, min));
          cycles.set(norm.join('>'), norm);
        }
      }
    }
    stack.pop();
    state.set(node, 2);
  };
  for (const node of graph.keys()) if ((state.get(node) ?? 0) === 0) visit(node);
  return [...cycles.values()];
}

/** 环与基线条目等价比较(节点集无序比较)。 */
function sameAsKnown(ring) {
  const key = [...ring].sort().join('|');
  return KNOWN_CYCLES.some((c) => [...c].sort().join('|') === key);
}

const stores = collectStores();
const cycles = findCycles(stores);
let newCycles = 0;
let knownCycles = 0;
for (const ring of cycles) {
  if (sameAsKnown(ring)) {
    knownCycles++;
    notes.push(`[债务] 已知循环依赖未偿还:${ring.join(' ↔ ')}(修完请从 KNOWN_CYCLES 删除)`);
  } else {
    newCycles++;
    failures.push(`新增 store 循环依赖:${ring.join(' → ')}(顶层 import 成环,须断环)`);
  }
}
if (VERBOSE) {
  console.log(`  [A] 扫描 ${stores.size} 个 store,发现 ${cycles.length} 个环(新增 ${newCycles},已知 ${knownCycles})`);
}

// ---------- B. 组件直改 store state ----------

/** 收集 .vue 文件(递归)。 */
function collectVue(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) out.push(...collectVue(p));
    else if (name.endsWith('.vue')) out.push(p);
  }
  return out;
}

/** 找出组件内 store 绑定名:const X = useYStore() / const X = store(门面)。 */
function storeBindings(src) {
  const names = new Map();
  for (const m of src.matchAll(/(?:const|let)\s+([A-Za-z_$][\w$]*)\s*=\s*use[A-Z][\w]*Store\(\)/g)) {
    names.set(m[1], 'store');
  }
  // 门面 store.ts 的 useAppStore() 也匹配上面;补 `= store` 形态(别名导入)
  for (const m of src.matchAll(/(?:const|let)\s+([A-Za-z_$][\w$]*)\s*=\s*store\b/g)) {
    names.set(m[1], 'facade');
  }
  return names;
}

/** 找出 `<binding>.<field> = ` 形态的赋值(排除 == / === 与对象字面量键)。 */
function directWrites(src, bindings) {
  const hits = [];
  for (const [bind, kind] of bindings) {
    const re = new RegExp(`\\b${bind}\\.([A-Za-z_$][\\w$]*)\\s*(?:=[^=]|\\+=|-=|\\*=|\\|=)`, 'g');
    for (const m of src.matchAll(re)) {
      hits.push({ field: m[1], kind, index: m.index });
    }
  }
  return hits;
}

/** 取赋值处的行号(用于报错定位)。 */
const lineOf = (src, index) => src.slice(0, index).split('\n').length;

let writeHits = 0;
let whitelisted = 0;
for (const file of collectVue(SRC)) {
  const src = readFileSync(file, 'utf8');
  const bindings = storeBindings(src);
  if (!bindings.size) continue;
  for (const hit of directWrites(src, bindings)) {
    writeHits++;
    if (UI_FLAG_WHITELIST.has(hit.field)) {
      whitelisted++;
      if (VERBOSE) {
        console.log(`  [B] 白名单命中:${relative(ROOT, file)}:${lineOf(src, hit.index)} → .${hit.field}`);
      }
      continue;
    }
    failures.push(
      `组件直改 store state:${relative(ROOT, file)}:${lineOf(src, hit.index)} → .${hit.field} = ...(应走 action;若确为无害 UI 开关,加 UI_FLAG_WHITELIST 并写理由)`,
    );
  }
}
if (VERBOSE) {
  console.log(`  [B] 命中赋值 ${writeHits} 处(白名单 ${whitelisted},待修 ${writeHits - whitelisted})`);
}

// ---------- 汇总 ----------

// ========== 后端分层检查(C/D/E) ==========
//
// 分级策略:
//   C(任务引擎不得依赖 task_service)、D(L1 不得依赖 services)——
//     2026-09-13 已完成断环/清理,违规数归零,**永久硬门禁**(出现在这里即 FAIL)。
//   E(services 生产代码不得 .expect)、G(不得新增裸 {error})——
//     同批已清零 E;G 存量 117 处待逐个补 code(需逐处判定 HTTP 状态码),故走
//     ratchet(数量不得超基线)。二者违规即 FAIL。
//   H(EJS builtin 表条数)——
//     自研迷你 JS 引擎冻结纪律的机器门禁,同样是 ratchet(见该规则注释)。
const SERVER = join(ROOT, 'server-rs', 'src');

/** 递归收集指定后缀文件(相对 ROOT 的路径)。 */
function collect(dir, exts) {
  const out = [];
  const walk = (d) => {
    for (const name of readdirSync(d)) {
      const p = join(d, name);
      if (statSync(p).isDirectory()) walk(p);
      else if (exts.some((e) => name.endsWith(e))) out.push(p);
    }
  };
  if (statSync(dir, { throwIfNoEntry: false })?.isDirectory()) walk(dir);
  return out;
}

/** 生产代码行(剔除 #[cfg(test)] 之后的内容)的行号集合。 */
function productionLineCount(src) {
  const idx = src.search(/^#\[cfg\(test\)\]/m);
  return idx === -1 ? Infinity : src.slice(0, idx).split('\n').length;
}

const backendFailures = [];
const backendWarnings = [];

// --- 规则 C:task_engine 不得依赖 task_service(批次 B 断环) ---
{
  const dir = join(SERVER, 'services', 'task_engine');
  let hits = 0;
  for (const f of collect(dir, ['.rs'])) {
    const src = readFileSync(f, 'utf8');
    src.split('\n').forEach((line, i) => {
      if (/^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+crate::services::task_service/.test(line)) {
        hits++;
        backendFailures.push(
          `[C] 任务引擎反向依赖 task_service:${relative(ROOT, f)}:${i + 1}(应经 task_core::TaskBackend 接口)`,
        );
      }
    });
  }
  if (VERBOSE) console.log(`  [C] task_engine → task_service 违规 import:${hits} 处`);
}

// --- 规则 D:L1 不得依赖 services ---
{
  const ALLOW = new Set([join(SERVER, 'models', 'transport.rs')]); // 传输子域类型引用豁免
  let hits = 0;
  for (const top of ['parsing', 'models', 'contracts']) {
    for (const f of collect(join(SERVER, top), ['.rs'])) {
      if (ALLOW.has(f)) continue;
      const src = readFileSync(f, 'utf8');
      src.split('\n').forEach((line, i) => {
        if (/^\s*use\s+crate::services::/.test(line)) {
          hits++;
          backendFailures.push(
            `[D] L1 层(${top}/)反向依赖 services:${relative(ROOT, f)}:${i + 1}(L1 不得被上层渗透)`,
          );
        }
      });
    }
  }
  if (VERBOSE) console.log(`  [D] parsing/models/contracts → services 违规 import:${hits} 处`);
}

// --- 规则 E:services 生产代码不得 .expect( ---
{
  // 存量白名单:格式 "相对路径:行号"。批次 D 清零过程中逐条删除。
  const EXPECT_BASELINE = new Set();
  let hits = 0;
  for (const f of collect(join(SERVER, 'services'), ['.rs'])) {
    const src = readFileSync(f, 'utf8');
    const prodLines = productionLineCount(src);
    src.split('\n').forEach((line, i) => {
      const ln = i + 1;
      if (ln > prodLines) return; // 测试模块内豁免
      if (!/\.expect\(/.test(line)) return;
      // 注释行不算
      if (/^\s*\/\//.test(line)) return;
      const rel = relative(ROOT, f);
      if (EXPECT_BASELINE.has(`${rel}:${ln}`)) return;
      hits++;
      backendFailures.push(
        `[E] services 生产代码使用 .expect(:${rel}:${ln}(应返回 Result 或 tracing::error,不得 panic 请求线程)`,
      );
    });
  }
  if (VERBOSE) console.log(`  [E] services 生产 .expect( 违规:${hits} 处`);
}

// --- 规则 G:HTTP 错误响应不得新增「裸 {error}」形状(错误形状统一 ratchet) ---
//
// 背景:`api/errors.rs` 提供 `err_status` / `err_with_code`(body `{error, code}`),但历史上
// 大量 handler 直接返回 `Json(json!({ "error": ... }))`——同一错误出现两种形状,前端无法统一解析。
// **2026-09-16 收口完成**:8 份私有 `err_json` 影子实现已删除,存量裸 `{error}` 由 105 处降到 1 处
// (仅剩 `api/security.rs` 的 403/429 拒绝路径——那两个状态码无匹配 ErrorCode,上批决定有意保留)。
// ratchet 继续守着:数量不得超基线,新代码一律用 errors.rs 的构造器。
{
  const BASELINE_BARE_ERROR = 1; // 2026-09-16 实测(错误出口收口后由 105 下调);只降不升
  const apiDir = join(SERVER, 'api');
  let hits = 0;
  const samples = [];
  for (const f of collect(apiDir, ['.rs'])) {
    // errors.rs 自身是错误出口的定义处,豁免
    if (f.endsWith('errors.rs')) continue;
    const src = readFileSync(f, 'utf8');
    const prodLines = productionLineCount(src);
    src.split('\n').forEach((line, i) => {
      const ln = i + 1;
      if (ln > prodLines) return;
      if (/^\s*\/\//.test(line)) return;
      if (!/json!\(\s*\{\s*"error"/.test(line)) return;
      hits++;
      if (samples.length < 3) samples.push(`${relative(ROOT, f)}:${ln}`);
    });
  }
  if (hits > BASELINE_BARE_ERROR) {
    backendFailures.push(
      `[G] 裸 {error} 错误形状新增:${hits} 处 > 基线 ${BASELINE_BARE_ERROR}` +
        `(${samples.join(', ')});请改用 api/errors.rs 的 err_with_code 携带 code`,
    );
  }
  if (VERBOSE) {
    console.log(`  [G] 裸 {error} 响应:${hits} 处(基线 ${BASELINE_BARE_ERROR},只降不升)`);
  }
}

// --- 规则 H:EJS 能力面冻结 ratchet(威胁模型 D5 / MAINTENANCE.md §0) ---
//
// 背景:`parsing/assistant/ejs/` 是自研迷你 JS 引擎(约 4000 行),纪律为「只接受安全
// 修复,不再扩展新能力」——任何新模板能力必须走 `scripts/runtime.rs` 的 rquickjs 沙箱。
// 此前该纪律仅靠文档约束(威胁模型 D5),此处把 `env.rs` 的能力面**字面量条数**钉成
// ratchet:任一处超过基线即 FAIL。
//
// 覆盖三处能力面(缺一不可;只锁 `builtin_global` 会漏掉方法表这条更大的口子):
//   - `builtin_global`:全局内建名(含别名),另有 JSON/Math/Object 内联成员名;
//   - `get_prop`:字符串/数组的**属性方法名**——往这里加 `"padStart" => ...` 同样是一次
//     真实的模板能力扩展,必须一并冻结;
//   - `set_prop`:当前无硬编码属性名(基线 0),一旦出现即视为能力面变化。
//
// 计数口径:match 分支的字符串模式名(**别名各计一条**)+ 内联对象成员名(`"name".into(`)。
//
// 基线纪律:**调低是唯一合法方向**。删除条目、收敛别名后请同步下调基线值;
// 上调等于关掉护栏,确属安全修复必须在变更说明里给出理由。
//
// 解析失败(文件缺失 / 三个函数任一未找到 / 计出 0 条 / strict 面出现无法分类的字面量)
// 一律**显式 FAIL**,绝不静默跳过(错误文案风格参照本文件 readModalFlags)。
{
  const EJS_ENV = join(SERVER, 'parsing', 'assistant', 'ejs', 'env.rs');
  const rel = relative(ROOT, EJS_ENV);
  // 能力面基线(2026-09-13 实测;只降不升)。
  // strict=true 的面要求「所有字面量都能归类为能力名」;get_prop/set_prop 内还有错误文案
  // 等非能力字面量,故不要求 pristine,只做条数 ratchet。
  const EJS_SURFACES = [
    { fn: 'builtin_global', baseline: 64, strict: true },
    { fn: 'get_prop', baseline: 26, strict: false },
    { fn: 'set_prop', baseline: 0, strict: false },
  ];
  let src = null;
  try {
    src = readFileSync(EJS_ENV, 'utf8');
  } catch (e) {
    backendFailures.push(`[H] EJS 能力面冻结检查失败:无法读取 ${rel}(${e.message})`);
  }
  if (src !== null) {
    // 按顶层 fn 切分函数体(比手工定位 match/结尾标记更稳):函数体延伸到下一个顶层 fn
    const heads = [...src.matchAll(/^(?:pub(?:\([^)]*\))?\s+)?fn\s+(\w+)/gm)];
    const bodyOf = (name) => {
      const i = heads.findIndex((h) => h[1] === name);
      if (i === -1) return null;
      const start = heads[i].index;
      const end = i + 1 < heads.length ? heads[i + 1].index : src.length;
      return src.slice(start, end).replace(/\/\/[^\n]*/g, ''); // 去注释:注释里提到的名字不算条目
    };
    const reports = [];
    for (const { fn, baseline, strict } of EJS_SURFACES) {
      const body = bodyOf(fn);
      if (body === null) {
        backendFailures.push(
          `[H] EJS 能力面冻结检查失败:${rel} 未找到 \`fn ${fn}\`(重构了该文件请同步 check-arch.mjs 的解析契约)`,
        );
        continue;
      }
      const names = [];
      const unknown = [];
      for (const m of body.matchAll(/"((?:[^"\\]|\\.)*)"/g)) {
        const after = body.slice(m.index + m[0].length);
        // 分支名(`"a" | "b" =>`,多别名各计一条)或内联对象成员名(`("name".into(), …)`)
        if (/^\s*(?:\|\s*"(?:[^"\\]|\\.)*"\s*)*=>/.test(after) || /^\s*\.into\s*\(/.test(after)) {
          names.push(m[1]);
        } else {
          unknown.push(m[1]);
        }
      }
      if (names.length > baseline) {
        backendFailures.push(
          `[H] EJS 能力面新增:${rel} 的 ${fn} 有 ${names.length} 条 > 基线 ${baseline}(EJS 自研解释器已冻结:` +
            `只接受安全修复,新模板能力请走 scripts/runtime.rs 的 rquickjs 沙箱;确属安全修复请说明理由并**下调**基线)`,
        );
      }
      if (strict && unknown.length) {
        backendFailures.push(
          `[H] EJS 能力面冻结检查失败:${rel} 的 ${fn} 出现无法分类的字面量:${unknown
            .map((s) => `"${s}"`)
            .join(', ')}(解析契约已失效,请同步 check-arch.mjs)`,
        );
      }
      reports.push(`${fn}=${names.length}/${baseline}${names.length < baseline ? '(可下调)' : ''}`);
    }
    if (VERBOSE) {
      console.log(`  [H] EJS 能力面(条数/基线,只降不升):${reports.join('、')}`);
    }
  }
}

// --- 规则 K:JSON body 提取器接入 ratchet(错误面收口,2026-09-15 起) ---
//
// 背景(批次 1 实测):axum 内建 `Json<T>` 的 `JsonRejection` 默认 `IntoResponse`
// 产生 **400 text/plain**(带 serde 类型名/行列),与「统一 JSON 错误体」的承诺不符,
// 前端 `client.ts` 的 `request()` 也解析不出 `code`。`api/json_body.rs` 的 `JsonBody<T>`
// 是既有改造路径(收口为 `{error, code: VALIDATION}` + 400)。
//
// 存量 32 处**逐个**接入需按域判定(部分 handler 的 body 语义待确认),分批推进;
// 此处先加 ratchet 防新增未接入点:直接写 `Json(x): Json<T>` 的 handler 数量不得超基线,
// 接入一处请下调一处(只降不升)。
{
  const BASELINE_RAW_JSON_BODY = 32; // 2026-09-15 实测;只降不升
  const apiDir = join(SERVER, 'api');
  let hits = 0;
  const samples = [];
  for (const f of collect(apiDir, ['.rs'])) {
    // json_body.rs 是提取器的定义处,豁免(其单测里有 `JsonBody` 的对照用法)
    if (f.endsWith('json_body.rs')) continue;
    const src = readFileSync(f, 'utf8');
    const prodLines = productionLineCount(src);
    src.split('\n').forEach((line, i) => {
      const ln = i + 1;
      if (ln > prodLines) return;
      if (/^\s*\/\//.test(line)) return;
      if (!/Json\([a-z_]+\):\s*Json</.test(line)) return;
      hits++;
      if (samples.length < 3) samples.push(`${relative(ROOT, f)}:${ln}`);
    });
  }
  if (hits > BASELINE_RAW_JSON_BODY) {
    backendFailures.push(
      `[K] 未接入 JsonBody 的 JSON 体提取器新增:${hits} 处 > 基线 ${BASELINE_RAW_JSON_BODY}` +
        `(${samples.join(', ')});请改用 api/json_body.rs 的 JsonBody<T> 以统一 JSON 错误体`,
    );
  }
  if (VERBOSE) {
    console.log(`  [K] 未接入 JsonBody 的提取器:${hits} 处(基线 ${BASELINE_RAW_JSON_BODY},只降不升)`);
  }
}

// ========== 三结合代际归属与依赖方向(规则 I / J) ==========
//
// 单一事实源:`tools/arch-layers.json`(代际定义、模块归属、允许方向、已知偏离登记)。
//
// 为什么需要这两条规则:docs/契约-架构与数据.md 定义了「老中青三结合」分层(L1 稳 / L2 干 /
// L3 活),但 2026-09-14 全仓实测发现两处护栏盲区——
//   ① 5 个顶层模块(migration/scripts/slash/utils/plugins)从未被列入任何代际清单,分层
//      叙事对它们完全失效(新代码不知该按哪一代纪律评审);
//   ② 存在 10 条真实越代依赖边(tools 名义 L3,实为被 L2 依赖的核心设施),此前无任何机器约束。
// 规则 I 把「每个模块都有明确代际」变成硬约束;规则 J 把「跨代方向」变成 ratchet 门禁。
//
// 设计取舍:registeredEdges 登记的是**已核实的存量结构债务**,只报债务不阻塞(与
// KNOWN_CYCLES / BASELINE_BARE_ERROR 同模式)——若不做登记,门禁将永久红而失去信号价值;
// 一旦登记,**任何新增**未登记越代边立即 FAIL。还债即删条目,禁止为过检查而加条目。
{
  let cfg = null;
  try {
    cfg = JSON.parse(readFileSync(join(ROOT, 'tools', 'arch-layers.json'), 'utf8'));
  } catch (e) {
    backendFailures.push(
      `[I] 代际归属清单读取失败:tools/arch-layers.json(${e.message});该文件是分层的单一事实源,缺失即护栏失效`,
    );
  }
  if (cfg) {
    const genOf = (n) => cfg.backend?.[n]?.generation ?? null;
    /** 目录代际(仅 frontend 表:api/contracts/mvu/utils/stores/composables/components/sandbox)。 */
    const feDirGen = (n) => cfg.frontend?.[n]?.generation ?? null;
    /**
     * 前端「模块代际」解析:先查目录,再查根文件(frontendEntryFiles 的键带扩展名)。
     *
     * 为什么要解析根文件:web/src 根下约 30 个模块此前既未登记、也不参与方向校验,
     * 造成 components→render / stores→sseReducer 这类边整体被跳过(实测 69 条跨目录边
     * 中 49 条因一端无代际而静默略过)。根文件既可能是被依赖方(如上),也可能是
     * 依赖发起方(如 cardScriptHost.ts 导入 mvu),故两侧都要按代际校验。
     * 返回 null 表示未登记(由规则 I 单独报错,此处不重复报)。
     */
    const feModuleGen = (name) => {
      const dir = feDirGen(name);
      if (dir) return dir;
      const file =
        cfg.frontendEntryFiles?.[`${name}.ts`] ?? cfg.frontendEntryFiles?.[`${name}.vue`];
      return file?.generation ?? null;
    };
    const allowedDirs = new Set(cfg.dependencyRules?.allowed ?? []);
    /**
     * 后端与前端的方向规则**必须分开**:两者的 L1/L2/L3 虽是同一套代际词汇,
     * 依赖位置却不同(2026-09-14 澄清,见 docs/契约-架构与数据.md §2.3)。
     *
     *   后端 = 隔离模型: L3(工具/沙箱) 与 L2(编排) **互斥**——青层不得反向依赖
     *          骨干层(否则「隔离」名存实亡),故 L3->L2 与 L2->L3 双向禁止。
     *   前端 = 层次模型: L3(展示) → L2(状态编排) → L1(纯契约) 是**严格向下**的
     *          正常依赖。组件读 store、store 调 composable 是该架构的常态,而非越代;
     *          真正要禁止的是 L2->L3(编排依赖 UI/沙箱)与 L1->L2/L3(纯逻辑依赖状态)。
     *
     * 因此前端额外允许 L3->L2。这**不是**为过检查而放宽:若禁止 L3->L2,几乎每个
     * 组件都会「违规」,护栏会因噪声过载而失去信号价值——那才是真正的失效。
     */
    const backendAllowed = new Set(cfg.dependencyRules?.backendAllowed ?? cfg.dependencyRules?.allowed ?? []);
    const frontendAllowed = new Set(
      cfg.dependencyRules?.frontendAllowed ??
        [...(cfg.dependencyRules?.allowed ?? []), 'L3->L2'],
    );
    const isAllowedDir = (a, b) => backendAllowed.has(`${a}->${b}`);
    const isAllowedFeDir = (a, b) => frontendAllowed.has(`${a}->${b}`);

    /** 生产代码切片:剔除 `#[cfg(test)]` 及其后(测试里的跨模块引用不是架构依赖)。 */
    const productionSource = (src) => {
      const i = src.search(/^#\[cfg\(test\)\]/m);
      return i === -1 ? src : src.slice(0, i);
    };

    // --- 规则 I:归属完整性 ---
    let unregistered = 0;
    for (const name of readdirSync(SERVER)) {
      const p = join(SERVER, name);
      if (statSync(p).isDirectory()) {
        if (!cfg.backend?.[name]) {
          unregistered++;
          backendFailures.push(
            `[I] 后端模块未登记代际:server-rs/src/${name}/ 不在 arch-layers.json 的 backend 中` +
              `(新增模块必须先声明 L1/L2/L3 归属与职责,否则无从按代际纪律评审)`,
          );
        }
      } else if (name.endsWith('.rs')) {
        if (!cfg.entryFiles?.[name]) {
          unregistered++;
          backendFailures.push(
            `[I] 后端根文件未登记:server-rs/src/${name} 不在 arch-layers.json 的 entryFiles 中`,
          );
        }
      }
    }
    for (const name of readdirSync(SRC)) {
      const p = join(SRC, name);
      if (statSync(p).isDirectory()) {
        if (!cfg.frontend?.[name]) {
          unregistered++;
          frontendFailures.push(
            `[I] 前端目录未登记代际:web/src/${name}/ 不在 arch-layers.json 的 frontend 中`,
          );
        }
      } else if (!isTest(name) && /\.(ts|vue)$/.test(name)) {
        if (!cfg.frontendEntryFiles?.[name]) {
          unregistered++;
          frontendFailures.push(
            `[I] 前端根文件未登记:web/src/${name} 不在 arch-layers.json 的 frontendEntryFiles 中` +
              `(根文件同样要声明代际:它既是规则的被依赖方,也是依赖的发起方)`,
          );
        }
      }
    }
    if (VERBOSE) console.log(`  [I] 代际归属完整性:未登记 ${unregistered} 项`);

    // --- 规则 J:跨代依赖方向 ---
    //
    // 组合根(`entry` 代际:lib.rs/config.rs/main.rs、App.vue/main.ts)被**允许依赖一切**,
    // 这是分层架构的通用豁免——它的职责就是把各层拼装起来,故不参与方向校验。
    const registeredBackend = new Set((cfg.registeredEdges ?? []).map((e) => `${e.from}->${e.to}`));
    const backendEdges = new Map();
    for (const f of collect(SERVER, ['.rs'])) {
      const parts = relative(SERVER, f).split(sep);
      const from = parts.length === 1 ? 'entry' : parts[0];
      if (from === 'entry') continue;
      const src = readFileSync(f, 'utf8');
      productionSource(src)
        .split('\n')
        .forEach((line, i) => {
          const m = /^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+crate::([a-z_]+)/.exec(line);
          if (!m) return;
          const to = m[1];
          if (to === from || to === 'entry') return;
          if (!genOf(from) || !genOf(to)) return; // 未登记模块由规则 I 单独报错
          const key = `${from}->${to}`;
          if (!backendEdges.has(key)) backendEdges.set(key, { from, to, samples: [] });
          const rec = backendEdges.get(key);
          if (rec.samples.length < 3) rec.samples.push(`${relative(ROOT, f)}:${i + 1}`);
        });
    }
    let backendCrossGen = 0;
    let backendRegistered = 0;
    for (const [key, e] of backendEdges) {
      const ga = genOf(e.from);
      const gb = genOf(e.to);
      if (ga === gb || isAllowedDir(ga, gb)) continue; // 同代内自由 / 允许方向
      backendCrossGen++;
      if (registeredBackend.has(key)) {
        backendRegistered++;
        notes.push(
          `[债务] 后端越代依赖 ${e.from}(${ga}) → ${e.to}(${gb}) 例:${e.samples[0]}` +
            `(已登记于 arch-layers.json;还清后请删除该条目)`,
        );
      } else {
        backendFailures.push(
          `[J] 新增越代依赖:${e.from}(${ga}) → ${e.to}(${gb}) 例:${e.samples.join(', ')}` +
            `(L1 不得依赖上层、L2 不得依赖 L3、L3 不得依赖 L2;确属本轮无法消除,须在 arch-layers.json 的 ` +
            `registeredEdges 登记理由与收口路径)`,
        );
      }
    }
    // 登记陈旧检测:已登记但实测不存在的边 → 提示删除,防止登记表变成「历史沉积」而失去意义
    for (const key of registeredBackend) {
      if (!backendEdges.has(key)) {
        notes.push(
          `[登记陈旧] arch-layers.json 的 registeredEdges 含 ${key},但实测已无该依赖,请删除该条目`,
        );
      }
    }
    if (VERBOSE) {
      console.log(
        `  [J] 后端跨代边:${backendCrossGen} 条(已登记 ${backendRegistered},新增 ${backendCrossGen - backendRegistered})`,
      );
    }

    // 前端同法校验(仅登记回边;components→api 等 L3→L1 属允许方向)
    //
    // 与后端的差异:前端的「模块」既可能是顶层目录,也可能是 web/src 根文件。
    // 根文件此前从不参与方向校验,导致大量真实边被静默跳过;现按 feModuleGen
    // 统一解析两侧代际——目录查 frontend、根文件查 frontendEntryFiles。
    // 入口根文件(App.vue/main.ts/modals.ts/store.ts,generation=entry)照 backend 的
    // 组合根豁免,不参与方向校验(登记仍是硬要求,由规则 I 负责)。
    const registeredFrontend = new Set(
      (cfg.frontendRegisteredEdges ?? []).map((e) => `${e.from}->${e.to}`),
    );
    const feEdges = new Map();
    for (const f of collect(SRC, ['.ts', '.vue'])) {
      if (isTest(f)) continue;
      const relPath = relative(SRC, f).split(sep).join('/');
      const parts = relPath.split('/');
      const fromFile = parts.length === 1 ? parts[0].replace(/\.(ts|vue)$/, '') : null;
      const from = fromFile ?? parts[0];
      if (feModuleGen(from) === 'entry') continue; // 组合根作为依赖发起方 → 豁免
      const src = readFileSync(f, 'utf8');
      for (const m of src.matchAll(/from\s+['"]([^'"]+)['"]/g)) {
        const spec = m[1];
        let to = null;
        if (spec.startsWith('@/')) to = spec.slice(2).split('/')[0];
        else if (spec.startsWith('.')) {
          const joined = join(dirname(relPath), spec).split(sep).join('/');
          const seg = joined.split('/')[0];
          // ./x 在根目录下解析为根文件 x;../x 等上跳路径不视作同层依赖
          to = seg === '.' || seg === '..' ? null : seg;
        } else continue;
        if (to === null || to === from) continue;
        const toName = to.replace(/\.(ts|vue)$/, '');
        if (!feModuleGen(toName)) continue; // 未登记由规则 I 单独报错
        // 组合根作为依赖接收方 → 也豁免:前端 entry 是**被广泛引用的公共装配面**
        // (store.ts 门面聚合 7 个 store、modals.ts 是弹窗注册表),组件/composable
        // 经门面访问状态正是该架构的核心模式——若禁止,门面即不可用。这与后端
        // entry 只作发起方的语义不同,是前端层次模型的固有差异。
        if (feModuleGen(toName) === 'entry') continue;
        const key = `${from}->${toName}`;
        if (!feEdges.has(key)) feEdges.set(key, { from, to: toName, samples: [] });
        const rec = feEdges.get(key);
        if (rec.samples.length < 3) rec.samples.push(relPath);
      }
    }
    let feCrossGen = 0;
    for (const [key, e] of feEdges) {
      const ga = feModuleGen(e.from);
      const gb = feModuleGen(e.to);
      if (ga === gb || isAllowedFeDir(ga, gb)) continue;
      feCrossGen++;
      if (registeredFrontend.has(key)) {
        notes.push(
          `[债务] 前端越代依赖 ${e.from}(${ga}) → ${e.to}(${gb}) 例:${e.samples[0]}` +
            `(已登记于 arch-layers.json;还清后请删除该条目)`,
        );
      } else {
        frontendFailures.push(
          `[J] 前端新增越代依赖:${e.from}(${ga}) → ${e.to}(${gb}) 例:${e.samples.join(', ')}` +
            `(L1 不得依赖上层、L2 不得依赖 L3、L3 不得依赖 L2)`,
        );
      }
    }
    for (const key of registeredFrontend) {
      if (!feEdges.has(key)) {
        notes.push(
          `[登记陈旧] arch-layers.json 的 frontendRegisteredEdges 含 ${key},但实测已无该依赖,请删除该条目`,
        );
      }
    }
    if (VERBOSE) console.log(`  [J] 前端跨代边:${feCrossGen} 条(登记 ${registeredFrontend.size})`);
  }
}

// ========== 汇总 ==========
//
// 退出纪律:后端桶与前端桶**都必须先打印再退出**。
// 此前规则 I 的前端分支写进 backendFailures,而这里在后端违规时立即 exit(1),
// 使前端报告段永不输出——前端规则 J 的结论被静默吞掉。现将两者的报告与退出
// 统一到本段末尾:任一侧非空即 exit(1),门禁强度不变,可见性恢复。

console.log('\n========== Kedai 后端分层与冻结护栏(C/D/E/G/H/I/J/K) ==========');
if (backendFailures.length) {
  console.log(`[FAIL] ${backendFailures.length} 处分层违规(全部规则均为硬门禁):`);
  for (const f of backendFailures.slice(0, 40)) console.log(`  - ${f}`);
  if (backendFailures.length > 40) console.log(`  ... 另有 ${backendFailures.length - 40} 处`);
} else {
  console.log('[ OK ] 后端分层无违规');
}

console.log('\n========== Kedai 前端架构护栏(A/B/I/J) ==========');
if (frontendFailures.length) {
  console.log(`[FAIL] ${frontendFailures.length} 处架构违规:`);
  for (const f of frontendFailures.slice(0, 40)) console.log(`  - ${f}`);
  if (frontendFailures.length > 40) console.log(`  ... 另有 ${frontendFailures.length - 40} 处`);
} else {
  console.log('[ OK ] 前端架构无违规');
}
console.log(`store 循环依赖:${cycles.length} 个(新增 ${newCycles})`);
console.log(`组件直改 state:${writeHits} 处(白名单 ${whitelisted})`);
if (notes.length) {
  console.log(`\n待偿还债务 ${notes.length} 条:`);
  for (const n of notes) console.log(`  - ${n}`);
}
if (failures.length) {
  console.log(`\n[FAIL] ${failures.length} 处架构违规:`);
  for (const f of failures) console.log(`  - ${f}`);
}
if (backendFailures.length || frontendFailures.length || failures.length) {
  process.exit(1);
}
console.log('\n[ OK ] 无新增架构违规');
