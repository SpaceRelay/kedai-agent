// 任务模式端到端实测探针:创建 → 运行 → 订阅 SSE 事件流 → 采集事件链、耗时与 token 增长。
//
// 用途:验证六模式的执行编排、事件链完整性,以及**上下文是否收敛**
// (2026-09-14 的工具参数裁剪盲区就是靠它发现的:plan 模式 prompt 曾 90 倍爆炸)。
// 跑完会打印各阶段 prompt_tokens 与累计值,异常增长一眼可见。
//
// 用法:
//   node tools/bench/task-probe.mjs <task_mode> [maxWaitMs]
//     task_mode: legacy | solo | multi | plan | team | custom
//   环境变量(同 e2e-probe):KEDAI_BASE / KEDAI_PROBE_TOKEN / KEDAI_PROBE_CHAR
//
// 前置:服务需已运行,且其 data_dir 应为**隔离目录**(探针会创建任务并落库)。
const BASE = (process.env.KEDAI_BASE || 'http://127.0.0.1:3001').replace(/\/$/, '');
let TOKEN = process.env.KEDAI_PROBE_TOKEN || '';
let CHAR_ID = process.env.KEDAI_PROBE_CHAR || '';
const H = () => ({ Authorization: `Bearer ${TOKEN}`, 'Content-Type': 'application/json' });

const mode = process.argv[2] || 'solo';
/** 等待终态的上限(毫秒);默认 7 分钟,便于观察是否失控 */
const MAX_WAIT_MS = Number(process.argv[3] || 7 * 60 * 1000);

async function ensureToken() {
  if (TOKEN) return;
  const res = await fetch(`${BASE}/api/bootstrap`);
  if (!res.ok) throw new Error(`bootstrap 失败: HTTP ${res.status}`);
  TOKEN = (await res.json()).token;
}

async function ensureCharacter() {
  if (CHAR_ID) return;
  const list = await (await fetch(`${BASE}/api/characters`, { headers: H() })).json();
  const arr = Array.isArray(list) ? list : list.characters || [];
  if (arr.length === 0) throw new Error('没有任何角色,请先在 UI 导入角色卡');
  const withBook = arr.find((c) => c.character_book || c.has_character_book);
  CHAR_ID = (withBook || arr[0]).id;
}

async function api(p, init) {
  const r = await fetch(BASE + p, { ...init, headers: { ...H(), ...(init?.headers || {}) } });
  if (!r.ok) throw new Error(`${p} -> ${r.status} ${(await r.text()).slice(0, 300)}`);
  return r;
}

await ensureToken();
await ensureCharacter();

const t0 = Date.now();
const t = () => ((Date.now() - t0) / 1000).toFixed(2);

// 先订阅事件流(在运行之前建连,避免丢事件)
const ac = new AbortController();
const counts = {};
const timeline = [];
let taskId = null;
let done = false;

(async () => {
  try {
    const r = await fetch(BASE + '/api/tasks/events', {
      headers: { ...H(), Accept: 'text/event-stream' },
      signal: ac.signal,
    });
    const reader = r.body.getReader();
    const dec = new TextDecoder();
    let buf = '';
    for (;;) {
      const { done: d, value } = await reader.read();
      if (d) break;
      buf += dec.decode(value, { stream: true });
      let i;
      while ((i = buf.indexOf('\n\n')) >= 0) {
        const frame = buf.slice(0, i);
        buf = buf.slice(i + 2);
        for (const line of frame.split('\n')) {
          if (!line.startsWith('data:')) continue;
          const p = line.slice(5).trim();
          if (!p) continue;
          let ev;
          try { ev = JSON.parse(p); } catch { continue; }
          const kind = ev.kind || 'unknown';
          if (taskId && ev.task_id && ev.task_id !== taskId) continue;
          counts[kind] = (counts[kind] || 0) + 1;
          if (kind !== 'delta') {
            const rec = { at: Date.now() - t0, kind, s: JSON.stringify(ev).slice(0, 190) };
            timeline.push(rec);
            console.log(`[${t()}] ${kind}: ${rec.s}`);
          }
          if (ev.status && ['done', 'error', 'partial', 'ended'].includes(ev.status)) done = true;
        }
      }
    }
  } catch (e) {
    if (e.name !== 'AbortError') console.log('事件流结束:', e.message);
  }
})();

await new Promise((r) => setTimeout(r, 800)); // 等事件流建连

// 1) 创建任务
{
  const r = await api('/api/tasks', {
    method: 'POST',
    body: JSON.stringify({
      title: `bench-${mode}-${Date.now()}`,
      character_id: CHAR_ID,
      task_mode: mode,
    }),
  });
  const j = await r.json();
  taskId = j.id || j.task_id || j.task?.id;
  if (!taskId) {
    // 事件里已捕获 task_id 时回退使用;否则从列表取最新
    const list = await (await api('/api/tasks')).json();
    const arr = Array.isArray(list) ? list : list.tasks || [];
    taskId = arr[0]?.id;
  }
  console.log(`[${t()}] 任务已建: ${taskId} mode=${mode}`);
}

// 2) 运行
{
  const r = await api(`/api/tasks/${taskId}/run`, {
    method: 'POST',
    body: JSON.stringify({ input: '用三句话说明「雨」对夜间便利店氛围的作用。不要调用任何工具,直接回答。' }),
  });
  console.log(`[${t()}] run 已提交 (HTTP ${r.status})`);
}

// 3) 轮询到终态(同时观察事件是否已把状态推进)
const deadline = Date.now() + MAX_WAIT_MS;
let lastStatus = null;
let approved = false;
while (Date.now() < deadline) {
  await new Promise((r) => setTimeout(r, 2000));
  try {
    const j = await (await api(`/api/tasks/${taskId}`)).json();
    const st = j.task?.status;
    if (st !== lastStatus) {
      console.log(`[${t()}] (轮询) 状态 -> ${st}`);
      lastStatus = st;
    }
    // plan 模式:计划生成后暂停待批准,自动批准以观察执行阶段
    if (st === 'planned' && !approved) {
      approved = true;
      console.log(`[${t()}] 检测到 planned,自动批准`);
      const r = await api(`/api/tasks/${taskId}/approve`, {
        method: 'POST',
        body: JSON.stringify({}),
      });
      console.log(`[${t()}] approve -> HTTP ${r.status}`);
      continue;
    }
    if (['done', 'error', 'partial', 'ended'].includes(st)) break;
  } catch { /* 忽略瞬时错误 */ }
}

const total = Date.now() - t0;
console.log('\n===== 汇总 =====');
console.log(`模式: ${mode}`);
console.log(`总耗时: ${total}ms`);
console.log(`事件计数:`, JSON.stringify(counts));
console.log('时间线(非 delta):');
timeline.forEach((e) => console.log(`  +${e.at}ms ${e.kind}: ${e.s}`));

// 4) 落库校验
try {
  const d = await (await api(`/api/tasks/${taskId}`)).json();
  console.log('\n--- 任务详情 ---');
  console.log('状态:', d.task?.status, '| 模式:', d.task?.task_mode);
  console.log('计划步数:', (d.task?.plan || []).length);
  console.log('子任务数:', (d.subtasks || []).length);
  console.log('usage:', JSON.stringify(d.usage_total));
  (d.task?.plan || []).forEach((s, i) =>
    console.log(`  步骤${i + 1} [${s.status}] ${s.name} | result_len=${(s.result || '').length}`));
  const calls = await (await api(`/api/tasks/${taskId}/calls`)).json();
  const arr = Array.isArray(calls) ? calls : calls.calls || [];
  console.log(`\nLLM 调用记录: ${arr.length} 条`);
  arr.forEach((c) =>
    console.log(`  [${c.phase}${c.step_index != null ? '#' + c.step_index : ''}] ${c.status} ${c.prompt_tokens}+${c.completion_tokens} finish=${c.finish_reason || '-'} ${c.elapsed_ms}ms`));

  // 5) 上下文收敛性检查(2026-09-14 新增:工具参数裁剪盲区曾在此暴露)
  //    健康特征:prompt_tokens 随步骤平稳或缓增;
  //    失控特征:后一步是前一步的数倍 → 大概率是上下文裁剪失效。
  const metas = arr.filter((c) => c.prompt_tokens > 0);
  if (metas.length >= 2) {
    const first = metas[0].prompt_tokens;
    const max = Math.max(...metas.map((c) => c.prompt_tokens));
    const growth = max / first;
    console.log(`\n--- 上下文收敛性 ---`);
    console.log(`首个调用 prompt=${first} | 峰值=${max} | 增长=${growth.toFixed(1)}×`);
    if (growth > 10) {
      console.log('⚠ 增长超过 10×:疑似上下文裁剪失效(检查 tool_history_budget_tokens 与裁剪逻辑)');
    } else {
      console.log('✓ 增长在合理范围');
    }
  }
} catch (e) {
  console.log('详情拉取失败:', e.message);
}

ac.abort();
console.log(`\nTASK_ID=${taskId}`);
process.exit(0);
