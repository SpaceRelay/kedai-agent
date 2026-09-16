<script setup lang="ts">
// 设置区:自定义 Agent 执行流程(custom 模式;流程库管理 + 步骤编辑)。
// 从 SettingsModal.vue 双模板合并而来:取 embedded 超集版本(standalone 分支缺失
// 「工具策略 / 并行调用」行,属模板漂移,合并后 standalone 一并补上)。
import { onMounted } from 'vue';
import { useAgentFlow, TOOL_MODE_LABELS } from '../../composables/useAgentFlow';
import { stepToolsWarnings } from '../../utils/agentFlowTools';

withDefaults(defineProps<{
  /** 是否显示(embedded 模式按 activeSection 切换;standalone 恒 true) */
  show?: boolean;
}>(), {
  show: true,
});

const {
  flowDraft, flowSaving, flowMsg, editingStepId, dragStepId, flowId, flowName, flowDesc,
  flowImportInput, flowImporting, flowLibFlows,
  loadFlowConfig, onFlowSelect, newFlow, duplicateFlow, deleteFlowNow, onFlowImport,
  exportFlowNow, saveFlowNow, addStep, removeStep, moveStep, onStepActionChange,
  onStepDragStart, onStepDragOver, onStepDrop, onStepDragEnd,
  stepToolMode, setStepToolMode, stepToolsText, setStepToolsText,
} = useAgentFlow();

onMounted(async () => {
  // 加载自定义 Agent 执行流程(custom 模式)
  await loadFlowConfig();
});
</script>

<template>
  <div v-show="show" class="sv-field">
    <div class="sv-field-label"><span class="sv-supreme orange" /> Agent 执行流程</div>
    <div class="sv-stack">
      <!-- 流程库管理:选择 / 新建 / 复制 / 删除 / 导入 / 导出 -->
      <div class="sv-inp-row">
        <label class="sv-inp-tag">当前流程</label>
        <select
          v-model="flowId"
          class="sv-select flow-select-wide"
          :disabled="!flowLibFlows.length"
          @change="onFlowSelect"
        >
          <option v-for="f in flowLibFlows" :key="f.id" :value="f.id">
            {{ f.name || '未命名流程' }}{{ f.enabled ? '' : '(未启用)' }}
          </option>
        </select>
        <span class="flow-lib-actions">
          <button class="sv-btn ghost sv-btn-square" title="新建流程" @click="newFlow">+</button>
          <button class="sv-btn ghost sv-btn-square" title="复制当前流程" @click="duplicateFlow">⧉</button>
          <button class="sv-btn danger sv-btn-square" title="删除当前流程" @click="deleteFlowNow">✕</button>
        </span>
      </div>
      <div class="sv-inp-row">
        <span class="flow-meta-pair">
          <label class="sv-inp-tag">流程名称</label>
          <input v-model="flowName" type="text" class="sv-input" placeholder="流程名称(保存时生效)" spellcheck="false" />
        </span>
        <span class="flow-meta-pair">
          <label class="sv-inp-tag flow-meta-gap">说明</label>
          <input v-model="flowDesc" type="text" class="sv-input" placeholder="流程说明(可选)" spellcheck="false" />
        </span>
      </div>
      <div class="sv-btn-row">
        <button class="sv-btn ghost sv-btn-fill" :disabled="flowImporting" @click="flowImportInput?.click()">
          {{ flowImporting ? '导入中...' : '导入流程(JSON)' }}
        </button>
        <button class="sv-btn ghost sv-btn-fill" :disabled="!flowDraft" @click="exportFlowNow">
          导出流程(JSON)
        </button>
      </div>
      <input
        ref="flowImportInput"
        type="file"
        accept=".json,application/json"
        class="hidden"
        @change="onFlowImport"
      />
      <div class="sv-inp-row">
        <label class="sv-inp-tag">启用</label>
        <button
          v-if="flowDraft"
          class="sv-btn ghost"
          :class="{ 'sv-btn-on': flowDraft.enabled }"
          @click="flowDraft.enabled = !flowDraft.enabled"
        >
          {{ flowDraft.enabled ? '已启用' : '已关闭' }}
        </button>
        <span v-else class="sv-note">加载中...</span>
      </div>
      <p class="sv-note">
        自定义流程:输入栏切换到 <b>CUSTOM</b> 模式后,按下方步骤从上到下依次执行;
        每步可独立设置系统提示词(支持酒馆宏,以 <code v-pre>[本步指令]</code> 追加到系统提示词末尾)、
        生成参数与工具范围。至少需要一步「生成正文」的 direct 步骤。
      </p>
      <div v-if="flowDraft && !flowDraft.steps.length" class="sv-note inject-empty">
        尚未添加步骤,点击下方「+ 新增步骤」。
      </div>
      <div
        v-for="step in flowDraft?.steps ?? []"
        :key="step.id"
        class="flow-row"
        :class="{ dragging: dragStepId === step.id }"
        draggable="true"
        @dragstart="onStepDragStart($event, step.id)"
        @dragover="onStepDragOver($event, step.id)"
        @drop="onStepDrop($event, step.id)"
        @dragend="onStepDragEnd"
      >
        <span class="floor-grip" title="拖拽排序">⋮⋮</span>
        <button
          class="sv-btn ghost"
          :class="{ 'sv-btn-on': step.enabled }"
          :title="step.enabled ? '点击停用' : '点击启用'"
          @click="step.enabled = !step.enabled"
        >
          {{ step.enabled ? '开' : '关' }}
        </button>
        <input v-model="step.name" type="text" class="sv-input floor-name" placeholder="步骤名称" spellcheck="false" />
        <select
          v-model="step.action"
          class="sv-select floor-select"
          title="动作:direct=执行/生成,reflect=反思(不生成)"
          @change="onStepActionChange(step)"
        >
          <option value="direct">执行</option>
          <option value="reflect">反思</option>
        </select>
        <button
          v-if="step.action === 'direct'"
          class="sv-btn ghost"
          :class="{ 'sv-btn-on': step.generates }"
          :title="step.generates ? '该步生成正文' : '该步不生成(如理解意图)'"
          @click="step.generates = !step.generates"
        >
          {{ step.generates ? '生成' : '不生成' }}
        </button>
        <div class="floor-actions">
          <button class="sv-btn ghost sv-btn-square" title="上移" @click="moveStep(step.id, -1)">↑</button>
          <button class="sv-btn ghost sv-btn-square" title="下移" @click="moveStep(step.id, 1)">↓</button>
          <button
            class="sv-btn ghost sv-btn-square"
            :title="editingStepId === step.id ? '收起编辑' : '编辑详情'"
            @click="editingStepId = editingStepId === step.id ? null : step.id"
          >
            ✎
          </button>
          <button class="sv-btn danger sv-btn-square" title="删除步骤" @click="removeStep(step.id)">✕</button>
        </div>
        <!-- 展开编辑区:跨整行、纵向堆叠,避免被步骤行 grid 挤压 -->
        <div v-if="editingStepId === step.id" class="flow-edit">
          <div class="sv-inp-row">
            <label class="sv-inp-tag">目标</label>
            <input v-model="step.goal" type="text" class="sv-input" placeholder="该步骤做什么(进度提示与计划摘要显示)" spellcheck="false" />
          </div>
          <template v-if="step.action === 'direct'">
            <div class="sv-inp-row">
              <label class="sv-inp-tag">系统提示词</label>
              <textarea
                v-model="step.system_prompt"
                rows="3"
                class="sv-input"
                placeholder="步骤级提示词(支持酒馆宏),以 [本步指令] 追加到系统提示词末尾;留空 = 不追加"
                spellcheck="false"
              />
            </div>
            <div class="sv-inp-row">
              <label class="sv-inp-tag">工具</label>
              <select
                class="sv-select flow-select-wide"
                :value="stepToolMode(step)"
                @change="setStepToolMode(step, ($event.target as HTMLSelectElement).value as 'none' | 'all' | 'list')"
              >
                <option v-for="(label, val) in TOOL_MODE_LABELS" :key="val" :value="val">{{ label }}</option>
              </select>
              <input
                v-if="stepToolMode(step) === 'list'"
                class="sv-input"
                :value="stepToolsText(step)"
                placeholder="工具名,逗号分隔(如 read, search, calculator)"
                spellcheck="false"
                @change="setStepToolsText(step, ($event.target as HTMLInputElement).value)"
              />
            </div>
            <!-- F8(2026-09-10 实跑修复):tools 三态语义易误配——「全部工具」会下发
                 全部已注册工具(含编排/写类),分析规划类步骤不应选它。
                 文案由 stepToolsWarnings 纯函数产出,便于单测覆盖 -->
            <p
              v-for="(warn, wi) in stepToolsWarnings(step)"
              :key="wi"
              class="sv-note flow-tool-warn"
            >
              {{ warn }}
            </p>
            <p v-if="stepToolMode(step) === 'none'" class="sv-note">
              「不使用工具」= 本步骤纯生成,不下发任何工具。
            </p>
            <div class="sv-inp-row">
              <label class="sv-inp-tag">工具策略</label>
              <select v-model="step.tool_choice" class="sv-select flow-select-wide">
                <option value="auto">auto（模型决定）</option>
                <option value="none">none（禁止调用）</option>
                <option value="required">required（至少调用一个）</option>
                <option value="function">function（指定工具）</option>
              </select>
              <input
                v-if="step.tool_choice === 'function'"
                v-model="step.tool_choice_function"
                class="sv-input"
                placeholder="必须是本步骤有效工具名"
                spellcheck="false"
              />
              <label class="sv-inp-tag">并行调用</label>
              <select v-model="step.parallel_tool_calls" class="sv-select flow-select-wide">
                <option :value="null">后端默认</option>
                <option :value="true">允许</option>
                <option :value="false">禁止</option>
              </select>
            </div>
            <div class="sv-inp-row">
              <label class="sv-inp-tag">温度</label>
              <input v-model.number="step.temperature" type="number" min="0" max="2" step="0.1" class="sv-input inject-num" placeholder="沿用全局" />
              <label class="sv-inp-tag">输出上限</label>
              <input v-model.number="step.max_tokens" type="number" min="1" max="131072" class="sv-input inject-num" placeholder="沿用全局" title="该步骤的输出上限(1~131072);留空沿用全局最大生成长度" />
            </div>
          </template>
        </div>
      </div>
      <div class="sv-btn-row">
        <button v-if="flowDraft" class="sv-btn ghost sv-btn-fill" @click="addStep">+ 新增步骤</button>
      </div>
      <div class="sv-btn-row">
        <button class="sv-btn primary sv-btn-fill" :disabled="flowSaving || !flowDraft" @click="saveFlowNow">
          {{ flowSaving ? '保存中...' : '保存执行流程' }}
        </button>
        <div
          v-if="flowMsg"
          class="sv-feedback-flex"
          :class="flowMsg.includes('失败') ? 'err' : 'ok'"
        >{{ flowMsg }}</div>
      </div>
    </div>
  </div>
</template>
