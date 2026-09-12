<script setup lang="ts">
// 右侧 Agent 面板(合并壳:状态总览 + 时间线推理链 + 工具调用卡片(浅色主题)
//   + 「调用情况」tab——原独立 CallTracePanel 面板收编为内部 tab 内容组件,两模式共用)
// 角色扮演模式 = 聊天 Agent 推理链/工具调用;任务模式 = 当前任务的计划步骤/子任务执行
// 生成中(或任务执行中)自动展开,用户可手动收起/展开
import { computed, onUpdated, ref, type ComponentPublicInstance } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import { listUndoSnapshots, resolveToolAuthorization, restoreUndoSnapshot } from '../api';
import type { TaskSubtaskStatus, UndoSnapshot } from '../api';
import { taskStatusClass, taskStatusLabel } from '../taskStatus';
import CallTracePanel from './CallTracePanel.vue';

const store = useAppStore();
const { agent, generating, agentPanelOpen, callTraceOpen, currentSessionId, agentMode, lastUsage, appMode, currentTask, undoEnabled } = storeToRefs(store);

/**
 * 内部分区 tab(面板合并):agent = Agent 状态(本组件原内容) / trace = 调用情况(CallTracePanel)。
 * tab 记忆复用既有 uiPrefs.callTraceOpen(localStorage 键 kedai.call-trace-open.v1 不变,
 * 旧用户「调用情况面板开着」的偏好升级后落在「调用情况」tab,语义连续);
 * 切 tab 即写回该键,下次打开面板落在同 tab。
 */
const activeTab = computed<'agent' | 'trace'>({
  get: () => (callTraceOpen.value ? 'trace' : 'agent'),
  set: (v) => { store.callTraceOpen = v === 'trace'; },
});
const authorizing = ref<string | null>(null);
/** 授权操作错误/成功反馈(就近显示在授权卡片下方) */
const authMsg = ref<string>('');
/** 已授权的工具调用(callId ?? name):授权成功后卡片转「已授权」态,不再重复请求 */
const grantedCalls = ref<Set<string>>(new Set());
/** 工具入参/出参折叠卡展开态(记录 key → 是否展开) */
const openIos = ref<Set<string>>(new Set());
function isIoOpen(key: string): boolean {
  return openIos.value.has(key);
}

/** 折叠卡(.sv-tool-io)模板 ref 登记:key → 元素;卸载时(Vue 以 null 回调)移除 */
const ioBoxEls = new Map<string, HTMLElement>();
function registerIoBox(key: string, el: Element | ComponentPublicInstance | null): void {
  if (el instanceof HTMLElement) ioBoxEls.set(key, el);
  else ioBoxEls.delete(key);
}

/** 对单张折叠卡求值 has-scroll(内容溢出 → 底部渐隐遮罩);只读本组件登记元素,不再全局选择器 */
function updateIoScrollShadow(key: string): void {
  const box = ioBoxEls.get(key);
  const body = box?.querySelector<HTMLElement>('.sv-tool-io-body');
  if (box && body) box.classList.toggle('has-scroll', body.scrollHeight > body.clientHeight);
}

/**
 * 合并到下一帧重估所有展开卡片的 has-scroll:
 * 展开/收起(toggleIo)与卡片内容流式变化(onUpdated)后都需重估;rAF 合帧避免逐次 layout 抖动。
 */
let ioScrollCheckScheduled = false;
function scheduleIoScrollRefresh(): void {
  if (ioScrollCheckScheduled) return;
  ioScrollCheckScheduled = true;
  requestAnimationFrame(() => {
    ioScrollCheckScheduled = false;
    for (const key of openIos.value) updateIoScrollShadow(key);
  });
}
onUpdated(scheduleIoScrollRefresh);

function toggleIo(key: string): void {
  const next = new Set(openIos.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  openIos.value = next;
  // 展开渲染完成后重估溢出标记(驱动底部渐隐遮罩)
  scheduleIoScrollRefresh();
}

/** 折叠卡滚到底时移除渐隐遮罩(提示后面还有内容才显示) */
function ioScroll(e: Event): void {
  const el = e.target as HTMLElement;
  const box = el.closest<HTMLElement>('.sv-tool-io');
  if (!box) return;
  box.classList.toggle('has-scroll', el.scrollTop + el.clientHeight < el.scrollHeight - 4);
}

/** 工具入参/出参序列化(展示与复制共用同一份文本) */
function ioText(v: unknown): string {
  return JSON.stringify(v, null, 2);
}

/** 最近一次复制成功的折叠卡 key(短暂显示「已复制」) */
const copiedKey = ref<string | null>(null);
let copiedTimer: ReturnType<typeof setTimeout> | null = null;
async function copyIo(key: string, text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // 非安全上下文兜底:临时 textarea + execCommand
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    document.execCommand('copy');
    ta.remove();
  }
  copiedKey.value = key;
  if (copiedTimer) clearTimeout(copiedTimer);
  copiedTimer = setTimeout(() => { copiedKey.value = null; }, 1200);
}

/** 工具调用唯一键:优先 callId,退化用 name+id 组合避免同名并发调用混淆 */
function callKey(t: { callId?: string; name: string; id?: number }): string {
  return t.callId ?? `${t.name}#${t.id ?? 0}`;
}

// ===== 回退快照(undo,批次 6.1b)=====
/** 会产生回退快照的写工具(与后端快照生成口径一致) */
const UNDO_WRITE_TOOLS = new Set(['write', 'replace', 'create', 'update_variables', 'memory_write']);
/** 回退进行中的工具调用键(防重复点击) */
const undoBusy = ref<Set<string>>(new Set());
/** 已回退完成的工具调用键(按钮转「已回退」禁用态) */
const undoneKeys = ref<Set<string>>(new Set());
/** 回退反馈(就近显示在工具调用清单底部,口径同 authMsg) */
const undoMsg = ref('');

function isUndoableTool(name: string): boolean {
  return UNDO_WRITE_TOOLS.has(name);
}

/**
 * 快照匹配(近似语义,与后端无一一对应 id,只能启发式):
 * 1) 优先在与本次点击同 tool_name 的快照中选;同名没有则退到全部快照;
 * 2) 列表新→旧,取第一个 anchor_message_id 不大于当前历史最大消息 id 的
 *    (锚点仍落在当前上下文内的最近一条);
 * 3) 无法可靠对应(锚点全缺或全部超出当前上下文)时取最新一条。
 */
function pickUndoSnapshot(snapshots: UndoSnapshot[], toolName: string, maxMessageId: number): UndoSnapshot | undefined {
  const sameTool = snapshots.filter((s) => s.tool_name === toolName);
  const candidates = sameTool.length > 0 ? sameTool : snapshots;
  return candidates.find((s) => s.anchor_message_id !== null && s.anchor_message_id <= maxMessageId) ?? candidates[0];
}

/** 「回退到此处」:选快照 → 恢复 → 刷新聊天历史;按钮转「已回退」禁用态 */
async function undoToHere(t: { callId?: string; name: string; id?: number }): Promise<void> {
  const sid = currentSessionId.value;
  if (!sid) return;
  const key = callKey(t);
  if (undoBusy.value.has(key) || undoneKeys.value.has(key)) return;
  undoBusy.value = new Set(undoBusy.value).add(key);
  undoMsg.value = '';
  try {
    // 点击时才拉快照列表(不做渲染期预拉/面板级缓存):避免每次渲染打 API,也避免缓存陈旧误导
    const snapshots = await listUndoSnapshots(sid);
    const maxMessageId = store.messages.reduce((m, msg) => Math.max(m, msg.id), 0);
    const target = pickUndoSnapshot(snapshots, t.name, maxMessageId);
    if (!target) {
      undoMsg.value = '当前会话没有可回退的快照';
      return;
    }
    await restoreUndoSnapshot(target.id);
    // 回退改了落库状态(文件/变量树),刷新历史对齐展示
    await store.loadHistory(sid);
    undoneKeys.value = new Set(undoneKeys.value).add(key);
    undoMsg.value = `已回退:${target.label}`;
  } catch (err) {
    undoMsg.value = `回退失败:${(err as Error).message}`;
  } finally {
    const next = new Set(undoBusy.value);
    next.delete(key);
    undoBusy.value = next;
  }
}

async function grant(tool: string, t: { callId?: string; runId?: string; name: string; id?: number }, scope: 'once' | 'session' | 'role' | 'deny') {
  if (!currentSessionId.value || !t.runId || !t.callId) return;
  const key = callKey(t);
  authorizing.value = `${key}:${scope}`;
  authMsg.value = '';
  try {
    await resolveToolAuthorization(tool, scope, currentSessionId.value, t.runId, t.callId);
    grantedCalls.value.add(key);
    const item = [...agent.value.toolCalls].reverse().find((call) =>
      (t.callId ? call.callId === t.callId : call.name === tool)
      && call.status === 'authorization_required');
    const label = scope === 'once' ? '仅本次允许' : scope === 'session' ? '当前会话已允许' : scope === 'role' ? '当前角色已允许' : '已拒绝';
    if (item) item.reason = `${label},正在恢复当前调用`;
    authMsg.value = `「${tool}」${label}`;
  } catch (err) {
    authMsg.value = `授权失败:${(err as Error).message}`;
  } finally {
    authorizing.value = null;
  }
}

/** 阶段文案 */
const phaseText = computed(() => {
  const t = agent.value.stepText;
  if (t.includes('计划')) return '规划';
  if (t.includes('反思')) return '反思';
  if (t.includes('工具')) return '工具';
  if (t.includes('生成')) return '生成';
  if (t.includes('中断')) return '已中断';
  if (t.includes('完成')) return '完成';
  return t || '就绪';
});

/**
 * 是否展示面板:完全由用户可控的 agentPanelOpen 决定。
 *
 * 历史行为是 `generating || agentPanelOpen`(任务模式 `taskActive || agentPanelOpen`),
 * 生成期间恒真、关闭按钮写了 false 也不生效 → 面板被锁死展开。现在自动展开只在
 * 生成/任务开始时触发一次(chat.startStream / task)、由 autoOpenAgentPanel 写入,
 * 用户随时可收起且收起后不会再被自动撑开。
 */
const showPanel = computed(() => agentPanelOpen.value);

/** 任务模式:计划步骤已完成数 */
const planDoneCount = computed(
  () => currentTask.value?.task.plan.filter((s) => s.status === 'done').length ?? 0,
);

/** 任务模式:计划步骤出错数(>0 时进度行追加「N 步出错」) */
const planErrorCount = computed(
  () => currentTask.value?.task.plan.filter((s) => s.status === 'error').length ?? 0,
);

/** 工具调用按状态分组 */
const groupedTools = computed(() => {
  const calls = agent.value.toolCalls;
  return {
    running: calls.filter((c) => c.status === 'running'),
    auth: calls.filter((c) => c.status === 'authorization_required'),
    done: calls.filter((c) => c.status === 'done'),
    error: calls.filter((c) => c.status === 'error'),
  };
});

/** 该工具调用是否已授权(本地记录,授权成功后卡片转「已授权」态) */
function isGranted(t: { callId?: string; name: string; id?: number }): boolean {
  return grantedCalls.value.has(callKey(t));
}

/** 任务模式:子任务状态 → 卡片修饰 class(running=黄左线,failed=红左线) */
function subtaskCardClass(status: TaskSubtaskStatus): string {
  const c = taskStatusClass(status);
  if (c === 'active') return 'running';
  if (c === 'error') return 'failed';
  return '';
}

/** 任务模式:子任务状态 → 状态点 class + 图标(复用工具调用状态点样式) */
function subtaskDot(status: TaskSubtaskStatus): { cls: string; icon: string } {
  const c = taskStatusClass(status);
  if (c === 'active') return { cls: 'running', icon: '...' };
  if (c === 'done') return { cls: 'done', icon: '✓' };
  if (c === 'error') return { cls: 'error', icon: '✗' };
  return { cls: '', icon: '·' };
}

</script>

<template>
  <aside class="sv-agent-panel" :class="{ open: showPanel }">
    <!-- 面板头 -->
    <div class="sv-agent-panel-head">
      <h2>
        <span class="sv-supreme pink xs" />
        AGENT
      </h2>
      <button class="sv-agent-panel-close" title="收起" @click="store.collapseAgentPanel()">
        ✕
      </button>
    </div>

    <!-- 分区标签页(面板合并:Agent 状态 / 调用情况;选择经 uiPrefs.callTraceOpen 持久化记忆) -->
    <div class="sv-agent-tabs" role="tablist">
      <button
        type="button"
        role="tab"
        class="sv-agent-tab-btn"
        :class="{ active: activeTab === 'agent' }"
        :aria-selected="activeTab === 'agent'"
        @click="activeTab = 'agent'"
      >Agent 状态</button>
      <button
        type="button"
        role="tab"
        class="sv-agent-tab-btn"
        :class="{ active: activeTab === 'trace' }"
        :aria-selected="activeTab === 'trace'"
        @click="activeTab = 'trace'"
      >调用情况</button>
    </div>

    <!-- 面板体:v-show 常驻双 tab(各自本地展开态/授权态不随切 tab 丢失) -->
    <div class="sv-agent-panel-body">
      <div v-show="activeTab === 'agent'" class="sv-agent-tab">
      <!-- 任务模式:当前任务执行状态(阶段/流程进度/计划步骤/子任务) -->
      <template v-if="appMode === 'task'">
        <!-- 状态总览 -->
        <div class="sv-agent-summary">
          <div class="sv-agent-summary-row">
            <span class="label">阶段</span>
            <span class="value phase">{{ currentTask ? taskStatusLabel(currentTask.task.status) : '空闲' }}</span>
          </div>
          <div class="sv-agent-summary-row">
            <span class="label">模式</span>
            <span class="value">TASK</span>
          </div>
          <div class="sv-agent-summary-row" v-if="currentTask && currentTask.task.plan.length">
            <span class="label">流程进度</span>
            <span class="value sv-tnum">
              完成 {{ planDoneCount }}/{{ currentTask.task.plan.length }}<template v-if="planErrorCount > 0"> · {{ planErrorCount }} 步出错</template>
            </span>
          </div>
          <div class="sv-agent-summary-row" v-if="currentTask?.task.error">
            <span class="label">详情</span>
            <span class="value detail">{{ currentTask.task.error }}</span>
          </div>
        </div>

        <!-- 计划步骤(时间线) -->
        <div>
          <div class="sv-agent-section-label">计划步骤({{ currentTask?.task.plan.length ?? 0 }})</div>
          <ol v-if="currentTask?.task.plan.length" class="sv-timeline">
            <li v-for="(s, i) in currentTask.task.plan" :key="i" class="sv-timeline-item" :class="{ running: taskStatusClass(s.status) === 'active' }">
              {{ s.name }}
              <span class="step-detail">{{ taskStatusLabel(s.status) }}</span>
            </li>
          </ol>
          <div v-else class="sv-empty panel">
            <p class="sv-note-mini">创建并执行任务后,计划步骤将在此展示</p>
          </div>
        </div>

        <!-- 子任务执行 -->
        <div>
          <div class="sv-agent-section-label">子任务执行({{ currentTask?.subtasks.length ?? 0 }})</div>
          <template v-if="currentTask?.subtasks.length">
            <div
              v-for="st in currentTask.subtasks"
              :key="st.id"
              class="sv-tool-panel-card"
              :class="subtaskCardClass(st.status)"
            >
              <div class="tool-name">
                <span class="tool-status" :class="subtaskDot(st.status).cls">{{ subtaskDot(st.status).icon }}</span>
                {{ st.name }}
                <span class="sv-subtask-status" :class="taskStatusClass(st.status)">
                  {{ taskStatusLabel(st.status) }}
                </span>
              </div>
              <p v-if="st.error" class="sv-note-mini err sv-mt6">{{ st.error }}</p>
            </div>
          </template>
          <div v-else class="sv-empty panel">
            <p class="sv-note-mini">暂无子任务</p>
          </div>
        </div>
      </template>

      <!-- 角色扮演模式:状态总览 + 推理链 + 工具调用 -->
      <template v-else>
      <!-- 状态总览 -->
      <div class="sv-agent-summary">
        <div class="sv-agent-summary-row">
          <span class="label">阶段</span>
          <span class="value phase">{{ generating ? phaseText : (agent.chain.length ? phaseText : '就绪') }}</span>
        </div>
        <div class="sv-agent-summary-row">
          <span class="label">模式</span>
          <span class="value">{{ agentMode.toUpperCase() }}</span>
        </div>
        <div class="sv-agent-summary-row" v-if="agent.flowProgress">
          <span class="label">流程进度</span>
            <span class="value sv-tnum">{{ agent.flowProgress.index }}/{{ agent.flowProgress.total }}</span>
        </div>
        <div class="sv-agent-summary-row" v-if="lastUsage">
          <span class="label">本次消耗</span>
            <span class="value sv-tnum">{{ lastUsage.total_tokens }} tokens</span>
        </div>
        <div class="sv-agent-summary-row" v-if="agent.detail">
          <span class="label">详情</span>
          <span class="value detail">{{ agent.detail }}</span>
        </div>
      </div>

      <!-- 推理链(时间线) -->
      <div>
        <div class="sv-agent-section-label">推理链({{ agent.chain.length }})</div>
        <ol v-if="agent.chain.length" class="sv-timeline">
          <li v-for="(c, i) in agent.chain" :key="i" class="sv-timeline-item">
            {{ c.text }}
            <span v-if="c.detail" class="step-detail">{{ c.detail }}</span>
          </li>
        </ol>
        <div v-else class="sv-empty panel">
          <p class="sv-note-mini">发送消息后,Agent 推理步骤将在此展示</p>
        </div>
      </div>

      <!-- 工具调用 -->
      <div>
        <div class="sv-agent-section-label">工具调用({{ agent.toolCalls.length }})</div>

        <!-- 执行中 -->
        <template v-if="groupedTools.running.length">
          <div v-for="(t, i) in groupedTools.running" :key="'r'+i" class="sv-tool-panel-card running">
            <div class="tool-name">
              <span class="tool-status running">...</span>
              {{ t.name }}
              <span class="sv-badge run sv-ml-auto">执行中</span>
            </div>
            <div class="sv-tool-io" :class="{ open: isIoOpen(callKey(t) + ':in') }" :ref="(el) => registerIoBox(callKey(t) + ':in', el)">
              <div class="sv-tool-io-bar">
                <button type="button" class="sv-tool-io-toggle" @click="toggleIo(callKey(t) + ':in')">入参</button>
                <button type="button" class="sv-tool-io-copy" @click="copyIo(callKey(t), ioText(t.input))">{{ copiedKey === callKey(t) ? '已复制' : '复制' }}</button>
              </div>
              <pre class="sv-tool-io-body" @scroll="ioScroll">{{ ioText(t.input) }}</pre>
            </div>
          </div>
        </template>

        <!-- 待授权 -->
        <template v-if="groupedTools.auth.length">
          <div v-for="(t, i) in groupedTools.auth" :key="'a'+i" class="sv-tool-panel-card authorization_required">
            <div class="tool-name">
              <span class="tool-status" :class="isGranted(t) ? 'done' : 'authorization_required'">{{ isGranted(t) ? '✓' : '!' }}</span>
              {{ t.name }}
              <span v-if="isGranted(t)" class="sv-badge done sv-ml-auto">{{ t.risk }} · 已授权</span>
              <span v-else class="sv-badge pending sv-ml-auto">{{ t.risk }} · 等待授权</span>
            </div>
            <div class="sv-tool-io" :class="{ open: isIoOpen(callKey(t) + ':in') }" :ref="(el) => registerIoBox(callKey(t) + ':in', el)">
              <div class="sv-tool-io-bar">
                <button type="button" class="sv-tool-io-toggle" @click="toggleIo(callKey(t) + ':in')">入参</button>
                <button type="button" class="sv-tool-io-copy" @click="copyIo(callKey(t), ioText(t.input))">{{ copiedKey === callKey(t) ? '已复制' : '复制' }}</button>
              </div>
              <pre class="sv-tool-io-body" @scroll="ioScroll">{{ ioText(t.input) }}</pre>
            </div>
            <p class="sv-note-mini sv-mt6">{{ t.reason }}</p>
            <div v-if="!isGranted(t)" class="tool-auth-btns">
              <button class="sv-btn primary sv-btn-sm" :disabled="!currentSessionId || authorizing !== null" @click="grant(t.name, t, 'once')">仅允许本次</button>
              <button class="sv-btn ghost" :disabled="!currentSessionId || authorizing !== null" @click="grant(t.name, t, 'session')">允许当前会话</button>
              <button class="sv-btn ghost" :disabled="!currentSessionId || authorizing !== null" @click="grant(t.name, t, 'role')">允许当前角色</button>
              <button class="sv-btn ghost" :disabled="!currentSessionId || authorizing !== null" @click="grant(t.name, t, 'deny')">拒绝</button>
            </div>
            <div v-else class="tool-auth-btns">
              <span class="sv-tag sv-tag-on">已授权</span>
            </div>
          </div>
          <div v-if="authMsg" class="sv-feedback sv-mt6" :class="authMsg.startsWith('授权失败') ? 'err' : 'ok'">{{ authMsg }}</div>
        </template>

        <!-- 已完成 -->
        <template v-if="groupedTools.done.length">
          <div v-for="(t, i) in groupedTools.done" :key="'d'+i" class="sv-tool-panel-card">
            <div class="tool-name">
              <span class="tool-status done">✓</span>
              {{ t.name }}
              <button
                v-if="undoEnabled && currentSessionId && isUndoableTool(t.name)"
                type="button"
                class="sv-btn ghost sv-btn-sm sv-ml-auto"
                :disabled="undoBusy.has(callKey(t)) || undoneKeys.has(callKey(t))"
                :title="`回退到「${t.name}」本次执行前的状态`"
                @click="undoToHere(t)"
              >{{ undoneKeys.has(callKey(t)) ? '已回退' : '回退到此处' }}</button>
            </div>
            <div class="sv-tool-io" :class="{ open: isIoOpen(callKey(t) + ':in') }" :ref="(el) => registerIoBox(callKey(t) + ':in', el)">
              <div class="sv-tool-io-bar">
                <button type="button" class="sv-tool-io-toggle" @click="toggleIo(callKey(t) + ':in')">入参</button>
                <button type="button" class="sv-tool-io-copy" @click="copyIo(callKey(t), ioText(t.input))">{{ copiedKey === callKey(t) ? '已复制' : '复制' }}</button>
              </div>
              <pre class="sv-tool-io-body" @scroll="ioScroll">{{ ioText(t.input) }}</pre>
            </div>
            <div v-if="t.output !== undefined" class="sv-tool-io" :class="{ open: isIoOpen(callKey(t) + ':out') }" :ref="(el) => registerIoBox(callKey(t) + ':out', el)">
              <div class="sv-tool-io-bar">
                <button type="button" class="sv-tool-io-toggle" @click="toggleIo(callKey(t) + ':out')">出参</button>
                <button type="button" class="sv-tool-io-copy" @click="copyIo(callKey(t), ioText(t.output))">{{ copiedKey === callKey(t) ? '已复制' : '复制' }}</button>
              </div>
              <pre class="sv-tool-io-body" @scroll="ioScroll">{{ ioText(t.output) }}</pre>
            </div>
          </div>
        </template>

        <!-- 失败 -->
        <template v-if="groupedTools.error.length">
          <div v-for="(t, i) in groupedTools.error" :key="'e'+i" class="sv-tool-panel-card failed">
            <div class="tool-name">
              <span class="tool-status error">✗</span>
              {{ t.name }}
            </div>
            <div class="sv-tool-io" :class="{ open: isIoOpen(callKey(t) + ':in') }" :ref="(el) => registerIoBox(callKey(t) + ':in', el)">
              <div class="sv-tool-io-bar">
                <button type="button" class="sv-tool-io-toggle" @click="toggleIo(callKey(t) + ':in')">入参</button>
                <button type="button" class="sv-tool-io-copy" @click="copyIo(callKey(t), ioText(t.input))">{{ copiedKey === callKey(t) ? '已复制' : '复制' }}</button>
              </div>
              <pre class="sv-tool-io-body" @scroll="ioScroll">{{ ioText(t.input) }}</pre>
            </div>
            <div v-if="t.output !== undefined" class="sv-tool-io" :class="{ open: isIoOpen(callKey(t) + ':out') }" :ref="(el) => registerIoBox(callKey(t) + ':out', el)">
              <div class="sv-tool-io-bar">
                <button type="button" class="sv-tool-io-toggle" @click="toggleIo(callKey(t) + ':out')">出参</button>
                <button type="button" class="sv-tool-io-copy" @click="copyIo(callKey(t), ioText(t.output))">{{ copiedKey === callKey(t) ? '已复制' : '复制' }}</button>
              </div>
              <pre class="sv-tool-io-body" @scroll="ioScroll">{{ ioText(t.output) }}</pre>
            </div>
          </div>
        </template>

        <!-- 回退反馈(成功/失败) -->
        <div v-if="undoMsg" class="sv-feedback sv-mt6" :class="undoMsg.startsWith('回退失败') ? 'err' : 'ok'">{{ undoMsg }}</div>

        <!-- 空态 -->
        <div v-if="!agent.toolCalls.length && !agent.pendingTool" class="sv-empty panel">
          <p class="sv-note-mini">暂无工具调用</p>
        </div>
      </div>
      </template>
      </div>

      <!-- 调用情况 tab(原独立 CallTracePanel 面板收编为内容组件,两模式共用) -->
      <CallTracePanel v-show="activeTab === 'trace'" />
    </div>
  </aside>
</template>
