/**
 * Kedai 文档一致性门禁(tools/check-docs.mjs)
 *
 * 背景:2026-09-16 把 68 份散落 md 整合为「契约/功能/计划/展望/经验/遗留」六类体系,
 * 并归档到 docs/archive/2026-09-16-consolidation/。整合的成果若无守护,会缓慢退化回
 * 「根下又散落一堆文档」「索引与实际不一致」「链接指向已归档文件」的状态。
 * 本脚本把该约定变成硬门禁(此前仓库没有任何 docs/ 的机器校验)。
 *
 * 用法:
 *   node tools/check-docs.mjs            # 校验,失败 exit 1
 *   node tools/check-docs.mjs --verbose  # 额外打印通过项明细
 *   node tools/check-docs.mjs --json     # 机器可读输出
 *
 * 规则:
 *   D1 六类活文档均存在
 *   D2 docs/ 根下只允许白名单(六类活文档 + README.md + fixtures/ + archive/)
 *   D3 docs/README.md 的六类表格与实际文件**双向一致**(列了必须存在;存在必须列了)
 *   D4 活文档内的相对 markdown 链接目标必须存在(锚点尽量校验)
 *   D5 归档映射表覆盖归档目录下全部文件
 *   D6 (WARN) 计划/遗留条目应带 ID(P- / L / T / D / 批次 / 项)
 *
 * 失败只影响本脚本自身结论;check-all.ps1 侧用统一 Invoke-Stage 聚合,
 * **不要**另建失败桶(见 MAINTENANCE.md §0「修复纪律」:曾因前端分支写进后端失败桶
 * 导致报告段被静默吞掉)。
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DOCS = path.join(ROOT, 'docs');
const VERBOSE = process.argv.includes('--verbose');
const JSON_OUT = process.argv.includes('--json');

// 六类活文档(2026-09-16 体系)
const CLASSIFIED = [
  'docs/契约.md',
  'docs/契约-协议与配置.md',
  'docs/契约-架构与数据.md',
  'docs/功能.md',
  'docs/功能-变更史.md',
  'docs/计划.md',
  'docs/展望.md',
  'docs/经验.md',
  'docs/遗留.md',
];

// docs/ 根下允许出现的条目(D2 白名单)
const ROOT_ALLOW = new Set([...CLASSIFIED, 'docs/README.md']);
const ROOT_ALLOW_DIRS = new Set(['docs/fixtures', 'docs/archive']);

const ARCHIVE_DIR = 'docs/archive/2026-09-16-consolidation';

/** 检查条目:level 为 'FAIL' | 'WARN' */
const findings = [];
const fail = (rule, msg) => findings.push({ level: 'FAIL', rule, msg });
const warn = (rule, msg) => findings.push({ level: 'WARN', rule, msg });
const okLog = [];
const ok = (rule, msg) => okLog.push(`[ OK ] ${rule} ${msg}`);

const read = (p) => fs.readFileSync(path.join(ROOT, p), 'utf8');
const exists = (p) => fs.existsSync(path.join(ROOT, p));
const rel = (abs) => path.relative(ROOT, abs).split(path.sep).join('/');

// ---------- D1 六类活文档存在 ----------
for (const f of CLASSIFIED) {
  if (!exists(f)) fail('D1', `六类活文档缺失:${f}`);
}
if (CLASSIFIED.every(exists)) ok('D1', `六类活文档齐备(${CLASSIFIED.length} 份)`);

// ---------- D2 docs/ 根下只允许白名单 ----------
if (!fs.existsSync(DOCS)) {
  fail('D2', 'docs/ 目录不存在');
} else {
  const stray = [];
  for (const e of fs.readdirSync(DOCS, { withFileTypes: true })) {
    const p = `docs/${e.name}`;
    if (e.isDirectory()) {
      if (!ROOT_ALLOW_DIRS.has(p)) stray.push(p + '/');
      continue;
    }
    if (!ROOT_ALLOW.has(p)) stray.push(p);
  }
  if (stray.length) {
    fail(
      'D2',
      `docs/ 根下出现未登记条目(应归档、或先在 docs/README.md 登记分类):\n` +
        stray.map((s) => `       - ${s}`).join('\n')
    );
  } else {
    ok('D2', 'docs/ 根下无未登记条目');
  }
}

// ---------- D3 docs/README.md 索引与实际双向一致 ----------
const README = 'docs/README.md';
if (!exists(README)) {
  fail('D3', `索引文件缺失:${README}`);
} else {
  const src = read(README);
  const linked = new Set();
  // 收集索引表里指向 docs/*.md 的链接(排除 archive/、fixtures/)
  const linkRe = /\]\((?!https?:)([^)#]+\.md)(?:#[^)]*)?\)/g;
  let m;
  while ((m = linkRe.exec(src))) {
    const t = m[1].replace(/^(\.\.\/)+/, '').replace(/^\.\//, '');
    if (t.startsWith('archive/') || t.startsWith('fixtures/')) continue;
    // 根文档(AGENTS/README/MAINTENANCE)用 ../ 形式指向仓库根,不算 docs 体系
    if (!src.slice(Math.max(0, m.index - 60), m.index).includes('../')) {
      linked.add('docs/' + path.posix.basename(t));
    }
  }
  const missingInIndex = CLASSIFIED.filter((f) => !linked.has(f));
  const citingGhost = [...linked].filter((f) => !exists(f));
  if (missingInIndex.length) {
    fail('D3', `新增活文档未登记进 ${README}:\n` + missingInIndex.map((f) => `       - ${f}`).join('\n'));
  }
  if (citingGhost.length) {
    fail('D3', `${README} 引用了不存在的文档:\n` + citingGhost.map((f) => `       - ${f}`).join('\n'));
  }
  if (!missingInIndex.length && !citingGhost.length) {
    ok('D3', `${README} 索引与 docs/ 实际文件一致(${linked.size} 条活文档链接)`);
  }
}

// ---------- D4 活文档相对链接目标存在 ----------
{
  const files = [...CLASSIFIED, README].filter(exists);
  let checked = 0;
  const broken = [];
  for (const f of files) {
    const src = read(f);
    const re = /\]\((?!https?:|mailto:)([^)#]+?)(#[^)]*)?\)/g;
    let m;
    while ((m = re.exec(src))) {
      let target = m[1].trim();
      if (!target) continue;
      if (/\.(png|jpg|jpeg|gif|svg|json|rs|ts|vue|ps1|mjs)$/i.test(target) && !target.endsWith('.md')) {
        // 非 markdown 目标:仅当是相对路径时才校验
      }
      const abs = path.resolve(path.dirname(path.join(ROOT, f)), target);
      checked++;
      if (!fs.existsSync(abs)) broken.push(`${f} -> ${target}`);
    }
  }
  if (broken.length) {
    fail('D4', `活文档内链接目标不存在(${broken.length} 条):\n` + broken.map((b) => `       - ${b}`).join('\n'));
  } else {
    ok('D4', `活文档内相对链接全部有效(${checked} 条)`);
  }
}

// ---------- D5 归档映射表覆盖归档目录全部文件 ----------
if (!exists(ARCHIVE_DIR)) {
  warn('D5', `归档目录不存在,跳过:${ARCHIVE_DIR}(若尚未执行归档属预期)`);
} else {
  const indexPath = `${ARCHIVE_DIR}/README.md`;
  if (!exists(indexPath)) {
    fail('D5', `归档映射表缺失:${indexPath}`);
  } else {
    const idx = read(indexPath);
    const files = fs
      .readdirSync(path.join(ROOT, ARCHIVE_DIR), { withFileTypes: true })
      .filter((e) => e.isFile() && e.name.endsWith('.md') && e.name !== 'README.md')
      .map((e) => e.name);
    const uncovered = files.filter((n) => !idx.includes(n));
    const ghostRefs = [];
    const re = /\[`([^`]+\.md)`\]\(([^)]+)\)/g;
    let m;
    while ((m = re.exec(idx))) {
      const encoded = m[2];
      const abs = path.join(ROOT, ARCHIVE_DIR, decodeURIComponent(encoded));
      if (!fs.existsSync(abs)) ghostRefs.push(m[1]);
    }
    if (uncovered.length) {
      fail(
        'D5',
        `归档目录下有 ${uncovered.length} 份文件未登记进映射表:\n` +
          uncovered.map((n) => `       - ${n}`).join('\n')
      );
    }
    if (ghostRefs.length) {
      fail('D5', `映射表引用了不存在的归档文件:\n` + ghostRefs.map((n) => `       - ${n}`).join('\n'));
    }
    if (!uncovered.length && !ghostRefs.length) {
      ok('D5', `归档映射表覆盖全部 ${files.length} 份归档文档`);
    }
  }
}

// ---------- D6 (WARN) 条目 ID 存在性 ----------
{
  const idRules = [
    { file: 'docs/遗留.md', re: /^#{2,3}\s+.*\b(L\d{1,2}|T\d|D\d)\b/m, hint: 'L*/T*/D*' },
    { file: 'docs/计划.md', re: /^#{2,3}\s+.*(P-\d+|批次\s*\d|[A-Z]\.\d)/m, hint: 'P-*/批次 N/X.N' },
  ];
  for (const { file, re, hint } of idRules) {
    if (!exists(file)) continue;
    const src = read(file);
    const h2 = (src.match(/^##\s+/gm) || []).length;
    if (h2 === 0) warn('D6', `${file} 无二级章节,疑似结构被破坏`);
    if (!re.test(src)) warn('D6', `${file} 未见条目 ID 形态(${hint}),编号是全仓引用锚点,请勿改成无语义标题`);
  }
  ok('D6', '条目 ID 形态检查完成(仅 WARN 级)');
}

// ---------- 输出 ----------
const fails = findings.filter((f) => f.level === 'FAIL');
const warns = findings.filter((f) => f.level === 'WARN');

if (JSON_OUT) {
  console.log(JSON.stringify({ ok: fails.length === 0, findings }, null, 2));
} else {
  if (VERBOSE) {
    console.log('\n---------- 通过项 ----------');
    okLog.forEach((l) => console.log(l));
  }
  if (warns.length) {
    console.log('\n[WARN] 文档检查警告(不阻断):');
    warns.forEach((w) => console.log(`  - [${w.rule}] ${w.msg}`));
  }
  if (fails.length) {
    console.log('\n[FAIL] 文档一致性检查未通过:');
    fails.forEach((f) => console.log(`  - [${f.rule}] ${f.msg}`));
    console.log('\n六类体系约定见 docs/README.md。');
    process.exit(1);
  }
  console.log('\n[ OK ] 文档六类体系一致(docs/ 结构 / 索引 / 链接 / 归档映射)');
}

process.exit(fails.length ? 1 : 0);
