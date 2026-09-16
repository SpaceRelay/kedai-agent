// 角色扮演端到端实测探针:消费 SSE 流,记录事件时间线与各阶段耗时。
//
// 用途:验证「角色扮演模式」的真实往返构成(TTFT / 截断自愈 / 反思重生成 / token 用量)。
// 当出现「回复很慢」类问题时,先用它定位耗时落在服务端预处理还是上游往返。
//
// 用法:
//   node tools/bench/e2e-probe.mjs <mode> [maxTokens]
//     mode: fast | deep | agent | custom
//   环境变量:
//     KEDAI_BASE       服务地址(默认 http://127.0.0.1:3001)
//     KEDAI_PROBE_TOKEN  Bearer token;未给则自动从 /api/bootstrap 领取
//     KEDAI_PROBE_CHAR   角色 id;未给则自动取第一个角色(优先带内嵌世界书的卡)
//
// 前置:服务需已运行,且其 data_dir 应为**隔离目录**(探针会建会话并写入消息,
// 不应指向真实用户数据)。
const BASE = (process.env.KEDAI_BASE || 'http://127.0.0.1:3001').replace(/\/$/, '');
let TOKEN = process.env.KEDAI_PROBE_TOKEN || '';
let CHAR_ID = process.env.KEDAI_PROBE_CHAR || '';

const mode = process.argv[2] || 'fast';
const maxTokens = Number(process.argv[3] || 300);

const H = () => ({ Authorization: `Bearer ${TOKEN}`, 'Content-Type': 'application/json' });

/** 未显式给 token 时从 bootstrap 领取(仅 loopback 可用) */
async function ensureToken() {
  if (TOKEN) return;
  const res = await fetch(`${BASE}/api/bootstrap`);
  if (!res.ok) throw new Error(`bootstrap 失败: HTTP ${res.status}`);
  TOKEN = (await res.json()).token;
}

/** 未显式给角色时自动挑选:优先带内嵌世界书(character_book)的卡,否则第一个 */
async function ensureCharacter() {
  if (CHAR_ID) return;
  const list = await (await fetch(`${BASE}/api/characters`, { headers: H() })).json();
  const arr = Array.isArray(list) ? list : list.characters || [];
  if (arr.length === 0) throw new Error('没有任何角色,请先在 UI 导入角色卡');
  const withBook = arr.find((c) => c.character_book || c.has_character_book);
  CHAR_ID = (withBook || arr[0]).id;
}

async function api(path, init) {
  const r = await fetch(BASE + path, { ...init, headers: { ...H(), ...(init?.headers || {}) } });
  if (!r.ok) throw new Error(`${path} -> ${r.status} ${await r.text()}`);
  return r;
}

await ensureToken();
await ensureCharacter();

const t0 = Date.now();
const t = () => ((Date.now() - t0) / 1000).toFixed(3);

// 1) 建会话(贴角色卡开场白)
let sessionId;
{
  const r = await api('/api/chat/sessions', {
    method: 'POST',
    body: JSON.stringify({ character_id: CHAR_ID, title: `bench-${mode}-${Date.now()}` }),
  });
  const j = await r.json();
  sessionId = j.id || j.session_id;
  console.log(`[${t()}] 会话已建: ${sessionId}`);
}

// 2) 发送并流式消费
const body = {
  session_id: sessionId,
  message: '请用一句话描述你此刻看到的窗外景象。',
  agent_mode: mode,
  max_tokens: maxTokens,
};

console.log(`[${t()}] 发送请求 mode=${mode} max_tokens=${maxTokens}`);
const res = await api('/api/chat/send', { method: 'POST', body: JSON.stringify(body) });

const reader = res.body.getReader();
const dec = new TextDecoder();
let buf = '';
const counts = {};
const events = [];
let firstTokenAt = null;
let finishAt = null;
let textLen = 0;

for (;;) {
  const { done, value } = await reader.read();
  if (done) break;
  buf += dec.decode(value, { stream: true });
  let idx;
  while ((idx = buf.indexOf('\n\n')) >= 0) {
    const frame = buf.slice(0, idx);
    buf = buf.slice(idx + 2);
    for (const line of frame.split('\n')) {
      if (!line.startsWith('data:')) continue;
      const payload = line.slice(5).trim();
      if (!payload || payload === '[DONE]') continue;
      let ev;
      try { ev = JSON.parse(payload); } catch { continue; }
      const kind = ev.type || ev.kind || 'unknown';
      counts[kind] = (counts[kind] || 0) + 1;
      if (kind === 'token') {
        if (firstTokenAt === null) { firstTokenAt = Date.now() - t0; console.log(`[${t()}] ★ 首 token (TTFT=${firstTokenAt}ms)`); }
        textLen += (ev.text || '').length;
      } else if (kind === 'finish') {
        finishAt = Date.now() - t0;
        console.log(`[${t()}] ★ finish 事件`);
        if (ev.usage) console.log(`       usage:`, JSON.stringify(ev.usage));
      } else {
        events.push({ at: Date.now() - t0, kind, detail: JSON.stringify(ev).slice(0, 160) });
        console.log(`[${t()}] ${kind}: ${JSON.stringify(ev).slice(0, 140)}`);
      }
    }
  }
}

const total = Date.now() - t0;
console.log('\n===== 汇总 =====');
console.log(`模式: ${mode}`);
console.log(`总耗时: ${total}ms`);
console.log(`TTFT: ${firstTokenAt ?? '(无 token)'}ms`);
console.log(`finish: ${finishAt ?? '(无)'}ms`);
console.log(`正文长度: ${textLen} 字符`);
console.log(`事件计数:`, JSON.stringify(counts));
console.log(`非 token 事件时间线:`);
events.forEach(e => console.log(`  +${e.at}ms ${e.kind}: ${e.detail}`));

// 3) 落库校验
{
  const r = await api(`/api/chat/history?session_id=${sessionId}`);
  const j = await r.json();
  const msgs = Array.isArray(j) ? j : (j.messages || []);
  console.log(`\n历史消息数: ${msgs.length}`);
  msgs.forEach(m => console.log(`  [${m.role}] len=${(m.content || '').length} extra=${String(m.extra ?? '').slice(0, 60)}`));
}
console.log(`\nSESSION_ID=${sessionId}`);
