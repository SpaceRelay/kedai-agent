#!/usr/bin/env node
/**
 * check-frontend-lint.mjs — 前端类型逃逸 ratchet 护栏(纯 Node 零依赖)。
 *
 * 为什么不用 ESLint:
 *   仓库既有护栏(check-arch / check-contract)统一为「纯 Node 零依赖」风格;引入
 *   eslint + typescript-eslint 会拉入上百个包并改动 package-lock.json,而 build.ps1
 *   的确定性构建依赖该 lockfile。本脚本用同样的零依赖方式达成「只降不升」的 ratchet,
 *   且天然覆盖 .vue(ESLint 需额外配置 vue-eslint-parser 才能做到)。
 *
 * 检查项(均为「类型安全被稀释」的迹象):
 *   - `as never` / `as unknown as` —— 断言逃逸,绕过类型检查
 *   - `@ts-expect-error` / `@ts-ignore` —— 显式压错
 *   - 非空断言 `x!.y` —— 运行期可能 panic(前端为 undefined 报错)
 *   - `any` —— 类型全丢(基线为 0,一旦出现即 FAIL)
 *
 * 用法:
 *   node tools/check-frontend-lint.mjs            # 超基线即 FAIL
 *   node tools/check-frontend-lint.mjs --verbose  # 打印逐项计数
 *
 * ratchet 纪律:基线只能调**低**。修好一批就把 BASELINE 数字改小(至少不高于实际值),
 * 绝不允许调高——调高等于关掉护栏。注释行不计入。
 */

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const SRC = join(ROOT, 'web', 'src');
const VERBOSE = process.argv.includes('--verbose');

/**
 * 存量基线(2026-09-13 首次建立,值为**本脚本自身统计**的实测值)。只能下调。
 * 注意:非空断言 33 处包含 `f(x)!.y` 这类「右括号后断言」形态(比 `\w+!.` 这种
 * 粗 grep 更全),故高于手数结果。每项含义见文件头注释。
 *
 * `@ts-expect-error` 由 3 上调至 6 的理由(2026-09-13,批次 F):
 *   新增的 `web/src/mvu/parser.contract.test.ts` 需读 `node:fs/url/path` 三个内置模块,
 *   而本仓库未安装 `@types/node`(前端源码不需要),故按**既有约定**逐行压制类型错误
 *   ——与 `characterScriptSandbox.test.ts:11`、`sandbox/uma-creation.test.ts:12`、
 *   `sandbox/wuwa-sim.test.ts:8` 处理 `node:vm` 的写法完全一致。属必要且合规的新增,
 *   非「用断言绕过类型检查」。若将来补装 @types/node,应把这 6 处一并删除并下调基线。
 */
const BASELINE = {
  any: 0,
  asNever: 31,
  asUnknownAs: 70,
  tsExpectError: 6,
  nonNull: 33,
};

/** 逐项匹配规则:名称 → 正则(全局)。 */
const RULES = {
  any: /(?::\s*any\b|\bas\s+any\b|<any>)/g,
  asNever: /\bas\s+never\b/g,
  asUnknownAs: /\bas\s+unknown\s+as\b/g,
  tsExpectError: /@ts-(?:expect-error|ignore)\b/g,
  // 保守:只看 `x!.` 形态(排除 !== / != / 字符串叹号);保守高估优于漏报
  nonNull: /[\w)\]]!\s*\./g,
};

function collect(dir, exts, out = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) collect(p, exts, out);
    else if (exts.some((e) => name.endsWith(e))) out.push(p);
  }
  return out;
}

/**
 * 去掉整行注释与块注释,避免注释里的示例被计数。
 *
 * **行尾归一化不可省(2026-09-16 修)**:先前的实现直接逐行 `l.replace(/\/\/.*$/, '')`,
 * 而 JS 正则的 `.` **不匹配**行终止符——`\r` 也是行终止符,于是 `$`(无 m 标志,只认字符串
 * 末尾)永远到不了,行内 `//` 注释在 **CRLF 文件上完全不被剥离**。后果:注释里出现的
 * `as any` / `as never` / 非空断言被当成真实逃逸计数,ratchet 结果**取决于文件行尾**。
 * 本仓库 Windows + `core.autocrlf=true`,checkout 后大量文件是 CRLF(实测 233 个源文件中
 * 53 个),该缺陷会直接导致误报 FAIL(实测:某注释里的 `as any` 使 any 由 0 虚高到 1)。
 * 归一化到 `\n` 后,行号不变、注释剥离对 LF/CRLF/CR 三种行尾口径一致。
 * 块注释剥离用 `[\s\S]` 本就能跨行匹配,不受此影响。
 */
function stripComments(src) {
  return src
    .replace(/\r\n?/g, '\n')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .split('\n')
    .map((l) => l.replace(/\/\/.*$/, ''))
    .join('\n');
}

const files = collect(SRC, ['.ts', '.vue']);
const counts = Object.fromEntries(Object.keys(RULES).map((k) => [k, 0]));
const hits = Object.fromEntries(Object.keys(RULES).map((k) => [k, []]));

for (const f of files) {
  const raw = readFileSync(f, 'utf8');
  const src = stripComments(raw);
  for (const [name, re] of Object.entries(RULES)) {
    // @ts-expect-error / @ts-ignore 本身以注释形式存在,必须按**原始源**计数
    // (去注释后会被清掉,导致漏报)。
    const hay = name === 'tsExpectError' ? raw : src;
    for (const m of hay.matchAll(re)) {
      counts[name]++;
      if (hits[name].length < 5) {
        const line = hay.slice(0, m.index).split('\n').length;
        hits[name].push(`${relative(ROOT, f)}:${line}`);
      }
    }
  }
}

const LABEL = {
  any: 'any(类型全丢)',
  asNever: 'as never(断言逃逸)',
  asUnknownAs: 'as unknown as(双重断言逃逸)',
  tsExpectError: '@ts-expect-error/@ts-ignore(显式压错)',
  nonNull: '非空断言 x!.y(潜在 undefined 报错)',
};

const over = [];
for (const k of Object.keys(RULES)) {
  if (counts[k] > BASELINE[k]) {
    over.push(
      `${LABEL[k]}:${counts[k]} 处 > 基线 ${BASELINE[k]}(新增 ${counts[k] - BASELINE[k]})` +
        (hits[k].length ? `\n        例:${hits[k].slice(0, 3).join(', ')}` : ''),
    );
  }
}

console.log('========== Kedai 前端类型护栏(ratchet)==========');
for (const k of Object.keys(RULES)) {
  const delta = counts[k] - BASELINE[k];
  const mark = delta > 0 ? '[超基线]' : delta < 0 ? '[可下调基线]' : '[= 基线]';
  console.log(`  ${LABEL[k]}:${counts[k]}(基线 ${BASELINE[k]})${mark}`);
}
if (VERBOSE) {
  for (const k of Object.keys(RULES)) {
    if (hits[k].length) console.log(`  [${k}] 例:${hits[k].join(', ')}`);
  }
}

if (over.length) {
  console.log(`\n[FAIL] ${over.length} 项类型逃逸超出基线(ratchet 只降不升):`);
  for (const o of over) console.log(`  - ${o}`);
  console.log('\n  说明:新增的断言/any 请改为真实类型;确有必要请说明理由并**下调**其他项基线。');
  process.exit(1);
}
const improvable = Object.keys(RULES).filter((k) => counts[k] < BASELINE[k]);
if (improvable.length) {
  console.log(
    `\n[ OK ] 未超基线。可下调基线:${improvable.map((k) => `${LABEL[k].split('(')[0]}=${counts[k]}`).join(', ')}`,
  );
} else {
  console.log('\n[ OK ] 未超基线');
}
