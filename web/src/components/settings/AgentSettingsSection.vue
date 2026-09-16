<script setup lang="ts">
// 设置区:Agent 设置(系统提示词/搜索端点/变量注入位置/反思提示词 + 主 Agent 提示词 + 最终提示词预览)。
// 从 SettingsModal.vue 双模板合并而来:取 embedded 超集版本(standalone 分支缺失
// 「最终提示词预览」块,属模板漂移,合并后 standalone 一并补上)。
import { computed, onMounted, watch } from 'vue';
import { useAppStore } from '../../store';
import { storeToRefs } from 'pinia';
import { useAgentSettings } from '../../composables/useAgentSettings';
import { useAgentPromptEditor } from '../../composables/useAgentPromptEditor';
import AuthorizationSection from './AuthorizationSection.vue';
import AndroidExecSection from './AndroidExecSection.vue';

withDefaults(defineProps<{
  /** 是否显示(embedded 模式按 activeSection 切换;standalone 恒 true) */
  show?: boolean;
}>(), {
  show: true,
});

const store = useAppStore();
// Agent 设置直接绑定 store(storeToRefs),与 useAgentSettings 保存逻辑读写同一 store;
// appMode 用于「系统提示词按模式独立存储」的 UI 标注(徽标/说明/按钮文案随模式即时切换)
const { agentSystemPrompt, searchEndpoint, mvuVarsPosition, reflectPrompt, taskPersonaFull, taskPromptInjectEnabled, appMode } = storeToRefs(store);

/** 模式徽标文案:标注当前编辑的是哪个模式的提示词,避免误以为两模式共用一份 */
const modeBadgeText = computed(() => (appMode.value === 'task' ? '任务模式专属' : '角色扮演专属'));
/** 占位符按模式区分空值回退语义:task 模式回退内置任务默认词,roleplay 回退内置角色扮演人设词 */
const promptPlaceholder = computed(() =>
  appMode.value === 'task'
    ? '留空使用内置默认任务提示词。'
    : '留空使用内置默认角色扮演提示词。支持占位符:{{character_name}} {{character_description}} {{world_info}}',
);
/** 「恢复默认」按钮文案:task 模式点明恢复的是任务模式默认词(与角色扮演默认词不同) */
const resetPromptLabel = computed(() => (appMode.value === 'task' ? '恢复任务模式默认提示词' : '恢复默认提示词'));

const { agentSaving, agentMsg, resetAgentPrompt, saveAgentNow } = useAgentSettings();

const {
  agentPromptMd, promptPreview, previewLoading, previewError, mdLoading, mdSaving, mdMsg,
  loadPromptPreview, loadAgentPromptMd, saveAgentPromptMd,
} = useAgentPromptEditor();

onMounted(async () => {
  // 加载 DATA_DIR 主 Agent 提示词与脱敏后的最终分层预览
  await loadAgentPromptMd();
  await loadPromptPreview();
});

// 预览按模式取 for_mode 合并值(task 追加三层固定提示词):切模式即重拉,与徽标/占位符同步
watch(appMode, () => void loadPromptPreview());
</script>

<template>
  <div v-show="show" class="sv-field">
    <div class="sv-field-label"><span class="sv-supreme blue" /> Agent</div>
    <div class="sv-stack">
      <!-- 模式徽标:标注当前编辑的是哪个模式的提示词(两模式独立存储),随 appMode 即时切换 -->
      <div class="sv-field-label sub">
        系统提示词(Agent 模式)
        <span class="sv-tag">{{ modeBadgeText }}</span>
      </div>
      <textarea
        v-model="agentSystemPrompt"
        rows="6"
        class="sv-input"
        :placeholder="promptPlaceholder"
        spellcheck="false"
      />
      <p class="sv-note">该提示词按模式独立存储,互不影响。</p>
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
      <div class="sv-inp-row">
        <label class="sv-inp-tag">执行者人设</label>
        <select v-model="taskPersonaFull" class="sv-input">
          <option :value="false">精简(默认)</option>
          <option :value="true">完整</option>
        </select>
      </div>
      <p class="sv-note">
        仅任务模式生效:任务执行者绑定角色卡时注入的「写作风格参考」人设段。<b>精简</b>只注入人设描述与人格两段(去掉
        情境与文风示例,显著省 token);<b>完整</b>为旧行为,注入角色卡全部四段。角色扮演模式下修改将作为任务模式未单独
        设置时的沿用值。
      </p>
      <div class="sv-inp-row">
        <label class="sv-inp-tag">任务继承提示词注入</label>
        <select v-model="taskPromptInjectEnabled" class="sv-input">
          <option :value="false">不继承(默认)</option>
          <option :value="true">继承</option>
        </select>
      </div>
      <p class="sv-note">
        仅任务模式生效:「提示词注入」配置(角色扮演的文章要求,如字数 / 视角 / 禁词)是否也注入任务执行、汇总与追加指令。
        <b>不继承(默认)</b>可避免角色扮演的写作要求与任务目标冲突(例如追加「压缩到 200 字」却因注入的「1200 字」要求
        反而变长);<b>继承</b>为旧行为,任务与角色扮演共用同一注入源。
      </p>
      <div class="sv-btn-row">
        <button class="sv-btn primary sv-btn-fill" :disabled="agentSaving" @click="saveAgentNow">
          {{ agentSaving ? '保存中...' : '保存 Agent 设置' }}
        </button>
        <button class="sv-btn ghost sv-btn-fill" :disabled="agentSaving" @click="resetAgentPrompt">
          {{ resetPromptLabel }}
        </button>
        <div v-if="agentMsg" class="sv-feedback ok sv-feedback-flex">{{ agentMsg }}</div>
      </div>

      <div class="sv-separator">
        <div class="sv-field-label sub">授权管理</div>
        <AuthorizationSection :show="show" />
      </div>

      <div class="sv-separator">
        <AndroidExecSection :show="show" />
      </div>

      <div class="sv-separator">
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

      <div class="sv-separator">
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
</template>
