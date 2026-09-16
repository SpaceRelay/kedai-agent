<script setup lang="ts">
// 设置区:界面(Agent 面板开关 / 安全 HTML 渲染 / 角色卡 JavaScript 授权管理)。
// 从 SettingsModal.vue 双模板合并而来:取 embedded 版本(授权说明与撤销列表更完整)。
import { useAppStore } from '../../store';
import { storeToRefs } from 'pinia';
import type { useDataManager } from '../../composables/useDataManager';

const props = withDefaults(defineProps<{
  /** useDataManager 的返回对象(壳共享实例,本区仅用 characterLabel / 撤销授权) */
  state: ReturnType<typeof useDataManager>;
  /** 是否显示(embedded 模式按 activeSection 切换;standalone 恒 true) */
  show?: boolean;
}>(), {
  show: true,
});

const { characterLabel, confirmRevokeScriptAuthorization } = props.state;

const store = useAppStore();
const { scriptAuthorizations } = storeToRefs(store);
</script>

<template>
  <div v-show="props.show" class="sv-field">
    <div class="sv-field-label"><span class="sv-supreme yellow" /> 界面</div>
    <div class="sv-datalist">
      <div class="sv-data-row">
        <div class="info">
          <b>Agent 面板</b>
          <span>生成时自动展开,展示步骤、推理链与工具调用</span>
        </div>
        <button class="sv-btn ghost" @click="store.toggleAgentPanel()">
          {{ store.agentPanelOpen ? '展开中' : '已折叠' }}
        </button>
      </div>
      <div class="sv-data-row">
        <div class="info">
          <b>安全 HTML 渲染</b>
          <span>应用角色卡正则替换并清理 script/style 属性、事件处理器、javascript URL 与外部资源；不代表允许 JavaScript。开关按角色卡记忆,未设置的角色卡回退全局默认</span>
        </div>
        <button type="button" class="sv-btn ghost" :aria-pressed="store.renderHtml" @click="store.toggleRenderHtml()">
          {{ store.renderHtml ? '已开启' : '已关闭' }}
        </button>
      </div>
      <div class="sv-data-row" style="align-items: flex-start">
        <div class="info">
          <b>角色卡 JavaScript 授权</b>
          <span>默认禁用；按角色 ID 与脚本哈希授权，内容变化后自动失效。授权记录仅保存在当前浏览器配置中。</span>
          <div v-if="scriptAuthorizations.length" style="display: grid; gap: 8px; margin-top: 10px">
            <div v-for="grant in scriptAuthorizations" :key="grant.characterId" class="flex items-center gap-2">
              <span>{{ characterLabel(grant.characterId) }} · {{ new Date(grant.authorizedAt).toLocaleString('zh-CN', { hour12: false }) }}</span>
              <button type="button" class="sv-btn ghost sv-btn-sm" @click="confirmRevokeScriptAuthorization(grant.characterId)">撤销</button>
            </div>
          </div>
          <span v-else>暂无已授权角色卡。</span>
        </div>
      </div>
    </div>
  </div>
</template>
