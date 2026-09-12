<script setup lang="ts">
// 设置模态框(壳):embedded 模式裸内容(供 SettingsHub 内容区嵌入,按 activeSection 切换显示),
// standalone 模式遮罩 + 头部 + 内容 + 底部(App.vue 深链接/旧入口兜底)。
// 两个模式共享同一组 section 组件(src/components/settings/),业务逻辑在 src/composables/。
// 历史背景:原实现 embedded / standalone 两套模板复制粘贴且已漂移(standalone 缺失
// 预设导入导出、提示词预览、工具策略行与压缩参数),现已合并为单组共享 section。
// 分区懒加载(M2-C5):默认首 tab(API 连接)两个分区同步导入(打开即见);其余 8 个分区
// 经 lazyModal 拆 chunk,embedded 模式首次切到对应 tab 才挂载加载,挂载后常驻不丢编辑态;
// 加载失败语义同弹窗(自动重试一次 → 全局错误条手动重试,重开综合设置触发重新加载)。
import { ref, watch } from 'vue';
import { useAppStore } from '../store';
import { lazyModal } from '../asyncModal';
import { useApiSettings } from '../composables/useApiSettings';
import { usePromptInject } from '../composables/usePromptInject';
import { useDataManager } from '../composables/useDataManager';
import ApiSettingsSection from './settings/ApiSettingsSection.vue';
import ConnectionSection from './settings/ConnectionSection.vue';

const GenParamsSection = lazyModal(() => import('./settings/GenParamsSection.vue'), '设置区:模型与生成', 'settingsOpen');
const AgentSettingsSection = lazyModal(() => import('./settings/AgentSettingsSection.vue'), '设置区:Agent 设置', 'settingsOpen');
const AgentFlowSection = lazyModal(() => import('./settings/AgentFlowSection.vue'), '设置区:执行流程', 'settingsOpen');
const PromptInjectSection = lazyModal(() => import('./settings/PromptInjectSection.vue'), '设置区:提示词注入', 'settingsOpen');
const PresetImportExportSection = lazyModal(() => import('./settings/PresetImportExportSection.vue'), '设置区:预设导入', 'settingsOpen');
const DataManagementSection = lazyModal(() => import('./settings/DataManagementSection.vue'), '设置区:数据管理', 'settingsOpen');
const McpSection = lazyModal(() => import('./settings/McpSection.vue'), '设置区:MCP 服务', 'settingsOpen');
const UiSection = lazyModal(() => import('./settings/UiSection.vue'), '设置区:界面', 'settingsOpen');
const EmbeddingSection = lazyModal(() => import('./settings/EmbeddingSection.vue'), '设置区:向量化模型', 'settingsOpen');
const AboutSection = lazyModal(() => import('./settings/AboutSection.vue'), '设置区:关于', 'settingsOpen');

const props = withDefaults(defineProps<{
  embedded?: boolean;
  activeSection?: string;
}>(), {
  embedded: false,
  activeSection: 'api',
});

const store = useAppStore();

// ===== 跨 section 共享的业务状态(在壳创建一次,经 prop 传入,避免重复实例化导致状态分叉) =====
// API 设置区 + 后端连接区共享
const apiSettings = useApiSettings();
// 提示词注入区 + 预设导入导出区共享
const promptInject = usePromptInject();
// 数据管理区 + 界面区共享
const dataManager = useDataManager();

/**
 * embedded 懒加载门禁:记录已访问分区,首次切到才挂载对应异步分区(触发 chunk 加载);
 * 挂载后常驻(:show 切显隐),切 tab 不丢分区本地编辑态。standalone 模式无 tab,全量渲染。
 */
const visitedSections = ref<ReadonlySet<string>>(new Set([props.activeSection]));
watch(
  () => props.activeSection,
  (s) => {
    if (!visitedSections.value.has(s)) {
      visitedSections.value = new Set(visitedSections.value).add(s);
    }
  },
);

const close = (): void => {
  store.settingsOpen = false;
};
</script>

<template>
  <!-- embedded 模式:仅渲染内容区,供 SettingsHub 嵌入(按 activeSection 切换显示;
       懒加载分区首次切到才挂载,挂载后常驻由 :show 控制显隐) -->
  <div v-if="props.embedded" class="sv-settings-embedded">
    <ApiSettingsSection :state="apiSettings" :show="props.activeSection === 'api'" />
    <ConnectionSection :state="apiSettings" :show="props.activeSection === 'api'" />
    <GenParamsSection v-if="visitedSections.has('model')" :show="props.activeSection === 'model'" />
    <AgentSettingsSection v-if="visitedSections.has('agent')" :show="props.activeSection === 'agent'" />
    <AgentFlowSection v-if="visitedSections.has('flow')" :show="props.activeSection === 'flow'" />
    <PromptInjectSection v-if="visitedSections.has('prompt')" :state="promptInject" :show="props.activeSection === 'prompt'" />
    <PresetImportExportSection v-if="visitedSections.has('preset')" :state="promptInject" :show="props.activeSection === 'preset'" />
    <DataManagementSection v-if="visitedSections.has('data')" :state="dataManager" :show="props.activeSection === 'data'" />
    <McpSection v-if="visitedSections.has('mcp')" :show="props.activeSection === 'mcp'" />
    <EmbeddingSection v-if="visitedSections.has('embedding')" :show="props.activeSection === 'embedding'" />
    <UiSection v-if="visitedSections.has('ui')" :state="dataManager" :show="props.activeSection === 'ui'" />
    <AboutSection v-if="visitedSections.has('about')" :show="props.activeSection === 'about'" />
  </div>

  <!-- 独立模态框模式(遮罩 + 头部 + 全部设置区 + 底部) -->
  <div v-else class="sv-modal-mask" @click.self="close">
    <div class="sv-modal">
      <!-- 头部 -->
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" /> 设置
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-modal-body">
        <ApiSettingsSection :state="apiSettings" />
        <ConnectionSection :state="apiSettings" />
        <GenParamsSection />
        <AgentSettingsSection />
        <AgentFlowSection />
        <PromptInjectSection :state="promptInject" />
        <PresetImportExportSection :state="promptInject" />
        <DataManagementSection :state="dataManager" />
        <McpSection />
    <EmbeddingSection />
    <UiSection :state="dataManager" />
    <AboutSection />
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>
