#!/usr/bin/env node
/**
 * Kedai 性能基线压测脚本(Node 零依赖,需 Node 18+ 自带 fetch)。
 *
 * 用法:
 *   node tools/perf-baseline.mjs [--base http://127.0.0.1:3001] [-c 20] [-n 100]
 *                                [--max-p95-factor 1.25] [--baseline tools/perf-baseline.json]
 *
 * 前提:Kedai 服务已在 --base 地址运行(脚本自动从 /api/bootstrap 取 token)。
 * 输出:各端点 p50 / p95 / 平均延迟与吞吐量,附 JSON 行便于落档对比。
 *
 * 退出码(2026-09-14 性能门禁):
 *   0 = 全部端点达标(或未启用阈值);
 *   1 = 任一端点超出阈值 / errors > 0 / 脚本自身失败。
 * JSON 行**先输出后判退**,失败时仍可落档对比。
 *
 * 阈值语义(与 docs/功能-变更史.md 口径一致):
 *   当前 p95 <= 基线 p95 × factor,且 errors == 0 视为达标。
 *   基线缺失该端点时跳过该项(仅记录,不判失败)。
 *   p95 < ABSOLUTE_FLOOR_MS 时忽略(本机数据量小、抖动大,微秒级差异无意义)。
 */

const args = process.argv.slice(2);
function argOf(flag, fallback) {
  const i = args.indexOf(flag);
  return i >= 0 && args[i + 1] ? args[i + 1] : fallback;
}
const BASE = argOf('--base', 'http://127.0.0.1:3001').replace(/\/$/, '');
const CONCURRENCY = Number(argOf('-c', '20'));
const REQUESTS = Number(argOf('-n', '100'));
/** p95 允许劣化倍数(相对基线);<=0 或未给基线文件时不判失败 */
const MAX_P95_FACTOR = Number(argOf('--max-p95-factor', '1.25'));
const BASELINE_PATH = argOf('--baseline', 'tools/perf-baseline.json');
/** 绝对地板:低于此值的 p95 不参与判失败(规避小数据集抖动) */
const ABSOLUTE_FLOOR_MS = 5;

async function getToken() {
  const res = await fetch(`${BASE}/api/bootstrap`);
  if (!res.ok) throw new Error(`bootstrap 失败: HTTP ${res.status}`);
  const data = await res.json();
  if (!data.token) throw new Error('bootstrap 响应缺少 token 字段');
  return data.token;
}

function authHeaders(token) {
  return { Authorization: `Bearer ${token}` };
}

/** 并发执行 REQUESTS 次 GET,返回延迟数组(ms)。 */
async function hammer(path, token) {
  const latencies = [];
  let next = 0;
  const started = performance.now();
  async function worker() {
    while (true) {
      const i = next++;
      if (i >= REQUESTS) return;
      const t0 = performance.now();
      const res = await fetch(`${BASE}${path}`, { headers: authHeaders(token) });
      await res.arrayBuffer(); // 读完响应体,计入完整传输耗时
      latencies.push({ ms: performance.now() - t0, status: res.status });
    }
  }
  await Promise.all(Array.from({ length: CONCURRENCY }, worker));
  const wallMs = performance.now() - started;
  return { latencies, wallMs };
}

function stats(samples) {
  const ok = samples.filter((s) => s.status < 400);
  const arr = ok.map((s) => s.ms).sort((a, b) => a - b);
  if (arr.length === 0) return null;
  const pick = (q) => arr[Math.min(arr.length - 1, Math.floor(arr.length * q))];
  const avg = arr.reduce((a, b) => a + b, 0) / arr.length;
  return {
    count: arr.length,
    errors: samples.length - ok.length,
    p50: +pick(0.5).toFixed(1),
    p95: +pick(0.95).toFixed(1),
    avg: +avg.toFixed(1),
  };
}

/** 找一个有消息的会话 id:角色列表 → 每个角色查会话 → 首个非空。 */
async function findSessionId(token) {
  const chars = await (
    await fetch(`${BASE}/api/characters`, { headers: authHeaders(token) })
  ).json();
  const list = Array.isArray(chars) ? chars : chars.characters || [];
  for (const c of list) {
    const res = await fetch(`${BASE}/api/chat/sessions?character_id=${encodeURIComponent(c.id)}`, {
      headers: authHeaders(token),
    });
    if (!res.ok) continue;
    const sessions = await res.json();
    const arr = Array.isArray(sessions) ? sessions : sessions.sessions || [];
    if (arr.length > 0) return arr[0].id;
  }
  return null;
}

/** 读基线文件;不存在或解析失败返回 null(不判失败,仅提示)。 */
async function loadBaseline() {
  try {
    const fs = await import('node:fs/promises');
    const txt = await fs.readFile(BASELINE_PATH, 'utf8');
    const json = JSON.parse(txt);
    return json.endpoints || json;
  } catch {
    return null;
  }
}

async function main() {
  console.log(`[perf] base=${BASE} 并发=${CONCURRENCY} 请求数/端点=${REQUESTS}`);
  console.log(`[perf] p95 阈值:基线×${MAX_P95_FACTOR}(绝对地板 ${ABSOLUTE_FLOOR_MS}ms)`);
  const token = await getToken();

  const targets = [{ name: 'characters_list', path: '/api/characters' }];
  const sid = await findSessionId(token);
  if (sid) {
    targets.push({ name: 'chat_history', path: `/api/chat/history?session_id=${encodeURIComponent(sid)}` });
  } else {
    console.warn('[perf] 未找到任何会话,跳过 chat_history 压测(可先在 UI 建会话再跑)');
  }

  const baseline = await loadBaseline();
  if (baseline) {
    console.log(`[perf] 基线文件: ${BASELINE_PATH}`);
  } else {
    console.log(`[perf] 未找到基线文件(${BASELINE_PATH}),仅输出测量值不判阈值`);
  }

  const report = { base: BASE, concurrency: CONCURRENCY, requestsPerEndpoint: REQUESTS, at: new Date().toISOString(), endpoints: {} };
  const failures = [];
  for (const t of targets) {
    // 预热 5 请求,消除首连/语句编译抖动
    for (let i = 0; i < 5; i++) {
      await (await fetch(`${BASE}${t.path}`, { headers: authHeaders(token) })).arrayBuffer();
    }
    const { latencies, wallMs } = await hammer(t.path, token);
    const s = stats(latencies);
    const rps = +(latencies.length / (wallMs / 1000)).toFixed(1);
    report.endpoints[t.name] = { ...s, rps };
    console.log(`[perf] ${t.name}: p50=${s.p50}ms p95=${s.p95}ms avg=${s.avg}ms rps=${rps} errors=${s.errors}`);

    // 阈值判定:基线缺失/未启用/低于地板 → 跳过
    const baseP95 = baseline?.[t.name]?.p95;
    if (typeof baseP95 !== 'number') {
      console.log(`[perf]   └ 无基线值,跳过阈值判定`);
      continue;
    }
    if (s.p95 < ABSOLUTE_FLOOR_MS || baseP95 < ABSOLUTE_FLOOR_MS) {
      console.log(`[perf]   └ p95 低于地板(${ABSOLUTE_FLOOR_MS}ms),跳过阈值判定`);
      continue;
    }
    const limit = +(baseP95 * MAX_P95_FACTOR).toFixed(1);
    if (s.errors > 0) {
      failures.push(`${t.name}: errors=${s.errors} > 0`);
      console.log(`[perf]   └ ✗ 存在请求错误(errors=${s.errors})`);
    }
    if (s.p95 > limit) {
      failures.push(`${t.name}: p95=${s.p95}ms > 阈值 ${limit}ms(基线 ${baseP95}ms × ${MAX_P95_FACTOR})`);
      console.log(`[perf]   └ ✗ p95 超阈值:${s.p95}ms > ${limit}ms(基线 ${baseP95}ms)`);
    } else {
      console.log(`[perf]   └ ✓ p95 达标:${s.p95}ms <= ${limit}ms(基线 ${baseP95}ms)`);
    }
  }

  report.thresholds = { maxP95Factor: MAX_P95_FACTOR, absoluteFloorMs: ABSOLUTE_FLOOR_MS, baselineLoaded: !!baseline };
  report.failures = failures;
  console.log('[perf] JSON: ' + JSON.stringify(report));

  // JSON 已输出,再决定退出码(失败时仍可落档)
  if (failures.length > 0) {
    console.error(`[perf] ✗ 性能门禁未通过(${failures.length} 项):`);
    for (const f of failures) console.error(`[perf]   - ${f}`);
    process.exit(1);
  }
  console.log('[perf] ✓ 性能门禁通过');
}

main().catch((e) => {
  console.error(`[perf] 失败: ${e.message}`);
  process.exit(1);
});
