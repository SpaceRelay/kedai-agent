#!/usr/bin/env node
// count-tests.mjs — 自动统计后端/前端测试数量,防止文档里的测试数字持续漂移。
//
// 背景:MAINTENANCE.md / docs 中长期记录「后端 923(740 单测+183 集成)、前端 697(71 文件)」,
// 而实际已增长到后端 953、前端 732——数字靠手抄必然过期,故改为脚本统计。
//
// 用法:
//   node tools/count-tests.mjs            # 打印统计(人读)
//   node tools/count-tests.mjs --json     # 输出 JSON(供 check-all 比对)
//   node tools/count-tests.mjs --check    # 与 MAINTENANCE.md 中记录的数字比对,不一致则 exit 1
//
// 统计口径:
//   后端单元测试 = src/**/*.rs 中 #[test] / #[tokio::test] 出现次数
//   后端集成测试 = tests/**/*.rs 中同上(文件数另计)
//   前端测试文件 = web/src/**/*.test.ts 数量
//   前端用例     = 上述文件内 it( / test( 调用次数(排除 it.each/test.each 之外的误配)
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function walk(dir, filter, out = []) {
  if (!fs.existsSync(dir)) return out;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, filter, out);
    else if (filter(p)) out.push(p);
  }
  return out;
}

function countMatches(file, re) {
  const src = fs.readFileSync(file, 'utf8');
  return (src.match(re) || []).length;
}

// ===== 后端 =====
const rsFilter = (p) => p.endsWith('.rs');
const srcFiles = walk(path.join(ROOT, 'server-rs', 'src'), rsFilter);
const testFiles = walk(path.join(ROOT, 'server-rs', 'tests'), rsFilter);
const RUST_TEST_RE = /#\[(?:tokio::)?test(?:\]|\()/g;

const backendUnit = srcFiles.reduce((n, f) => n + countMatches(f, RUST_TEST_RE), 0);
const backendIntegration = testFiles.reduce((n, f) => n + countMatches(f, RUST_TEST_RE), 0);

// ===== 前端 =====
const tsFiles = walk(path.join(ROOT, 'web', 'src'), (p) => p.endsWith('.test.ts'));
// 用例:行首(可缩进) it( / test( ;排除 .each / .skip 等链式误配
const TS_CASE_RE = /^\s*(?:it|test)\s*\(/gm;
const frontendCases = tsFiles.reduce((n, f) => n + countMatches(f, TS_CASE_RE), 0);

const result = {
  backendUnit,
  backendIntegration,
  backendTotal: backendUnit + backendIntegration,
  backendIntegrationFiles: testFiles.length,
  frontendFiles: tsFiles.length,
  frontendCases,
};

if (process.argv.includes('--json')) {
  console.log(JSON.stringify(result, null, 2));
  process.exit(0);
}

const summary = [
  `后端单元测试 : ${backendUnit}`,
  `后端集成测试 : ${backendIntegration}(${testFiles.length} 个文件)`,
  `后端合计     : ${result.backendTotal}`,
  `前端测试文件 : ${tsFiles.length}`,
  `前端用例     : ${frontendCases}`,
].join('\n');

if (process.argv.includes('--check')) {
  const maint = fs.readFileSync(path.join(ROOT, 'MAINTENANCE.md'), 'utf8');
  // 文档中形如:「953 个测试(770 单测 + 183 集成)」「732 个(78 文件)」
  const mTotal = maint.match(/(\d+)\s*个测试\s*\((\d+)\s*单测\s*\+\s*(\d+)\s*集成/);
  const mFront = maint.match(/(\d+)\s*个\s*\((\d+)\s*文件/);
  const problems = [];
  if (mTotal) {
    const [, total, unit, integ] = mTotal.map(Number);
    if (total !== result.backendTotal) {
      problems.push(`后端合计:文档 ${total} ≠ 实际 ${result.backendTotal}`);
    }
    if (unit !== backendUnit) problems.push(`后端单测:文档 ${unit} ≠ 实际 ${backendUnit}`);
    if (integ !== backendIntegration) problems.push(`后端集成:文档 ${integ} ≠ 实际 ${backendIntegration}`);
  } else {
    problems.push('MAINTENANCE.md 未找到「N 个测试(M 单测 + K 集成)」格式,无法比对');
  }
  if (mFront) {
    const [, cases, files] = mFront.map(Number);
    if (cases !== frontendCases) problems.push(`前端用例:文档 ${cases} ≠ 实际 ${frontendCases}`);
    if (files !== tsFiles.length) problems.push(`前端文件:文档 ${files} ≠ 实际 ${tsFiles.length}`);
  }
  if (problems.length) {
    console.error('[WARN] 测试数字与 MAINTENANCE.md 不一致(文档漂移):');
    problems.forEach((p) => console.error('       - ' + p));
    console.error('       请更新 MAINTENANCE.md,或在文档中注明「随脚本统计」。');
    process.exit(1);
  }
  console.log(summary);
  console.log('\n[OK] 与 MAINTENANCE.md 记录一致');
  process.exit(0);
}

console.log(summary);
