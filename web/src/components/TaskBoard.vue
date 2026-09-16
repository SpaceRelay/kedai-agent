<script setup lang="ts">
// 任务详情区(任务模式):当前任务的计划步骤 / 子任务执行 / 最终成果。
// 下达目标与任务历史已移入左侧 Sidebar(单列布局);任务数据持久化到后端 SQLite。
// 批次 4 六模式:模式徽标、plan 模式批准区(批准/放弃/修改后批准)、
// team 模式分工卡与审计结论卡、solo/multi 调用情况面板入口、custom 流程步骤进度。
import { computed, onUnmounted, ref, watch } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import { renderMarkdown } from '../markdown';
import { splitTaskResult } from '../taskResult';
import { taskStatusClass as statusClass, taskStatusLabel as statusLabel } from '../taskStatus';
import { MODE_LABELS, messageKindLabel } from '../api/labels';
import type { TaskRecord, TaskRunMode, TaskStep } from '../api';

const store = useAppStore();
const { currentTask, currentTaskId, model, currentTaskUsage } = storeToRefs(store);

/** 当前任务是否在执行中(planning / running) */
const taskRunning = computed(() => {
  const s = currentTask.value?.task.status;
  return s === 'planning' || s === 'running';
});

/** 当前任务累计 token(prompt + completion;由任务详情 usage_total 带出) */
const taskTotalTokens = computed(() => {
  const u = currentTaskUsage.value;
  return u ? u.prompt_tokens + u.completion_tokens : 0;
});

// ===== 批次 4:六模式呈现 =====

// 模式中文文案统一取自 api/labels.ts(唯一源;穷尽校验见该文件)。
// 原此处手写 MODE_LABELS 与 TaskModeSelect.vue 内联 option 是两份,已收敛。

/** 当前任务执行模式(旧服务端不带 task_mode 时按 legacy 处理) */
const taskMode = computed<TaskRunMode>(() => currentTask.value?.task.task_mode ?? 'legacy');
const taskModeLabel = computed(() => MODE_LABELS[taskMode.value]);

/**
 * 计划步骤的 markdown 预渲染(定向响应式,multi/team 卡顿修复):
 * 原写法在模板里直接 v-html="renderMarkdown(s.result)",每次组件重渲染(事件驱动的
 * 详情刷新)都对全部步骤重跑 markdown + 重设 innerHTML。改为 computed 缓存:
 * 依赖精确到 plan 引用——store 侧签名去重保证数据未变时引用不换,此处零重算;
 * 即便重算后 html 值相同,Vue patch 对 v-html 值不变也跳过 DOM 更新。
 */
interface RenderedStep {
  /** 原始步骤(名称/状态展示用) */
  step: TaskStep;
  /** 预渲染的 result HTML(空结果为空串) */
  html: string;
}
const planRendered = computed<RenderedStep[]>(() =>
  (currentTask.value?.task.plan ?? []).map((s) => ({
    step: s,
    html: s.result ? renderMarkdown(s.result) : '',
  })),
);

/** 当前任务是否处于计划待批准(plan 模式 run 后的暂停态) */
const taskPlanned = computed(() => currentTask.value?.task.status === 'planned');

// ----- 批次 R2:多轮用户输入(追加指令 / 用户指令历史) -----
/** 终态(done/partial/error/ended)才可追加指令(与后端 followup 门禁一致) */
const taskTerminal = computed(() => {
  const s = currentTask.value?.task.status;
  return s === 'done' || s === 'partial' || s === 'error' || s === 'ended';
});

/** 用户指令历史(详情 messages 字段;旧服务端无此字段,容错为空数组)。
 *  planned 态时 plan_chat 消息由批准区「与规划器对话」专属渲染(防同屏重复);
 *  其他状态(含完成后的回顾)底部历史区全量显示。 */
const taskMessages = computed(() =>
  (currentTask.value?.messages ?? []).filter((m) => m.kind !== 'plan_chat' || !taskPlanned.value),
);

/** 批准环节规划对话记录(plan_chat 消息;批准区内渲染,批次 R2b) */
const planChatMessages = computed(() =>
  (currentTask.value?.messages ?? []).filter((m) => m.kind === 'plan_chat'),
);

/** 规划对话输入草稿与修订中标记(批次 R2b) */
const planChatDraft = ref('');
const planChatting = ref(false);

/** 发送规划对话反馈:同步等待规划器修订完成,计划区随详情刷新(R2b) */
async function sendPlanChat(): Promise<void> {
  const id = currentTaskId.value;
  const content = planChatDraft.value.trim();
  if (!id || !content || planChatting.value) return;
  planChatting.value = true;
  try {
    await store.planChatTask(id, content);
    planChatDraft.value = '';
  } catch (err) {
    alert(`修订失败:${(err as Error).message}`);
  } finally {
    planChatting.value = false;
  }
}

/** 追加指令输入草稿与发送中标记 */
const followupDraft = ref('');
const followupSending = ref(false);
/** 追加模式(2026-09-10 实跑修复 F5):append=追加进成果,replace=整体重写成果。
 *  append 无法表达「压缩 / 重写 / 改前面」类指令(原文仍在),故提供重写模式。 */
const followupMode = ref<'append' | 'replace'>('append');

/** 非终态时的禁用提示(按状态给出可操作的下一步) */
const followupDisabledHint = computed(() => {
  const s = currentTask.value?.task.status;
  if (s === 'planned') return '计划待批准:请先批准或放弃(批准区可修改计划)';
  if (s === 'pending') return '任务尚未执行:请先执行,产出首轮成果后可追加';
  return '任务执行中:完成或停止后可追加指令';
});

/** 消息种类小标签(文案源见 api/labels.ts;未登记种类返回空串、不显示) */
// 原此处手写 MESSAGE_KIND_LABELS,已收敛到 api/labels.ts 的 messageKindLabel()

/** 助手发言的署名:优先任务绑定角色的名字,无绑定(如纯任务)回退「任务 Agent」 */
const taskMessageAuthor = computed(() => {
  const cid = currentTask.value?.task.character_id;
  if (cid) {
    const c = store.characters.find((x) => x.id === cid);
    if (c?.chara_name) return c.chara_name;
  }
  return '任务 Agent';
});

/**
 * 对话记录里 assistant 消息的 markdown 预渲染(与 planRendered 同款缓存口径):
 * 依赖精确到 messages 数组引用——store 侧按内容签名去重保证数据未变时引用不换,
 * 事件风暴中这里零重算;按消息 id 缓存避免同一列表反复渲染时重复跑 markdown。
 */
const messageHtmlCache = computed(() => {
  const map = new Map<string, string>();
  for (const m of currentTask.value?.messages ?? []) {
    if (m.role === 'assistant') map.set(m.id, renderMarkdown(m.content));
  }
  return map;
});
function renderMessageHtml(m: { id: string; content: string }): string {
  return messageHtmlCache.value.get(m.id) ?? '';
}

/** 成果汇总卡展开态(默认关闭;持久化在 uiPrefs,跨会话/重启记忆) */
const summaryOpen = computed(() => store.taskResultSummaryOpen);
function toggleSummary(): void {
  store.taskResultSummaryOpen = !store.taskResultSummaryOpen;
}

/** 发送追加指令:成功后草稿清空(详情经 store.followupTask 内刷新带出 messages/result) */
async function sendFollowup(): Promise<void> {
  const id = currentTaskId.value;
  const content = followupDraft.value.trim();
  if (!id || !content || followupSending.value || !taskTerminal.value) return;
  followupSending.value = true;
  try {
    await store.followupTask(id, content, followupMode.value);
    followupDraft.value = '';
    followupMode.value = 'append';
  } catch (err) {
    alert(`追加失败:${(err as Error).message}`);
  } finally {
    followupSending.value = false;
  }
}

// ----- plan 模式批准区 -----
/** 计划编辑器开关(「修改后批准」展开) */
const planEditing = ref(false);
/** 编辑中的计划副本(name/goal 可改;status/result 保留原值随提交回传) */
const editedPlan = ref<TaskStep[]>([]);
const approving = ref(false);

// 切换任务时收起编辑态与 pending 操作;追加指令/规划对话草稿一并清空(批次 R2)
watch(currentTaskId, () => {
  planEditing.value = false;
  editedPlan.value = [];
  followupDraft.value = '';
  planChatDraft.value = '';
});

/** 进入「修改后批准」:复制当前计划为可编辑副本 */
function startEditPlan(): void {
  const plan = currentTask.value?.task.plan ?? [];
  editedPlan.value = plan.map((s) => ({ ...s }));
  planEditing.value = true;
}

/** 批准执行:plan 为空表示按原计划批准;给了 plan 则替换后由 ApprovedPlanExecutor 逐步执行 */
async function approveCurrent(plan?: TaskStep[]): Promise<void> {
  const id = currentTaskId.value;
  if (!id || approving.value) return;
  approving.value = true;
  try {
    await store.approveTask(id, plan);
    planEditing.value = false;
    editedPlan.value = [];
  } catch (err) {
    alert(`批准失败:${(err as Error).message}`);
  } finally {
    approving.value = false;
  }
}

/** 放弃计划:复用既有停止接口(任务进入已停止终态) */
async function discardPlan(): Promise<void> {
  if (!confirm('确定放弃该计划?任务将停止,不再执行。')) return;
  await stopCurrent();
}

// ----- team 模式:分工卡(按步骤名「【主Agent-N】」前缀分组;契约见批次 4) -----
/** 分工前缀捕获:【主Agent-N】子目标名 */
const TEAM_PREFIX_RE = /^【(主Agent-\d+)】\s*/;

interface TeamGroup {
  /** 分工标签(如「主Agent-1」;空前缀步骤归为 '' 未分工组) */
  agent: string;
  steps: Array<{ step: TaskStep; index: number }>;
}

const teamGroups = computed<TeamGroup[]>(() => {
  const plan = currentTask.value?.task.plan ?? [];
  const groups: TeamGroup[] = [];
  const byAgent = new Map<string, TeamGroup>();
  const unassigned: TeamGroup = { agent: '', steps: [] };
  plan.forEach((step, index) => {
    const m = TEAM_PREFIX_RE.exec(step.name);
    if (!m) {
      unassigned.steps.push({ step, index });
      return;
    }
    let g = byAgent.get(m[1]);
    if (!g) {
      g = { agent: m[1], steps: [] };
      byAgent.set(m[1], g);
      groups.push(g);
    }
    g.steps.push({ step, index });
  });
  if (unassigned.steps.length) groups.push(unassigned);
  return groups;
});

/** 分工卡的 markdown 预渲染(同 planRendered:缓存到 plan 引用维度,事件风暴中不重跑) */
interface RenderedTeamGroup {
  agent: string;
  steps: Array<{ step: TaskStep; index: number; html: string }>;
}
const teamGroupsRendered = computed<RenderedTeamGroup[]>(() =>
  teamGroups.value.map((g) => ({
    agent: g.agent,
    steps: g.steps.map((it) => ({
      step: it.step,
      index: it.index,
      html: it.step.result ? renderMarkdown(it.step.result) : '',
    })),
  })),
);

/** 去掉分工前缀后的步骤名(卡内不再重复显示前缀) */
function teamStepName(name: string): string {
  return name.replace(TEAM_PREFIX_RE, '');
}

// ----- 结果拆卡:team 模式尾部「## 审计结论」/ plan 模式尾部「## 最终计划」各自单独成卡 -----
// (拆段纯函数抽至 ../taskResult,契约与 server-rs task_engine 对齐,单测锁定)
const resultSplit = computed(() => splitTaskResult(taskMode.value, currentTask.value?.task.result ?? ''));

/** 结果卡仅在终态(done/partial)渲染(批次 R1):approve 后的 planning/running 过渡窗口
 *  result 仍持有 planned 态写入的计划清单文本,不门控会把计划清单误显示为「最终成果」;
 *  planned 态的计划清单由批准区内的专属卡渲染(见 plannedPlanHtml) */
const resultFinal = computed(() => {
  const s = currentTask.value?.task.status;
  return s === 'done' || s === 'partial';
});

/** 最终成果/审计结论/最终计划的 markdown 预渲染(同 planRendered 的缓存口径) */
const resultMainHtml = computed(() =>
  resultFinal.value && resultSplit.value.main ? renderMarkdown(resultSplit.value.main) : '',
);
const resultAuditHtml = computed(() =>
  resultFinal.value && resultSplit.value.audit ? renderMarkdown(resultSplit.value.audit) : '',
);
const resultFinalPlanHtml = computed(() =>
  resultFinal.value && resultSplit.value.finalPlan ? renderMarkdown(resultSplit.value.finalPlan) : '',
);

/** planned 态:批准区内渲染 result 的计划清单文本(批次 R1:planned 态 result 语义 =
 *  待批准的计划清单,markdown 渲染供批准前审阅)。非空时下方「计划步骤」区隐藏(视觉去重:
 *  planned 态步骤全 pending,徽标无信息量;result 为空的旧数据回退显示步骤区) */
const plannedPlanHtml = computed(() => {
  if (!taskPlanned.value || taskMode.value !== 'plan') return '';
  const result = currentTask.value?.task.result ?? '';
  return result ? renderMarkdown(result) : '';
});

/** solo/multi 模式入口:打开 Agent 合并面板并落在「调用情况」tab(主/子 Agent 调用时间线) */
function openCallTrace(): void {
  store.callTraceOpen = true; // tab 记忆指向「调用情况」
  store.openAgentPanel();     // 用户显式展开:清除自动展开抑制
}

// ----- 批次 R4 流式输出:「正在生成」块 -----
/** 流式缓冲原文(全部活跃调用的攒批增量;单缓冲纯文本,多缓冲(team 并行)带 key 前缀区分) */
const liveRaw = computed(() => {
  const entries = [...store.liveBuffers.entries()];
  if (entries.length === 1) return entries[0][1];
  return entries.map(([k, t]) => `【${k}】\n${t}`).join('\n\n');
});

/**
 * 节流展示文本(100ms):delta 攒批后仍可能每 200ms 一条,渲染只做纯文本插值,
 * 流式期间绝不跑 renderMarkdown(防每 token 重跑);落库后缓冲清空、本块消失,
 * 权威文本由计划步骤/最终成果区的 markdown 渲染接管(即「结束一次 markdown」)。
 * 初始值直接取 liveRaw:SSR/挂载瞬间缓冲已有数据时首帧即含文本。
 */
const liveText = ref(liveRaw.value);
let liveTimer: ReturnType<typeof setTimeout> | null = null;
watch(liveRaw, (v) => {
  if (liveTimer) return; // 冷却中:到点后统一对齐最新值
  liveText.value = v; // 首帧/冷却外立即同步
  liveTimer = setTimeout(() => {
    liveTimer = null;
    if (liveRaw.value !== liveText.value) liveText.value = liveRaw.value;
  }, 100);
});
onUnmounted(() => {
  if (liveTimer) clearTimeout(liveTimer);
});

/** 执行当前任务 */
async function runCurrent(): Promise<void> {
  const id = currentTaskId.value;
  if (!id) return;
  try {
    await store.runTask(id);
  } catch (err) {
    alert(`执行失败:${(err as Error).message}`);
  }
}

/** 停止当前任务 */
async function stopCurrent(): Promise<void> {
  const id = currentTaskId.value;
  if (!id) return;
  try {
    await store.stopTask(id);
  } catch (err) {
    alert(`停止失败:${(err as Error).message}`);
  }
}

/** 删除任务 */
async function removeTask(task: TaskRecord): Promise<void> {
  if (!confirm(`确定删除任务「${task.title}」?其子任务将一并删除。`)) return;
  try {
    await store.deleteTask(task.id);
  } catch (err) {
    alert(`删除失败:${(err as Error).message}`);
  }
}
</script>

<template>
  <section class="flex min-h-0 min-w-0 flex-1 flex-col">
    <!-- 顶栏 -->
    <header class="sv-topbar">
      <span class="flex items-center gap-2">
        <span class="sv-supreme md" aria-hidden="true" />
        <span class="sv-topbar-title">任务工作台</span>
        <span v-if="model" class="sv-topbar-sub">{{ model }}</span>
      </span>

      <span class="sv-topbar-right">
        <!-- Agent 面板开关(面板合并后原「调用情况」独立拨杆收编:与聊天顶栏同一入口,两模式共用)
             sv-topbar-agent:移动端隐藏,该入口已由底部导航「AGENT」承载 -->
        <div class="sv-topbar-group sv-topbar-agent">
          <button
            type="button"
            class="sv-render-toggle-v2"
            :aria-pressed="store.agentPanelOpen"
            :title="store.agentPanelOpen ? 'Agent 面板已开启(含调用情况)' : 'Agent 面板已关闭(含调用情况)'"
            @click="store.toggleAgentPanel()"
          >
            <span class="toggle-track" :class="{ on: store.agentPanelOpen }">
              <span class="toggle-thumb" />
            </span>
            <span class="toggle-label">AGENT</span>
          </button>
        </div>
      </span>
    </header>

    <!-- 主体:任务详情占满(新建与历史列表在左侧 Sidebar) -->
    <div class="sv-taskboard">
      <!-- 详情区 -->
      <div class="sv-task-detail">
        <template v-if="currentTask">
          <div class="sv-task-head">
            <div class="sv-task-head-title">
              <span class="sv-supreme pink-deep" />
              {{ currentTask.task.title }}
            </div>
            <div class="sv-task-head-actions">
              <button
                v-if="!taskRunning"
                class="sv-btn primary sv-btn-sm"
                title="执行(重新)此任务"
                @click="runCurrent"
              >▶ 执行</button>
              <button
                v-else
                class="sv-btn sv-btn-sm stop"
                title="停止执行"
                @click="stopCurrent"
              >■ 停止</button>
              <button
                class="sv-btn ghost sv-btn-sm"
                title="删除任务"
                @click="removeTask(currentTask.task)"
              >删除</button>
            </div>
          </div>

          <!-- 状态 + 模式徽标 + 累计 token + 错误 -->
          <div class="sv-task-status-line">
            <span class="sv-tag" :class="statusClass(currentTask.task.status)">
              {{ statusLabel(currentTask.task.status) }}
            </span>
            <span class="sv-tag sm" title="任务执行模式(批次 4 六模式)">模式:{{ taskModeLabel }}</span>
            <span v-if="taskTotalTokens > 0" class="sv-task-usage">累计 token {{ taskTotalTokens.toLocaleString() }}</span>
            <span v-if="currentTask.task.error" class="sv-task-error">{{ currentTask.task.error }}</span>
          </div>

          <!-- plan 模式批准区:计划已生成待批准(批准 / 修改后批准 / 放弃) -->
          <div v-if="taskPlanned" class="sv-task-approve">
            <div class="sv-task-section-title">计划待批准</div>
            <!-- 批次 R1:planned 态 result = 待批准的计划清单,批准区内渲染供批准前审阅 -->
            <div v-if="plannedPlanHtml" class="sv-task-result sv-task-planned-plan" v-html="plannedPlanHtml" />
            <template v-if="!planEditing">
              <div class="sv-task-approve-actions">
                <button
                  class="sv-btn primary sv-btn-sm"
                  :disabled="approving"
                  title="按当前计划开始执行"
                  @click="approveCurrent()"
                >✓ 批准执行</button>
                <button
                  class="sv-btn ghost sv-btn-sm"
                  :disabled="approving || currentTask.task.plan.length === 0"
                  title="修改计划步骤后批准执行"
                  @click="startEditPlan"
                >修改后批准</button>
                <button
                  class="sv-btn ghost sv-btn-sm"
                  :disabled="approving"
                  title="放弃该计划,任务停止不再执行"
                  @click="discardPlan"
                >放弃</button>
              </div>
            </template>
            <template v-else>
              <!-- 计划简易编辑:每步 名称/目标 两个输入框(增删步不做,保持最小) -->
              <div v-for="(s, i) in editedPlan" :key="i" class="sv-task-plan-edit">
                <span class="sv-task-step-idx">{{ i + 1 }}</span>
                <div class="sv-task-plan-edit-body">
                  <input
                    v-model="s.name"
                    type="text"
                    class="sv-input"
                    placeholder="步骤名称"
                    spellcheck="false"
                  />
                  <input
                    v-model="s.goal"
                    type="text"
                    class="sv-input"
                    placeholder="步骤目标"
                    spellcheck="false"
                  />
                </div>
              </div>
              <div class="sv-task-approve-actions">
                <button
                  class="sv-btn primary sv-btn-sm"
                  :disabled="approving"
                  @click="approveCurrent(editedPlan)"
                >✓ 确认并批准执行</button>
                <button
                  class="sv-btn ghost sv-btn-sm"
                  :disabled="approving"
                  @click="planEditing = false"
                >取消</button>
              </div>
            </template>

            <!-- 与规划器对话(批次 R2b):plan_chat 对话记录 + 反馈输入框;
                 发送后规划器按反馈修订计划(任务保持 planned,需重新批准) -->
            <div class="sv-task-plan-chat">
              <div class="sv-task-plan-chat-title">与规划器对话(按反馈修订计划)</div>
              <div v-if="planChatMessages.length" class="sv-task-plan-chat-log">
                <div v-for="m in planChatMessages" :key="m.id" class="sv-task-msg-row">
                  <div v-if="m.role === 'user'" class="sv-msg user">
                    <div class="sv-msg-bubble">{{ m.content }}</div>
                  </div>
                  <div v-else class="sv-msg assistant">
                    <div class="min-w-0">
                      <div class="sv-msg-bubble sv-msg-md" v-html="renderMessageHtml(m)" />
                    </div>
                  </div>
                </div>
              </div>
              <div class="sv-task-followup-bar">
                <textarea
                  v-model="planChatDraft"
                  class="sv-input"
                  rows="2"
                  :disabled="planChatting || approving"
                  placeholder="对计划提出修改意见,例如:把第二步换成先做竞品调研…"
                  spellcheck="false"
                />
                <button
                  class="sv-btn primary sv-btn-sm"
                  :disabled="planChatting || approving || !planChatDraft.trim()"
                  title="发送反馈,规划器将修订计划"
                  @click="sendPlanChat"
                >{{ planChatting ? '修订中…' : '发送' }}</button>
              </div>
            </div>
          </div>

          <!-- solo / multi 模式:执行细节在 Agent 面板「调用情况」tab(主/子 Agent 调用时间线) -->
          <div v-if="taskMode === 'solo' || taskMode === 'multi'" class="sv-task-hint">
            <span class="sv-task-hint-text">
              {{ taskModeLabel }}模式的执行细节(主/子 Agent 调用)请查看 Agent 面板「调用情况」
            </span>
            <button class="sv-btn ghost sv-btn-sm" @click="openCallTrace">查看调用情况</button>
          </div>

          <!-- team 模式:分工卡(步骤名「【主Agent-N】」前缀分组;契约见批次 4) -->
          <div v-if="taskMode === 'team' && currentTask.task.plan.length" class="sv-task-section">
            <div class="sv-task-section-title">主 Agent 分工</div>
            <div
              v-for="g in teamGroupsRendered"
              :key="g.agent || 'unassigned'"
              class="sv-task-team-card"
            >
              <div class="sv-task-team-agent">
                <span class="sv-supreme xs" />
                {{ g.agent ? `【${g.agent}】` : '未分工' }}
              </div>
              <div
                v-for="item in g.steps"
                :key="item.index"
                class="sv-task-step"
                :class="statusClass(item.step.status)"
              >
                <span class="sv-task-step-idx" :class="statusClass(item.step.status)">{{ item.index + 1 }}</span>
                <div class="sv-task-step-body">
                  <div class="sv-task-step-name">
                    {{ teamStepName(item.step.name) }}
                    <span class="sv-tag sm" :class="statusClass(item.step.status)">{{ statusLabel(item.step.status) }}</span>
                  </div>
                  <div v-if="item.step.result" class="sv-task-step-result" v-html="item.html" />
                </div>
              </div>
            </div>
          </div>

          <!-- 计划步骤(legacy / solo / multi / plan;custom 为流程步骤进度,步骤 + 状态徽标)。
               批次 R1:planned 态且 result 计划清单已渲染于批准区时隐藏本区(视觉去重) -->
          <div v-else-if="currentTask.task.plan.length && !plannedPlanHtml" class="sv-task-section">
            <div class="sv-task-section-title">{{ taskMode === 'custom' ? '流程步骤进度' : '计划步骤' }}</div>
            <div
              v-for="(rs, i) in planRendered"
              :key="i"
              class="sv-task-step"
              :class="statusClass(rs.step.status)"
            >
              <span class="sv-task-step-idx" :class="statusClass(rs.step.status)">{{ i + 1 }}</span>
              <div class="sv-task-step-body">
                <div class="sv-task-step-name">
                  {{ rs.step.name }}
                  <span class="sv-tag sm" :class="statusClass(rs.step.status)">{{ statusLabel(rs.step.status) }}</span>
                </div>
                <!-- 步骤目标(plan 模式待批准清单的完整内容:名称+目标;
                     批准后/完成后同区持续显示,状态徽标与 result 随 SSE 刷新实时反映) -->
                <div v-if="rs.step.goal" class="sv-task-step-goal">{{ rs.step.goal }}</div>
                <div v-if="rs.step.result" class="sv-task-step-result" v-html="rs.html" />
              </div>
            </div>
          </div>

          <!-- 子任务(multi 子 agent 记录)。legacy 不渲染:legacy 的 subtasks 与上方
               「计划步骤」同源重复(每个步骤名会各出现一次),信息以计划步骤区为准
               (2026-09-10 实跑修复 F7) -->
          <div
            v-if="currentTask.subtasks.length && taskMode !== 'legacy'"
            class="sv-task-section"
          >
            <div class="sv-task-section-title">子任务执行</div>
            <div
              v-for="st in currentTask.subtasks"
              :key="st.id"
              class="sv-task-subtask"
            >
              <span class="sv-supreme xs" :class="statusClass(st.status)" />
              <div class="sv-task-subtask-body">
                <div class="sv-task-subtask-name">
                  {{ st.name }}
                  <span class="sv-tag sm" :class="statusClass(st.status)">{{ statusLabel(st.status) }}</span>
                </div>
                <div v-if="st.error" class="sv-task-error">{{ st.error }}</div>
              </div>
            </div>
          </div>

          <!-- 正在生成(批次 R4 流式输出):计划步骤区与最终成果区之间的流式块。
               流式期间纯文本 + 光标(不跑 renderMarkdown,防每 token 重跑);
               调用落库后 store 清缓冲、本块消失,权威文本由最终成果/步骤区 markdown 接管 -->
          <div v-if="liveText" class="sv-task-section sv-task-live">
            <div class="sv-task-section-title">正在生成</div>
            <div class="sv-task-live-text sv-stream-cursor">{{ liveText }}</div>
          </div>

          <!-- 最终结果(team 模式拆尾部「## 审计结论」、plan 模式拆尾部「## 最终计划」
               各自单独成卡;批次 R1:仅终态 done/partial 渲染,过渡窗口不显示)。
               实跑问题 1:逐轮对话记录区已是产出的权威视图,本卡默认关闭(与气泡重复),
               可经标题栏开关展开;team 审计结论 / plan 最终计划为独立信息,保持常显。 -->
          <div v-if="resultMainHtml" class="sv-task-section">
            <div class="sv-task-section-title">
              成果汇总
              <button
                class="sv-task-summary-toggle"
                :title="summaryOpen ? '收起成果汇总(逐轮对话记录已含全部产出)' : '展开成果汇总(合并后的完整成果)'"
                @click="toggleSummary"
              >
                {{ summaryOpen ? '收起' : '展开' }}
              </button>
            </div>
            <div v-if="summaryOpen" class="sv-task-result" v-html="resultMainHtml" />
          </div>
          <div v-if="resultAuditHtml" class="sv-task-section">
            <div class="sv-task-section-title">审计结论</div>
            <div class="sv-task-result sv-task-audit" v-html="resultAuditHtml" />
          </div>
          <div v-if="resultFinalPlanHtml" class="sv-task-section">
            <div class="sv-task-section-title">最终计划</div>
            <div class="sv-task-result sv-task-final-plan" v-html="resultFinalPlanHtml" />
          </div>

          <!-- 对话记录(实跑问题 1):任务目标与各轮产出按 user/assistant 逐轮气泡呈现,
               与 chat 模式同款视觉(用户气泡右对齐、模型气泡左对齐完整 markdown)。
               数据源 = 详情 messages 字段(kind: goal/result/followup/plan_chat);
               plan_chat 在 planned 态由批准区专属渲染,此处跳过防同屏重复。 -->
          <div v-if="taskMessages.length" class="sv-task-section sv-task-messages">
            <div class="sv-task-section-title">对话记录</div>
            <div v-for="m in taskMessages" :key="m.id" class="sv-task-msg-row">
              <div v-if="m.role === 'user'" class="sv-msg user">
                <div class="sv-msg-bubble">
                  <span v-if="messageKindLabel(m.kind)" class="sv-tag sm sv-task-msg-kind">{{ messageKindLabel(m.kind) }}</span>{{ m.content }}
                </div>
              </div>
              <div v-else class="sv-msg assistant">
                <div class="min-w-0">
                  <div class="sv-msg-name">
                    {{ taskMessageAuthor }}
                    <span v-if="messageKindLabel(m.kind)" class="sv-tag sm sv-task-msg-kind">{{ messageKindLabel(m.kind) }}</span>
                  </div>
                  <div class="sv-msg-bubble sv-msg-md" v-html="renderMessageHtml(m)" />
                </div>
              </div>
            </div>
          </div>

          <!-- 追加指令(批次 R2a;R2b+ 扩 mode):终态任务可继续下达;
               追加=在既有成果后附加新段(不改原文),重写=用新产出整体替换成果
               (支持「压缩/重写/改前面」类指令);执行中/待批准/未执行时禁用 -->
          <div class="sv-task-section sv-task-followup">
            <div class="sv-task-section-title">
              追加指令
              <select
                v-model="followupMode"
                class="sv-task-followup-mode"
                :disabled="!taskTerminal || followupSending"
                title="追加:在成果后附加新段(保留原文);重写:用本轮产出整体替换成果"
              >
                <option value="append">追加</option>
                <option value="replace">重写</option>
              </select>
            </div>
            <div class="sv-task-followup-bar">
              <textarea
                v-model="followupDraft"
                class="sv-input"
                rows="2"
                :disabled="!taskTerminal || followupSending"
                :placeholder="taskTerminal ? (followupMode === 'replace' ? '用本轮产出整体替换成果,例如:把全文压缩到 200 字以内…' : '在既有成果基础上继续补充,例如:再润色一遍结尾…') : followupDisabledHint"
                spellcheck="false"
              />
              <button
                class="sv-btn primary sv-btn-sm"
                :disabled="!taskTerminal || followupSending || !followupDraft.trim()"
                :title="taskTerminal ? (followupMode === 'replace' ? '发送并整体替换成果' : '发送追加指令,任务将继续执行') : followupDisabledHint"
                @click="sendFollowup"
              >{{ followupSending ? '发送中…' : '发送' }}</button>
            </div>
            <div class="sv-task-followup-hint">
              <template v-if="followupMode === 'replace'">重写模式:本轮产出将<b>整体替换</b>现有成果,旧内容不再保留</template>
              <template v-else>追加模式:新产出附加在成果末尾,<b>不会修改</b>原有内容;若要压缩或改写全文请切换到「重写」</template>
            </div>
            <div v-if="!taskTerminal" class="sv-task-followup-hint">{{ followupDisabledHint }}</div>
          </div>

          <div v-if="currentTask.task.status === 'pending'" class="sv-task-empty sv-mt16">
            <p class="sub">任务已创建,点击右上「执行」开始</p>
          </div>
        </template>

        <div v-else class="sv-task-empty fill">
          <div class="sv-empty-geo lg mb14">
            <span class="sq black" />
            <span class="sq pink" />
            <span class="sq deep" />
          <i class="diag" />
          </div>
          <p class="lead">选择或创建一个任务</p>
          <p class="sub">在左侧栏输入目标,系统会拆解计划、派子智能体执行并汇总结果</p>
        </div>
      </div>
    </div>
  </section>
</template>
