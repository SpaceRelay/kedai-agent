<script setup lang="ts">
// 调用追踪内容(批次 3 L3;面板合并后作为 AgentPanel「调用情况」tab 的内容组件,两模式共用):
// 任务模式 = 当前任务 LLM 调用时间线(GET /api/tasks/{id}/calls;
//   tab 激活时由 SSE llm_call 事件驱动刷新,挂载/切换任务且记录为空时主动拉取一次);
// 聊天模式 = 复用 chat store 的 AgentActivity(推理链 + 工具调用),纯本地状态,零后端请求。
// 注意:本组件不再自带抽屉壳/面板头(合并后由 AgentPanel 提供),样式类名保持不变。
import { computed, onMounted, ref, watch } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import type { TaskLlmCall } from '../api';

const store = useAppStore();
const { appMode, taskCalls, currentTaskId, agent, callTraceOpen } = storeToRefs(store);

/** 已展开的记录 key(任务模式调用行 / 聊天模式工具出参) */
const openKeys = ref<Set<string>>(new Set());
function isOpen(key: string): boolean {
  return openKeys.value.has(key);
}
function toggle(key: string): void {
  const next = new Set(openKeys.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  openKeys.value = next;
}

/** 阶段中文标签(step 阶段带步骤序号——后端落库 step_index 为 0 起,展示 +1;
 *  final_audit = team 模式终审,与后端契约并列于 audit;summary = team 模式汇总行) */
function phaseText(phase: string, stepIndex?: number | null): string {
  switch (phase) {
    case 'planner': return '规划';
    case 'step': return stepIndex != null ? `步骤 #${stepIndex + 1}` : '步骤';
    case 'summarize': return '汇总';
    case 'summary': return '汇总';
    case 'agent': return '主Agent';
    case 'subagent': return '子Agent';
    case 'audit': return '审计';
    case 'final_audit': return '终审';
    default: return phase;
  }
}

function phaseLabel(call: TaskLlmCall): string {
  return phaseText(call.phase, call.step_index);
}

/**
 * 「进行中」伪行(批次 R4 流式输出):store.liveBuffers 非空 = 有调用正在流式生成
 * (delta 攒批增量就地累积,零请求);调用落库后 llm_call 事件清对应缓冲,伪行消失、
 * 正式行经 loadTaskCalls 全量重拉出现。展开区显示流式纯文本(不跑 markdown)。
 */
const liveRows = computed(() =>
  [...store.liveBuffers.entries()].map(([key, text]) => {
    // key 契约:`${phase}:${step_index ?? ''}`(phase 为固定词,不含冒号)
    const sep = key.indexOf(':');
    const phase = key.slice(0, sep);
    const idx = key.slice(sep + 1);
    return {
      key: `live:${key}`,
      label: phaseText(phase, idx === '' ? undefined : Number(idx)),
      text,
    };
  }),
);

/** 状态徽标:ok 绿 / empty 黄 / error 红(复用 sv-badge 色系) */
function statusBadge(status: string): { cls: string; text: string } {
  if (status === 'ok') return { cls: 'done', text: '正常' };
  if (status === 'empty') return { cls: 'pending', text: '空响应' };
  if (status === 'error') return { cls: 'fail', text: '错误' };
  return { cls: '', text: status };
}

/** 耗时格式化:≥1s 显示秒,否则毫秒 */
function elapsedText(ms: number): string {
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
}

/** 行内 token 合计(prompt + completion) */
function callTokens(call: TaskLlmCall): number {
  return call.prompt_tokens + call.completion_tokens;
}

/** 截断标记(后端契约:finish_reason === 'length' = 撞 max_tokens 上限,响应为半截文本;
 *  其余值(stop / '' 未知 / 缺省)不显示徽标) */
function isTruncated(call: TaskLlmCall): boolean {
  return call.finish_reason === 'length';
}

/** 工具调用状态点(复用 AgentPanel 状态点色系) */
function toolStatusDot(status: string): { cls: string; icon: string } {
  if (status === 'running') return { cls: 'running', icon: '...' };
  if (status === 'done') return { cls: 'done', icon: '✓' };
  if (status === 'error') return { cls: 'error', icon: '✗' };
  return { cls: 'authorization_required', icon: '!' };
}

/** 工具调用状态中文文案 */
function toolStatusText(status: string): string {
  if (status === 'running') return '执行中';
  if (status === 'done') return '完成';
  if (status === 'error') return '失败';
  return '待授权';
}

/** 工具出参序列化(展示用) */
function outputText(v: unknown): string {
  return typeof v === 'string' ? v : JSON.stringify(v, null, 2);
}

/** tab 激活(v-show 常驻)/挂载/切换任务且记录为空时,主动拉取一次调用记录 */
function ensureLoaded(): void {
  if (appMode.value === 'task' && currentTaskId.value && taskCalls.value.length === 0) {
    void store.loadTaskCalls(currentTaskId.value);
  }
}
onMounted(ensureLoaded);
watch(currentTaskId, ensureLoaded);
// 实跑问题 4:启动时 appMode 已是 task(停在任务模式)而 currentTaskId 由
// store.restoreSelectedTask 稍后异步恢复;此处 appMode 变化也补拉一次,
// 避免 ensureLoaded 早于 currentTaskId 就绪而漏掉首屏加载。
watch(appMode, ensureLoaded);
// 合并面板内 tab 由 callTraceOpen 记忆(true = 落在「调用情况」tab):切入本 tab 时补拉一次
watch(callTraceOpen, (open) => {
  if (open) ensureLoaded();
});
</script>

<template>
  <!-- 调用情况 tab 内容(无独立抽屉壳;根元素单节点,供 AgentPanel v-show 控制显隐) -->
  <div class="sv-calltrace-tab">
    <!-- 任务模式:当前任务 LLM 调用时间线 -->
    <template v-if="appMode === 'task'">
      <div>
        <div class="sv-agent-section-label">LLM 调用({{ taskCalls.length }})</div>
        <!-- 进行中伪行(批次 R4):流式中的调用置顶显示;落库后伪行消失、正式行出现 -->
        <div
          v-for="row in liveRows"
          :key="row.key"
          class="sv-tool-panel-card running sv-calltrace-live"
        >
          <div class="tool-name">
            <span class="tool-status running">...</span>
            {{ row.label }} 正在生成
            <span class="sv-badge sv-ml-auto pending">流式中</span>
          </div>
          <div class="sv-mt6">
            <button type="button" class="sv-tool-io-toggle" @click="toggle(row.key)">
              {{ isOpen(row.key) ? '收起流式' : '展开流式' }}
            </button>
          </div>
          <pre v-if="isOpen(row.key)" class="sv-calltrace-pre sv-stream-cursor">{{ row.text }}</pre>
        </div>
        <template v-if="taskCalls.length">
          <div
            v-for="c in taskCalls"
            :key="c.id"
            class="sv-tool-panel-card"
            :class="{ failed: c.status === 'error' }"
          >
            <div class="tool-name">
              {{ phaseLabel(c) }}
              <span
                v-if="isTruncated(c)"
                class="sv-badge trunc"
                title="响应撞 max_tokens 上限被截断(finish_reason=length),内容为半截文本"
              >截断</span>
              <span class="sv-badge sv-ml-auto" :class="statusBadge(c.status).cls">
                {{ statusBadge(c.status).text }}
              </span>
            </div>
            <div class="sv-calltrace-meta">
              <span>{{ c.model }}</span>
              <span class="sv-tnum">{{ callTokens(c).toLocaleString() }} tokens</span>
              <span class="sv-tnum">{{ elapsedText(c.elapsed_ms) }}</span>
            </div>
            <div class="sv-mt6">
              <button type="button" class="sv-tool-io-toggle" @click="toggle(c.id)">
                {{ isOpen(c.id) ? '收起摘要' : '展开摘要' }}
              </button>
            </div>
            <template v-if="isOpen(c.id)">
              <div class="sv-agent-section-label sv-mt6">提示词摘要</div>
              <pre class="sv-calltrace-pre">{{ c.prompt_summary || '(空)' }}</pre>
              <div class="sv-agent-section-label sv-mt6">
                响应摘要
                <span v-if="isTruncated(c)" class="sv-badge trunc">截断</span>
              </div>
              <p v-if="isTruncated(c)" class="sv-note-mini sv-mt6">该次调用撞 max_tokens 上限(finish_reason=length),响应仅含前半部分;如需完整结果请调大 max_tokens 后重试。</p>
              <pre class="sv-calltrace-pre">{{ c.response_summary || '(空)' }}</pre>
            </template>
          </div>
        </template>
        <div v-else class="sv-empty panel">
          <p class="sv-note-mini">暂无调用记录</p>
        </div>
      </div>
    </template>

    <!-- 聊天模式:推理链 + 工具调用(本地 AgentActivity,零请求) -->
    <template v-else>
      <div>
        <div class="sv-agent-section-label">推理链({{ agent.chain.length }})</div>
        <ol v-if="agent.chain.length" class="sv-timeline">
          <li v-for="(c, i) in agent.chain" :key="i" class="sv-timeline-item">
            {{ c.text }}
            <span v-if="c.detail" class="step-detail">{{ c.detail }}</span>
          </li>
        </ol>
        <div v-else class="sv-empty panel">
          <p class="sv-note-mini">暂无调用记录</p>
        </div>
      </div>

      <div>
        <div class="sv-agent-section-label">工具调用({{ agent.toolCalls.length }})</div>
        <template v-if="agent.toolCalls.length">
          <div
            v-for="(t, i) in agent.toolCalls"
            :key="t.callId ?? i"
            class="sv-tool-panel-card"
            :class="{ running: t.status === 'running', failed: t.status === 'error' }"
          >
            <div class="tool-name">
              <span class="tool-status" :class="toolStatusDot(t.status).cls">
                {{ toolStatusDot(t.status).icon }}
              </span>
              {{ t.name }}
              <span
                v-if="t.risk"
                class="sv-badge sv-ml-auto"
                :class="t.risk === 'safe' ? 'done' : t.risk === 'sensitive' ? 'pending' : 'fail'"
              >{{ t.risk }}</span>
              <span class="sv-badge" :class="t.status === 'done' ? 'done' : t.status === 'error' ? 'fail' : t.status === 'running' ? 'run' : 'pending'">
                {{ toolStatusText(t.status) }}
              </span>
            </div>
            <div v-if="t.output !== undefined" class="sv-mt6">
              <button type="button" class="sv-tool-io-toggle" @click="toggle('out:' + (t.callId ?? i))">
                {{ isOpen('out:' + (t.callId ?? i)) ? '收起输出' : '展开输出' }}
              </button>
              <pre v-if="isOpen('out:' + (t.callId ?? i))" class="sv-calltrace-pre">{{ outputText(t.output) }}</pre>
            </div>
          </div>
        </template>
        <div v-else class="sv-empty panel">
          <p class="sv-note-mini">暂无调用记录</p>
        </div>
      </div>
    </template>
  </div>
</template>
