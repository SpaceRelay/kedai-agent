<script setup lang="ts">
// 设置模态框:API 连接(前端直接编辑 Base URL / Key / 模型 + 从 API 加载模型列表)、
// 生成参数(温度/Top-P/长度/上下文窗口)、Agent 设置、主 Agent 提示词、自定义执行流程、
// 提示词注入、数据管理、界面。
// API 连接与生成参数保存后持久化到服务端 data/settings.json(API Key 仅回显脱敏)。
// embedded 模式:作为 SettingsHub 的内容区渲染,不显示遮罩/头部/底部。
// 各业务逻辑拆分在 src/composables/ 下,本组件仅组合装配。
import { ref, onMounted } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import * as api from '../api';
import { useApiSettings, normalizeUrl, modelLoadHint } from '../composables/useApiSettings';
import { useAgentSettings } from '../composables/useAgentSettings';
import { useAgentFlow, TOOL_MODE_LABELS } from '../composables/useAgentFlow';
import { useAgentPromptEditor } from '../composables/useAgentPromptEditor';
import { usePromptInject, FLOOR_ROLE_LABELS, FLOOR_POS_LABELS, MACRO_HINTS } from '../composables/usePromptInject';
import { useDataManager, useGenerationParams } from '../composables/useDataManager';

const props = withDefaults(defineProps<{
  embedded?: boolean;
  activeSection?: string;
}>(), {
  embedded: false,
  activeSection: 'api',
});

const store = useAppStore();
const { currentSessionId, currentCharacterId, agentDockOpen, scriptAuthorizations } =
  storeToRefs(store);

// ===== 各域业务逻辑(composables) =====
const apiSettings = useApiSettings();
const {
  apiBaseUrl, apiKey, apiKeyMasked, hasApiKey, savingApi, apiFeedback,
  connInfo, models, connecting, connFeedback, switchingModel, modelFeedback, refreshing,
  loadApiSettings, onBaseUrlBlur, doConnect, saveApi, refreshModelList, onModelChange,
} = apiSettings;

const agentSettings = useAgentSettings();
const { agentSaving, agentMsg, resetAgentPrompt, saveAgentNow } = agentSettings;

const promptEditor = useAgentPromptEditor();
const {
  agentPromptMd, promptPreview, previewLoading, previewError, mdLoading, mdSaving, mdMsg,
  loadPromptPreview, loadAgentPromptMd, saveAgentPromptMd,
} = promptEditor;

const promptInject = usePromptInject();
const {
  injectDraft, injectSaving, injectMsg, editingFloorId, dragFloorId, injectImportInput, injectImporting,
  loadInjectConfig, saveInjectNow, addFloor, removeFloor, moveFloor, onImportPreset, exportInjectConfig,
  onDragStart, onDragOver, onDrop, onDragEnd,
} = promptInject;

const agentFlow = useAgentFlow();
const {
  flowDraft, flowSaving, flowMsg, editingStepId, dragStepId, flowId, flowName, flowDesc,
  flowImportInput, flowImporting, flowLibFlows,
  loadFlowConfig, onFlowSelect, newFlow, duplicateFlow, deleteFlowNow, onFlowImport,
  exportFlowNow, saveFlowNow, addStep, removeStep, moveStep, onStepActionChange,
  onStepDragStart, onStepDragOver, onStepDrop, onStepDragEnd,
  stepToolMode, setStepToolMode, stepToolsText, setStepToolsText,
} = agentFlow;

const dataManager = useDataManager();
const {
  importInput, importError, exportMsg, clearMsg, characterLabel, confirmRevokeScriptAuthorization,
  onImportFile, clearAllData, exportChat,
} = dataManager;

const generationParams = useGenerationParams();
const { tempLabel, topPLabel, ctxLabel, saveParams, paramsMsg, saveParamsNow } = generationParams;

// ===== 生成参数(storeToRefs 直接绑定) =====
const { temperature, topP, maxTokens, maxContextTokens, maxToolRounds, compactionMode, compactionThreshold } = storeToRefs(store);

// ===== Agent 设置(storeToRefs 直接绑定) =====
const { agentSystemPrompt, searchEndpoint, mvuVarsPosition, reflectPrompt } = storeToRefs(store);

const close = (): void => {
  store.settingsOpen = false;
};

onMounted(async () => {
  await loadApiSettings();
  maxToolRounds.value = maxToolRounds.value ?? 32;
  // 加载 DATA_DIR 主 Agent 提示词与脱敏后的最终分层预览
  await loadAgentPromptMd();
  await loadPromptPreview();
  // 加载提示词注入配置(简单模式 + 楼层)
  await loadInjectConfig();
  // 独立模态框模式的禁词词条表(embedded 模式不展示该表)
  syncBannedWordsFromPrompt();
  // 加载自定义 Agent 执行流程(custom 模式)
  await loadFlowConfig();
});

// ===== 禁词词条表(旧版词条编辑,仅独立模态框模式使用;embedded 模式用纯文本 banned_prompt) =====
// 注:注入配置当前以 banned_prompt 纯文本承载(见 usePromptInject 的导入迁移),
// 此处词条表仅作可视化编辑入口:编辑结果同步写回 banned_prompt 文本。
const simpleBannedWords = ref<{ word: string; replacement: string }[]>([]);

/** 从 banned_prompt 文本反解析词条表(旧格式兼容;词条间以「、」分隔) */
function syncBannedWordsFromPrompt(): void {
  if (!injectDraft.value) return;
  const text = injectDraft.value.simple.banned_prompt ?? '';
  // 仅当文本形如「禁止…:词1、词2」时尝试解析;否则保留空表
  const m = /:(.+)$/.exec(text);
  simpleBannedWords.value = m
    ? m[1].split('、').map((w) => w.trim()).filter(Boolean).map((word) => ({ word, replacement: '' }))
    : [];
}

function addBannedWord(): void {
  simpleBannedWords.value.push({ word: '', replacement: '' });
}

function removeBannedWord(i: number): void {
  simpleBannedWords.value.splice(i, 1);
  syncBannedPromptFromWords();
}

/** 词条表 → banned_prompt 文本 */
function syncBannedPromptFromWords(): void {
  if (!injectDraft.value) return;
  const words = simpleBannedWords.value.map((b) => b.word.trim()).filter(Boolean);
  const prompt = words.length > 0
    ? `输出中禁止出现以下词语,若涉及请用含义相近、更得体的表达替换:${words.join('、')}`
    : '';
  if (prompt || !(injectDraft.value.simple.banned_prompt ?? '').trim()) {
    injectDraft.value.simple.banned_prompt = prompt;
  }
}
</script>

<template>
  <!-- embedded 模式:仅渲染内容区,供 SettingsHub 嵌入 -->
  <div v-if="props.embedded" class="sv-settings-embedded">
    <!-- API 设置 -->
    <div v-show="props.activeSection === 'api'" class="sv-field">
      <div class="sv-field-label"><span class="sv-supreme red" /> API 设置</div>
      <div class="sv-stack">
        <div class="sv-inp-row">
          <label class="sv-inp-tag">BASE URL</label>
          <input
            v-model="apiBaseUrl"
            type="text"
            class="sv-input"
            placeholder="https://api.openai.com/v1"
            spellcheck="false"
            @blur="onBaseUrlBlur"
          />
        </div>
        <div class="sv-inp-row">
          <label class="sv-inp-tag">API KEY</label>
          <input
            v-model="apiKey"
            type="password"
            class="sv-input"
            :placeholder="hasApiKey ? `已配置 ${apiKeyMasked} · 留空保持现有` : '粘贴 API Key'"
            autocomplete="off"
            spellcheck="false"
          />
        </div>

        <!-- 模型选择 + 从 API 加载 -->
        <div class="sv-inp-row">
          <label class="sv-inp-tag">MODEL</label>
          <select
            class="sv-select"
            :value="model || connInfo?.model || ''"
            :disabled="switchingModel"
            @change="onModelChange"
          >
            <option value="" disabled>— 选择模型 —</option>
            <option v-for="m in [...new Set([model, ...models])].filter(Boolean)" :key="m" :value="m">
              {{ m }}
            </option>
          </select>
        </div>

        <div class="sv-btn-row">
          <button class="sv-btn primary sv-btn-fill" :disabled="savingApi" @click="saveApi">
            {{ savingApi ? '保存中...' : '保存 API 设置' }}
          </button>
          <button class="sv-btn ghost sv-btn-fill" :disabled="refreshing" @click="refreshModelList">
            {{ refreshing ? '加载中...' : '从 API 加载模型列表' }}
          </button>
        </div>
        <p class="sv-note">
          保存后立即生效并写入 data/settings.json;Key 仅存本地服务端,不回显明文。地址会自动补全
          协议与 /v1。
        </p>
      </div>
      <div v-if="apiFeedback" class="sv-feedback" :class="apiFeedback.kind">{{ apiFeedback.text }}</div>
    </div>

    <!-- 后端连接 -->
    <div v-show="props.activeSection === 'api'" class="sv-field">
      <div class="sv-field-label"><span class="sv-supreme orange" /> 后端连接</div>
      <div class="sv-stack">
        <div class="sv-inp-row">
          <span class="sv-inp-tag">CONN</span>
          <code class="sv-code">{{ connInfo?.connector ?? '...' }}</code>
        </div>
        <button class="sv-btn ghost sv-btn-fill" :disabled="connecting" @click="doConnect">
          {{ connecting ? '连接测试中...' : '测试连接' }}
        </button>
        <div v-if="models.length" class="sv-count">可用 {{ models.length }} 个模型</div>
      </div>
      <div v-if="modelFeedback" class="sv-feedback info">{{ modelFeedback }}</div>
      <div v-if="connFeedback" class="sv-feedback" :class="connFeedback.kind">{{ connFeedback.text }}</div>
    </div>

    <!-- 生成参数 -->
    <div v-show="props.activeSection === 'model'" class="sv-field">
      <div class="sv-field-label"><span class="sv-supreme pink" /> 生成参数</div>
      <div class="sv-stack">
        <div>
          <div class="sv-range-label">
            随机性(Temperature) <span class="sv-range-val">{{ tempLabel }}</span>
          </div>
          <div class="sv-range-row">
            <input v-model.number="temperature" type="range" min="0" max="2" step="0.05" class="sv-range" />
            <output>{{ temperature.toFixed(2) }}</output>
          </div>
          <div class="sv-range-hints"><span>稳定 · 严谨</span><span>多样 · 创意</span></div>
        </div>
        <div>
          <div class="sv-range-label">
            核采样(Top-P) <span class="sv-range-val">{{ topPLabel }}</span>
          </div>
          <div class="sv-range-row">
            <input v-model.number="topP" type="range" min="0" max="1" step="0.01" class="sv-range" />
            <output>{{ topP.toFixed(2) }}</output>
          </div>
          <div class="sv-range-hints"><span>严格 · 确定</span><span>宽松 · 多样</span></div>
        </div>
        <div>
          <div class="sv-range-label">最大生成长度</div>
          <div class="sv-range-row">
            <input v-model.number="maxTokens" type="range" min="128" max="10000" step="1" class="sv-range" />
            <output>{{ maxTokens }}</output>
          </div>
        </div>
        <div>
          <div class="sv-range-label">
            最大上下文窗口(Token) <span class="sv-range-val">{{ ctxLabel }}</span>
          </div>
          <div class="sv-range-row">
            <input
              v-model.number="maxContextTokens"
              type="range"
              min="65536"
              max="1048576"
              step="1024"
              class="sv-range"
            />
            <output>{{ maxContextTokens.toLocaleString() }}</output>
          </div>
          <div class="sv-range-hints"><span>64k · 最低</span><span>1M · 上限</span></div>
          <p class="sv-note">超出上限时按时间裁剪最旧的历史消息(角色设定始终保留)。</p>
        </div>
        <div class="sv-inp-row">
          <label class="sv-inp-tag">工具轮次上限</label>
          <input
            v-model.number="maxToolRounds"
            type="number"
            min="1"
            max="200"
            step="1"
            class="sv-input inject-num"
            title="AGENT/CUSTOM 模式工具循环轮次上限(每轮可执行多个工具调用;默认 32)"
          />
          <span class="sv-note">AGENT/CUSTOM 工具循环轮次上限(1-200,默认 32)</span>
        </div>
        <div class="sv-btn-row">
          <button class="sv-btn ghost sv-btn-fill" :disabled="saveParams" @click="saveParamsNow">
            {{ saveParams ? '保存中...' : '保存为默认参数' }}
          </button>
          <div v-if="paramsMsg" class="sv-feedback ok sv-feedback-flex">{{ paramsMsg }}</div>
        </div>
      </div>
    </div>

    <!-- Agent 设置 -->
    <div v-show="props.activeSection === 'agent'" class="sv-field">
      <div class="sv-field-label"><span class="sv-supreme blue" /> Agent</div>
      <div class="sv-stack">
        <div class="sv-field-label sub">系统提示词(Agent 模式)</div>
        <textarea
          v-model="agentSystemPrompt"
          rows="6"
          class="sv-input"
          placeholder="留空使用内置默认角色扮演提示词。支持占位符:{{character_name}} {{character_description}} {{world_info}}"
          spellcheck="false"
        />
        <p class="sv-note">
          <b>Custom Prompt 会完整替换内置模板，不是追加。</b>占位符替换为当前角色与已命中的世界书内容。不写
          <code v-pre>{{world_info}}</code> 时世界书不会自动注入。可用工具清单与「何时调用」由引擎在
          Agent 模式下动态注入到系统提示词末尾,无需在此手写工具说明。
        </p>
        <div class="sv-inp-row">
          <label class="sv-inp-tag">搜索端点</label>
          <input
            v-model="searchEndpoint"
            type="text"
            class="sv-input"
            placeholder="https://html.duckduckgo.com/html/(留空 = 默认 DuckDuckGo)"
            spellcheck="false"
          />
        </div>
        <p class="sv-note">search 工具联网搜索使用的端点;自定义服务需返回相似 HTML 结构。</p>
        <div class="sv-inp-row">
          <label class="sv-inp-tag">变量状态注入位置</label>
          <select v-model="mvuVarsPosition" class="sv-input">
            <option value="system">system 提示词(默认,兼容)</option>
            <option value="user_tail">最新用户消息尾部(缓存友好)</option>
          </select>
        </div>
        <p class="sv-note">
          世界书/变量状态(<code v-pre>{{format_message_variable}}</code>)的注入位置。「最新用户消息尾部」模式把
          变量块移出 system 提示词,变量更新不再使 system + 早期历史的前缀缓存整体失效,可显著提升
          DeepSeek / Anthropic / OpenAI 等提供商的 prompt caching 命中率;需角色卡把
          <code v-pre>{{format_message_variable}}</code> 写在常驻世界书条目中才生效。
        </p>
        <div class="sv-inp-row" style="align-items: flex-start">
          <label class="sv-inp-tag" style="padding-top: 8px">反思提示词</label>
          <textarea
            v-model="reflectPrompt"
            rows="3"
            class="sv-input"
            placeholder="留空 = 内置规则检查;填写后反思步骤调用一次 LLM 判定输出 PASS / FAIL"
            spellcheck="false"
          />
        </div>
        <p class="sv-note">
          深度 / Agent 模式的反思步骤:留空时用内置规则(空输出、截断、未答疑问)检查;填写后改为调用模型,按本提示词检查
          草稿是否满足人设、字数、格式等要求,判定输出须以 <code>PASS</code> 或 <code>FAIL</code> 开头,失败自动重新生成
          (最多 3 次)。模型输出无法解析时自动回退内置规则,不影响流程。
        </p>
        <div class="sv-btn-row">
          <button class="sv-btn primary sv-btn-fill" :disabled="agentSaving" @click="saveAgentNow">
            {{ agentSaving ? '保存中...' : '保存 Agent 设置' }}
          </button>
          <button class="sv-btn ghost sv-btn-fill" :disabled="agentSaving" @click="resetAgentPrompt">
            恢复默认提示词
          </button>
          <div v-if="agentMsg" class="sv-feedback ok sv-feedback-flex">{{ agentMsg }}</div>
        </div>

        <div style="margin-top: 16px; border-top: 1px solid var(--sv-line); padding-top: 12px">
          <div class="sv-field-label sub">主 Agent 提示词(运行时,注入模型)</div>
          <textarea
            v-model="agentPromptMd"
            rows="14"
            class="sv-input sv-md-area"
            placeholder="加载中..."
            spellcheck="false"
          />
          <p class="sv-note">
            运行时主提示词保存在 DATA_DIR/AGENTS_RUNTIME.md，保存后下次会话生效；
            内容会注入到模型 system 消息开头(角色定位 / 创作原则 / 工具使用原则 / 输出纪律)。
            项目根 AGENTS.md 仅供开发 agent 使用，不在此编辑，也绝不会注入运行时模型。
          </p>
          <div class="sv-btn-row">
            <button class="sv-btn primary sv-btn-fill" :disabled="mdSaving" @click="saveAgentPromptMd">
              {{ mdSaving ? '保存中...' : '保存主 Agent 提示词' }}
            </button>
            <button class="sv-btn ghost sv-btn-fill" :disabled="mdLoading" @click="loadAgentPromptMd">
              {{ mdLoading ? '加载中...' : '重新加载' }}
            </button>
            <div
              v-if="mdMsg"
              class="sv-feedback-flex"
              :class="mdMsg.kind === 'ok' ? 'ok' : 'err'"
            >{{ mdMsg.text }}</div>
          </div>
        </div>

        <div style="margin-top: 16px; border-top: 1px solid var(--sv-line); padding-top: 12px">
          <div class="sv-field-label sub">最终提示词预览（脱敏）</div>
          <p class="sv-note">按 source / role / layer / order 展示。历史仅显示角色、长度与哈希，不返回聊天正文或 API Key。</p>
          <button class="sv-btn ghost sv-btn-fill" :disabled="previewLoading" @click="loadPromptPreview">
            {{ previewLoading ? '加载中...' : '刷新最终提示词预览' }}
          </button>
          <div v-if="previewError" class="sv-feedback err">{{ previewError }}</div>
          <div v-if="promptPreview" class="sv-stack" style="margin-top: 10px">
            <div v-for="layer in promptPreview.layers" :key="`${layer.order}-${layer.source}`" class="sv-data-row">
              <div class="info">
                <b>#{{ layer.order }} · L{{ layer.layer }} · {{ layer.role }} · {{ layer.source }}</b>
                <pre class="sv-code" style="white-space: pre-wrap; max-height: 180px; overflow: auto">{{ layer.content }}</pre>
              </div>
            </div>
            <p class="sv-note">{{ promptPreview.note }}</p>
          </div>
        </div>
      </div>
    </div>

    <!-- 自定义 Agent 执行流程(custom 模式) -->
    <div v-show="props.activeSection === 'flow'" class="sv-field">
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
          <button class="sv-btn ghost sv-btn-square" title="新建流程" @click="newFlow">+</button>
          <button class="sv-btn ghost sv-btn-square" title="复制当前流程" @click="duplicateFlow">⧉</button>
          <button class="sv-btn danger sv-btn-square" title="删除当前流程" @click="deleteFlowNow">✕</button>
        </div>
        <div class="sv-inp-row">
          <label class="sv-inp-tag">流程名称</label>
          <input v-model="flowName" type="text" class="sv-input" placeholder="流程名称(保存时生效)" spellcheck="false" />
          <label class="sv-inp-tag" style="padding-top: 8px">说明</label>
          <input v-model="flowDesc" type="text" class="sv-input" placeholder="流程说明(可选)" spellcheck="false" />
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
                <input v-model.number="step.max_tokens" type="number" min="1" max="32768" class="sv-input inject-num" placeholder="沿用全局" />
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

    <!-- 提示词注入(简单模式 + 楼层系统) -->
    <div v-show="props.activeSection === 'prompt'" class="sv-field">
      <div class="sv-field-label"><span class="sv-supreme orange" /> 提示词注入</div>
      <div class="sv-stack">
        <div class="sv-inp-row">
          <label class="sv-inp-tag">模式</label>
          <div v-if="injectDraft" class="sv-mode">
            <button :class="{ active: injectDraft.mode === 'simple' }" @click="injectDraft.mode = 'simple'">
              简单
            </button>
            <button :class="{ active: injectDraft.mode === 'complex' }" @click="injectDraft.mode = 'complex'">
              复杂
            </button>
          </div>
          <span v-else class="sv-note">加载中...</span>
        </div>

        <!-- 禁词库:输出含禁用词时注入此提示词(所有模式);deep/agent/custom 另由引擎收尾工具同义替换 -->
        <div v-if="injectDraft" class="sv-inp-row" style="align-items: flex-start">
          <label class="sv-inp-tag">禁词库</label>
          <button
            class="sv-btn ghost"
            :class="{ 'sv-btn-on': injectDraft.simple.banned_words_enabled }"
            @click="injectDraft.simple.banned_words_enabled = !injectDraft.simple.banned_words_enabled"
          >
            {{ injectDraft.simple.banned_words_enabled ? '已启用' : '已关闭' }}
          </button>
          <textarea
            v-model="injectDraft.simple.banned_prompt"
            rows="2"
            class="sv-input"
            placeholder="输出中禁止出现以下词语,如:笨蛋、脏话、滚"
            :disabled="!injectDraft.simple.banned_words_enabled"
            spellcheck="false"
          />
        </div>
        <p v-if="injectDraft" class="sv-note">输出含禁用词时注入此提示词,对所有模式生效;deep/agent/custom 模式另由引擎收尾做同义替换。</p>

        <!-- 简单模式:字数/转述/对话/视角 -->
        <template v-if="injectDraft?.mode === 'simple'">
          <div class="sv-inp-row">
            <label class="sv-inp-tag">字数</label>
            <button
              class="sv-btn ghost"
              :class="{ 'sv-btn-on': injectDraft.simple.word_count_enabled }"
              @click="injectDraft.simple.word_count_enabled = !injectDraft.simple.word_count_enabled"
            >
              {{ injectDraft.simple.word_count_enabled ? '已启用' : '已关闭' }}
            </button>
            <input
              v-model.number="injectDraft.simple.word_count"
              type="number"
              min="1"
              max="100000"
              class="sv-input inject-num"
              :disabled="!injectDraft.simple.word_count_enabled"
            />
          </div>
          <div class="sv-inp-row">
            <label class="sv-inp-tag">转述</label>
            <button
              class="sv-btn ghost"
              :class="{ 'sv-btn-on': injectDraft.simple.paraphrase_enabled }"
              @click="injectDraft.simple.paraphrase_enabled = !injectDraft.simple.paraphrase_enabled"
            >
              {{ injectDraft.simple.paraphrase_enabled ? '已启用' : '已关闭' }}
            </button>
            <span class="sv-note">用自己的话复述内容,不直接照抄原文</span>
          </div>
          <div class="sv-inp-row">
            <label class="sv-inp-tag">对话</label>
            <button
              class="sv-btn ghost"
              :class="{ 'sv-btn-on': injectDraft.simple.dialogue_enabled }"
              @click="injectDraft.simple.dialogue_enabled = !injectDraft.simple.dialogue_enabled"
            >
              {{ injectDraft.simple.dialogue_enabled ? '已启用' : '已关闭' }}
            </button>
            <span class="sv-note">只输出台词与必要动作描写,不输出旁白段落</span>
          </div>
          <div class="sv-inp-row">
            <label class="sv-inp-tag">视角</label>
            <button
              class="sv-btn ghost"
              :class="{ 'sv-btn-on': injectDraft.simple.perspective_enabled }"
              @click="injectDraft.simple.perspective_enabled = !injectDraft.simple.perspective_enabled"
            >
              {{ injectDraft.simple.perspective_enabled ? '已启用' : '已关闭' }}
            </button>
            <select
              v-model="injectDraft.simple.perspective"
              class="sv-select"
              :disabled="!injectDraft.simple.perspective_enabled"
            >
              <option>第一人称</option>
              <option>第二人称</option>
              <option>第三人称</option>
            </select>
          </div>
          <p class="sv-note">
            启用的项目会合成一条注入提示词拼入系统提示词末尾,对所有会话生效;支持宏(如
            <code v-pre>{{time}}</code>、<code v-pre>{{random:1,100}}</code>)。
          </p>
        </template>

        <!-- 复杂模式:楼层系统(仿 SillyTavern Prompt Manager) -->
        <template v-else-if="injectDraft">
          <p class="sv-note">
            楼层按从上到下的顺序注入:可拖拽排序,自由选择消息角色与位置,内容支持完整酒馆宏。
            <code v-pre>{{setvar::键::值}}</code> 会持久化到会话变量,后续楼层可
            <code v-pre>{{getvar::键}}</code> 读取。
          </p>
          <div v-if="!injectDraft.floors.length" class="sv-note inject-empty">尚未添加楼层,点击下方「新增楼层」。</div>
          <div
            v-for="floor in injectDraft.floors"
            :key="floor.id"
            class="floor-row"
            :class="{ dragging: dragFloorId === floor.id }"
            draggable="true"
            @dragstart="onDragStart($event, floor.id)"
            @dragover="onDragOver($event, floor.id)"
            @drop="onDrop($event, floor.id)"
            @dragend="onDragEnd"
          >
            <span class="floor-grip" title="拖拽排序">⋮⋮</span>
            <button
              class="sv-btn ghost"
              :class="{ 'sv-btn-on': floor.enabled }"
              :title="floor.enabled ? '点击停用' : '点击启用'"
              @click="floor.enabled = !floor.enabled"
            >
              {{ floor.enabled ? '开' : '关' }}
            </button>
            <input v-model="floor.name" type="text" class="sv-input floor-name" placeholder="楼层名称" spellcheck="false" />
            <select v-model="floor.role" class="sv-select floor-select" title="消息角色">
              <option v-for="(label, val) in FLOOR_ROLE_LABELS" :key="val" :value="val">{{ label }}</option>
            </select>
            <select v-model="floor.position" class="sv-select floor-select" title="注入位置">
              <option v-for="(label, val) in FLOOR_POS_LABELS" :key="val" :value="val">{{ label }}</option>
            </select>
            <input
              v-if="floor.position === 'depth'"
              v-model.number="floor.depth"
              type="number"
              min="0"
              class="sv-input floor-depth"
              title="从最新消息往前数第 N 条之后插入(0 = 最新消息后)"
            />
            <div class="floor-actions">
              <button class="sv-btn ghost sv-btn-square" title="上移" @click="moveFloor(floor.id, -1)">↑</button>
              <button class="sv-btn ghost sv-btn-square" title="下移" @click="moveFloor(floor.id, 1)">↓</button>
              <button
                class="sv-btn ghost sv-btn-square"
                :title="editingFloorId === floor.id ? '收起编辑' : '编辑内容'"
                @click="editingFloorId = editingFloorId === floor.id ? null : floor.id"
              >
                ✎
              </button>
              <button class="sv-btn danger sv-btn-square" title="删除楼层" @click="removeFloor(floor.id)">✕</button>
            </div>
            <textarea
              v-if="editingFloorId === floor.id"
              v-model="floor.content"
              rows="5"
              class="sv-input floor-content"
              placeholder="楼层内容,支持酒馆宏。例如:{{char}}内心提醒:{{setvar::场景::酒馆}}今晚在{{getvar::场景}}见面。"
              spellcheck="false"
            />
          </div>
          <div class="sv-btn-row">
            <button class="sv-btn ghost sv-btn-fill" @click="addFloor">+ 新增楼层</button>
          </div>
          <details class="macro-hint">
            <summary>酒馆宏速查(点击展开)</summary>
            <code class="sv-code">{{ MACRO_HINTS.join('  ·  ') }}</code>
          </details>
        </template>

        <div class="sv-btn-row">
          <button
            class="sv-btn ghost sv-btn-fill"
            :disabled="injectImporting"
            title="导入 SillyTavern 预设 JSON,替换当前全部楼层(保留各条目启用状态)"
            @click="injectImportInput?.click()"
          >
            {{ injectImporting ? '导入中...' : '导入酒馆预设(替换楼层)' }}
          </button>
          <button class="sv-btn ghost sv-btn-fill" :disabled="!injectDraft" @click="exportInjectConfig">
            导出注入配置(JSON)
          </button>
          <input
            ref="injectImportInput"
            type="file"
            accept=".json,application/json"
            class="hidden"
            @change="onImportPreset"
          />
        </div>
        <div class="sv-btn-row">
          <button class="sv-btn primary sv-btn-fill" :disabled="injectSaving || !injectDraft" @click="saveInjectNow">
            {{ injectSaving ? '保存中...' : '保存注入设置' }}
          </button>
          <div v-if="injectMsg" class="sv-feedback ok sv-feedback-flex">{{ injectMsg }}</div>
        </div>
      </div>
    </div>

    <!-- 预设导入/导出(SillyTavern 预设 ↔ 注入配置) -->
    <div v-show="props.activeSection === 'preset'" class="sv-field">
      <div class="sv-field-label"><span class="sv-supreme yellow" /> 预设导入 / 导出</div>
      <div class="sv-stack">
        <div class="sv-data-row">
          <div class="info">
            <b>导入酒馆预设</b>
            <span>导入 SillyTavern 预设 JSON(顶层 prompts + prompt_order),替换当前全部楼层并自动切换复杂模式</span>
          </div>
          <button class="sv-btn ghost" :disabled="injectImporting" @click="injectImportInput?.click()">
            {{ injectImporting ? '导入中...' : '导入' }}
          </button>
        </div>
        <div class="sv-data-row">
          <div class="info">
            <b>导出注入配置</b>
            <span>当前模式 + 简单项 + 全部楼层导出为 JSON,可在本机保存或共享</span>
          </div>
          <button class="sv-btn ghost" :disabled="!injectDraft" @click="exportInjectConfig">导出</button>
        </div>
        <input
          ref="injectImportInput"
          type="file"
          accept=".json,application/json"
          class="hidden"
          @change="onImportPreset"
        />
        <div v-if="injectMsg" class="sv-feedback" :class="injectMsg.startsWith('导入失败') ? 'err' : 'ok'">{{ injectMsg }}</div>
        <p class="sv-note">
          导入格式:SillyTavern 预设 JSON(含 <code>prompts</code> 数组与 <code>prompt_order</code>);
          导出的注入配置 JSON 亦可用「提示词注入 → 导入酒馆预设」回导。
        </p>
      </div>
    </div>

    <!-- 数据管理 -->
    <div v-show="props.activeSection === 'data'" class="sv-field">
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
      </div>
      <div v-if="importError" class="sv-feedback err">{{ importError }}</div>
      <div v-if="exportMsg" class="sv-feedback ok">{{ exportMsg }}</div>
      <div v-if="clearMsg" class="sv-feedback ok">{{ clearMsg }}</div>
      <input ref="importInput" type="file" accept=".json,application/json" class="hidden" @change="onImportFile" />
      <p class="sv-note">
        导入格式与 SillyTavern 兼容:<code>[{"role":"user","content":"..."}]</code>
      </p>
    </div>

    <!-- 界面 -->
    <div v-show="props.activeSection === 'ui'" class="sv-field">
      <div class="sv-field-label"><span class="sv-supreme yellow" /> 界面</div>
      <div class="sv-datalist">
        <div class="sv-data-row">
          <div class="info">
            <b>Agent 面板</b>
            <span>生成时自动展开,展示步骤、推理链与工具调用</span>
          </div>
          <button class="sv-btn ghost" @click="store.agentPanelOpen = !store.agentPanelOpen">
            {{ store.agentPanelOpen ? '展开中' : '已折叠' }}
          </button>
        </div>
        <div class="sv-data-row">
          <div class="info">
            <b>安全 HTML 渲染</b>
            <span>应用角色卡正则替换并清理 script/style 属性、事件处理器、javascript URL 与外部资源；不代表允许 JavaScript。开关按角色卡记忆,未设置的角色卡回退全局默认</span>
          </div>
          <button type="button" class="sv-btn ghost" :aria-pressed="store.renderHtml" @click="store.renderHtml = !store.renderHtml">
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
  </div>

  <!-- 独立模态框模式(原样) -->
  <div v-else class="sv-modal-mask" @click.self="close">
    <div class="sv-modal">
      <!-- 头部 -->
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" style="width: 18px; height: 18px" /> 设置
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-modal-body">
        <!-- API 设置(前端直接编辑) -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme red" /> API 设置</div>
          <div class="sv-stack">
            <div class="sv-inp-row">
              <label class="sv-inp-tag">BASE URL</label>
              <input
                v-model="apiBaseUrl"
                type="text"
                class="sv-input"
                placeholder="https://api.openai.com/v1"
                spellcheck="false"
                @blur="onBaseUrlBlur"
              />
            </div>
            <div class="sv-inp-row">
              <label class="sv-inp-tag">API KEY</label>
              <input
                v-model="apiKey"
                type="password"
                class="sv-input"
                :placeholder="hasApiKey ? `已配置 ${apiKeyMasked} · 留空保持现有` : '粘贴 API Key'"
                autocomplete="off"
                spellcheck="false"
              />
            </div>

            <!-- 模型选择 + 从 API 加载 -->
            <div class="sv-inp-row">
              <label class="sv-inp-tag">MODEL</label>
              <select
                class="sv-select"
                :value="model || connInfo?.model || ''"
                :disabled="switchingModel"
                @change="onModelChange"
              >
                <option value="" disabled>— 选择模型 —</option>
                <option v-for="m in [...new Set([model, ...models])].filter(Boolean)" :key="m" :value="m">
                  {{ m }}
                </option>
              </select>
            </div>

            <div class="sv-btn-row">
              <button class="sv-btn primary sv-btn-fill" :disabled="savingApi" @click="saveApi">
                {{ savingApi ? '保存中...' : '保存 API 设置' }}
              </button>
              <button class="sv-btn ghost sv-btn-fill" :disabled="refreshing" @click="refreshModelList">
                {{ refreshing ? '加载中...' : '从 API 加载模型列表' }}
              </button>
            </div>
            <p class="sv-note">
              保存后立即生效并写入 data/settings.json;Key 仅存本地服务端,不回显明文。地址会自动补全
              协议与 /v1。
            </p>
          </div>
          <div v-if="apiFeedback" class="sv-feedback" :class="apiFeedback.kind">{{ apiFeedback.text }}</div>
        </div>

        <!-- 后端连接 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme orange" /> 后端连接</div>
          <div class="sv-stack">
            <div class="sv-inp-row">
              <span class="sv-inp-tag">CONN</span>
              <code class="sv-code">{{ connInfo?.connector ?? '...' }}</code>
            </div>
            <button class="sv-btn ghost sv-btn-fill" :disabled="connecting" @click="doConnect">
              {{ connecting ? '连接测试中...' : '测试连接' }}
            </button>
            <div v-if="models.length" class="sv-count">可用 {{ models.length }} 个模型</div>
          </div>
          <div v-if="modelFeedback" class="sv-feedback info">{{ modelFeedback }}</div>
          <div v-if="connFeedback" class="sv-feedback" :class="connFeedback.kind">{{ connFeedback.text }}</div>
        </div>

        <!-- 生成参数 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme" /> 生成参数</div>
          <div class="sv-stack">
            <div>
              <div class="sv-range-label">
                随机性(Temperature) <span class="sv-range-val">{{ tempLabel }}</span>
              </div>
              <div class="sv-range-row">
                <input v-model.number="temperature" type="range" min="0" max="2" step="0.05" class="sv-range" />
                <output>{{ temperature.toFixed(2) }}</output>
              </div>
              <div class="sv-range-hints"><span>稳定 · 严谨</span><span>多样 · 创意</span></div>
            </div>
            <div>
              <div class="sv-range-label">
                核采样(Top-P) <span class="sv-range-val">{{ topPLabel }}</span>
              </div>
              <div class="sv-range-row">
                <input v-model.number="topP" type="range" min="0" max="1" step="0.01" class="sv-range" />
                <output>{{ topP.toFixed(2) }}</output>
              </div>
              <div class="sv-range-hints"><span>严格 · 确定</span><span>宽松 · 多样</span></div>
            </div>
            <div>
              <div class="sv-range-label">最大生成长度</div>
              <div class="sv-range-row">
                <input v-model.number="maxTokens" type="range" min="128" max="10000" step="1" class="sv-range" />
                <output>{{ maxTokens }}</output>
              </div>
            </div>
            <div>
              <div class="sv-range-label">
                最大上下文窗口(Token) <span class="sv-range-val">{{ ctxLabel }}</span>
              </div>
              <div class="sv-range-row">
                <input
                  v-model.number="maxContextTokens"
                  type="range"
                  min="65536"
                  max="1048576"
                  step="1024"
                  class="sv-range"
                />
                <output>{{ maxContextTokens.toLocaleString() }}</output>
              </div>
              <div class="sv-range-hints"><span>64k · 最低</span><span>1M · 上限</span></div>
              <p class="sv-note">超出上限时按时间裁剪最旧的历史消息(角色设定始终保留)。</p>
            </div>
            <div class="sv-inp-row">
              <label class="sv-inp-tag">工具轮次上限</label>
              <input
                v-model.number="maxToolRounds"
                type="number"
                min="1"
                max="200"
                step="1"
                class="sv-input inject-num"
                title="AGENT/CUSTOM 模式工具循环轮次上限(每轮可执行多个工具调用;默认 32)"
              />
              <span class="sv-note">AGENT/CUSTOM 工具循环轮次上限(1-200,默认 32)</span>
            </div>
            <div class="sv-inp-row">
              <label class="sv-inp-tag">压缩模式</label>
              <select v-model="compactionMode" class="sv-select" title="上下文压缩模式:off 不压缩 / manual 手动触发 / auto token 超阈值自动压缩">
                <option value="off">off · 不压缩</option>
                <option value="manual">manual · 手动触发</option>
                <option value="auto">auto · 自动压缩</option>
              </select>
              <span class="sv-note">压缩较早对话为摘要(原文保留可恢复);manual 需在优化面板手动触发</span>
            </div>
            <div>
              <div class="sv-range-label">压缩触发阈值 <span class="sv-range-val">{{ Math.round(compactionThreshold * 100) }}%</span></div>
              <div class="sv-range-row">
                <input
                  v-model.number="compactionThreshold"
                  type="range"
                  min="0.5"
                  max="0.95"
                  step="0.05"
                  class="sv-range"
                />
                <output>{{ Math.round(compactionThreshold * 100) }}%</output>
              </div>
              <div class="sv-range-hints"><span>50% · 更早压缩</span><span>95% · 更晚压缩</span></div>
              <p class="sv-note">仅 auto 模式生效:历史 token 达到该占比时自动压缩。</p>
            </div>
            <div class="sv-btn-row">
              <button class="sv-btn ghost sv-btn-fill" :disabled="saveParams" @click="saveParamsNow">
                {{ saveParams ? '保存中...' : '保存为默认参数' }}
              </button>
              <div v-if="paramsMsg" class="sv-feedback ok sv-feedback-flex">{{ paramsMsg }}</div>
            </div>
          </div>
        </div>

        <!-- Agent -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme blue" /> Agent</div>
          <div class="sv-stack">
            <div class="sv-field-label sub">系统提示词(Agent 模式)</div>
            <textarea
              v-model="agentSystemPrompt"
              rows="6"
              class="sv-input"
              placeholder="留空使用内置默认角色扮演提示词。支持占位符:{{character_name}} {{character_description}} {{world_info}}"
              spellcheck="false"
            />
            <p class="sv-note">
              仅在 Agent 模式使用;占位符替换为当前角色与已命中的世界书内容。不写
              <code v-pre>{{world_info}}</code> 时世界书不会自动注入。可用工具清单与「何时调用」由引擎在
              Agent 模式下动态注入到系统提示词末尾,无需在此手写工具说明。
            </p>
            <div class="sv-inp-row">
              <label class="sv-inp-tag">搜索端点</label>
              <input
                v-model="searchEndpoint"
                type="text"
                class="sv-input"
                placeholder="https://html.duckduckgo.com/html/(留空 = 默认 DuckDuckGo)"
                spellcheck="false"
              />
            </div>
            <p class="sv-note">search 工具联网搜索使用的端点;自定义服务需返回相似 HTML 结构。</p>
            <div class="sv-inp-row">
              <label class="sv-inp-tag">变量状态注入位置</label>
              <select v-model="mvuVarsPosition" class="sv-input">
                <option value="system">system 提示词(默认,兼容)</option>
                <option value="user_tail">最新用户消息尾部(缓存友好)</option>
              </select>
            </div>
            <p class="sv-note">
              世界书/变量状态(<code v-pre>{{format_message_variable}}</code>)的注入位置。「最新用户消息尾部」模式把
              变量块移出 system 提示词,变量更新不再使 system + 早期历史的前缀缓存整体失效,可显著提升
              DeepSeek / Anthropic / OpenAI 等提供商的 prompt caching 命中率;需角色卡把
              <code v-pre>{{format_message_variable}}</code> 写在常驻世界书条目中才生效。
            </p>
            <div class="sv-inp-row" style="align-items: flex-start">
              <label class="sv-inp-tag" style="padding-top: 8px">反思提示词</label>
              <textarea
                v-model="reflectPrompt"
                rows="3"
                class="sv-input"
                placeholder="留空 = 内置规则检查;填写后反思步骤调用一次 LLM 判定输出 PASS / FAIL"
                spellcheck="false"
              />
            </div>
            <p class="sv-note">
              深度 / Agent 模式的反思步骤:留空时用内置规则(空输出、截断、未答疑问)检查;填写后改为调用模型,按本提示词检查
              草稿是否满足人设、字数、格式等要求,判定输出须以 <code>PASS</code> 或 <code>FAIL</code> 开头,失败自动重新生成
              (最多 3 次)。模型输出无法解析时自动回退内置规则,不影响流程。
            </p>
            <div class="sv-btn-row">
              <button class="sv-btn primary sv-btn-fill" :disabled="agentSaving" @click="saveAgentNow">
                {{ agentSaving ? '保存中...' : '保存 Agent 设置' }}
              </button>
              <button class="sv-btn ghost sv-btn-fill" :disabled="agentSaving" @click="resetAgentPrompt">
                恢复默认提示词
              </button>
              <div v-if="agentMsg" class="sv-feedback ok sv-feedback-flex">{{ agentMsg }}</div>
            </div>

            <div style="margin-top: 16px; border-top: 1px solid var(--sv-line); padding-top: 12px">
              <div class="sv-field-label sub">主 Agent 提示词(运行时,注入模型)</div>
              <textarea
                v-model="agentPromptMd"
                rows="14"
                class="sv-input sv-md-area"
                placeholder="加载中..."
                spellcheck="false"
              />
              <p class="sv-note">
                项目级运行时主提示词(项目根 AGENTS_RUNTIME.md),保存后下次会话生效;
                内容会注入到模型 system 消息开头(角色定位 / 创作原则 / 工具使用原则 / 输出纪律)。
                开发 agent 用的项目文档为项目根 AGENTS.md(不在此编辑、不注入模型)。
              </p>
              <div class="sv-btn-row">
                <button class="sv-btn primary sv-btn-fill" :disabled="mdSaving" @click="saveAgentPromptMd">
                  {{ mdSaving ? '保存中...' : '保存主 Agent 提示词' }}
                </button>
                <button class="sv-btn ghost sv-btn-fill" :disabled="mdLoading" @click="loadAgentPromptMd">
                  {{ mdLoading ? '加载中...' : '重新加载' }}
                </button>
                <div
                  v-if="mdMsg"
                  class="sv-feedback-flex"
                  :class="mdMsg.kind === 'ok' ? 'ok' : 'err'"
                >{{ mdMsg.text }}</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 自定义 Agent 执行流程(custom 模式) -->
        <div class="sv-field">
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
              <button class="sv-btn ghost sv-btn-square" title="新建流程" @click="newFlow">+</button>
              <button class="sv-btn ghost sv-btn-square" title="复制当前流程" @click="duplicateFlow">⧉</button>
              <button class="sv-btn danger sv-btn-square" title="删除当前流程" @click="deleteFlowNow">✕</button>
            </div>
            <div class="sv-inp-row">
              <label class="sv-inp-tag">流程名称</label>
              <input v-model="flowName" type="text" class="sv-input" placeholder="流程名称(保存时生效)" spellcheck="false" />
              <label class="sv-inp-tag" style="padding-top: 8px">说明</label>
              <input v-model="flowDesc" type="text" class="sv-input" placeholder="流程说明(可选)" spellcheck="false" />
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
                  <div class="sv-inp-row">
                    <label class="sv-inp-tag">温度</label>
                    <input v-model.number="step.temperature" type="number" min="0" max="2" step="0.1" class="sv-input inject-num" placeholder="沿用全局" />
                    <label class="sv-inp-tag">输出上限</label>
                    <input v-model.number="step.max_tokens" type="number" min="1" max="32768" class="sv-input inject-num" placeholder="沿用全局" />
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

        <!-- 提示词注入(简单模式 + 楼层系统) -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme orange" /> 提示词注入</div>
          <div class="sv-stack">
            <div class="sv-inp-row">
              <label class="sv-inp-tag">模式</label>
              <div v-if="injectDraft" class="sv-mode">
                <button :class="{ active: injectDraft.mode === 'simple' }" @click="injectDraft.mode = 'simple'">
                  简单
                </button>
                <button :class="{ active: injectDraft.mode === 'complex' }" @click="injectDraft.mode = 'complex'">
                  复杂
                </button>
              </div>
              <span v-else class="sv-note">加载中...</span>
            </div>

            <!-- 禁词库:输出含禁词时注入自省提示词(所有模式);deep/agent/custom 另由引擎收尾工具同义替换 -->
            <div v-if="injectDraft" class="sv-inp-row" style="align-items: flex-start">
              <label class="sv-inp-tag">禁词库</label>
              <button
                class="sv-btn ghost"
                :class="{ 'sv-btn-on': injectDraft.simple.banned_words_enabled }"
                @click="injectDraft.simple.banned_words_enabled = !injectDraft.simple.banned_words_enabled"
              >
                {{ injectDraft.simple.banned_words_enabled ? '已启用' : '已关闭' }}
              </button>
              <div style="flex: 1; display: flex; flex-direction: column; gap: 6px; min-width: 0">
                <div v-for="(bw, i) in simpleBannedWords" :key="i" class="sv-inp-row" style="gap: 6px; width: 100%">
                  <input v-model="bw.word" type="text" class="sv-input" placeholder="禁词(精确匹配,如:笨蛋)" :disabled="!injectDraft.simple.banned_words_enabled" spellcheck="false" @input="syncBannedPromptFromWords" />
                  <span class="sv-note" style="flex: none">→</span>
                  <input v-model="bw.replacement" type="text" class="sv-input" placeholder="替换词(同义改写,如:傻瓜;留空=删除)" :disabled="!injectDraft.simple.banned_words_enabled" spellcheck="false" @input="syncBannedPromptFromWords" />
                  <button class="sv-btn danger sv-btn-square" title="删除词条" :disabled="!injectDraft.simple.banned_words_enabled" @click="removeBannedWord(i)">✕</button>
                </div>
                <button
                  class="sv-btn ghost sv-btn-sm"
                  style="align-self: flex-start"
                  :disabled="!injectDraft.simple.banned_words_enabled"
                  @click="addBannedWord"
                >＋ 新增禁词</button>
              </div>
            </div>
            <p v-if="injectDraft" class="sv-note">输出含禁词时注入自省提示词,对所有模式生效;deep/agent/custom 模式另由引擎收尾做同义替换。</p>

            <!-- 简单模式:字数/转述/对话/视角 -->
            <template v-if="injectDraft?.mode === 'simple'">
              <div class="sv-inp-row">
                <label class="sv-inp-tag">字数</label>
                <button
                  class="sv-btn ghost"
                  :class="{ 'sv-btn-on': injectDraft.simple.word_count_enabled }"
                  @click="injectDraft.simple.word_count_enabled = !injectDraft.simple.word_count_enabled"
                >
                  {{ injectDraft.simple.word_count_enabled ? '已启用' : '已关闭' }}
                </button>
                <input
                  v-model.number="injectDraft.simple.word_count"
                  type="number"
                  min="1"
                  max="100000"
                  class="sv-input inject-num"
                  :disabled="!injectDraft.simple.word_count_enabled"
                />
              </div>
              <div class="sv-inp-row">
                <label class="sv-inp-tag">转述</label>
                <button
                  class="sv-btn ghost"
                  :class="{ 'sv-btn-on': injectDraft.simple.paraphrase_enabled }"
                  @click="injectDraft.simple.paraphrase_enabled = !injectDraft.simple.paraphrase_enabled"
                >
                  {{ injectDraft.simple.paraphrase_enabled ? '已启用' : '已关闭' }}
                </button>
                <span class="sv-note">用自己的话复述内容,不直接照抄原文</span>
              </div>
              <div class="sv-inp-row">
                <label class="sv-inp-tag">对话</label>
                <button
                  class="sv-btn ghost"
                  :class="{ 'sv-btn-on': injectDraft.simple.dialogue_enabled }"
                  @click="injectDraft.simple.dialogue_enabled = !injectDraft.simple.dialogue_enabled"
                >
                  {{ injectDraft.simple.dialogue_enabled ? '已启用' : '已关闭' }}
                </button>
                <span class="sv-note">只输出台词与必要动作描写,不输出旁白段落</span>
              </div>
              <div class="sv-inp-row">
                <label class="sv-inp-tag">视角</label>
                <button
                  class="sv-btn ghost"
                  :class="{ 'sv-btn-on': injectDraft.simple.perspective_enabled }"
                  @click="injectDraft.simple.perspective_enabled = !injectDraft.simple.perspective_enabled"
                >
                  {{ injectDraft.simple.perspective_enabled ? '已启用' : '已关闭' }}
                </button>
                <select
                  v-model="injectDraft.simple.perspective"
                  class="sv-select"
                  :disabled="!injectDraft.simple.perspective_enabled"
                >
                  <option>第一人称</option>
                  <option>第二人称</option>
                  <option>第三人称</option>
                </select>
              </div>
            </template>

            <!-- 复杂模式:楼层系统(仿 SillyTavern Prompt Manager) -->
            <template v-else-if="injectDraft">
              <p class="sv-note">
                楼层按从上到下的顺序注入:可拖拽排序,自由选择消息角色与位置,内容支持完整酒馆宏。
                <code v-pre>{{setvar::键::值}}</code> 会持久化到会话变量,后续楼层可
                <code v-pre>{{getvar::键}}</code> 读取。
              </p>
              <div v-if="!injectDraft.floors.length" class="sv-note inject-empty">尚未添加楼层,点击下方「新增楼层」。</div>
              <div
                v-for="floor in injectDraft.floors"
                :key="floor.id"
                class="floor-row"
                :class="{ dragging: dragFloorId === floor.id }"
                draggable="true"
                @dragstart="onDragStart($event, floor.id)"
                @dragover="onDragOver($event, floor.id)"
                @drop="onDrop($event, floor.id)"
                @dragend="onDragEnd"
              >
                <span class="floor-grip" title="拖拽排序">⋮⋮</span>
                <button
                  class="sv-btn ghost"
                  :class="{ 'sv-btn-on': floor.enabled }"
                  :title="floor.enabled ? '点击停用' : '点击启用'"
                  @click="floor.enabled = !floor.enabled"
                >
                  {{ floor.enabled ? '开' : '关' }}
                </button>
                <input v-model="floor.name" type="text" class="sv-input floor-name" placeholder="楼层名称" spellcheck="false" />
                <select v-model="floor.role" class="sv-select floor-select" title="消息角色">
                  <option v-for="(label, val) in FLOOR_ROLE_LABELS" :key="val" :value="val">{{ label }}</option>
                </select>
                <select v-model="floor.position" class="sv-select floor-select" title="注入位置">
                  <option v-for="(label, val) in FLOOR_POS_LABELS" :key="val" :value="val">{{ label }}</option>
                </select>
                <input
                  v-if="floor.position === 'depth'"
                  v-model.number="floor.depth"
                  type="number"
                  min="0"
                  class="sv-input floor-depth"
                  title="从最新消息往前数第 N 条之后插入(0 = 最新消息后)"
                />
                <div class="floor-actions">
                  <button class="sv-btn ghost sv-btn-square" title="上移" @click="moveFloor(floor.id, -1)">↑</button>
                  <button class="sv-btn ghost sv-btn-square" title="下移" @click="moveFloor(floor.id, 1)">↓</button>
                  <button
                    class="sv-btn ghost sv-btn-square"
                    :title="editingFloorId === floor.id ? '收起编辑' : '编辑内容'"
                    @click="editingFloorId = editingFloorId === floor.id ? null : floor.id"
                  >
                    ✎
                  </button>
                  <button class="sv-btn danger sv-btn-square" title="删除楼层" @click="removeFloor(floor.id)">✕</button>
                </div>
                <textarea
                  v-if="editingFloorId === floor.id"
                  v-model="floor.content"
                  rows="5"
                  class="sv-input floor-content"
                  placeholder="楼层内容,支持酒馆宏。例如:{{char}}内心提醒:{{setvar::场景::酒馆}}今晚在{{getvar::场景}}见面。"
                  spellcheck="false"
                />
              </div>
              <div class="sv-btn-row">
                <button class="sv-btn ghost sv-btn-fill" @click="addFloor">+ 新增楼层</button>
              </div>
              <details class="macro-hint">
                <summary>酒馆宏速查(点击展开)</summary>
                <code class="sv-code">{{ MACRO_HINTS.join('  ·  ') }}</code>
              </details>
            </template>

            <div class="sv-btn-row">
              <button
                class="sv-btn ghost sv-btn-fill"
                :disabled="injectImporting"
                title="导入 SillyTavern 预设 JSON,替换当前全部楼层(保留各条目启用状态)"
                @click="injectImportInput?.click()"
              >
                {{ injectImporting ? '导入中...' : '导入酒馆预设(替换楼层)' }}
              </button>
              <button class="sv-btn ghost sv-btn-fill" :disabled="!injectDraft" @click="exportInjectConfig">
                导出注入配置(JSON)
              </button>
              <input
                ref="injectImportInput"
                type="file"
                accept=".json,application/json"
                class="hidden"
                @change="onImportPreset"
              />
            </div>
            <div class="sv-btn-row">
              <button class="sv-btn primary sv-btn-fill" :disabled="injectSaving || !injectDraft" @click="saveInjectNow">
                {{ injectSaving ? '保存中...' : '保存注入设置' }}
              </button>
              <div v-if="injectMsg" class="sv-feedback ok sv-feedback-flex">{{ injectMsg }}</div>
            </div>
          </div>
        </div>

        <!-- 数据管理 -->
        <div class="sv-field">
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
          </div>
          <div v-if="importError" class="sv-feedback err">{{ importError }}</div>
          <div v-if="exportMsg" class="sv-feedback ok">{{ exportMsg }}</div>
          <div v-if="clearMsg" class="sv-feedback ok">{{ clearMsg }}</div>
          <input ref="importInput" type="file" accept=".json,application/json" class="hidden" @change="onImportFile" />
          <p class="sv-note">
            导入格式与 SillyTavern 兼容:<code>[{"role":"user","content":"..."}]</code>
          </p>
        </div>

        <!-- 界面 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme yellow" /> 界面</div>
          <div class="sv-datalist">
            <div class="sv-data-row">
              <div class="info">
                <b>Agent 面板</b>
                <span>生成时自动展开,展示步骤、推理链与工具调用</span>
              </div>
              <button class="sv-btn ghost" @click="store.agentPanelOpen = !store.agentPanelOpen">
                {{ store.agentPanelOpen ? '展开中' : '已折叠' }}
              </button>
            </div>
            <div class="sv-data-row">
              <div class="info">
                <b>安全 HTML 渲染</b>
                <span>角色卡替换 HTML 会经过白名单清理；此开关不授予 JavaScript 执行权限。开关按角色卡记忆,未设置的角色卡回退全局默认</span>
              </div>
              <button type="button" class="sv-btn ghost" :aria-pressed="store.renderHtml" @click="store.renderHtml = !store.renderHtml">
                {{ store.renderHtml ? '已开启' : '已关闭' }}
              </button>
            </div>
            <div class="sv-data-row" style="align-items: flex-start">
              <div class="info">
                <b>角色卡 JavaScript 授权</b>
                <span>默认禁用；按角色与当前脚本哈希授权，脚本变化自动失效。</span>
                <div v-if="scriptAuthorizations.length" style="display: grid; gap: 8px; margin-top: 10px">
                  <div v-for="grant in scriptAuthorizations" :key="grant.characterId" class="flex items-center gap-2">
                    <span>{{ characterLabel(grant.characterId) }}</span>
                    <button type="button" class="sv-btn ghost sv-btn-sm" @click="confirmRevokeScriptAuthorization(grant.characterId)">撤销</button>
                  </div>
                </div>
                <span v-else>暂无已授权角色卡。</span>
              </div>
            </div>
          </div>
        </div>
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>
