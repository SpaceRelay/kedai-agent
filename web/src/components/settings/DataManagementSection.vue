<script setup lang="ts">
// 设置区:数据管理(聊天导出 / 导入 / 清空当前会话)。
// 从 SettingsModal.vue 双模板合并而来:两分支内容一致。
// 状态由壳(SettingsModal)创建一次后经 prop 传入,与 UiSection 共享 useDataManager。
import { useAppStore } from '../../store';
import { storeToRefs } from 'pinia';
import type { useDataManager } from '../../composables/useDataManager';

const props = withDefaults(defineProps<{
  /** useDataManager 的返回对象(壳共享实例) */
  state: ReturnType<typeof useDataManager>;
  /** 是否显示(embedded 模式按 activeSection 切换;standalone 恒 true) */
  show?: boolean;
}>(), {
  show: true,
});

const {
  importInput, importError, exportMsg, clearMsg,
  onImportFile, clearAllData, exportChat,
  undoEnabled, undoMsg, saveUndoEnabled,
} = props.state;

const { currentSessionId } = storeToRefs(useAppStore());
</script>

<template>
  <div v-show="props.show" class="sv-field">
    <div class="sv-field-label"><span class="sv-supreme blue" /> 数据管理</div>
    <div class="sv-datalist">
      <div class="sv-data-row">
        <div class="info">
          <b>导出聊天</b>
          <span>当前会话导出为 SillyTavern 兼容 JSON</span>
        </div>
        <button class="sv-btn ghost" :disabled="!currentSessionId" @click="exportChat">导出</button>
      </div>
      <div class="sv-data-row">
        <div class="info">
          <b>导入聊天</b>
          <span>导入 JSON 替换当前会话内容</span>
        </div>
        <button class="sv-btn ghost" :disabled="!currentSessionId" @click="importInput?.click()">导入</button>
      </div>
      <div class="sv-data-row">
        <div class="info">
          <b>清空当前会话</b>
          <span>删除本会话全部消息,保留角色定义</span>
        </div>
        <button class="sv-btn danger" :disabled="!currentSessionId" @click="clearAllData">清空</button>
      </div>
      <div class="sv-data-row">
        <div class="info">
          <b>回退快照(undo)</b>
          <span>写工具(写文件/改变量等)执行前自动存档,可在 Agent 面板回退到该次修改前</span>
        </div>
        <label style="display: flex; gap: 6px; align-items: center; cursor: pointer" title="开启后写工具执行前自动保存快照,Agent 面板工具调用项出现「回退到此处」入口">
          <input v-model="undoEnabled" type="checkbox" style="flex-shrink: 0" @change="saveUndoEnabled" />
          <span class="sv-note">{{ undoEnabled ? '已开启' : '已关闭' }}</span>
        </label>
      </div>
    </div>
    <div v-if="importError" class="sv-feedback err">{{ importError }}</div>
    <div v-if="exportMsg" class="sv-feedback ok">{{ exportMsg }}</div>
    <div v-if="clearMsg" class="sv-feedback ok">{{ clearMsg }}</div>
    <div v-if="undoMsg" class="sv-feedback" :class="undoMsg.startsWith('保存失败') ? 'err' : 'ok'">{{ undoMsg }}</div>
    <input ref="importInput" type="file" accept=".json,application/json" class="hidden" @change="onImportFile" />
    <p class="sv-note">
      导入格式与 SillyTavern 兼容:<code>[{"role":"user","content":"..."}]</code>
    </p>
  </div>
</template>
