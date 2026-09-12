// 角色卡检查工具:解析 PNG/JPEG/WebP/JSON 角色卡(chara_card_v2/v3),
// 打印关键字段结构——名称、开场白规模、正则脚本位置与键命名风格(camelCase/snake_case)。
// 用法: node tools/inspect-card.mjs <卡片文件路径> [--full]
import { readFileSync } from 'node:fs';

const file = process.argv[2];
const full = process.argv.includes('--full');
if (!file) {
  console.error('用法: node tools/inspect-card.mjs <卡片文件> [--full]');
  process.exit(1);
}
const buf = readFileSync(file);

/** 从 PNG tEXt/iTXt chunk 提取 chara base64 */
function extractPng(b) {
  const out = [];
  let off = 8; // 跳过 PNG 签名
  while (off + 8 <= b.length) {
    const len = b.readUInt32BE(off);
    const type = b.toString('ascii', off + 4, off + 8);
    const dataStart = off + 8;
    if (type === 'tEXt' || type === 'iTXt') {
      const nul = b.indexOf(0, dataStart);
      const keyword = b.toString('latin1', dataStart, nul);
      if (keyword === 'chara' || keyword === 'ccv3') {
        out.push({ keyword, type, raw: b.subarray(nul + 1, dataStart + len) });
      }
    }
    off = dataStart + len + 4; // 数据 + CRC
    if (type === 'IEND') break;
  }
  return out;
}

let cardJson = null;
if (buf.length > 8 && buf.readUInt32BE(0) === 0x89504e47) {
  const chunks = extractPng(buf);
  for (const c of chunks) {
    const text = c.type === 'tEXt' ? c.raw.toString('latin1') : c.raw.toString('utf8');
    // tEXt 的 chara 是 base64
    const decoded = Buffer.from(text.replace(/^[\s\S]*?\0/, ''), 'base64');
    const str = (decoded.length ? decoded : Buffer.from(text, 'latin1')).toString('utf8');
    try {
      cardJson = JSON.parse(str);
      console.log(`[chunk] keyword=${c.keyword} type=${c.type} 解析成功`);
      break;
    } catch {
      // 尝试直接当 base64 解整段
      try {
        cardJson = JSON.parse(Buffer.from(text, 'base64').toString('utf8'));
        console.log(`[chunk] keyword=${c.keyword} type=${c.type} 解析成功(整段 base64)`);
        break;
      } catch { /* 继续下一块 */ }
    }
  }
} else {
  cardJson = JSON.parse(buf.toString('utf8'));
  console.log('[json] 纯 JSON 卡解析成功');
}
if (!cardJson) {
  console.error('未能从文件解析出角色卡 JSON');
  process.exit(1);
}

const isV3 = cardJson.spec === 'chara_card_v3';
const data = cardJson.data && typeof cardJson.data === 'object' ? cardJson.data : cardJson;
console.log('--- 基本信息 ---');
console.log('spec:', cardJson.spec ?? '(v2/无)', '| name:', data.name);
console.log('description 长度:', (data.description ?? '').length);
console.log('first_mes 长度:', (data.first_mes ?? '').length);
console.log('alternate_greetings 数量:', (data.alternate_greetings ?? []).length,
  '长度:', (data.alternate_greetings ?? []).map((g) => (g ?? '').length).join(','));
console.log('mes_example 长度:', (data.mes_example ?? '').length);
console.log('system_prompt 长度:', (data.system_prompt ?? '').length);
console.log('character_book 条目数:', data.character_book?.entries?.length ?? 0);
console.log('extensions 键:', Object.keys(data.extensions ?? {}).join(', ') || '(无)');

// 正则脚本三个候选位置
const spots = [
  ['extensions.regex_scripts', data.extensions?.regex_scripts],
  ['顶层 regex_scripts', data.regex_scripts],
  ['data.extensions.regex_scripts(未提升时)', cardJson.data !== cardJson ? cardJson.extensions?.regex_scripts : undefined],
];
console.log('--- 正则脚本 ---');
let scripts = null;
for (const [label, val] of spots) {
  if (Array.isArray(val) && val.length > 0) {
    console.log(`位置 ${label}: ${val.length} 条`);
    if (!scripts) scripts = val;
  }
}
if (!scripts) {
  console.log('未找到任何正则脚本!');
} else {
  scripts.forEach((s, i) => {
    const keys = Object.keys(s);
    const style = keys.some((k) => k.includes('_')) ? 'snake_case' : 'camelCase';
    const name = s.scriptName ?? s.script_name ?? s.name ?? `(未命名#${i})`;
    const findRe = s.findRegex ?? s.find_regex ?? '';
    const replaceStr = s.replaceString ?? s.replace_string ?? '';
    console.log(`  [${i}] ${name} | 键风格=${style} | disabled=${s.disabled ?? false} markdownOnly=${s.markdownOnly ?? s.markdown_only ?? '-'}` +
      ` | findRegex=${findRe.length}字符 | replaceString=${replaceStr.length}字符`);
    if (full) console.log(`      键: ${keys.join(', ')}`);
  });
  const totalReplace = scripts.reduce((n, s) => n + ((s.replaceString ?? s.replace_string ?? '').length), 0);
  console.log(`替换体总规模: ${totalReplace} 字符`);
}

console.log('--- first_mes 前 300 字符 ---');
console.log((data.first_mes ?? '').slice(0, 300).replace(/\n/g, '\\n'));
if (full) {
  console.log('--- 顶层键 ---', Object.keys(cardJson).join(', '));
  console.log('--- data 键 ---', Object.keys(data).join(', '));
}
