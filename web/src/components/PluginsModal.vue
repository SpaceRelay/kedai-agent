<script setup lang="ts">
// 插件管理弹窗:
//   1) 角色卡内嵌插件 —— 从当前角色卡自动检测(酒馆助手 SillyTavern-Assistant 等),kedai 内置实现
//   2) 自定义工具插件 —— 导入/删除/热重载(JSON 白名单脚本),已注册工具 + 磁盘文件
import { ref, onMounted, watch } from 'vue';
import { useAppStore } from '../store';
import * as api from '../api';
import { detectCardPluginsClient } from '../plugins';

const store = useAppStore();

const loading = ref(false);
const tools = ref<api.ToolPluginInfo[]>([]);
const files = ref<string[]>([]);
const msg = ref<{ kind: 'ok' | 'err' | 'info'; text: string } | null>(null);
const fileInput = ref<HTMLInputElement | null>(null);

// 角色卡内嵌插件
const cardPlugins = ref<api.CardPluginInfo[]>([]);
const cardLoading = ref(false);

const close = (): void => {
  store.pluginsOpen = false;
};

async function refresh(): Promise<void> {
  loading.value = true;
  try {
    const status = await api.listPluginTools();
    tools.value = status.tools;
    files.value = status.files;
  } catch (e) {
    msg.value = { kind: 'err', text: `加载失败:${(e as Error).message}` };
  } finally {
    loading.value = false;
  }
}

/** 拉取当前角色详情,取内嵌插件检测结果(详情接口 card_plugins 优先,
 *  旧后端无该字段时从 data_raw 前端兜底检测) */
async function refreshCardPlugins(): Promise<void> {
  const cid = store.currentCharacterId;
  if (!cid) {
    cardPlugins.value = [];
    return;
  }
  cardLoading.value = true;
  try {
    const c = await api.getCharacter(cid);
    cardPlugins.value = c.card_plugins ?? detectCardPluginsClient(c.data_raw);
  } catch {
    cardPlugins.value = [];
  } finally {
    cardLoading.value = false;
  }
}

async function onFilePicked(e: Event): Promise<void> {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  if (!file) return;
  msg.value = null;
  try {
    const r = await api.uploadPluginTool(file);
    msg.value = { kind: 'ok', text: `已导入插件「${r.name}」(${r.file})` };
    await refresh();
  } catch (err) {
    msg.value = { kind: 'err', text: `导入失败:${(err as Error).message}` };
  }
}

async function onReload(): Promise<void> {
  msg.value = null;
  try {
    const r = await api.reloadPluginTools();
    msg.value = r.ok
      ? { kind: 'ok', text: `已重载 ${r.loaded} 个工具插件` }
      : { kind: 'err', text: `重载部分失败:${(r.errors ?? []).join('; ')}` };
    await refresh();
  } catch (err) {
    msg.value = { kind: 'err', text: `重载失败:${(err as Error).message}` };
  }
}

async function onDelete(file: string): Promise<void> {
  if (!confirm(`确定删除插件文件「${file}」?其对应工具将不再可用。`)) return;
  try {
    const r = await api.deletePluginTool(file);
    msg.value = { kind: 'ok', text: `已删除 ${r.removed}` };
    await refresh();
  } catch (err) {
    msg.value = { kind: 'err', text: `删除失败:${(err as Error).message}` };
  }
}

onMounted(() => {
  void refresh();
  void refreshCardPlugins();
});
// 切换角色时刷新内嵌插件检测
watch(
  () => store.currentCharacterId,
  () => void refreshCardPlugins(),
);
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal lg">
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" /> 插件
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-modal-body">
        <!-- ===== 角色卡内嵌插件 ===== -->
        <div class="sv-field-label"><span class="sv-supreme green" /> 角色卡内嵌插件</div>
        <p class="sv-note" style="margin: 4px 0 10px">
          自动检测当前角色卡内嵌的插件(如酒馆助手 SillyTavern-Assistant)。kedai 已内置完整兼容实现,无需安装或启用。
        </p>

        <div v-if="!store.currentCharacterId" class="sv-plugin-empty">
          <span class="sv-plugin-empty-icon">◼</span>
          <p>尚未选择角色</p>
          <p class="sv-plugin-empty-sub">在左侧选择一个角色后,这里会显示其角色卡内嵌的插件</p>
        </div>
        <div v-else-if="cardLoading" class="sv-plugin-empty">
          <span class="sv-plugin-empty-icon">…</span>
          <p>正在检测当前角色卡…</p>
        </div>
        <div v-else-if="cardPlugins.length === 0" class="sv-plugin-empty">
          <span class="sv-plugin-empty-icon">◼</span>
          <p>当前角色卡未检测到内嵌插件</p>
          <p class="sv-plugin-empty-sub">普通角色卡无需插件;使用酒馆助手插件编写的角色卡(含 EJS 模板 / [InitVar] / UpdateVariable 等特征)会自动识别</p>
        </div>
        <div v-else class="sv-plugin-cards">
          <div v-for="p in cardPlugins" :key="p.id" class="sv-plugin-card">
            <div class="sv-plugin-card-head">
              <span class="sv-supreme green" style="width: 14px; height: 14px; flex: none" />
              <div class="min-w-0 flex-1">
                <div class="sv-plugin-name">
                  {{ p.name }}
                  <code class="sv-plugin-name-en">{{ p.name_en }}</code>
                </div>
                <div class="sv-plugin-desc">{{ p.description }}</div>
              </div>
              <span class="sv-plugin-badge" :title="'来源:' + p.source">已启用</span>
            </div>
            <div class="sv-plugin-features">
              <span
                v-for="f in p.features"
                :key="f.id"
                class="sv-chip"
                :class="{ off: !f.detected }"
                :title="f.detected ? '当前角色卡已使用此能力' : '插件支持此能力,当前角色卡未使用'"
              >
                {{ f.label }}
              </span>
            </div>
          </div>
        </div>

        <!-- ===== 自定义工具插件 ===== -->
        <div class="sv-field-label" style="margin-top: 22px"><span class="sv-supreme blue" /> 自定义工具插件</div>
        <p class="sv-note" style="margin: 4px 0 10px">
          以 JSON 文件形式存放在 <code>data/plugins/tools/</code> 目录,由后端白名单脚本执行器加载
          (不使用 eval,仅支持字面量/算术/字符串方法/JSON 等安全子集)。导入后即可被 Agent 调用。
        </p>

        <!-- 操作区 -->
        <div class="flex items-center gap-2" style="margin-bottom: 12px">
          <button class="sv-btn primary sv-btn-sm" @click="fileInput?.click()">导入插件</button>
          <button class="sv-btn ghost sv-btn-sm" :disabled="loading" @click="onReload">
            重载插件
          </button>
          <span v-if="loading" class="sv-topbar-sub">加载中…</span>
        </div>
        <input
          ref="fileInput"
          type="file"
          accept=".json,application/json"
          class="hidden"
          @change="onFilePicked"
        />

        <div v-if="msg" class="sv-feedback" :class="msg.kind === 'err' ? 'err' : msg.kind === 'ok' ? 'ok' : ''" style="margin-bottom: 10px">
          {{ msg.text }}
        </div>

        <!-- 已注册工具 -->
        <div class="sv-field-label"><span class="sv-supreme green" /> 已注册工具</div>
        <div class="sv-plugin-list">
          <div v-for="t in tools" :key="t.name" class="sv-plugin-item">
            <span class="sv-plugin-item-dot" />
            <div class="min-w-0 flex-1">
              <div class="sv-char-name">{{ t.name }}</div>
              <div class="sv-char-desc">{{ t.description || '无描述' }}</div>
            </div>
          </div>
          <div v-if="tools.length === 0" class="sv-note" style="padding: 8px 0">
            暂无自定义工具插件(内置 calculator / memory 不在此列表)。
          </div>
        </div>

        <!-- 插件文件 -->
        <div class="sv-field-label" style="margin-top: 16px"><span class="sv-supreme red" /> 插件文件</div>
        <div class="sv-plugin-list">
          <div v-for="f in files" :key="f" class="sv-plugin-item">
            <div class="min-w-0 flex-1">
              <div class="sv-char-desc" style="font-family: ui-monospace, Consolas, monospace">{{ f }}</div>
            </div>
            <button class="sv-btn ghost sv-btn-sm" @click="onDelete(f)">删除</button>
          </div>
          <div v-if="files.length === 0" class="sv-note" style="padding: 8px 0">
            目录为空。可导入 JSON 插件,或参考 <code>data/plugins/tools/greeting.json</code> 示例。
          </div>
        </div>
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>
