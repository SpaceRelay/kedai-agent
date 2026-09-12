#!/usr/bin/env node
/**
 * Kedai 性能基线压测脚本(Node 零依赖,需 Node 18+ 自带 fetch)。
 *
 * 用法:
 *   node tools/perf-baseline.mjs [--base http://127.0.0.1:3001] [-c 20] [-n 100]
 *
 * 前提:Kedai 服务已在 --base 地址运行(脚本自动从 /api/bootstrap 取 token)。
 * 输出:各端点 p50 / p95 / 平均延迟与吞吐量,附 JSON 行便于落档对比。
 */

const args = process.argv.slice(2);
function argOf(flag, fallback) {
  const i = args.indexOf(flag);
  return i >= 0 && args[i + 1] ? args[i + 1] : fallback;
}
const BASE = argOf('--base', 'http://127.0.0.1:3001').replace(/\/$/, '');
const CONCURRENCY = Number(argOf('-c', '20'));
const REQUESTS = Number(argOf('-n', '100'));

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

async function main() {
  console.log(`[perf] base=${BASE} 并发=${CONCURRENCY} 请求数/端点=${REQUESTS}`);
  const token = await getToken();

  const targets = [{ name: 'characters_list', path: '/api/characters' }];
  const sid = await findSessionId(token);
  if (sid) {
    targets.push({ name: 'chat_history', path: `/api/chat/history?session_id=${encodeURIComponent(sid)}` });
  } else {
    console.warn('[perf] 未找到任何会话,跳过 chat_history 压测(可先在 UI 建会话再跑)');
  }

  const report = { base: BASE, concurrency: CONCURRENCY, requestsPerEndpoint: REQUESTS, at: new Date().toISOString(), endpoints: {} };
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
  }
  console.log('[perf] JSON: ' + JSON.stringify(report));
}

main().catch((e) => {
  console.error(`[perf] 失败: ${e.message}`);
  process.exit(1);
});
