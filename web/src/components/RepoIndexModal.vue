<script setup lang="ts">
// 仓库索引面板:读取 .kedai-index 生成的代码索引(GET /api/repo-index),只读展示。
// 结构仿 DevToolsModal.vue(筛选 + 列表 + 展开详情):
//   - 顶部搜索框:按路径 / 摘要 / 深摘要 / 符号名本地过滤(纯函数在 repoIndex.ts);
//   - kind 筛选下拉:按文件类别过滤;
//   - 点行展开:显示完整摘要、深摘要、备注与符号列表(名称 / 类型 / 行号);
//   - available=false 时显示空态,提示先运行 node .kedai-index/build.mjs --full。
// onServerPrefetch:服务端渲染测试通道(renderToString 会等待预取完成)。
import { computed, onMounted, onServerPrefetch, ref } from 'vue';
import { useAppStore } from '../store';
import { fetchRepoIndex, type RepoIndexItem, type RepoIndexResult } from '../api';
import {
  baseName,
  filterRepoByKind,
  filterRepoItems,
  formatBytes,
  repoKindLabel,
  repoKindOptions,
} from './repoIndex';

const store = useAppStore();
const result = ref<RepoIndexResult | null>(null);
const loading = ref(false);
const error = ref('');
/** 搜索关键词(本地过滤,不发请求) */
const query = ref('');
/** kind 筛选('all' = 全部) */
const kindFilter = ref('all');
/** 展开详情的文件路径(空 = 全折叠) */
const expandedPath = ref<string | null>(null);

const items = computed<RepoIndexItem[]>(() => result.value?.items ?? []);
/** kind 下拉选项:随索引数据动态生成 */
const kindOptions = computed(() => repoKindOptions(items.value));
/** 搜索 + kind 双重过滤后的展示列表 */
const shown = computed(() =>
  filterRepoByKind(filterRepoItems(items.value, query.value), kindFilter.value),
);

async function load(): Promise<void> {
  loading.value = true;
  error.value = '';
  try {
    result.value = await fetchRepoIndex();
    expandedPath.value = null;
  } catch (e) {
    error.value = `加载失败:${(e as Error).message}`;
    result.value = null;
  } finally {
    loading.value = false;
  }
}

function toggleExpand(path: string): void {
  expandedPath.value = expandedPath.value === path ? null : path;
}

const close = (): void => {
  store.repoIndexOpen = false;
};

onMounted(load);
onServerPrefetch(load);
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal wb-modal">
      <!-- 头部 -->
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme blue" /> 仓库索引
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-modal-body">
        <!-- 说明 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme blue" /> 说明</div>
          <p class="sv-note" style="margin: 0; line-height: 1.8">
            由 <code>.kedai-index/build.mjs</code> 生成的代码索引(只读):
            展示每个文件的模块 / 摘要 / 重要性 / 符号清单,供快速定位实现。
            <template v-if="result?.available">
              当前索引
              <b>{{ result.fileCount ?? items.length }}</b> 个文件
              <template v-if="result.generatedAt">,生成于 {{ result.generatedAt.replace('T', ' ').slice(0, 19) }}</template>
              <template v-if="result.gitHead">,HEAD <code>{{ result.gitHead }}</code></template>
              。
            </template>
          </p>
        </div>

        <!-- 索引未生成:空态 -->
        <div v-if="result && !result.available" class="sv-empty" style="padding: 28px 12px">
          <div class="sv-empty-geo mb10">
            <span class="sq black" />
            <span class="sq pink" />
            <span class="sq deep" />
          <i class="diag" />
          </div>
          <p style="font-size: 12px">索引未生成</p>
          <p style="font-size: 11px">
            请先运行 <code>node .kedai-index/build.mjs --full</code>
          </p>
          <p v-if="result.reason" style="font-size: 11px">{{ result.reason }}</p>
        </div>

        <template v-else>
          <!-- 顶部控件:搜索 + kind 筛选 -->
          <div class="sv-field">
            <div class="sv-stack" style="flex-direction: row; gap: 8px; align-items: center; flex-wrap: wrap">
              <input
                v-model="query"
                class="sv-input repo-index-search"
                type="search"
                placeholder="搜索路径 / 摘要 / 符号名…"
              />
              <select v-model="kindFilter" class="sv-wb-select" style="width: 140px">
                <option value="all">全部类别</option>
                <option v-for="k in kindOptions" :key="k" :value="k">{{ repoKindLabel(k) }}</option>
              </select>
              <button class="sv-btn ghost sv-btn-sm" :disabled="loading" @click="load">
                {{ loading ? '加载中…' : '刷新' }}
              </button>
              <span class="sv-note" style="margin: 0">
                {{ shown.length }} / {{ items.length }} 个文件
              </span>
            </div>
            <div v-if="error" class="sv-feedback err" style="margin-top: 8px">{{ error }}</div>
          </div>

          <!-- 文件列表(按 importance 降序,后端已排序;点行展开详情) -->
          <div class="sv-field">
            <div class="sv-field-label"><span class="sv-supreme yellow" /> 文件清单</div>
            <div v-if="loading && items.length === 0" class="sv-empty" style="padding: 28px 12px">
              <p style="font-size: 12px">加载中…</p>
            </div>
            <div v-else-if="shown.length === 0" class="sv-empty" style="padding: 28px 12px">
              <div class="sv-empty-geo mb10">
                <span class="sq black" />
                <span class="sq pink" />
                <span class="sq deep" />
              <i class="diag" />
              </div>
              <p style="font-size: 12px">没有匹配的文件</p>
              <p style="font-size: 11px">可调整搜索词或类别筛选</p>
            </div>
            <div v-else class="sv-datalist" style="max-height: 460px; overflow-y: auto">
              <div
                v-for="it in shown"
                :key="it.path"
                class="sv-data-row repo-index-row"
                style="cursor: pointer"
                @click="toggleExpand(it.path)"
              >
                <div class="min-w-0 flex-1">
                  <div class="flex items-center gap-2">
                    <span class="sv-tag repo-index-kind">{{ repoKindLabel(it.kind) }}</span>
                    <b style="font-size: 12px">{{ baseName(it.path) }}</b>
                    <span class="sv-script-meta" style="font-size: 11px">{{ it.path }}</span>
                    <span
                      class="sv-script-meta"
                      style="font-size: 11px; margin-left: auto; white-space: nowrap"
                    >
                      重要性 {{ it.importance }} · {{ it.lines }} 行 · {{ formatBytes(it.bytes) }}
                      · {{ expandedPath === it.path ? '收起' : '展开' }}
                    </span>
                  </div>
                  <p class="sv-script-meta repo-index-summary">{{ it.summary || '(无摘要)' }}</p>

                  <!-- 展开:深摘要 / 备注 / 符号列表 -->
                  <div v-if="expandedPath === it.path" class="repo-index-detail">
                    <p v-if="it.module" class="sv-script-meta" style="font-size: 11px">
                      模块 {{ it.module }} · 类别 {{ it.category || '—' }} · 引用 {{ it.usageCount }} 次
                    </p>
                    <p v-if="it.deepSummary" class="repo-index-deep">{{ it.deepSummary }}</p>
                    <p v-if="it.note" class="sv-script-meta" style="font-size: 11px">备注:{{ it.note }}</p>
                    <div v-if="it.symbols.length" class="repo-index-symbols">
                      <span class="sv-script-meta" style="font-size: 11px">符号({{ it.symbols.length }}):</span>
                      <span v-for="s in it.symbols" :key="`${s.name}:${s.line}`" class="repo-index-symbol">
                        <code>{{ s.name }}</code>
                        <span class="sv-script-meta" style="font-size: 10px">{{ s.kind }} · L{{ s.line }}</span>
                      </span>
                    </div>
                    <p v-else class="sv-script-meta" style="font-size: 11px">(未提取到符号)</p>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </template>
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 搜索框:与 DevToolsModal 的控件行同宽策略 */
.repo-index-search {
  flex: 1;
  min-width: 200px;
  padding: 6px 10px;
  font-size: 12px;
}
/* 类别标签:实心蓝底(与 kind-tag 同族的只读展示) */
.repo-index-kind {
  background: var(--sv-blue);
  color: var(--sv-white);
  white-space: nowrap;
  flex-shrink: 0;
}
.repo-index-summary {
  margin: 4px 0 0;
  font-size: 11px;
  line-height: 1.6;
}
.repo-index-detail {
  margin-top: 6px;
  padding-top: 6px;
  border-top: 1px dashed var(--sv-line-strong);
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.repo-index-deep {
  margin: 0;
  font-size: 11px;
  line-height: 1.6;
  color: var(--sv-ink-dim);
  white-space: pre-wrap;
}
.repo-index-symbols {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 10px;
  align-items: center;
}
.repo-index-symbol {
  display: inline-flex;
  gap: 4px;
  align-items: baseline;
}
</style>
