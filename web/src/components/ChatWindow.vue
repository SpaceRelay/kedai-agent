<script setup lang="ts">
// 聊天窗口:顶栏(ChatTopbar)+ 消息列表(编辑/删除,画布容器 RenderPanelHost)+ 底部输入区
import { ref, nextTick, watch, computed, onMounted, onBeforeUnmount } from 'vue';
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';
import { renderScopedScripts, hasStatusPlaceholderScript, buildMessageRenderText } from '../render';
import { installMvuGlobals } from '../mvu/host';
import { ScriptRunner, type SandboxRpcExtensions } from '../scriptRunner';
import { executeCurrentMessageScripts as scheduleCurrentMessageScripts } from '../chatMessageScriptScheduler';
import { enabledCardScriptsOf } from '../cardScripts';
import { cleanupCardScriptSandbox, ensureCardScriptSandbox, makeCardScriptRpcExtensions } from '../cardScriptHost';
import { useVirtualMessages } from '../composables/useVirtualMessages';
import { useResourceFrames } from '../composables/useResourceFrames';
import type { UiMessage } from '../sseReducer';
import ChatInput from './ChatInput.vue';
import ChatMessageItem from './ChatMessageItem.vue';
import ChatTopbar from './ChatTopbar.vue';
import GreetingPickerModal from './GreetingPickerModal.vue';
import RenderPanelHost from './RenderPanelHost.vue';

const store = useAppStore();
const {
  messages, generating, currentSessionId, currentCharacterId, currentCharacter, currentCharacterName,
  renderHtml, currentScriptHash, currentScriptAuthorized, mvuVariables, initVarEntries,
} = storeToRefs(store);

/** 消息画布宿主(渲染面板折叠切换与水合的实现组件;el 为原 scrollArea 容器) */
const panelHost = ref<InstanceType<typeof RenderPanelHost> | null>(null);
const scrollArea = computed<HTMLElement | null>(() => panelHost.value?.el ?? null);
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

/**
 * 消息渲染文本:正文 + 状态栏占位符。
 * 带状态栏(extra.status_bar)且角色卡存在 HTML 状态栏脚本时,追加 <StatusPlaceHolderImpl/>
 * 使状态栏被「状态栏」正则脚本识别,渲染为 HTML 卡片(脚本用变量树填充动态数据);
 * 否则原样返回正文(状态栏走纯文本气泡)。渲染与脚本执行共用,保证两者看到的文本一致。
 */
function renderTextFor(m: { id: number; content: string; content_display?: string; extra?: Record<string, unknown> }): string {
  return buildMessageRenderText(displayText(m), statusBar(m), hasStatusScript.value);
}

/**
 * 脚本块解析缓存(流式性能优化):调度器触发时会为每条历史 assistant 消息执行
 * renderScopedScripts(全量正则 + sanitize,重活),流式期间每个 token 触发一次,
 * 历史消息解析结果却不变。按 (scopeId + scriptHash + 渲染文本) 记忆结果,
 * 历史消息零重算;消息数组被替换(会话切换/刷新历史)或脚本数组引用变化时整体失效。
 */
let scriptBlocksCache = new Map<string, ReturnType<typeof renderScopedScripts>>();
let scriptBlocksOwner: unknown = null;
let scriptBlocksScripts: unknown = null;

function renderScriptsCached(
  text: string,
  scripts: Parameters<typeof renderScopedScripts>[1],
  scopeId: string,
  depth: number,
): ReturnType<typeof renderScopedScripts> {
  if (scriptBlocksOwner !== messages.value || scriptBlocksScripts !== scripts) {
    scriptBlocksCache = new Map();
    scriptBlocksOwner = messages.value;
    scriptBlocksScripts = scripts;
  }
  // depth 参与缓存键:同一条消息在不同楼层深度下脚本适用性不同(实跑问题 7 R2)
  const key = `${scopeId}\n${currentScriptHash.value}\n${depth}\n${text}`;
  const hit = scriptBlocksCache.get(key);
  if (hit !== undefined || scriptBlocksCache.has(key)) return hit ?? null;
  const scoped = renderScopedScripts(text, scripts, scopeId, depth, {
    charName: currentCharacter.value?.chara_name ?? currentCharacter.value?.name ?? '',
  });
  scriptBlocksCache.set(key, scoped);
  return scoped;
}

// 消息级沙箱只读 RPC 扩展(世界书读取 / 聊天消息读取),供首楼脚本获取聊天上下文。
// 实跑问题 7 主因:此前不注入,数据 RPC 落到 DOM 白名单分发抛错 → 沙箱 cleanup
// 摘除全部事件监听。复用卡级沙箱同一实现(opts 与卡级一致:一角色一内嵌世界书 +
// 当前消息列表),不扩大写权限(消息级仍无世界书写入口)。
function messageRpcExtensions(): SandboxRpcExtensions | undefined {
  const id = currentCharacter.value?.id;
  if (!id) return undefined;
  const book = (currentCharacter.value?.data_raw as Record<string, unknown> | undefined)
    ?.character_book as { name?: unknown } | undefined;
  const lorebookName =
    typeof book?.name === 'string' && book.name.trim() ? book.name : null;
  return makeCardScriptRpcExtensions({
    characterId: id,
    lorebookName,
    readMessages: () => messages.value,
  });
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
    charName: currentCharacter.value?.chara_name ?? currentCharacter.value?.name ?? '',
    rpcExtensions: messageRpcExtensions(),
    isAuthorized: store.isCharacterScriptAuthorized,
    getScrollArea: () => scrollArea.value,
    afterRender: nextTick,
    renderText: renderTextFor,
    depthOf: (_m, i, total) => total - 1 - i,
    renderScripts: renderScriptsCached,
    runMessageScripts: (...args) => scriptRunner.runMessageScripts(...args),
  });
}

// 远程资源卡片宿主(扫描/投递/存储同步/TavernHelper RPC 桥,实现见 composable);
// 宽屏按钮点击委托由 composable 挂载时自行接线,window message 监听下方显式接线
const frames = useResourceFrames({ scrollArea, currentCharacter, currentCharacterId });

/** 渲染面板投递(实现已迁入 RenderPanelHost;此处保留薄封装,原调用点不动) */
async function hydrateRenderPanels(): Promise<void> {
  await panelHost.value?.hydrate();
}

onMounted(async () => {
  installMvuGlobals();
  // 资源框架常驻消息监听(ready 重 boot / store-sync 持久化),生命周期与组件一致
  window.addEventListener('message', frames.handleMessage);
  await executeCurrentMessageScripts();
  // 初始消息(会话历史)可能在 watch 建立前已渲染,资源卡片 iframe 需要显式补一次
  await frames.hydrate();
  // 渲染面板(TH-render 等价物)同理由 watch 驱动,初始历史补一次
  await hydrateRenderPanels();
  // 卡级脚本长驻沙箱首次同步(watch 无 immediate:此时 scrollArea 已挂载,
  // 重进恢复的角色若已授权,swipe 世界书联动等卡级脚本从这里起跑)
  void syncCardScriptSandbox();
});

onBeforeUnmount(() => {
  window.removeEventListener('message', frames.handleMessage);
  scriptRunner.cleanup();
  cleanupCardScriptSandbox();
});

/**
 * 消息列表变更纪元:中部消息被编辑保存 / swipe 切换(经子组件 mutated 事件)时 +1。
 * O(1) 签名只跟踪尾部消息,中部内容变更借纪元显式触发脚本重跑与滚动,语义与原全量签名一致。
 */
const listEpoch = ref(0);

/** 编辑中的消息 id(单编辑语义;编辑文本由子组件本地管理,击键不触发列表重渲染) */
const editingId = ref<number | null>(null);

// 消息变化时自动滚到底部
// 签名 O(1):仅读「长度 + 最后一条消息 + 生成态 + 编辑态 + 纪元」,
// 替代原 messages.map(displayText).join('|') 的全量遍历(流式每 token 拼全量字符串)。
watch(
  () => {
    const arr = messages.value;
    const last = arr[arr.length - 1];
    return [
      arr.length,
      last?.id ?? 0,
      last?.streaming ? 1 : 0,
      typeof last?.extra?.swipe_id === 'number' ? last.extra.swipe_id : -1,
      last ? displayText(last) : '',
      generating.value,
      editingId.value ?? 0,
      listEpoch.value,
    ].join('\u0001');
  },
  async () => {
    await nextTick();
    if (scrollArea.value) scrollArea.value.scrollTop = scrollArea.value.scrollHeight;
  },
);

// 渲染后副作用(脚本执行 + 资源卡片/渲染面板投递)
// 签名同样 O(1):最后一条消息渲染文本 + HTML 开关 + 脚本版本(hash/长度/授权)+ 编辑态 + 纪元;
// 历史消息内容不可变(编辑/swipe 走纪元),脚本列表变化经 hash/长度捕获。
watch(
  () => {
    const arr = messages.value;
    const last = arr[arr.length - 1];
    return [
      arr.length,
      last?.id ?? 0,
      last?.streaming ? 1 : 0,
      last ? renderTextFor(last) : '',
      renderHtml.value,
      currentScripts.value.length,
      currentScriptHash.value,
      currentScriptAuthorized.value,
      editingId.value ?? 0,
      listEpoch.value,
    ].join('\u0001');
  },
  async () => {
    await nextTick();
    await Promise.allSettled([executeCurrentMessageScripts(), frames.hydrate(), hydrateRenderPanels()]);
  },
  { flush: 'post', immediate: true },
);

// 卡级脚本(角色卡内嵌酒馆助手脚本)长驻沙箱:随角色选中常驻,监听 swipe 事件流、
// 读写内嵌世界书(舰娘卡「随开场白切换世界书」)。只过 JS 授权门禁,不要求 renderHtml
// 开关(它不是渲染)。键 = characterId:scriptHash(哈希含卡级脚本正文):换卡/卡更新
// 脚本/授权变化→重建或销毁,由 ensureCardScriptSandbox 内部幂等处理。
async function syncCardScriptSandbox(force = false): Promise<void> {
  const id = currentCharacterId.value;
  const authorized = currentScriptAuthorized.value;
  const hash = currentScriptHash.value;
  if (!id || !authorized || !hash) {
    cleanupCardScriptSandbox();
    return;
  }
  // 列表项无 data_raw(体积大):卡级脚本与主世界书名必须读详情缓存
  const detail = await store.fetchCharacterDetail(id);
  // await 期间可能已切卡/撤授权/哈希升级:以最新值为准,失效则放弃本次启动
  if (currentCharacterId.value !== id || !currentScriptAuthorized.value || currentScriptHash.value !== hash) return;
  const scripts = enabledCardScriptsOf(detail);
  if (scripts.length === 0 || !scrollArea.value) {
    cleanupCardScriptSandbox();
    return;
  }
  const book = (detail?.data_raw as Record<string, unknown> | undefined)?.character_book as
    | { name?: unknown }
    | undefined;
  await ensureCardScriptSandbox({
    characterId: id,
    scriptHash: hash,
    scripts,
    container: scrollArea.value,
    lorebookName: typeof book?.name === 'string' && book.name.trim() ? book.name : null,
    readMessages: () => messages.value,
    force,
  });
}
watch([currentCharacterId, currentScriptAuthorized, currentScriptHash], () => {
  void syncCardScriptSandbox();
});

// 实跑问题 7 R5:卡级脚本常在 boot 时扫描聊天历史注入界面。历史尚未就绪时启动会
// 读到空列表且不再补跑 → 首屏空。这里在「历史首次变为非空」时为当前会话强制重建一次;
// 会话切换后重置闩锁,下一次历史就绪同样补跑。
let cardSandboxHistoryReady = false;
watch(
  () => `${currentSessionId.value ?? ''}\u0001${messages.value.length}`,
  () => {
    const hasHistory = messages.value.length > 0;
    if (!hasHistory) {
      cardSandboxHistoryReady = false;
      return;
    }
    if (cardSandboxHistoryReady) return;
    cardSandboxHistoryReady = true;
    void syncCardScriptSandbox(true);
  },
);

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

/** 头像来源(优先角色头像) */
const avatarUrl = computed(() => {
  const c = currentCharacter.value;
  if (!c?.avatar_path) return null;
  return `/api/avatars/${c.avatar_path.split(/[\\/]/).pop()}`;
});

// assistant 消息 HTML 渲染逻辑已迁入 ChatMessageItem(按消息 computed 缓存);
// 本组件仅保留列表编排、脚本调度与滚动行为。

/** 进入编辑态(编辑文本在 ChatMessageItem 本地维护,进入时用消息原文初始化) */
function startEdit(m: UiMessage): void {
  editingId.value = m.id;
}

/** 保存编辑:resend=true 时保存并丢弃其后所有消息重新生成;成功后推进列表纪元(触发脚本重跑) */
async function saveEdit(payload: { id: number; text: string; resend: boolean }): Promise<void> {
  if (!currentSessionId.value) return;
  try {
    if (payload.resend) {
      await store.resendMessage(payload.id, payload.text);
    } else {
      await store.updateMessage(payload.id, payload.text);
    }
  } catch (err) {
    alert(`编辑失败:${(err as Error).message}`);
    return;
  }
  editingId.value = null;
  listEpoch.value += 1;
}

// 切换会话/角色时清空消息编辑草稿:ChatWindow 常驻挂载,若不清除,
// 旧草稿会残留到新会话并误保存到别的角色/会话(「草稿未正确削除」)。
watch(
  [currentSessionId, currentCharacterId],
  () => {
    editingId.value = null;
  },
);

/**
 * 消息列表虚拟滚动(轻量实现,无依赖):长会话(>80 条)时视口缓冲区外的消息
 * 渲染为等高占位 div(实测高度缓存/角色估算),进入缓冲区再真实挂载;
 * 尾部 30 条常驻真实挂载,流式自动吸底行为不变。行挂载状态变化后补跑
 * 脚本执行与资源/面板水合(新挂载的消息可能含脚本容器或 iframe)。
 */
const vm = useVirtualMessages({
  messages,
  scrollArea,
  pinnedId: editingId,
  resetKey: currentSessionId,
  onRowsChanged: () => {
    void Promise.allSettled([executeCurrentMessageScripts(), frames.hydrate(), hydrateRenderPanels()]);
  },
});

// 模板 ref 回调参数被 Vue 推断为 Element | ComponentPublicInstance | null,
// 与虚拟滚动 bindRow 期望的 RowRefTarget(HTMLElement 或带 rootEl 的组件暴露,
// 未导出)不直接兼容;此处仅做类型收窄,运行时透传(resolveRowEl 内部读 rootEl)。
type VmRowTarget = Parameters<typeof vm.bindRow>[1];
function bindRowRef(id: number, target: unknown): void {
  vm.bindRow(id, target as VmRowTarget);
}
</script>

<template>
  <section class="flex min-h-0 min-w-0 flex-1 flex-col">
    <!-- 顶栏(含渲染引导条):会话/开场/渲染开关/JS 授权/Agent 面板/token 统计 -->
    <ChatTopbar @open-greeting-picker="openGreetingPicker" />

    <!-- 消息画布(渲染面板折叠切换与水合由宿主组件承载) -->
    <RenderPanelHost ref="panelHost">
      <div v-if="messages.length === 0" class="sv-empty">
        <div class="sv-empty-geo">
          <span class="sq black" />
          <span class="sq pink" />
          <span class="sq deep" />
          <i class="diag" />
        </div>
        <p>选择左侧角色开始对话,或上传一张角色卡。</p>
        <p class="hint">
          输入含算式(如「帮我算 12*34」)时,Agent 会自动调用计算器工具。
        </p>
      </div>

      <!-- 消息列表(渲染缓存:每条消息独立子组件,流式仅重算当前消息;
           props 未变的子组件在父重渲染时自动跳过;长会话虚拟滚动:缓冲区外渲染等高占位) -->
      <template v-for="(m, i) in messages" :key="m.id">
        <div
          v-if="!vm.isActive(m.id, i)"
          :style="vm.placeholderStyle(m)"
          :ref="(el) => bindRowRef(m.id, el)"
        ></div>
        <ChatMessageItem
          v-else
          :ref="(inst) => bindRowRef(m.id, inst)"
          :m="m"
          :editing="editingId === m.id"
          :avatar-url="avatarUrl"
          :character-name="currentCharacterName"
          :render-html="renderHtml"
          :scripts="currentScripts"
          :script-hash="currentScriptHash"
          :depth="messages.length - 1 - i"
          @start-edit="startEdit"
          @save-edit="saveEdit"
          @cancel-edit="editingId = null"
          @mutated="listEpoch += 1"
        />
      </template>

      <!-- 思考中占位 -->
      <div v-if="generating" class="sv-msg assistant">
        <span class="sv-avatar char">
          <template v-if="avatarUrl"><img :src="avatarUrl" alt="" /></template>
          <template v-else>{{ currentCharacterName.charAt(0) }}</template>
        </span>
        <div>
          <div class="sv-msg-name">{{ currentCharacterName }}</div>
          <div class="sv-msg-bubble thinking">
            <span class="sv-thinking"><i /><i /><i /></span>
          </div>
        </div>
      </div>
    </RenderPanelHost>

    <!-- 底部输入区 -->
    <ChatInput />

    <!-- 开场选择器(多开场角色:新建会话选择 / 会话内切换) -->
    <GreetingPickerModal v-if="greetingPickerOpen" :mode="greetingPickerMode" @close="greetingPickerOpen = false" />
  </section>
</template>
