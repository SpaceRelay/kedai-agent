<script setup lang="ts">
// 底部输入区:多行文本域,Enter 发送 / Shift+Enter 换行;
// slash 联想(输入 / 开头时下拉补全命令)+ 快速回复(快捷填入常用消息);
// 最底部工具栏:模式切换 + 授权模式 + 模型选择 + 文件上传
import { ref, computed, onMounted, watch, nextTick } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import * as api from '../api';
import type { AgentMode } from '../api';
import { filterCommands, wordBeforeCursor } from './slashSuggest';

const store = useAppStore();
const { generating, currentCharacterId, agentMode, authorizationMode, model, models } = storeToRefs(store);

/** 授权三档:严格(读/写/删都需授权)/ 宽松(读/写放行,删需授权)/ 放行(仅系统路径写删需授权) */
const AUTH_MODES = [
  { key: 'strict' as const, label: '严格', title: '严格:读文件、写文件、删文件都需手动授权' },
  { key: 'loose' as const, label: '宽松', title: '宽松:读/写文件自动放行,删文件需授权' },
  { key: 'bypass' as const, label: '放行', title: '放行:仅写/删系统路径(如 C 盘)需授权,其余自动放行' },
];
const text = ref('');
const textareaRef = ref<HTMLTextAreaElement | null>(null);

/** 输入框自动增高:随内容撑开,上限 160px,超出后内部滚动;清空后回落 rows=2 初始高 */
function autoGrow(): void {
  const el = textareaRef.value;
  if (!el) return;
  el.style.height = 'auto';
  el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
}
// watch 覆盖全部 text 变更入口(手动输入、发送清空、快速回复填入、slash 补全)
watch(text, () => void nextTick(autoGrow));

/** 四档模式按钮(自定义模式需先在设置中启用执行流程) */
const MODES: Array<{ key: AgentMode; label: string; title: string }> = [
  { key: 'fast', label: 'FAST', title: '快速模式:单步直接生成' },
  { key: 'deep', label: 'DEEP', title: '深度模式:计划 → 执行 → 反思' },
  {
    key: 'agent',
    label: 'AGENT',
    title: 'Agent 模式:完整工具调用(read/write/replace/agentgo/search 等),模型可多轮自主调用工具',
  },
  { key: 'custom', label: 'CUSTOM', title: '自定义流程:按设置中的步骤序列执行(含步骤提示词/参数/工具)' },
];

/** 附件 */
const attachments = ref<File[]>([]);
const fileInput = ref<HTMLInputElement | null>(null);

function onFilePicked(e: Event): void {
  const input = e.target as HTMLInputElement;
  const files = input.files;
  input.value = '';
  if (!files?.length) return;
  for (const f of files) {
    if (f.size > 10 * 1024 * 1024) {
      alert(`文件「${f.name}」超过 10MB 限制`);
      continue;
    }
    attachments.value.push(f);
  }
}

function removeAttachment(index: number): void {
  attachments.value.splice(index, 1);
}

function isImage(file: File): boolean {
  return file.type.startsWith('image/');
}

function getFilePreview(file: File): string | null {
  if (isImage(file)) {
    return URL.createObjectURL(file);
  }
  return null;
}

/** 读取文件为 base64 或文本 */
function readFileContent(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as string);
    reader.onerror = reject;
    if (isImage(file)) {
      reader.readAsDataURL(file);
    } else {
      reader.readAsText(file);
    }
  });
}

async function send(): Promise<void> {
  const value = text.value.trim();
  if ((!value && attachments.value.length === 0) || generating.value) return;

  // 构建消息内容(含附件)
  let content = value;
  if (attachments.value.length) {
    const parts: string[] = [];
    for (const f of attachments.value) {
      const data = await readFileContent(f);
      if (isImage(f)) {
        parts.push(`[图片: ${f.name}]\n${data}`);
      } else {
        parts.push(`[文件: ${f.name}]\n${data}`);
      }
    }
    content = content ? `${content}\n\n${parts.join('\n\n')}` : parts.join('\n\n');
  }

  text.value = '';
  attachments.value = [];
  await store.sendMessage(content);
}

function onKeydown(e: KeyboardEvent): void {
  // slash 联想菜单打开:↑↓ 导航、Enter/Tab 补全、Esc 关闭(补全命中前不触发发送)
  if (slashMenuOpen.value) {
    const list = slashFiltered.value;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      slashIndex.value = (slashIndex.value + 1) % list.length;
      return;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      slashIndex.value = (slashIndex.value - 1 + list.length) % list.length;
      return;
    }
    if (e.key === 'Escape') {
      slashMenuOpen.value = false;
      return;
    }
    if (e.key === 'Enter' || e.key === 'Tab') {
      const hit = list[slashIndex.value];
      if (hit) {
        e.preventDefault();
        applySlash(hit.name);
        return;
      }
    }
  }
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    void send();
  }
}

// ===== 快速回复(阶段四 4b):快捷填入常用消息 =====
const quickMenuOpen = ref(false);
const quickReplies = ref<api.QuickReplyRecord[]>([]);
/** 快捷列表:仅启用且内容非空的条目 */
const enabledQuickReplies = computed(() => quickReplies.value.filter((r) => r.enabled && r.content.trim()));

function toggleQuickMenu(): void {
  quickMenuOpen.value = !quickMenuOpen.value;
}

/** 点击条目:填入输入框(空则替换,非空则追加换行);不自动发送,用户确认后 Enter 发送 */
function fillQuickReply(r: api.QuickReplyRecord): void {
  const content = r.content.trim();
  text.value = text.value.trim() ? `${text.value}\n\n${content}` : content;
  quickMenuOpen.value = false;
  textareaRef.value?.focus();
}

// ===== slash 输入联想(阶段四 4c):/ 开头下拉补全命令 =====
const slashCommands = ref<api.SlashCommandMeta[]>([]);
const slashMenuOpen = ref(false);
const slashFiltered = ref<api.SlashCommandMeta[]>([]);
const slashIndex = ref(0);

/** 输入时按光标前单词更新联想菜单(仅当该词以 / 开头且有匹配时打开) */
function updateSlashMenu(): void {
  const cursor = textareaRef.value?.selectionStart ?? text.value.length;
  const word = wordBeforeCursor(text.value, cursor);
  if (word.startsWith('/')) {
    const filtered = filterCommands(word.slice(1), slashCommands.value);
    if (filtered.length) {
      slashFiltered.value = filtered;
      slashIndex.value = 0;
      slashMenuOpen.value = true;
      return;
    }
  }
  slashMenuOpen.value = false;
}

function closeSlashMenu(): void {
  slashMenuOpen.value = false;
}

/** 补全:用 /命令名 替换光标前单词,光标移到补全后 */
function applySlash(name: string): void {
  const cursor = textareaRef.value?.selectionStart ?? text.value.length;
  const prefix = text.value.slice(0, cursor).replace(/(\S+)$/, '');
  const tail = text.value.slice(cursor);
  text.value = `${prefix}/${name}${tail}`;
  slashMenuOpen.value = false;
  requestAnimationFrame(() => {
    const el = textareaRef.value;
    if (el) {
      el.focus();
      const pos = prefix.length + name.length + 1;
      el.setSelectionRange(pos, pos);
    }
  });
}

onMounted(() => {
  // 联想数据与快速回复列表:加载失败静默(输入功能不受影响)
  void api.listSlashCommands().then((cmds) => (slashCommands.value = cmds)).catch(() => {});
  void api.listQuickReplies().then((rs) => (quickReplies.value = rs)).catch(() => {});
});

/** 模型切换 */
const switchingModel = ref(false);
async function onModelChange(e: Event): Promise<void> {
  const value = (e.target as HTMLSelectElement).value;
  if (!value || value === model.value) return;
  switchingModel.value = true;
  try {
    await store.switchModel(value);
  } catch (err) {
    alert(`切换模型失败:${(err as Error).message}`);
  } finally {
    switchingModel.value = false;
  }
}
</script>

<template>
  <footer class="sv-inputbar">
    <!-- 附件预览条 -->
    <div v-if="attachments.length" class="sv-attach-bar">
      <div v-for="(f, i) in attachments" :key="i" class="sv-attach-chip">
        <img v-if="getFilePreview(f)" :src="getFilePreview(f)!" alt="" />
        <span class="name">{{ f.name }}</span>
        <button class="remove" title="移除" @click="removeAttachment(i)">✕</button>
      </div>
    </div>

    <!-- 输入框 -->
    <div class="sv-inputbox">
      <textarea
        ref="textareaRef"
        v-model="text"
        rows="2"
        placeholder="输入消息...(Enter 发送,Shift+Enter 换行;/ 触发命令联想)"
        @input="updateSlashMenu"
        @blur="closeSlashMenu"
        @keydown="onKeydown"
      />

      <!-- slash 命令联想下拉(向上弹出) -->
      <div
        v-if="slashMenuOpen && slashFiltered.length"
        class="sv-slash-menu sv-popover"
      >
        <button
          v-for="(c, i) in slashFiltered"
          :key="c.name"
          class="sv-slash-item"
          :class="{ active: i === slashIndex }"
          title="Enter/Tab 补全,↑↓ 选择,Esc 关闭"
          @mousedown.prevent="applySlash(c.name)"
        >
          <b>/{{ c.name }}</b>
          <span class="sv-slash-desc">{{ c.description }}</span>
        </button>
      </div>

      <!-- 停止 / 发送 -->
      <button
        v-if="generating"
        class="sv-btn-send stop"
        title="停止生成"
        @click="store.stop()"
      >
        <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor" aria-hidden="true">
          <rect x="6" y="6" width="12" height="12" />
        </svg>
      </button>
      <button
        v-else
        class="sv-btn-send"
        :disabled="(!text.trim() && !attachments.length) || !currentCharacterId"
        title="发送"
        @click="send()"
      >
        <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor" aria-hidden="true">
          <polygon points="5,3 19,12 5,21 5,3" />
        </svg>
      </button>
    </div>

    <!-- 最底部工具栏 -->
    <div class="sv-input-toolbar">
      <!-- 模式选择(最左) -->
      <div class="sv-mode">
        <button
          v-for="(m, i) in MODES"
          :key="m.key"
          :class="{ active: agentMode === m.key }"
          :title="m.title"
          @click="store.setAgentMode(m.key)"
        >
          <span class="sv-mode-idx">{{ (i + 1).toString().padStart(2, '0') }}</span>{{ m.label }}
        </button>
      </div>

      <!-- 授权模式(三档) -->
      <div class="sv-auth-mode">
        <span class="sv-auth-mode-label">授权</span>
        <div class="sv-auth-toggle">
          <button
            v-for="m in AUTH_MODES"
            :key="m.key"
            :class="{ active: authorizationMode === m.key }"
            :title="m.title"
            @click="store.setAuthorizationMode(m.key)"
          >
            {{ m.label }}
          </button>
        </div>
      </div>

      <!-- 模型选择 -->
      <select
        class="sv-select sv-model-select"
        :value="model"
        :disabled="switchingModel"
        @change="onModelChange"
      >
        <option value="" disabled>— 模型 —</option>
        <option v-for="m in [...new Set([model, ...models])].filter(Boolean)" :key="m" :value="m">{{ m }}</option>
      </select>

      <!-- 上下文提示 -->
      <span class="sv-hint sv-tnum sv-ml-auto">CTX {{ store.contextTokens.toLocaleString() }} · {{ agentMode.toUpperCase() }}</span>

      <!-- 快速回复按钮(最右,选择文件前):弹出启用列表→点击填入输入框 -->
      <div class="sv-rel">
        <button class="sv-btn-attach" title="快速回复:选择常用回复填入输入框(不自动发送)" @click="toggleQuickMenu">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
          </svg>
          快速回复
        </button>
        <!-- 快速回复下拉(向上弹出) -->
        <div
          v-if="quickMenuOpen"
          class="sv-slash-menu sv-popover sv-popover-right"
          @mousedown.stop.prevent
        >
          <button
            v-for="r in enabledQuickReplies"
            :key="r.id"
            class="sv-slash-item"
            title="点击填入输入框"
            @mousedown.prevent="fillQuickReply(r)"
          >
            <b>{{ r.label || r.name }}</b>
            <span class="sv-slash-desc">{{ r.content }}</span>
          </button>
          <div v-if="enabledQuickReplies.length === 0" class="sv-slash-empty">
            <span>暂无启用的快速回复(可在「快速回复」管理中创建)</span>
          </div>
        </div>
      </div>

      <!-- 选择文件按钮(最右) -->
      <button class="sv-btn-attach" title="选择文件发送给模型" @click="fileInput?.click()">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
          <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48" />
        </svg>
        选择文件
      </button>
      <input
        ref="fileInput"
        type="file"
        multiple
        accept="image/*,.txt,.md,.json,.js,.ts,.py,.html,.css,.pdf,.doc,.docx"
        class="hidden"
        @change="onFilePicked"
      />
    </div>
  </footer>
</template>
