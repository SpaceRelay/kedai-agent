#!/usr/bin/env node
// 双 Cargo.lock 漂移检查(批次 1 基础设施,2026-09-13)
//
// 背景:src-tauri 以 path 内嵌 server-rs(`kedai-server = { path = "../server-rs" }`)。
// 构建 src-tauri 时 cargo **忽略 server-rs/Cargo.lock**,在 src-tauri/Cargo.lock 里重新
// 解析整棵依赖树 —— 同一份后端源码在「测试版 kedai-server.exe」与「便携版 Kedai.exe」
// 中可能编译出不同版本的依赖,且跨 lock 不会报 links 冲突,属静默漂移。
//
// 判定规则:server-rs 锁中的每个 (name, version) 必须同样出现在 src-tauri 锁的
// 该 name 版本集合里(内嵌方选中的版本集合须覆盖被内嵌方)。仅存在于单侧的 crate
// 跳过(平台 cfg 差异:server-rs 的 android/windows 专属依赖、src-tauri 的 tauri 依赖)。
//
// 用法:
//   node tools/check-lock-sync.mjs          # 检查;新增漂移 → exit 1
//   node tools/check-lock-sync.mjs --json   # 机器可读输出
// 基线:tools/lock-sync-baseline.json 的 allowedDrift(接受的历史漂移名单)。
// 收紧纪律:漂移修复后应把该 name 从基线删除(脚本会提示可收紧项)。
import { readFileSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const SERVER_LOCK = join(ROOT, 'server-rs', 'Cargo.lock');
const TAURI_LOCK = join(ROOT, 'src-tauri', 'Cargo.lock');
const BASELINE = join(ROOT, 'tools', 'lock-sync-baseline.json');

/// 本仓自身包(工作区成员):版本号由 tools/bump-version.ps1 单点变更,lock 里的
/// 自身版本由各自构建更新 —— 版本号刚改、便携版尚未重建时两侧会暂时不一致,
/// 那是**版本更新的正常中间态**,不是依赖漂移,必须排除(否则每次发版都误报)。
const WORKSPACE_PACKAGES = new Set(['kedai-server', 'kedai-desktop', 'kedai-web', 'kedai', 'launcher']);

/** 解析 Cargo.lock → Map<name, Set<version>> */
export function parseLock(text) {
  const map = new Map();
  let inPackage = false;
  let name = null;
  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (line === '[[package]]') {
      inPackage = true;
      name = null;
      continue;
    }
    if (!inPackage) continue;
    if (line.startsWith('name = ')) {
      name = JSON.parse(line.slice('name = '.length));
      continue;
    }
    if (line.startsWith('version = ') && name) {
      const version = JSON.parse(line.slice('version = '.length));
      if (!map.has(name)) map.set(name, new Set());
      map.get(name).add(version);
      inPackage = false; // 版本行之后即为 source/dependencies,本包解析结束
    }
  }
  return map;
}

function loadBaseline() {
  if (!existsSync(BASELINE)) return { allowedDrift: [] };
  const raw = JSON.parse(readFileSync(BASELINE, 'utf8'));
  return { allowedDrift: Array.isArray(raw.allowedDrift) ? raw.allowedDrift : [] };
}

/** 计算漂移:server 侧选中的版本未出现在 tauri 侧版本集合 */
export function findDrift(serverMap, tauriMap) {
  const drift = [];
  for (const [name, versions] of serverMap) {
    if (WORKSPACE_PACKAGES.has(name)) continue; // 本仓自身包:版本更迭的中间态,非漂移
    const other = tauriMap.get(name);
    if (!other) continue; // 单侧存在:平台 cfg 差异,跳过
    for (const v of versions) {
      if (!other.has(v)) {
        drift.push({ name, serverVersion: v, tauriVersions: [...other].sort() });
      }
    }
  }
  drift.sort((a, b) => a.name.localeCompare(b.name));
  return drift;
}

function main() {
  const jsonMode = process.argv.includes('--json');
  for (const [label, path] of [
    ['server-rs/Cargo.lock', SERVER_LOCK],
    ['src-tauri/Cargo.lock', TAURI_LOCK],
  ]) {
    if (!existsSync(path)) {
      console.error(`[FAIL] 缺少 ${label}(${path})`);
      process.exit(1);
    }
  }
  const serverMap = parseLock(readFileSync(SERVER_LOCK, 'utf8'));
  const tauriMap = parseLock(readFileSync(TAURI_LOCK, 'utf8'));
  const drift = findDrift(serverMap, tauriMap);
  const { allowedDrift } = loadBaseline();
  const allowed = new Set(allowedDrift);
  const unexplained = drift.filter((d) => !allowed.has(d.name));
  const staleBaseline = allowedDrift.filter((n) => !drift.some((d) => d.name === n));

  if (jsonMode) {
    console.log(
      JSON.stringify(
        {
          serverCrates: serverMap.size,
          tauriCrates: tauriMap.size,
          drift,
          unexplained: unexplained.map((d) => d.name),
          staleBaseline,
        },
        null,
        2,
      ),
    );
  } else {
    console.log('========== 双 Cargo.lock 漂移检查 ==========');
    console.log(`server-rs: ${serverMap.size} 个包;src-tauri: ${tauriMap.size} 个包`);
    if (drift.length === 0) {
      console.log('[ OK ] 无漂移:server-rs 侧依赖版本全部被 src-tauri 锁覆盖');
    } else {
      console.log(`漂移 ${drift.length} 处(基线允许 ${allowed.size} 处):`);
      for (const d of drift) {
        const tag = allowed.has(d.name) ? '基线' : '新增';
        console.log(
          `  [${tag}] ${d.name}: server-rs=${d.serverVersion} / src-tauri={${d.tauriVersions.join(', ')}}`,
        );
      }
    }
    if (staleBaseline.length > 0) {
      console.log(
        `[提示] 基线中以下条目已不再漂移,可从 tools/lock-sync-baseline.json 删除:${
          staleBaseline.join(', ')
        }`,
      );
    }
    if (unexplained.length > 0) {
      console.log(
        `\n[FAIL] 新增 lock 漂移 ${unexplained.length} 处:${unexplained
          .map((d) => `${d.name}@${d.serverVersion}`)
          .join(', ')}`,
      );
      console.log(
        '处理:①对齐版本(在对应 lock 内 cargo update -p <crate> --precise <ver>);' +
          '②确属不可避免时登记进 tools/lock-sync-baseline.json 的 allowedDrift。',
      );
    } else {
      console.log('[ OK ] 无新增漂移');
    }
  }
  process.exit(unexplained.length > 0 ? 1 : 0);
}

main();
