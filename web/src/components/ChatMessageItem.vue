<script setup lang="ts">
// 单条消息渲染组件(前端性能优化·渲染缓存):
// 消息气泡模板与 assistant 渲染逻辑从 ChatWindow 迁入,渲染结果用 computed 按消息维度缓存,
// 依赖精确到 (消息 id/content/content_display/extra、正则脚本版本 hash、renderHtml 偏好);
// 流式 token 到达时仅当前生成中的消息重算,历史消息零重渲染。
// 注意:样式类名与 DOM 结构语义与迁入前完全一致(style.css 全局选择器依赖),不得随意改名。
import { computed, onUnmounted, ref, watch } from 'vue';
import { useAppStore } from '../store';
import type { UiMessage } from '../sseReducer';
import type { RegexScript } from '../api';
import {
  renderScopedScripts,
  stripHiddenPlaceholders,
  hasStatusPlaceholderScript,
  buildMessageRenderText,
  extractBodyLoadUrl,
  buildRemoteResourceHtml,
  stableFrameNonce,
} from '../render';
import { renderMarkdown } from '../markdown';
import { isRenderCodeBlock } from '../renderPanel';

const props = defineProps<{
  /** 消息对象(store 响应式;流式期间 content 被就地追加) */
  m: UiMessage;
  /** 是否处于编辑态(同一时刻仅一条,单编辑语义由父组件 editingId 保证) */
  editing: boolean;
  /** 头像来源(优先角色头像;无则父组件传 null,显示角色名首字) */
  avatarUrl: string | null;
  characterName: string;
  /** HTML 渲染开关(scoped HTML / 纯 markdown 分流) */
  renderHtml: boolean;
  /** 当前角色正则脚本(渲染依赖;引用替换或字段变化都会使 computed 失效) */
  scripts: RegexScript[];
  /** 消息楼层深度(0 = 最新一条):脚本 min_depth/max_depth 过滤依据;缺省不过滤 */
  depth?: number;
  /** 正则脚本内容版本 hash:作为缓存失效兜底依赖(脚本变化时必变) */
  scriptHash: string;
}>();

const emit = defineEmits<{
  (e: 'start-edit', m: UiMessage): void;
  (e: 'save-edit', payload: { id: number; text: string; resend: boolean }): void;
  (e: 'cancel-edit'): void;
  /** 本条消息内容被自身 UI 动作改变(目前为 swipe 切换):父组件据此重跑脚本调度与滚动签名 */
  (e: 'mutated'): void;
}>();

const store = useAppStore();

/** 角色卡是否含渲染 HTML 状态栏的脚本(命中 <StatusPlaceHolderImpl/> 占位符) */
const hasStatusScript = computed(() => hasStatusPlaceholderScript(props.scripts));

/** 消息显示文本:优先服务端宏展开后的 content_display({{char}}/{{getvar}} 等已展开),
 *  缺省回退原文。编辑消息时仍用 content(原文),避免污染存储。 */
function displayText(m: { content: string; content_display?: string }): string {
  return m.content_display ?? m.content;
}

/** 消息状态栏文本(两步生成落库 extra.status_bar;无则 null) */
function statusBar(m: { extra?: Record<string, unknown> }): string | null {
  const bar = m.extra?.status_bar;
  return typeof bar === 'string' && bar.trim() ? bar : null;
}

/** 该消息是否被 max_tokens 截断(extra.truncated;后端按 finish_reason=length 写入,
 *  finish 事件到达时前端也会即时写入,两者同键,刷新前后表现一致) */
function truncated(m: { extra?: Record<string, unknown> }): boolean {
  return m.extra?.truncated === true;
}

/** 当前 swipe 版本序号(1-based;无多版本返回 0,供角标与切换按钮) */
function swipePosition(m: { extra?: Record<string, unknown> }): number {
  const idx = m.extra?.swipe_id;
  return typeof idx === 'number' && Number.isInteger(idx) ? idx + 1 : 0;
}

/** swipe 版本总数(无多版本返回 0,隐藏切换 UI) */
function swipeTotal(m: { extra?: Record<string, unknown> }): number {
  const arr = m.extra?.swipes;
  return Array.isArray(arr) ? arr.length : 0;
}

// 消息渲染文本的组装已内联到下方 renderTextSource computed(需参与节流依赖链),
// 原独立函数 renderText() 随之移除。

/** iframe srcdoc 注入用:HTML 属性转义(资源页原文仅作 srcdoc 字符串,不拼接进本页面) */
function escapeAttr(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

/**
 * 消息渲染面板占位 HTML(TH-render 等价物,阶段五 5b):消息正文含整页 HTML
 * 代码块(含 <html 与 <head/<body 标记)时,包面板壳(标题栏 + 折叠按钮 +
 * iframe 宿主文档 + 说明),面板 HTML 由 ChatWindow 的 hydrateRenderPanels 经 postMessage 投递。
 * code 存入 data-kd-render-code 属性(HTML 转义),宿主用 textContent 取回原文。
 * seed 用于生成稳定容器 id(同一消息重渲染复用,避免 iframe 重复创建)。
 */
function buildRenderPanelHtml(code: string, seed: string): string {
  const id = `sv-rp-${seed}`;
  const escCode = code
    .replace(/&/g, '&amp;')
    .replace(/"/g, '&quot;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
  // 稳定 nonce(放 URL fragment,不随 HTTP 请求发送):同一消息重渲染产出相同 HTML,
  // v-html 不变则 iframe 不被重建(随机 nonce 会导致每次重算都重建面板)
  const nonce = stableFrameNonce(`panel${seed}`);
  return (
    `<div class="sv-render-panel" id="${escapeAttr(id)}" data-kd-render-panel="1" data-kd-render-nonce="${escapeAttr(nonce)}" data-kd-render-code="${escCode}">` +
    `<div class="sv-render-panel-head">` +
    `<span class="sv-render-panel-title">渲染面板</span>` +
    `<button type="button" class="sv-render-panel-toggle" title="折叠回代码块">收起 ▾</button>` +
    `</div>` +
    `<iframe class="sv-render-panel-frame" data-kd-render-frame="1" sandbox="allow-scripts" ` +
    `referrerpolicy="no-referrer" src="/render-frame.html#${escapeAttr(nonce)}" title="渲染面板"></iframe>` +
    `<div class="sv-render-panel-note">面板 HTML 由消息正文代码块渲染,已隔离加载(无网络、无宿主访问)。</div>` +
    `</div>`
  );
}

/**
 * 流式渲染节流(批次 G.1):
 *
 * 问题:后端逐 token 推送、sseReducer 逐 token 就地追加 `m.content`,若 `html` computed
 * 直接依赖 content,则每个 token 都对**整条增长中的消息**重跑 markdown + 重设 `v-html`,
 * 长回复呈 O(n²)(渲染成本随累计长度线性增长 × token 数)。
 *
 * 处置(不改数据层,内容仍逐 token 累积,不丢字):
 *   渲染**只读 `paintText`**,它是一份「按帧推进的文本快照」——
 *   - 非流式(streaming !== true):`paintText` 恒等于最新渲染文本,行为与节流前一致;
 *   - 流式中:content 变化只**记录**待绘文本并排期一帧(rAF 合帧),帧到达时统一推进
 *     `paintText`。一帧内到达的多个 token 因此只触发一次 markdown 重算。
 *
 * 关键取舍:必须让 computed **不直接依赖 content**——否则 Vue 的依赖追踪会因 content
 * 变化立即失效并重算,ref 节拍无法阻止(这正是初版 `void renderTick.value` 写法无效的原因)。
 * 故此处以「快照 + 按文本内容缓存」实现:computed 依赖 paintText(节拍变量),
 * 非流式时 paintText 与 content 同步,语义不变。
 *
 * rAF 在 node 测试环境不存在,退化用 16ms 定时器。
 */
const renderTextSource = computed(() => {
  void props.scriptHash; // 脚本版本变化时强制重算依赖链
  return buildMessageRenderText(
    displayText(props.m),
    statusBar(props.m),
    hasStatusScript.value,
  );
});

/** 实际参与渲染的文本:非流式直通;流式按帧推进(见上方说明) */
const paintText = ref('');

// 可变状态须全部声明在下方 immediate watch **之前**:immediate 会立即执行回调,
// 若用 let 声明在 watch 之后会命中 TDZ(ReferenceError)。
let pendingText: string | null = null;
/** 待执行的绘制句柄:rAF 与 setTimeout 返回类型不同,但此处置为同一数值槽位,
 *  两种环境互不混用(分支内独立赋值/取消),无需断言。 */
let paintHandle: number | null = null;

function paint(): void {
  paintHandle = null;
  if (pendingText !== null) {
    paintText.value = pendingText;
    pendingText = null;
  }
}

/** 排期一次绘制(同帧内重复调用被合并;rAF 缺失时退化 16ms 定时器) */
function schedulePaint(): void {
  if (paintHandle !== null) return;
  if (typeof requestAnimationFrame === 'function') {
    paintHandle = requestAnimationFrame(paint);
  } else {
    paintHandle = setTimeout(paint, 16);
  }
}

watch(
  renderTextSource,
  (next) => {
    if (!props.m.streaming) {
      paintText.value = next; // 非流式:立即同步(历史消息/终态消息零延迟)
      return;
    }
    pendingText = next;
    schedulePaint();
  },
  { immediate: true },
);

onUnmounted(() => {
  if (paintHandle !== null) {
    if (typeof cancelAnimationFrame === 'function') cancelAnimationFrame(paintHandle);
    else clearTimeout(paintHandle);
    paintHandle = null;
  }
});

/**
 * assistant 消息渲染(缓存于 computed,键见下):
 *  - 远程资源界面优先:消息含 `$('body').load('https://…')` → 沙箱 iframe 资源卡片,独立于 HTML 开关。
 *  - 渲染面板:消息正文含整页 HTML 代码块 → 面板壳(与 scoped 注入互斥,独立 iframe 隔离执行)。
 *  - HTML 渲染开启:脚本命中走 scoped HTML(样式作用域化 + 脚本受控执行);未命中走 markdown。
 *  - HTML 渲染关闭:先剥隐藏占位符(<StatusPlaceHolderImpl/> 等),再 markdown。
 *
 * 缓存键(被读取即成为依赖):**paintText**(按帧推进的渲染文本快照,见上方节流说明;
 * 非流式时它同步跟随 content/content_display/extra.status_bar)、
 * props.scripts(渲染时逐字段读取)、props.scriptHash(版本兜底)、props.renderHtml。
 * swipe 切换由 store 改写 content(新对象)→ renderTextSource 变化 → 同步进 paintText
 * (非流式)或下一帧进(流式),渲染随之更新。
 */
const html = computed<string>(() => {
  void props.scriptHash; // 脚本版本变化时强制重算(即便 scripts 引用与字段未触发)
  // 渲染只读按帧推进的 paintText(不直接依赖 m.content):见上方节流说明
  const text = paintText.value;
  const resourceUrl = extractBodyLoadUrl(text);
  if (resourceUrl) {
    return buildRemoteResourceHtml(resourceUrl, `m${props.m.id}`);
  }
  if (isRenderCodeBlock(text)) {
    return buildRenderPanelHtml(text, `m${props.m.id}`);
  }
  if (props.renderHtml && props.scripts.length > 0) {
    // 脚本替换串新引入的 {{user}}/{{char}} 宏随渲染展开(对齐 ST substituteParams;
    // userName 缺省「用户」,与后端 content_display 展开一致)
    const scoped = renderScopedScripts(text, props.scripts, `msg-${props.m.id}`, props.depth, {
      charName: store.currentCharacterName,
    });
    if (scoped) return scoped.html;
  }
  const clean = stripHiddenPlaceholders(text, props.scripts, props.depth);
  return renderMarkdown(clean);
});

/** 状态栏是否以纯文本气泡展示:HTML 渲染开启且角色卡有状态栏脚本时,
 *  状态栏已由 HTML 卡片承载,隐藏纯文本气泡避免重复。 */
const statusBarVisible = computed<boolean>(() => {
  if (!statusBar(props.m)) return false;
  if (props.renderHtml && hasStatusScript.value) return false;
  return true;
});

/** 纯文本展示(用户消息 / system) */
function messagePlain(content: string): string {
  return content || ' ';
}

// ===== 编辑态(文本本地管理,击键不触发父组件重渲染;保存时经事件上抛) =====
const editText = ref('');
watch(
  () => props.editing,
  (on) => {
    editText.value = on ? props.m.content : '';
  },
);

/** swipe 版本切换:store 替换消息对象(新引用),渲染缓存自动失效;
 *  完成后通知父组件重跑脚本调度(中部消息变更不在 O(1) 签名内)。 */
async function onSwipe(target: number): Promise<void> {
  await store.swipeMessage(props.m.id, target);
  emit('mutated');
}

/** 三分支各自的根元素(v-if 同时仅一个挂载),供虚拟滚动观察器定位 */
const rootEl = ref<HTMLElement | null>(null);
defineExpose({ rootEl });
</script>

<template>
  <!-- 用户消息 -->
  <div v-if="m.role === 'user'" ref="rootEl" class="sv-msg user" :data-kd-vm-id="m.id">
    <!-- 编辑态 -->
    <div v-if="editing" class="sv-edit-box">
      <div class="sv-edit-label">编辑用户消息</div>
      <textarea v-model="editText" rows="5"></textarea>
      <div class="sv-edit-actions">
        <button class="sv-btn ghost sv-btn-sm" @click="emit('cancel-edit')">取消</button>
        <button class="sv-btn ghost sv-btn-sm" @click="emit('save-edit', { id: m.id, text: editText, resend: false })">保存</button>
        <button
          class="sv-btn primary sv-btn-sm"
          title="保存修改并丢弃其后所有消息,重新生成"
          @click="emit('save-edit', { id: m.id, text: editText, resend: true })"
        >
          保存并重发
        </button>
      </div>
    </div>
    <!-- 展示态 -->
    <template v-else>
      <div class="sv-msg-bubble" :class="{ 'sv-stream-cursor': m.streaming }">
        {{ messagePlain(displayText(m)) }}
      </div>
      <div v-if="!m.streaming" class="sv-msg-actions">
        <button class="sv-msg-action" @click="emit('start-edit', m)">编辑</button>
        <button class="sv-msg-action" title="丢弃其后所有消息,以本条内容重新生成" @click="store.resendMessage(m.id)">
          重发
        </button>
        <button class="sv-msg-action" @click="store.removeMessage(m.id)">删除</button>
      </div>
    </template>
  </div>

  <!-- assistant 消息 -->
  <div v-else-if="m.role === 'assistant'" ref="rootEl" class="sv-msg assistant" :data-kd-vm-id="m.id">
    <span class="sv-avatar char">
      <img v-if="avatarUrl" :src="avatarUrl" alt="" />
      <template v-else>{{ characterName.charAt(0) }}</template>
    </span>
    <div class="min-w-0">
      <div class="sv-msg-name">{{ characterName }}</div>

      <!-- 编辑态 -->
      <div v-if="editing" class="sv-edit-box">
        <div class="sv-edit-label">编辑消息 · {{ characterName }}</div>
        <textarea v-model="editText" rows="6"></textarea>
        <div class="sv-edit-actions">
          <button class="sv-btn ghost sv-btn-sm" @click="emit('cancel-edit')">取消</button>
          <button class="sv-btn primary sv-btn-sm" @click="emit('save-edit', { id: m.id, text: editText, resend: false })">保存</button>
        </div>
      </div>

      <!-- 展示态 -->
      <template v-else>
        <div
          class="sv-msg-bubble sv-msg-md"
          :class="{ 'sv-stream-cursor': m.streaming }"
          v-html="html"
        ></div>
        <!-- 截断提示(可观测性问题①,2026-09-15):上游 finish_reason=length 表示
             触达 max_tokens、正文被腰斩。任务模式的调用情况面板早有「截断」徽标,
             聊天此前完全静默,用户会把半截回复当完整内容。标记取自 extra.truncated
             (finish 事件即时写入 + 后端落库,刷新后仍在)。 -->
        <div v-if="truncated(m)" class="sv-trunc-note">
          <span class="sv-badge trunc">截断</span>
          <span class="sv-trunc-text">回复达到输出上限被截断,内容可能不完整;可增大「最大生成长度」后重发。</span>
        </div>
        <div v-if="statusBarVisible" class="sv-status-bar">{{ statusBar(m) }}</div>
        <div v-if="!m.streaming" class="sv-msg-actions">
          <!-- 阶段六 6f:多版本切换(◀ n/N ▶)+ 生成新版本;单版本不显示切换 -->
          <template v-if="swipeTotal(m) > 1">
            <button
              class="sv-swipe-btn"
              :disabled="swipePosition(m) <= 1"
              title="上一个版本"
              @click="onSwipe(swipePosition(m) - 2)"
            >◀</button>
            <span class="sv-swipe-count">{{ swipePosition(m) }}/{{ swipeTotal(m) }}</span>
            <button
              class="sv-swipe-btn"
              :disabled="swipePosition(m) >= swipeTotal(m)"
              title="下一个版本"
              @click="onSwipe(swipePosition(m))"
            >▶</button>
          </template>
          <button
            class="sv-msg-action"
            title="保留本条,生成新版本(原内容并入版本列表)"
            @click="store.regenerateMessage(m.id)"
          >生成新版本</button>
          <button class="sv-msg-action" @click="emit('start-edit', m)">编辑</button>
          <button class="sv-msg-action" @click="store.removeMessage(m.id)">删除</button>
        </div>
      </template>
    </div>
  </div>

  <!-- system 消息 -->
  <div v-else ref="rootEl" class="sv-msg system" :data-kd-vm-id="m.id">
    <div class="sv-msg-bubble">{{ displayText(m) }}</div>
  </div>
</template>

<style scoped>
/* 截断提示条(可观测性问题①,2026-09-15)。按 style.css 的设计纪律,新增组件样式
   写在组件 scoped 块内(非 style.css)。视觉沿用至上主义 token:海报红细描边 +
   白底小牌,与调用情况面板的 .sv-badge.trunc 同一语言,左侧红竖线做几何分隔。 */
.sv-trunc-note {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  margin-top: 8px;
  padding: 6px 10px;
  border-left: var(--bw) solid var(--sv-red);
  background: var(--sv-white);
  box-shadow: var(--shadow-card);
}
.sv-trunc-note .sv-trunc-text {
  font-size: var(--text-sm);
  line-height: 1.5;
  color: var(--sv-ink-soft);
  min-width: 0;
}
.sv-trunc-note .sv-badge {
  flex-shrink: 0;
  margin-top: 1px;
}
</style>
