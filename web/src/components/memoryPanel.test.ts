import { describe, expect, it } from 'vitest';
import {
  distillErrorText,
  distillSuccessText,
  filterRows,
  formatMemoryTime,
  highlightSegments,
  kindClass,
  kindLabel,
  memoryRows,
  nextPendingDelete,
  pruneSuccessText,
  type MemoryRow,
} from './memoryPanel';
import type { MemoryEntry } from '../api';

// 记忆库面板纯函数测试:kind 标签映射 / 时间格式化 / 列表行映射排序 /
// 本地 kind+query 筛选 / 高亮分段 / 蒸馏与清理反馈文案 / 删除两段式确认状态机。

function entry(overrides: Partial<MemoryEntry> = {}): MemoryEntry {
  return {
    id: 1,
    character_id: 'charA',
    source_session_id: null,
    kind: 'manual',
    content: '记忆',
    usage_count: 0,
    last_usage: null,
    selected: true,
    pinned: false,
    created_at: '2026-08-14T08:00:00Z',
    updated_at: '2026-08-14T08:00:00Z',
    ...overrides,
  };
}

/** 由条目构造展示行(走真实 memoryRows,保证测试对象与组件同源) */
function row(overrides: Partial<MemoryEntry> = {}): MemoryRow {
  return memoryRows([entry(overrides)])[0];
}

describe('kindLabel / kindClass(kind 标签映射)', () => {
  it('三种已知 kind 映射为中文标签与配色 class', () => {
    expect(kindLabel('distilled')).toBe('蒸馏');
    expect(kindLabel('tool')).toBe('工具');
    expect(kindLabel('manual')).toBe('手动');
    expect(kindClass('distilled')).toBe('kind-distilled');
    expect(kindClass('tool')).toBe('kind-tool');
    expect(kindClass('manual')).toBe('kind-manual');
  });

  it('未知 kind 不吞异常数据:标签原样、配色中性灰', () => {
    expect(kindLabel('bogus')).toBe('bogus');
    expect(kindClass('bogus')).toBe('kind-unknown');
  });
});

describe('formatMemoryTime(last_usage 格式化)', () => {
  it('ISO 时间统一截断到分(T → 空格)', () => {
    expect(formatMemoryTime('2026-08-15T10:30:00Z')).toBe('2026-08-15 10:30');
  });

  it('null(从未注入)显示「未使用」而非空串', () => {
    expect(formatMemoryTime(null)).toBe('未使用');
  });
});

describe('memoryRows(列表行映射与排序)', () => {
  it('映射展示行并按 id 降序(最新在前,与后端 ORDER BY id DESC 一致,幂等)', () => {
    const rows = memoryRows([
      entry({ id: 2, kind: 'distilled', content: '旧', usage_count: 0, last_usage: null }),
      entry({ id: 5, kind: 'tool', content: '新', usage_count: 4, last_usage: '2026-08-15T09:00:00Z', selected: false }),
    ]);
    expect(rows.map((r) => r.id)).toEqual([5, 2]);
    expect(rows[0]).toMatchObject({
      content: '新',
      kindLabel: '工具',
      kindClass: 'kind-tool',
      usage: 4,
      lastUsage: '2026-08-15 09:00',
      selected: false,
      pinned: false,
    });
    expect(rows[1].lastUsage).toBe('未使用');
    // 幂等:同输入两次调用顺序一致
    expect(memoryRows([
      entry({ id: 2 }),
      entry({ id: 5 }),
    ]).map((r) => r.id)).toEqual([5, 2]);
  });

  it('置顶行排在非置顶之前(与后端 pinned DESC 注入排序口径一致),组内仍按 id 降序', () => {
    const rows = memoryRows([
      entry({ id: 9, pinned: false }),
      entry({ id: 3, pinned: true }),
      entry({ id: 7, pinned: true }),
      entry({ id: 11, pinned: false }),
    ]);
    expect(rows.map((r) => r.id)).toEqual([7, 3, 11, 9]);
    expect(rows[0].pinned).toBe(true);
    expect(rows[2].pinned).toBe(false);
  });

  it('pinned 字段原样映射到展示行(默认 false 不吞数据)', () => {
    expect(memoryRows([entry({ pinned: true })])[0].pinned).toBe(true);
    expect(memoryRows([entry()])[0].pinned).toBe(false);
  });

  it('空列表返回空数组(空态判定)', () => {
    expect(memoryRows([])).toEqual([]);
  });
});

describe('filterRows(本地 kind + query 筛选)', () => {
  const rows: MemoryRow[] = [
    row({ id: 1, kind: 'distilled', content: '图书馆初识' }),
    row({ id: 2, kind: 'tool', content: '用户喜欢薄荷茶' }),
    row({ id: 3, kind: 'manual', content: '用户讨厌噪音' }),
  ];

  it("kindFilter='all' 且 query 为空 → 原样返回全部", () => {
    expect(filterRows(rows, 'all', '')).toHaveLength(3);
    expect(filterRows(rows, 'all', '   ')).toHaveLength(3);
  });

  it('kindFilter 按 kind 精确过滤', () => {
    expect(filterRows(rows, 'tool', '').map((r) => r.id)).toEqual([2]);
    expect(filterRows(rows, 'manual', '').map((r) => r.id)).toEqual([3]);
    expect(filterRows(rows, 'distilled', '').map((r) => r.id)).toEqual([1]);
  });

  it('query 按内容子串匹配且大小写不敏感,与 kind 筛选叠加(AND)', () => {
    expect(filterRows(rows, 'all', '用户').map((r) => r.id)).toEqual([2, 3]);
    expect(filterRows(rows, 'manual', '用户').map((r) => r.id)).toEqual([3]);
    expect(filterRows(rows, 'tool', '图书馆')).toEqual([]);
    expect(filterRows([row({ content: 'Hello World' })], 'all', 'hello')).toHaveLength(1);
  });

  it('query 前后空白被 trim(避免用户误输空格筛空)', () => {
    expect(filterRows(rows, 'all', '  薄荷  ').map((r) => r.id)).toEqual([2]);
  });
});

describe('highlightSegments(高亮分段)', () => {
  it('无 query(或纯空白)→ 单段 hit=false', () => {
    expect(highlightSegments('图书馆初识', '')).toEqual([{ text: '图书馆初识', hit: false }]);
    expect(highlightSegments('图书馆初识', '  ')).toEqual([{ text: '图书馆初识', hit: false }]);
  });

  it('命中一次 → 前/命中/后三段,命中段保留原文大小写', () => {
    expect(highlightSegments('用户喜欢薄荷茶', '薄荷')).toEqual([
      { text: '用户喜欢', hit: false },
      { text: '薄荷', hit: true },
      { text: '茶', hit: false },
    ]);
  });

  it('多次命中 → 逐段切分;大小写不敏感但输出保留原文', () => {
    expect(highlightSegments('abABab', 'ab')).toEqual([
      { text: 'ab', hit: true },
      { text: 'AB', hit: true },
      { text: 'ab', hit: true },
    ]);
  });

  it('命中在首尾 → 不产生空段;无命中 → 单段 hit=false', () => {
    expect(highlightSegments('薄荷茶', '薄荷')).toEqual([
      { text: '薄荷', hit: true },
      { text: '茶', hit: false },
    ]);
    expect(highlightSegments('薄荷茶', '噪音')).toEqual([{ text: '薄荷茶', hit: false }]);
  });
});

describe('蒸馏反馈文案', () => {
  it('成功:inserted>0 → 新增 N 条;0(空历史)→ 无可提取提示', () => {
    expect(distillSuccessText(3)).toBe('蒸馏完成:新增 3 条记忆');
    expect(distillSuccessText(0)).toBe('蒸馏完成:当前会话没有可提取的记忆');
  });

  it('失败:未开启(400)→ 追加去设置开启的指引;其余错误原样带前缀', () => {
    expect(distillErrorText('跨会话记忆蒸馏未开启,请先在设置中打开 memory_distill_enabled')).toBe(
      '蒸馏失败:跨会话记忆蒸馏未开启,请先在设置中打开 memory_distill_enabled。可到 设置 → 生成参数 打开「记忆蒸馏」后重试',
    );
    expect(distillErrorText('会话不存在: s9')).toBe('蒸馏失败:会话不存在: s9');
  });
});

describe('pruneSuccessText(清理归档反馈文案)', () => {
  it('deleted>0 → 已清理 N 条;0 → 没有可清理条目', () => {
    expect(pruneSuccessText(4)).toBe('已清理 4 条归档记忆');
    expect(pruneSuccessText(0)).toBe('没有可清理的归档记忆');
  });
});

describe('nextPendingDelete(删除两段式确认状态机)', () => {
  it('request 进入确认态;confirm / cancel 退出', () => {
    expect(nextPendingDelete(null, 'request', 7)).toBe(7);
    expect(nextPendingDelete(7, 'request', 7)).toBe(7);
    expect(nextPendingDelete(7, 'cancel', 7)).toBeNull();
    expect(nextPendingDelete(7, 'confirm', 7)).toBeNull();
  });

  it('切换目标行时确认态跟随新行(只允许一行处于确认)', () => {
    expect(nextPendingDelete(7, 'request', 9)).toBe(9);
  });
});
