import { describe, expect, it } from 'vitest';
import {
  baseName,
  filterRepoByKind,
  filterRepoItems,
  formatBytes,
  repoKindLabel,
  repoKindOptions,
} from './repoIndex';
import type { RepoIndexItem } from '../api';

// 仓库索引面板纯函数测试:本地筛选(路径/摘要/符号名)、kind 分组与过滤、
// 字节格式化、路径末段。RepoIndexModal 的渲染断言在 RepoIndexModal.test.ts。

function item(overrides: Partial<RepoIndexItem> = {}): RepoIndexItem {
  return {
    path: 'web/src/api/memory.ts',
    kind: 'ts',
    module: 'web/src/api',
    lines: 100,
    bytes: 2048,
    importance: 5,
    summary: '记忆 API',
    deepSummary: '',
    usageCount: 0,
    category: '',
    note: '',
    symbols: [{ name: 'listMemories', kind: 'function', line: 10 }],
    ...overrides,
  };
}

describe('filterRepoItems(本地搜索)', () => {
  const items = [
    item(),
    item({ path: 'server-rs/src/api/memory.rs', kind: 'rust', summary: '记忆库路由', symbols: [{ name: 'distill', kind: 'function', line: 1 }] }),
    item({ path: 'web/src/App.vue', kind: 'vue', summary: '根组件', deepSummary: '注册全部懒加载弹窗', symbols: [] }),
  ];

  it('空查询返回原列表(不做无谓拷贝语义)', () => {
    expect(filterRepoItems(items, '')).toBe(items);
    expect(filterRepoItems(items, '   ')).toBe(items);
  });

  it('按路径子串匹配(大小写不敏感)', () => {
    expect(filterRepoItems(items, 'app.vue').map((i) => i.path)).toEqual(['web/src/App.vue']);
  });

  it('按摘要与深摘要匹配', () => {
    expect(filterRepoItems(items, '记忆库').map((i) => i.path)).toEqual([
      'server-rs/src/api/memory.rs',
    ]);
    expect(filterRepoItems(items, '懒加载').map((i) => i.path)).toEqual(['web/src/App.vue']);
  });

  it('按符号名匹配', () => {
    expect(filterRepoItems(items, 'distill').map((i) => i.path)).toEqual([
      'server-rs/src/api/memory.rs',
    ]);
    expect(filterRepoItems(items, 'LISTMEMORIES').map((i) => i.path)).toEqual([
      'web/src/api/memory.ts',
    ]);
  });

  it('无匹配返回空数组', () => {
    expect(filterRepoItems(items, '不存在的词')).toEqual([]);
  });
});

describe('repoKindOptions / filterRepoByKind', () => {
  const items = [
    item({ kind: 'ts' }),
    item({ kind: 'ts' }),
    item({ kind: 'rust' }),
    item({ kind: 'vue' }),
    item({ kind: 'vue' }),
  ];

  it('kind 选项按出现次数降序,同次数按字母序', () => {
    expect(repoKindOptions(items)).toEqual(['ts', 'vue', 'rust']);
  });

  it('filterRepoByKind:all 原样;指定 kind 精确过滤', () => {
    expect(filterRepoByKind(items, 'all')).toHaveLength(5);
    expect(filterRepoByKind(items, 'vue')).toHaveLength(2);
    expect(filterRepoByKind(items, 'md')).toEqual([]);
  });

  it('repoKindLabel 原样返回(便于看到新类型)', () => {
    expect(repoKindLabel('rust')).toBe('rust');
    expect(repoKindLabel('新类型')).toBe('新类型');
  });
});

describe('formatBytes / baseName', () => {
  it('字节 / KB / MB 三档,KB 起保留一位小数', () => {
    expect(formatBytes(512)).toBe('512B');
    expect(formatBytes(2048)).toBe('2.0KB');
    expect(formatBytes(1536)).toBe('1.5KB');
    expect(formatBytes(3 * 1024 * 1024)).toBe('3.0MB');
  });

  it('baseName 取路径末段;无斜杠原样返回', () => {
    expect(baseName('web/src/App.vue')).toBe('App.vue');
    expect(baseName('App.vue')).toBe('App.vue');
  });
});
