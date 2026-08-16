<script setup lang="ts">
// 右侧 Agent 面板:状态总览 + 时间线推理链 + 工具调用卡片(浅色主题)
// 生成时自动展开,用户可手动收起/展开
import { computed, ref } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import { resolveToolAuthorization } from '../api';

const store = useAppStore();
const { agent, generating, agentPanelOpen, currentSessionId, agentMode, lastUsage } = storeToRefs(store);
const authorizing = ref<string | null>(null);
/** 授权操作错误/成功反馈(就近显示在授权卡片下方) */
const authMsg = ref<string>('');
/** 已授权的工具调用(callId ?? name):授权成功后卡片转「已授权」态,不再重复请求 */
const grantedCalls = ref<Set<string>>(new Set());

/** 工具调用唯一键:优先 callId,退化用 name+id 组合避免同名并发调用混淆 */
function callKey(t: { callId?: string; name: string; id?: number }): string {
  return t.callId ?? `${t.name}#${t.id ?? 0}`;
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

/** 是否展示面板(生成中强制展开) */
const showPanel = computed(() => generating.value || agentPanelOpen.value);

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
</script>

<template>
  <aside class="sv-agent-panel" :class="{ open: showPanel }">
    <!-- 面板头 -->
    <div class="sv-agent-panel-head">
      <h2>
        <span class="sv-supreme pink" style="width: 8px; height: 8px" />
        AGENT
      </h2>
      <button class="sv-agent-panel-close" title="收起" @click="store.agentPanelOpen = false">
        ✕
      </button>
    </div>

    <!-- 面板体 -->
    <div class="sv-agent-panel-body">
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
          <span class="value">{{ agent.flowProgress.index }}/{{ agent.flowProgress.total }}</span>
        </div>
        <div class="sv-agent-summary-row" v-if="lastUsage">
          <span class="label">本次消耗</span>
          <span class="value">{{ lastUsage.total_tokens }} tokens</span>
        </div>
        <div class="sv-agent-summary-row" v-if="agent.detail">
          <span class="label">详情</span>
          <span class="value" style="font-size: 11px; font-weight: 500">{{ agent.detail }}</span>
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
        <div v-else class="sv-empty" style="padding: 16px">
          <p style="font-size: 11px">发送消息后,Agent 推理步骤将在此展示</p>
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
              <span style="margin-left: auto; font-size: 10px; color: var(--sv-yellow)">执行中</span>
            </div>
            <pre>{{ JSON.stringify(t.input, null, 2) }}</pre>
          </div>
        </template>

        <!-- 待授权 -->
        <template v-if="groupedTools.auth.length">
          <div v-for="(t, i) in groupedTools.auth" :key="'a'+i" class="sv-tool-panel-card authorization_required">
            <div class="tool-name">
              <span class="tool-status" :class="isGranted(t) ? 'done' : 'authorization_required'">{{ isGranted(t) ? '✓' : '!' }}</span>
              {{ t.name }}
              <span v-if="isGranted(t)" style="margin-left: auto; font-size: 10px; color: var(--sv-green)">{{ t.risk }} · 已授权</span>
              <span v-else style="margin-left: auto; font-size: 10px; color: var(--sv-red)">{{ t.risk }} · 等待授权</span>
            </div>
            <pre>{{ JSON.stringify(t.input, null, 2) }}</pre>
            <p style="font-size: 11px; color: var(--sv-ink-dim); margin: 6px 0 0">{{ t.reason }}</p>
            <div v-if="!isGranted(t)" class="tool-auth-btns">
              <button class="sv-btn primary" :disabled="!currentSessionId || authorizing !== null" @click="grant(t.name, t, 'once')">仅允许本次</button>
              <button class="sv-btn ghost" :disabled="!currentSessionId || authorizing !== null" @click="grant(t.name, t, 'session')">允许当前会话</button>
              <button class="sv-btn ghost" :disabled="!currentSessionId || authorizing !== null" @click="grant(t.name, t, 'role')">允许当前角色</button>
              <button class="sv-btn ghost" :disabled="!currentSessionId || authorizing !== null" @click="grant(t.name, t, 'deny')">拒绝</button>
            </div>
            <div v-else class="tool-auth-btns">
              <span class="sv-tag sv-tag-on">已授权</span>
            </div>
          </div>
          <div v-if="authMsg" class="sv-feedback" :class="authMsg.startsWith('授权失败') ? 'err' : 'ok'" style="margin-top: 6px">{{ authMsg }}</div>
        </template>

        <!-- 已完成 -->
        <template v-if="groupedTools.done.length">
          <div v-for="(t, i) in groupedTools.done" :key="'d'+i" class="sv-tool-panel-card">
            <div class="tool-name">
              <span class="tool-status done">✓</span>
              {{ t.name }}
            </div>
            <pre>{{ JSON.stringify(t.input, null, 2) }}</pre>
            <pre v-if="t.output !== undefined" style="border-left: 2px solid var(--sv-green); margin-top: 4px">{{ JSON.stringify(t.output, null, 2) }}</pre>
          </div>
        </template>

        <!-- 失败 -->
        <template v-if="groupedTools.error.length">
          <div v-for="(t, i) in groupedTools.error" :key="'e'+i" class="sv-tool-panel-card" style="border-left-color: var(--sv-red)">
            <div class="tool-name">
              <span class="tool-status error">✗</span>
              {{ t.name }}
            </div>
            <pre>{{ JSON.stringify(t.input, null, 2) }}</pre>
            <pre v-if="t.output !== undefined" style="border-left: 2px solid var(--sv-red); margin-top: 4px">{{ JSON.stringify(t.output, null, 2) }}</pre>
          </div>
        </template>

        <!-- 空态 -->
        <div v-if="!agent.toolCalls.length && !agent.pendingTool" class="sv-empty" style="padding: 16px">
          <p style="font-size: 11px">暂无工具调用</p>
        </div>
      </div>
    </div>
  </aside>
</template>
