// mvu 输出协议跨端一致性测试(批次 F)。
//
// 与 Rust 侧 `server-rs/src/parsing/assistant/patch.rs` 的
// `mvu_protocol_fixture_is_consistent` 读**同一份** fixture:
//   docs/fixtures/mvu_patch_cases.json
// 两端各自把解析结果规范化后与 fixture 的 expectations 比较。任一端的协议实现发生漂移,
// 本测试或 Rust 侧测试即变红——这取代了此前仅靠注释声明「逐字对齐前端」的弱保证。
//
// 注意:fixture 只覆盖 `_.set`(MagVarUpdate)协议。JSON Patch 的路径格式两端确有差异
// (Rust 保留 /a/b;TS 经 pathToDots 转 a.b),其是否需统一属待评审项,故意不纳入本对拍
// (见 fixture 内 `_comment` 与 docs/遗留.md),避免用品固化未定的设计。
import { describe, it, expect } from 'vitest';
import { parseUpdateVariable } from './parser';

// 本仓库未安装 @types/node(前端源码不需要),测试里用 node: 内置模块须逐行压制类型错误
// ——与 characterScriptSandbox.test.ts / sandbox/*.test.ts 处理 node:vm 的既有约定一致。
// @ts-expect-error -- node:fs 缺少类型声明
import { readFileSync } from 'node:fs';
// @ts-expect-error -- node:url 缺少类型声明
import { fileURLToPath } from 'node:url';
// @ts-expect-error -- node:path 缺少类型声明
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE = join(HERE, '..', '..', '..', 'docs', 'fixtures', 'mvu_patch_cases.json');

interface FixtureOp {
  op: 'set' | 'delta' | 'remove' | 'move';
  path: string;
  from?: string;
  new?: unknown;
  reason: string | null;
}
interface FixtureCase {
  name: string;
  input: string;
  ops: FixtureOp[];
}
interface Fixture {
  cases: FixtureCase[];
}

/** 把 TS 的 UpdateCommand 规范化成 fixture 形态(空串/undefined 统一为 null)。 */
function normalize(cmd: {
  op?: string;
  path: string;
  from?: string;
  newValue: unknown;
  reason?: string;
}): FixtureOp {
  const out: FixtureOp = {
    op: (cmd.op ?? 'set') as FixtureOp['op'],
    path: cmd.path,
    new: cmd.newValue === undefined ? null : cmd.newValue,
    reason: cmd.reason ? cmd.reason : null,
  };
  if (cmd.from) out.from = cmd.from;
  return out;
}

const fixture: Fixture = JSON.parse(readFileSync(FIXTURE, 'utf8'));

describe('mvu 协议 fixture 跨端一致性(TS 侧)', () => {
  it('fixture 至少 15 个用例', () => {
    expect(fixture.cases.length).toBeGreaterThanOrEqual(15);
  });

  for (const c of fixture.cases) {
    it(c.name, () => {
      const { commands } = parseUpdateVariable(c.input);
      const actual = commands.map(normalize);
      expect(actual).toEqual(c.ops);
    });
  }

  it('剥离块后的文本不含 UpdateVariable 标签', () => {
    const { cleaned } = parseUpdateVariable(
      '前<UpdateVariable><%= _.set(\'a\', 1); %></UpdateVariable>后',
    );
    expect(cleaned).not.toContain('UpdateVariable');
    expect(cleaned).toContain('前');
    expect(cleaned).toContain('后');
  });
});
