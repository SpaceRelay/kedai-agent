<script setup lang="ts">
// 聊天窗口:会话切换条 + 消息列表(编辑/删除)+ 底部输入区
import { ref, nextTick, watch, computed, onMounted, onBeforeUnmount } from 'vue';
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';
import { renderScopedScripts, stripHiddenPlaceholders, hasStatusPlaceholderScript, buildMessageRenderText, extractBodyLoadUrl, buildRemoteResourceHtml } from '../render';
import { renderMarkdown } from '../markdown';
import { installMvuGlobals } from '../mvu/host';
import { ScriptRunner } from '../scriptRunner';
import { executeCurrentMessageScripts as scheduleCurrentMessageScripts } from '../chatMessageScriptScheduler';
import { isRenderCodeBlock, buildRenderDocument, RENDER_PANEL_CHANNEL } from '../renderPanel';
import { authorizedFetch, BASE } from '../api/client';
import { computeHitRate } from '../contextStats';
import ChatInput from './ChatInput.vue';

const store = useAppStore();
const {
  messages, generating, model, currentSessionId, currentCharacterId, sessions, currentCharacter, currentCharacterName,
  currentGreetings, renderHtml, currentScriptHash, currentScriptAuthorized, mvuVariables, initVarEntries,
} = storeToRefs(store);

const scrollArea = ref<HTMLElement | null>(null);
/** 脚本执行器(内部含 [InitVar] 解析缓存;角色切换/条目变化自动失效) */
const scriptRunner = new ScriptRunner();

/** 当前角色正则脚本(用于 HTML 渲染) */
const currentScripts = computed(() => currentCharacter.value?.regex_scripts ?? []);

/** 角色卡是否含渲染 HTML 状态栏的脚本(命中 <StatusPlaceHolderImpl/> 占位符) */
const hasStatusScript = computed(() => hasStatusPlaceholderScript(currentScripts.value));

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

/**
 * 消息渲染文本:正文 + 状态栏占位符。
 * 带状态栏(extra.status_bar)且角色卡存在 HTML 状态栏脚本时,追加 <StatusPlaceHolderImpl/>
 * 使状态栏被「状态栏」正则脚本识别,渲染为 HTML 卡片(脚本用变量树填充动态数据);
 * 否则原样返回正文(状态栏走纯文本气泡)。渲染与脚本执行共用,保证两者看到的文本一致。
 */
function renderTextFor(m: { id: number; content: string; content_display?: string; extra?: Record<string, unknown> }): string {
  return buildMessageRenderText(displayText(m), statusBar(m), hasStatusScript.value);
}

// 脚本执行:非流式 assistant 消息渲染完成后,按 scopeId 定位容器并执行脚本
async function executeCurrentMessageScripts(): Promise<void> {
  await scheduleCurrentMessageScripts({
    renderHtml: renderHtml.value,
    authorized: currentScriptAuthorized.value,
    scripts: currentScripts.value,
    messages: messages.value,
    characterId: currentCharacter.value?.id ?? '',
    scriptHash: currentScriptHash.value,
    initVarEntries: initVarEntries.value,
    mvuVariables: mvuVariables.value,
    isAuthorized: store.isCharacterScriptAuthorized,
    getScrollArea: () => scrollArea.value,
    afterRender: nextTick,
    renderText: renderTextFor,
    renderScripts: renderScopedScripts,
    runMessageScripts: (...args) => scriptRunner.runMessageScripts(...args),
  });
}

onMounted(async () => {
  installMvuGlobals();
  await executeCurrentMessageScripts();
  // 初始消息(会话历史)可能在 watch 建立前已渲染,资源卡片 iframe 需要显式补一次
  await hydrateRemoteResources();
  // 渲染面板(TH-render 等价物)同理由 watch 驱动,初始历史补一次
  await hydrateRenderPanels();
  // 渲染面板折叠按钮:全局事件委托(面板由 v-html 动态注入)
  scrollArea.value?.addEventListener('click', onRenderPanelToggleClick);
});

onBeforeUnmount(() => {
  scrollArea.value?.removeEventListener('click', onRenderPanelToggleClick);
  scriptRunner.cleanup();
});

/**
 * 消息渲染面板占位 HTML(TH-render 等价物,阶段五 5b):消息正文含整页 HTML
 * 代码块(含 <html 与 <head/<body 标记)时,包面板壳(标题栏 + 折叠按钮 +
 * iframe 宿主文档 + 说明),面板 HTML 由 hydrateRenderPanels 经 postMessage 投递。
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
  // 随机 nonce 放 URL fragment(不随 HTTP 请求发送),宿主文档 ready/boot 双向认证
  const nonce = `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`;
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
 * 渲染面板投递(阶段五 5b):面板 iframe 加载后,宿主经 postMessage 投递面板 HTML,
 * 宿主文档 appendChild 注入执行(仿 hydrateRemoteResources 的 ready/boot 双向认证 +
 * 超时兜底;面板 HTML 直接内嵌于 data-kd-render-code,无后端代理)。
 * 高度自适应:面板内 parent.postMessage 上报 {type:'kd-panel-resize', height},
 * 宿主据此调整 iframe 高度;事件上报 {type:'kd-panel-event'} 走 console 记录(扩展位)。
 */
async function hydrateRenderPanels(): Promise<void> {
  if (!scrollArea.value) return;
  const panels = scrollArea.value.querySelectorAll<HTMLElement>('[data-kd-render-panel="1"]');
  for (const panel of panels) {
    const frame = panel.querySelector<HTMLIFrameElement>('[data-kd-render-frame="1"]');
    if (!frame || frame.dataset.kdLoaded) continue;
    const nonce = panel.dataset.kdRenderNonce;
    const code = panel.dataset.kdRenderCode;
    if (!nonce || code === undefined) continue;
    const html = buildRenderDocument(code, RENDER_PANEL_CHANNEL, nonce);
    const win = frame.contentWindow;
    if (!win) {
      // 宿主文档尚未加载完成(contentWindow 为 null):延迟重试(仿资源卡片);
      // 此时 kdLoaded 未标记,重扫会再次进入本分支直至就绪
      const retry = (attempt: number): void => {
        if (frame.dataset.kdLoaded || !frame.isConnected) return;
        if (frame.contentWindow) void hydrateRenderPanels();
        else if (attempt < 50) setTimeout(() => retry(attempt + 1), 300);
      };
      setTimeout(() => retry(1), 300);
      continue;
    }
    frame.dataset.kdLoaded = '1';
    // 高度上报监听(宿主文档面板脚本触发;按 nonce 认证,防伪造面板)
    const onSize = (ev: MessageEvent): void => {
      const m = ev.data;
      if (!m || ev.source !== win || m.channel !== RENDER_PANEL_CHANNEL || m.nonce !== nonce) return;
      if (m.type === 'kd-panel-resize' && typeof m.height === 'number') {
        const h = Math.min(Math.max(Math.round(m.height), 80), 3000);
        if (frame.style.height !== `${h}px`) frame.style.height = `${h}px`;
      } else if (m.type === 'kd-panel-event') {
        console.info('[kedai-render-panel]', m.message ?? '');
      }
    };
    window.addEventListener('message', onSize);
    // ready 后投递;ready 先于监听发出时用超时兜底直接投递(闩锁只接受第一条 boot)
    let ready = false;
    let injected = false;
    const inject = (): void => {
      if (injected) return;
      injected = true;
      win.postMessage({ channel: RENDER_PANEL_CHANNEL, nonce, type: 'boot', html }, '*');
    };
    const onReady = (ev: MessageEvent): void => {
      const m = ev.data;
      if (ev.source !== win || !m) return;
      if (m.channel === RENDER_PANEL_CHANNEL && m.nonce === nonce && m.type === 'ready' && !ready) {
        ready = true;
        window.removeEventListener('message', onReady);
        inject();
      }
    };
    window.addEventListener('message', onReady);
    setTimeout(() => {
      if (!ready) {
        window.removeEventListener('message', onReady);
        inject();
      }
    }, 12000);
  }
}

/** iframe srcdoc 注入用:HTML 属性转义(资源页原文仅作 srcdoc 字符串,不拼接进本页面) */
function escapeAttr(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

/**
 * 渲染面板折叠切换(阶段五 5b):面板由 v-html 动态注入,直接绑定会随重渲染丢失,
 * 故用 scrollArea 上的全局事件委托。收起 = 隐藏 iframe 并展示代码块原文,
 * 展开 = 恢复 iframe(已注入的宿主文档保留,无需重新投递)。
 */
function onRenderPanelToggleClick(e: MouseEvent): void {
  const btn = (e.target as HTMLElement).closest<HTMLElement>('.sv-render-panel-toggle');
  if (!btn || !scrollArea.value?.contains(btn)) return;
  const panel = btn.closest<HTMLElement>('[data-kd-render-panel="1"]');
  const code = panel?.dataset.kdRenderCode;
  if (!panel || code === undefined) return;
  const frame = panel.querySelector<HTMLElement>('[data-kd-render-frame="1"]');
  const note = panel.querySelector<HTMLElement>('.sv-render-panel-note');
  if (panel.dataset.kdFolded === '1') {
    // 展开回面板
    panel.dataset.kdFolded = '';
    if (frame) frame.style.display = '';
    if (note) note.style.display = '';
    panel.querySelector<HTMLElement>('[data-kd-render-code-text]')?.remove();
    btn.textContent = '收起 ▾';
    void hydrateRenderPanels();
  } else {
    // 收起为代码块原文
    panel.dataset.kdFolded = '1';
    if (frame) frame.style.display = 'none';
    if (note) note.style.display = 'none';
    const codeEl = document.createElement('pre');
    codeEl.dataset.kdRenderCodeText = '1';
    codeEl.textContent = code;
    panel.appendChild(codeEl);
    btn.textContent = '展开 ▸';
  }
}

/**
 * 远程资源界面加载:卡片渲染后,父页面(带 token)经 /api/resource/proxy 取作者
 * 页面 HTML,投递给独立宿主文档 /resource-frame.html(iframe src 加载,不继承
 * 父页面 CSP,作者脚本可执行;宿主文档 DOM 重建触发脚本,详见其模板)。
 * 绕开作者服务器 X-Frame-Options 与 CSP 对 srcdoc/iframe 直嵌的拦截;
 * iframe sandbox 无 same-origin,与主页面完全隔离。
 */
async function hydrateRemoteResources(): Promise<void> {
  if (!scrollArea.value) return;
  const frames = scrollArea.value.querySelectorAll<HTMLIFrameElement>('iframe[data-kd-resource-frame="1"]');
  for (const frame of frames) {
    if (frame.dataset.kdLoaded) continue;
    const card = frame.closest('[data-kd-resource-url]') as HTMLElement | null;
    const rawUrl = card?.dataset.kdResourceUrl;
    const nonce = card?.dataset.kdResourceNonce;
    if (!rawUrl || !nonce) continue;
    const url = decodeURIComponent(rawUrl);
    const win = frame.contentWindow;
    if (!win) {
      // iframe 宿主文档尚未加载完成(contentWindow 为 null):延迟重试,
      // 不依赖 load 事件(load 可能先于监听触发或 lazy 加载延后,导致永不被投递)。
      // 每 300ms 重试,最多 50 次(15s);期间 kdLoaded 未标记,超时后放弃(卡片留有"新窗口打开"兜底)。
      const retry = (attempt: number): void => {
        if (frame.dataset.kdLoaded || !frame.isConnected) return;
        if (frame.contentWindow) {
          void hydrateRemoteResources();
        } else if (attempt < 50) {
          setTimeout(() => retry(attempt + 1), 300);
        }
      };
      setTimeout(() => retry(1), 300);
      continue;
    }
    frame.dataset.kdLoaded = '1';
    // 等宿主文档 ready 后投递(ready/boot 双向认证);ready 先于监听发出时
    // 用超时兜底直接投递(宿主文档闩锁只接受第一条 boot,重复投递无害)
    let ready = false;
    let injected = false;
    const inject = async (): Promise<void> => {
      if (injected) return;
      injected = true;
      try {
        const res = await authorizedFetch(`${BASE}/resource/proxy?url=${encodeURIComponent(url)}`, undefined, false);
        const body = (await res.json().catch(() => ({}))) as { ok?: boolean; html?: string; base_url?: string; error?: string };
        if (body.ok) {
          const html = body.base_url ? `<base href="${escapeAttr(body.base_url)}">\n${body.html ?? ''}` : (body.html ?? '');
          win.postMessage({ channel: 'kedai-resource-frame-v1', nonce, type: 'boot', html }, '*');
        } else {
          frame.srcdoc = `<div style="padding:16px;font-family:sans-serif;color:#c0392b">资源界面加载失败：${escapeAttr(body.error ?? '未知错误')}</div>`;
        }
      } catch (e) {
        frame.srcdoc = `<div style="padding:16px;font-family:sans-serif;color:#c0392b">资源界面加载失败：${escapeAttr(String((e as Error).message || '网络错误'))}</div>`;
      }
    };
    const onMessage = (ev: MessageEvent): void => {
      const m = ev.data;
      if (ev.source !== win || !m) return;
      if (m.channel === 'kedai-resource-frame-v1' && m.nonce === nonce && m.type === 'ready' && !ready) {
        ready = true;
        window.removeEventListener('message', onMessage);
        void inject();
      }
    };
    window.addEventListener('message', onMessage);
    // ready 等待上限:资源页(如吸血鬼卡 1.25MB)经后端代理下载需要数秒,
    // 4s 内 ready 未到时直接投递易在宿主文档监听就绪前丢失 boot → 界面空白。
    // 放宽到 12s 覆盖代理下载 + 宿主文档重建时间。
    setTimeout(() => {
      if (!ready) {
        window.removeEventListener('message', onMessage);
        void inject();
      }
    }, 12000);
  }
}

// 消息变化时自动滚到底部
watch(
  () => messages.value.map((m) => displayText(m)).join('|') + generating.value,
  async () => {
    await nextTick();
    if (scrollArea.value) scrollArea.value.scrollTop = scrollArea.value.scrollHeight;
  },
);

watch(
  () =>
    messages.value.map((m) => renderTextFor(m)).join('|') + renderHtml.value + currentScripts.value.length + currentScriptHash.value + currentScriptAuthorized.value,
  async () => {
    await nextTick();
    await Promise.allSettled([executeCurrentMessageScripts(), hydrateRemoteResources(), hydrateRenderPanels()]);
  },
  { flush: 'post', immediate: true },
);

// 会话切换
async function onSessionChange(e: Event): Promise<void> {
  const id = (e.target as HTMLSelectElement).value;
  if (id) await store.switchSession(id);
}

// ===== 多开场切换 =====
/** 开场选择器:null=关闭;mode='new'=新建会话时选择开场,'switch'=当前会话内重置开场 */
const greetingPickerOpen = ref(false);
const greetingPickerMode = ref<'new' | 'switch'>('new');

/** 打开开场选择器(调用方已保证 currentGreetings.length > 1) */
function openGreetingPicker(mode: 'new' | 'switch'): void {
  if (generating.value) return;
  greetingPickerMode.value = mode;
  greetingPickerOpen.value = true;
}

/** 选定开场:mode=new → 用该开场新建会话;mode=switch → 当前会话清空并重置为该开场 */
async function pickGreeting(index: number): Promise<void> {
  greetingPickerOpen.value = false;
  try {
    if (greetingPickerMode.value === 'new') {
      await store.newSession(index);
    } else {
      await store.switchGreeting(index);
    }
  } catch (err) {
    alert(`开场切换失败:${(err as Error).message}`);
  }
}

/** 当前请求的 prompt 缓存命中率:命中率计算抽到 contextStats.ts(computeHitRate,与优化面板共用) */
const hitRate = computed<number | null>(() => computeHitRate(store.lastUsage));

/** 头像来源(优先角色头像) */
const avatarUrl = computed(() => {
  const c = currentCharacter.value;
  if (!c?.avatar_path) return null;
  return `/api/avatars/${c.avatar_path.split(/[\\/]/).pop()}`;
});

/**
 * assistant 消息渲染:
 *  - 远程资源界面优先:消息含 `$('body').load('https://…')`(作者下载资源界面,
 *    如「板板鸭」模板开场)→ 渲染为沙箱 iframe 资源卡片,独立于 HTML 开关
 *    (iframe 隔离加载,不执行到主页面,安全性与 markdown 相当)。
 *  - HTML 渲染开启:脚本命中走 scoped HTML(样式作用域化 + 脚本受控执行);
 *    未命中走 markdown。UpdateVariable 块由 renderScopedScripts 内剥离。
 *  - HTML 渲染关闭:先剥隐藏占位符(<StatusPlaceHolderImpl/> 等),再 markdown。
 */
function assistantHtml(m: { id: number; content: string; content_display?: string; extra?: Record<string, unknown> }): string {
  const text = renderTextFor(m);
  const resourceUrl = extractBodyLoadUrl(text);
  if (resourceUrl) {
    return buildRemoteResourceHtml(resourceUrl, `m${m.id}`);
  }
  // 渲染面板(TH-render 等价物):消息正文含整页 HTML 代码块(<html + <head/<body)
  // 时优先于 scoped 注入(两者互斥;面板独立 iframe 隔离执行,不依赖 HTML 渲染开关)
  if (isRenderCodeBlock(text)) {
    return buildRenderPanelHtml(text, `m${m.id}`);
  }
  if (renderHtml.value && currentScripts.value.length > 0) {
    const scoped = renderScopedScripts(text, currentScripts.value, `msg-${m.id}`);
    if (scoped) return scoped.html;
  }
  const clean = stripHiddenPlaceholders(text, currentScripts.value);
  return renderMarkdown(clean);
}

/** 状态栏是否以纯文本气泡展示:HTML 渲染开启且角色卡有状态栏脚本时,
 *  状态栏已由 HTML 卡片承载,隐藏纯文本气泡避免重复。 */
function statusBarVisible(m: { extra?: Record<string, unknown> }): boolean {
  if (!statusBar(m)) return false;
  if (renderHtml.value && hasStatusScript.value) return false;
  return true;
}

function confirmScriptAuthorization(): void {
  if (currentScriptAuthorized.value) {
    if (confirm(`撤销“${currentCharacterName.value}”当前脚本版本的执行授权？\n\n安全 HTML 仍可显示，但角色卡 JavaScript 将停止执行。`)) {
      store.revokeCharacterScripts(currentCharacter.value?.id ?? '');
    }
    return;
  }
  const accepted = confirm(
    `允许“${currentCharacterName.value}”执行角色卡 JavaScript？\n\n` +
    '脚本可能修改消息卡片并读取当前会话变量。Kedai 会隔离宿主 window/document,并遮蔽网络、存储与 API token，' +
    '但兼容执行器不是浏览器级安全沙箱。仅在信任角色卡来源时开启。\n\n' +
    '授权仅适用于此角色与当前脚本内容；脚本变化后会自动失效。',
  );
  if (accepted) store.authorizeCurrentCharacterScripts();
}

/** 纯文本展示(用户消息 / system) */
function messagePlain(content: string): string {
  return content || ' ';
}

/** 编辑消息 */
const editingId = ref<number | null>(null);
const editingText = ref('');
function startEdit(m: { id: number; content: string }): void {
  editingId.value = m.id;
  editingText.value = m.content;
}
async function saveEdit(resend = false): Promise<void> {
  if (editingId.value === null || !currentSessionId.value) return;
  const id = editingId.value;
  const text = editingText.value;
  try {
    if (resend) {
      await store.resendMessage(id, text);
    } else {
      await store.updateMessage(id, text);
    }
  } catch (err) {
    alert(`编辑失败:${(err as Error).message}`);
    return;
  }
  editingId.value = null;
}

// 切换会话/角色时清空消息编辑草稿:ChatWindow 常驻挂载,若不清除,
// 旧草稿会残留到新会话并误保存到别的角色/会话(「草稿未正确削除」)。
watch(
  [currentSessionId, currentCharacterId],
  () => {
    editingId.value = null;
    editingText.value = '';
  },
);
</script>

<template>
  <section class="flex min-h-0 min-w-0 flex-1 flex-col">
    <!-- 顶栏 -->
    <header class="sv-topbar">
      <span class="flex items-center gap-2">
        <span class="sv-supreme blue" style="width: 10px; height: 10px" />
        <span class="sv-topbar-title">{{ currentCharacterName }}</span>
        <span v-if="model" class="sv-topbar-sub">{{ model }}</span>
      </span>

      <!-- 会话切换 -->
      <span v-if="sessions.length > 1" class="flex items-center gap-2">
        <label class="sv-topbar-sub" for="session-select">会话</label>
        <select
          id="session-select"
          class="sv-session-select"
          :value="currentSessionId ?? ''"
          @change="onSessionChange"
        >
          <option v-for="s in sessions" :key="s.id" :value="s.id">
            {{ s.title }} · {{ new Date(s.updated_at).toLocaleString('zh-CN', { hour12: false }) }}
          </option>
        </select>
        <button
          class="sv-icon-btn"
          title="新建会话(多开场角色可先选开场)"
          @click="currentGreetings.length > 1 ? openGreetingPicker('new') : store.newSession()"
        >＋</button>
      </span>

      <span class="sv-topbar-right">
        <!-- 开场切换(多开场角色:重置当前会话并以所选开场重新开始) -->
        <button
          v-if="currentGreetings.length > 1"
          class="sv-btn ghost sv-btn-sm"
          title="切换开场(清空当前会话并以所选开场重新开始)"
          @click="openGreetingPicker('switch')"
        >
          开场
        </button>
        <!-- HTML 渲染开关(Toggle 样式;按角色卡记忆;常驻显示——无脚本的卡同样记忆开关状态,
             只是无脚本时开关不产生 scoped 渲染效果) -->
        <button
          type="button"
          class="sv-render-toggle-v2"
          :aria-pressed="renderHtml"
          :title="renderHtml ? '安全 HTML 渲染已开启(仅此角色卡记忆)' : '安全 HTML 渲染已关闭(仅此角色卡记忆)'"
          @click="store.renderHtml = !store.renderHtml"
        >
          <span class="toggle-track" :class="{ on: renderHtml }">
            <span class="toggle-thumb" />
          </span>
          <span class="toggle-label">HTML</span>
        </button>
        <!--
          JS 授权按钮:常驻显示,无论角色卡是否带脚本。
          执行门禁是「有脚本 && 已授权」(chatMessageScriptScheduler),与 replace_string
          是否含字面 <script 无关;按旧条件,脚本不含 <script 字样的卡(如爱托邦卡)
          按钮消失 → 永远无法授权/撤销,与执行条件不一致。无脚本卡授权仅记忆状态,
          执行层因脚本数组为空永不执行。
        -->
        <button
          type="button"
          class="sv-btn ghost sv-btn-sm"
          :aria-pressed="currentScriptAuthorized"
          :title="currentScriptAuthorized ? '撤销当前角色脚本授权' : '查看风险并授权当前角色脚本'"
          @click="confirmScriptAuthorization"
        >
          JS {{ currentScriptAuthorized ? '已授权' : '禁用' }}
        </button>
        <span class="sv-token-inline">
          CTX {{ store.contextTokens.toLocaleString() }}
          <span class="sv-supreme pink" style="width: 6px; height: 6px" />
          {{ store.lastUsage?.total_tokens ?? 0 }}
          <template v-if="hitRate !== null">
            <span class="sv-supreme green" style="width: 6px; height: 6px" />
            命中 {{ hitRate }}%
          </template>
        </span>
      </span>
    </header>

    <!-- 消息画布 -->
    <div ref="scrollArea" class="sv-canvas">
      <div v-if="messages.length === 0" class="sv-empty">
        <div class="sv-empty-geo">
          <span class="sq black" />
          <span class="sq pink" />
          <span class="sq deep" />
          <i class="diag" />
        </div>
        <p>选择左侧角色开始对话,或上传一张角色卡。</p>
        <p style="font-size: 11px; color: var(--sv-ink-faint)">
          输入含算式(如「帮我算 12*34」)时,Agent 会自动调用计算器工具。
        </p>
      </div>

      <!-- 消息列表 -->
      <template v-for="m in messages" :key="m.id">
        <div v-if="m.role === 'user'" class="sv-msg user">
          <!-- 编辑态 -->
          <div v-if="editingId === m.id" class="sv-edit-box">
            <div class="sv-edit-label">编辑用户消息</div>
            <textarea v-model="editingText" rows="5"></textarea>
            <div class="sv-edit-actions">
              <button class="sv-btn ghost sv-btn-sm" @click="editingId = null">取消</button>
              <button class="sv-btn ghost sv-btn-sm" @click="saveEdit()">保存</button>
              <button class="sv-btn primary sv-btn-sm" title="保存修改并丢弃其后所有消息,重新生成" @click="saveEdit(true)">
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
              <button class="sv-msg-action" @click="startEdit(m)">编辑</button>
              <button class="sv-msg-action" title="丢弃其后所有消息,以本条内容重新生成" @click="store.resendMessage(m.id)">
                重发
              </button>
              <button class="sv-msg-action" @click="store.removeMessage(m.id)">删除</button>
            </div>
          </template>
        </div>
        <div v-else-if="m.role === 'assistant'" class="sv-msg assistant">
          <span class="sv-avatar char">
            <img v-if="avatarUrl" :src="avatarUrl" alt="" />
            <template v-else>{{ currentCharacterName.charAt(0) }}</template>
          </span>
          <div class="min-w-0">
            <div class="sv-msg-name">{{ currentCharacterName }}</div>

            <!-- 编辑态 -->
            <div v-if="editingId === m.id" class="sv-edit-box">
              <div class="sv-edit-label">编辑消息 · {{ currentCharacterName }}</div>
              <textarea v-model="editingText" rows="6"></textarea>
              <div class="sv-edit-actions">
                <button class="sv-btn ghost sv-btn-sm" @click="editingId = null">取消</button>
                <button class="sv-btn primary sv-btn-sm" @click="saveEdit()">保存</button>
              </div>
            </div>

            <!-- 展示态 -->
            <template v-else>
              <div
                class="sv-msg-bubble sv-msg-md"
                :class="{ 'sv-stream-cursor': m.streaming }"
                v-html="assistantHtml(m)"
              ></div>
              <div v-if="statusBarVisible(m)" class="sv-status-bar">{{ statusBar(m) }}</div>
              <div v-if="!m.streaming" class="sv-msg-actions">
                <!-- 阶段六 6f:多版本切换(◀ n/N ▶)+ 生成新版本;单版本不显示切换 -->
                <template v-if="swipeTotal(m) > 1">
                  <button
                    class="sv-msg-action"
                    :disabled="swipePosition(m) <= 1"
                    title="上一个版本"
                    @click="store.swipeMessage(m.id, swipePosition(m) - 2)"
                  >◀</button>
                  <span class="sv-msg-action-label">{{ swipePosition(m) }}/{{ swipeTotal(m) }}</span>
                  <button
                    class="sv-msg-action"
                    :disabled="swipePosition(m) >= swipeTotal(m)"
                    title="下一个版本"
                    @click="store.swipeMessage(m.id, swipePosition(m))"
                  >▶</button>
                </template>
                <button
                  class="sv-msg-action"
                  title="保留本条,生成新版本(原内容并入版本列表)"
                  @click="store.regenerateMessage(m.id)"
                >生成新版本</button>
                <button class="sv-msg-action" @click="startEdit(m)">编辑</button>
                <button class="sv-msg-action" @click="store.removeMessage(m.id)">删除</button>
              </div>
            </template>
          </div>
        </div>
        <div v-else class="sv-msg system">
          <div class="sv-msg-bubble">{{ displayText(m) }}</div>
        </div>
      </template>

      <!-- 思考中占位 -->
      <div v-if="generating" class="sv-msg assistant">
        <span class="sv-avatar char">
          <template v-if="avatarUrl"><img :src="avatarUrl" alt="" /></template>
          <template v-else>{{ currentCharacterName.charAt(0) }}</template>
        </span>
        <div>
          <div class="sv-msg-name">{{ currentCharacterName }}</div>
          <div class="sv-msg-bubble" style="border-top-width: 2px">
            <span class="sv-thinking"><i /><i /><i /></span>
          </div>
        </div>
      </div>
    </div>

    <!-- 底部输入区 -->
    <ChatInput />

    <!-- 开场选择器(多开场角色:新建会话选择 / 会话内切换) -->
    <div v-if="greetingPickerOpen" class="sv-modal-mask" @click.self="greetingPickerOpen = false">
      <div class="sv-modal sm">
        <div class="sv-modal-head">
          <h2 class="flex items-center gap-2">
            <span class="sv-supreme pink-deep" style="width: 18px; height: 18px" />
            {{ greetingPickerMode === 'new' ? '选择开场 · 新建会话' : '选择开场 · 重置当前会话' }}
          </h2>
          <button class="sv-btn ghost sv-btn-square" @click="greetingPickerOpen = false">✕</button>
        </div>
        <div class="sv-modal-body">
          <p class="sv-note" style="margin-bottom: 12px">
            {{ greetingPickerMode === 'new'
              ? '选择一个开场,以其作为新会话的第一条消息。'
              : '切换开场将清空当前会话全部消息,并以所选开场重新开始。' }}
          </p>
          <div class="sv-datalist">
            <button
              v-for="(g, i) in currentGreetings"
              :key="i"
              class="sv-data-row"
              style="width: 100%; text-align: left; cursor: pointer; align-items: flex-start; font-family: var(--font-body)"
              @click="pickGreeting(i)"
            >
              <div class="info" style="min-width: 0">
                <b style="display: flex; align-items: center; gap: 8px">
                  {{ i === 0 ? '主开场' : `备用 ${i}` }}
                  <span v-if="i === 0" class="sv-tag sv-tag-on">默认</span>
                </b>
                <span
                  style="display: block; white-space: pre-wrap; word-break: break-word; max-height: 110px; overflow-y: auto; margin-top: 4px"
                >{{ g }}</span>
              </div>
            </button>
          </div>
        </div>
        <div class="sv-modal-foot">
          <button class="sv-btn ghost" @click="greetingPickerOpen = false">取消</button>
        </div>
      </div>
    </div>
  </section>
</template>
