<script setup lang="ts">
// 快速回复管理模态框(阶段四 4b):
// 草稿数组 + 行内编辑(name/label/content/enabled/position/sort_order),
// 新增/删除/↑↓ 排序,保存时 diff:新增 POST、行内改 PUT、删除 DELETE。
// 条目可被世界书 EJS 模板经 getqr/getQuickReply 读取并嵌套渲染(计划一已实现)。
import { ref, computed, onMounted } from 'vue';
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';
import * as api from '../api';

const store = useAppStore();
const { quickReplies } = storeToRefs(store);

/** 行内草稿;负 id 表示「尚未落库的新行」 */
const drafts = ref<api.QuickReplyRecord[]>([]);
/** 已删除的落库行 id(保存时逐个 DELETE) */
const removedIds = ref<Set<number>>(new Set());
const saving = ref(false);
const loadError = ref('');
const msg = ref<{ kind: 'ok' | 'err'; text: string } | null>(null);

const enabledCount = computed(() => drafts.value.filter((d) => d.enabled && d.content.trim()).length);

const close = (): void => {
  store.quickRepliesOpen = false;
};

function newDraft(): api.QuickReplyRecord {
  const minId = drafts.value.reduce((m, d) => Math.min(m, d.id), 0);
  return { id: (minId < 0 ? minId : 0) - 1, name: '', label: '', content: '', enabled: true, position: 0, sort_order: 100 };
}

function addRow(): void {
  drafts.value.push(newDraft());
}

function removeRow(i: number): void {
  const d = drafts.value[i];
  if (d && d.id >= 0) removedIds.value.add(d.id);
  drafts.value.splice(i, 1);
}

/** 组内上移/下移;移动后重排 sort_order(升序),drafts 顺序即显示顺序 */
function moveRow(i: number, dir: -1 | 1): void {
  const to = i + dir;
  if (to < 0 || to >= drafts.value.length) return;
  [drafts.value[i], drafts.value[to]] = [drafts.value[to], drafts.value[i]];
  drafts.value.forEach((d, idx) => (d.sort_order = idx));
}

async function save(): Promise<void> {
  if (saving.value) return;
  const invalid = drafts.value.find((d) => !d.name.trim());
  if (invalid) {
    msg.value = { kind: 'err', text: '存在名称为空的快速回复,请填写或删除后保存' };
    return;
  }
  saving.value = true;
  msg.value = null;
  try {
    // 1) 删除:先处理已移除的落库行
    for (const id of removedIds.value) {
      await api.deleteQuickReply(id);
    }
    removedIds.value = new Set();
    // 2) 新增/更新:负 id 为新建(POST),其余行内改(PUT)
    for (const d of drafts.value) {
      const { id: _id, created_at: _c, updated_at: _u, ...input } = d;
      if (d.id < 0) {
        await api.createQuickReply(input);
      } else {
        await api.updateQuickReply(d.id, input);
      }
    }
    await store.loadQuickReplies();
    drafts.value = quickReplies.value.map((r) => ({ ...r }));
    msg.value = { kind: 'ok', text: '快速回复已保存(世界书模板 getqr 下次渲染生效)' };
    setTimeout(() => (msg.value = null), 2500);
  } catch (err) {
    msg.value = { kind: 'err', text: `保存失败:${(err as Error).message}` };
  } finally {
    saving.value = false;
  }
}

onMounted(() => {
  void load();
});

async function load(): Promise<void> {
  loadError.value = '';
  try {
    await store.loadQuickReplies();
    drafts.value = quickReplies.value.map((r) => ({ ...r }));
    removedIds.value = new Set();
  } catch (err) {
    loadError.value = `加载快速回复失败:${(err as Error).message}`;
  }
}
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal wb-modal">
      <!-- 头部 -->
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" /> 快速回复
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-modal-body">
        <!-- 说明 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme blue" /> 说明</div>
          <p class="sv-note" style="margin: 0; line-height: 1.8">
            快速回复(Quick Replies)是预置的常用消息片段:在输入框上方点「快速回复」选择后
            <b>填入输入框</b>(不自动发送,按 Enter 确认)。<br />
            也可被世界书 EJS 模板经 <code>getqr/getQuickReply(name[, label])</code> 读取并嵌套渲染
            (酒馆助手生态兼容,渲染数据源已接入引擎)。<br />
            <b>启用</b> = 参与输入框快捷列表与 getqr 渲染;<b>内容为空</b> 的条目不出现在快捷列表。
          </p>
        </div>

        <!-- 列表 -->
        <div class="sv-field">
          <div class="sv-field-label">
            <span class="sv-supreme yellow" /> 快速回复列表
            <span class="sv-wb-count">{{ drafts.length }} 条(启用 {{ enabledCount }} 条)</span>
          </div>
          <div v-if="loadError" class="sv-feedback err" style="margin: 8px 0">{{ loadError }}</div>
          <div v-else-if="drafts.length === 0" class="sv-empty" style="padding: 28px 12px">
            <div class="sv-empty-geo mb10">
              <span class="sq black" />
              <span class="sq pink" />
              <span class="sq deep" />
            <i class="diag" />
            </div>
            <p style="font-size: 12px">暂无快速回复</p>
            <p style="font-size: 11px">点下方「＋ 新增」创建第一条</p>
          </div>
          <div v-else class="sv-datalist" style="max-height: 380px; overflow-y: auto">
            <div v-for="(d, i) in drafts" :key="`qr-${d.id}-${i}`" class="sv-data-row sv-wb-row" style="align-items: flex-start">
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-2">
                  <input v-model="d.name" type="text" class="sv-input sv-wb-comment" placeholder="名称(name)" spellcheck="false" />
                  <input v-model="d.label" type="text" class="sv-input sv-wb-comment" placeholder="显示标签(可选)" spellcheck="false" />
                </div>
                <textarea v-model="d.content" rows="2" class="sv-input sv-wb-entry-content" placeholder="回复内容" spellcheck="false" style="margin-top: 6px" />
                <div class="flex items-center gap-2" style="margin-top: 6px">
                  <button
                    class="sv-btn ghost sv-btn-sm"
                    :class="{ 'sv-btn-on': d.enabled }"
                    title="启用:参与输入框快捷列表与 getqr 渲染"
                    @click="d.enabled = !d.enabled"
                  >
                    {{ d.enabled ? '启用' : '停用' }}
                  </button>
                  <select v-model.number="d.position" class="sv-wb-select" title="注入位置权重(酒馆 injection position;当前仅用于排序)">
                    <option :value="0">位置 0</option>
                    <option :value="1">位置 1</option>
                    <option :value="2">位置 2</option>
                    <option :value="3">位置 3</option>
                    <option :value="4">位置 4</option>
                  </select>
                </div>
              </div>

              <div class="sv-wb-ops" style="flex-shrink: 0">
                <button class="sv-btn ghost sv-btn-sm" title="上移(排序)" :disabled="i === 0" @click="moveRow(i, -1)">↑</button>
                <button class="sv-btn ghost sv-btn-sm" title="下移(排序)" :disabled="i === drafts.length - 1" @click="moveRow(i, 1)">↓</button>
                <button class="sv-btn danger sv-btn-sm" title="删除" @click="removeRow(i)">✕</button>
              </div>
            </div>
          </div>
          <div class="sv-wb-edit-foot">
            <button class="sv-btn ghost sv-btn-sm" @click="addRow">＋ 新增</button>
            <button class="sv-btn primary sv-btn-sm" :disabled="saving" @click="save">
              {{ saving ? '保存中…' : '保存全部' }}
            </button>
          </div>
          <div v-if="msg" class="sv-feedback" :class="msg.kind">{{ msg.text }}</div>
        </div>
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>
