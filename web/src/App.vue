<script setup lang="ts">
// 根组件:左侧功能区 + 中间消息区 + 右侧 Agent 抽屉 + 启动动画
// 弹窗懒加载(前端性能优化):模态弹窗组件统一在 web/src/modals.ts 注册表声明,
// 首屏 bundle 不再包含其实现,首次打开对应弹窗时才加载 chunk;
// <Transition name="sv-modal"> 包裹与开合过渡语义不变(由注册表 v-for 派生)。
import { nextTick, onMounted, onUnmounted, watch } from 'vue';
import { useAppStore } from './store';
import { lazyModal } from './asyncModal';
import { MODALS, MODAL_FLAGS } from './modals';
import { inTauri } from './platform';
import { useExternalLinks } from './composables/useExternalLinks';
import { useKeepAlive } from './composables/useKeepAlive';
import Sidebar from './components/Sidebar.vue';
import ChatWindow from './components/ChatWindow.vue';
import TaskBoard from './components/TaskBoard.vue';
import AgentPanel from './components/AgentPanel.vue';
import SplashScreen from './components/SplashScreen.vue';

// 音频播放器:右下角浮层,非首屏(默认收起为开关按钮),一并懒加载。
// 它是常驻浮层而非模态弹窗,故不进 modals.ts 注册表。
const AudioPlayer = lazyModal(() => import('./components/AudioPlayer.vue'), '音频播放器', 'audioOpen');

const store = useAppStore();

/** 壳事件监听的注销句柄(listen 返回的 unlisten;未注册成功时为 null) */
let unlistenCloseRequested: (() => void) | null = null;

/** 动态 flag 访问的统一收口:ModalDecl.flag 是运行期字符串,无法静态索引 store,
 *  故只在此处做一次类型放宽(而非每个使用点各自断言);合法 flag 集合由
 *  web/src/modals.ts 单点定义,并由 web/src/modals.test.ts 元测试锁定。 */
const storeFlags = store as unknown as Record<string, boolean | undefined>;

/** 按 flag 名读弹窗开关(渲染与返回键共用同一来源)。 */
function isModalOpen(flag: string): boolean {
  return storeFlags[flag] === true;
}

/** 窄屏(<768px)判定:抽屉与底部导航仅在窄屏有意义。
 *  用 matchMedia 而非 CSS 之外的 UA 嗅探:桌面端该查询恒为 false,行为与改动前一致。 */
const narrowViewport = typeof window !== 'undefined' && typeof window.matchMedia === 'function'
  ? window.matchMedia('(max-width: 767px)')
  : null;

/** 收起全部移动端抽屉(遮罩点击、切换模式、打开设置时调用)。
 *  Agent 面板走 collapseAgentPanel(收起时记抑制标记,本会话生成不再自动弹);
 *  直接置 agentPanelOpen=false 会漏掉该标记,导致刚收起又被生成撑开。 */
function closeDrawers(): void {
  store.sidebarOpen = false;
  store.collapseAgentPanel();
}

/** 底部导航抽屉互斥开合:开一个时收起另一个,避免两层抽屉叠加。
 *  Agent 抽屉走 toggleAgentPanel(收起时记抑制,生成不会再自动弹);左栏为程序性收起。 */
function toggleDrawer(which: 'sidebar' | 'agent'): void {
  if (which === 'sidebar') {
    const next = !store.sidebarOpen;
    store.collapseAgentPanel();
    store.sidebarOpen = next;
  } else {
    store.sidebarOpen = false;
    store.toggleAgentPanel();
  }
}

/** 底部导航切换角色扮演/任务模式(切换后收起抽屉,让用户直接看到新视图) */
function toggleAppMode(): void {
  closeDrawers();
  store.setAppMode(store.appMode === 'roleplay' ? 'task' : 'roleplay');
}

/** 从底部导航打开综合设置:先收起抽屉,避免抽屉叠在弹窗下 */
function openSettingsFromNav(): void {
  closeDrawers();
  store.settingsOpen = true;
}

/** 底部导航音频开关:开合音频播放器并收起抽屉 */
function toggleAudio(): void {
  closeDrawers();
  store.audioOpen = !store.audioOpen;
}

/**
 * Android 返回键处理(桌面端不触发)。
 *
 * 曾有的问题:壳的初始 URL 是 about:blank,导航到 http://127.0.0.1:<port>/ 后
 * WebView 历史里有两条记录,系统返回键会退回 about:blank —— 表现为整页白屏且无法恢复。
 *
 * 处理顺序(符合 Android 习惯):
 *   1) 有打开的弹窗 → 关掉最上层的那个(MODAL_FLAGS 按渲染逆序,先关最上层);
 *   2) 有打开的抽屉 → 收起;
 *   3) 都没有 → 弹出退出确认弹窗(与桌面端「关闭窗口」同一个弹窗,两平台语义一致;
 *      此前是直接退出,误触返回键即丢未保存内容)。
 *
 * 注册本监听后,壳不再自行处理返回键(见 AppPlugin 的 hasListener 分支)。
 */
async function handleAndroidBack(): Promise<void> {
  // 弹窗 flag 集合单点定义在 web/src/modals.ts;逆序即「后声明者在上层」
  for (let i = MODAL_FLAGS.length - 1; i >= 0; i--) {
    const key = MODAL_FLAGS[i];
    if (storeFlags[key]) {
      storeFlags[key] = false;
      return;
    }
  }
  if (store.sidebarOpen || store.agentPanelOpen) {
    closeDrawers();
    return;
  }
  if (store.audioOpen) {
    store.audioOpen = false;
    return;
  }
  // 无处可退 → 打开退出确认弹窗(与桌面端「关闭窗口」走同一个弹窗,两平台语义一致)。
  // 此前是直接 emit 退出,误触返回键即丢未保存内容,且与桌面需二次确认的行为不一致。
  store.exitConfirmOpen = true;
}

/**
 * 监听壳下发的「用户点了窗口关闭」(仅桌面)。
 *
 * 这是本仓库前端第一处 `listen`(其余原生桥都是「前端 emit → 原生执行」单向):
 * 关闭确认要出前端至上主义样式,而 Tauri 的 window.dialog() 是 Windows MessageBox,
 * 外观不受前端 CSS 影响,故改为「壳 prevent_close + 下发事件 → 前端自绘弹窗」。
 * 必须在 onUnmounted 注销:否则热更新/重挂载会累积监听器,一次关闭弹出多个确认框。
 */
async function registerCloseRequestListener(): Promise<void> {
  if (!inTauri) return;
  try {
    const { listen } = await import('@tauri-apps/api/event');
    unlistenCloseRequested = await listen('kedai://close-requested', () => {
      store.exitConfirmOpen = true;
    });
  } catch (e) {
    // 非桌面平台或 API 不可用时静默忽略:桌面壳另有二次关闭兜底,窗口仍关得掉
    console.debug('[kedai] 关闭确认事件监听未注册', e);
  }
}

/** 注销壳事件监听(存在才注销;未注册成功时为 null) */
function unregisterCloseRequestListener(): void {
  const off = unlistenCloseRequested;
  unlistenCloseRequested = null;
  if (!off) return;
  try {
    off();
  } catch (e) {
    console.warn('[kedai] 关闭确认事件注销失败', e);
  }
}

/** 仅在 Android WebView 内注册返回键监听(桌面端该事件不会触发,注册本身也无副作用) */
async function registerBackButton(): Promise<void> {
  if (!inTauri) return;
  try {
    const { onBackButtonPress } = await import('@tauri-apps/api/app');
    await onBackButtonPress(() => {
      void handleAndroidBack();
    });
  } catch (e) {
    // 非 Android 平台或 API 不可用时静默忽略:返回键走系统默认行为即可
    console.debug('[kedai] 返回键监听未注册(非 Android 或不可用)', e);
  }
}

/** 弹窗懒加载失败后的手动重试:关掉再重开对应面板开关,触发 chunk 重新加载 */
async function retryModalLoad(): Promise<void> {
  const err = store.modalLoadError;
  if (!err) return;
  store.modalLoadError = null;
  storeFlags[err.flag] = false;
  await nextTick();
  storeFlags[err.flag] = true;
}

/** 面板 chunk 加载失败后的兜底恢复:整页刷新重新拉取 index.html(带最新 chunk hash)。
 *  旧页面缓存引用已不存在的旧 chunk 时,「重试」必然继续失败,只有整页刷新能恢复。 */
function reloadPage(): void {
  store.modalLoadError = null;
  if (typeof window !== 'undefined') window.location.reload();
}

/** 数据加载失败后的手动重试:重新拉角色列表(内部会级联恢复会话与历史) */
async function retryDataLoad(): Promise<void> {
  store.dataLoadError = null;
  await store.loadCharacters();
}

/** Tauri(桌面/exe)环境检测:WebView2 中 location.hash 赋值会触发导航事件,
 * 深链接仅服务浏览器场景;桌面端跳过 hash 写入与清理(见 platform.ts)。 */

/** 消息正文/资源卡外链统一交给系统浏览器(Android 走原生桥,桌面走系统 opener)。
 *  内部自行 onMounted 注册 / onUnmounted 注销捕获阶段委托。 */
useExternalLinks();

/** 长任务前台服务保活(仅 Android):生成/任务进行中启停,防止切后台被冻结 */
useKeepAlive();

function shouldOpenSettings(): boolean {
  // 支持 #settings 与 ?settings=1 两种深链接(仅浏览器)
  return (
    window.location.hash === '#settings' ||
    new URLSearchParams(window.location.search).get('settings') === '1'
  );
}

function onHashChange(): void {
  if (shouldOpenSettings()) store.settingsOpen = true;
}

onMounted(() => {
  // 启动加载竞态修复:loadTasks 依赖 loadSettings 按 appMode 写入的运行期设置,
  // 显式串行(loadSettings 完成后再 loadTasks),消除「并行发射后不管」的时序竞争。
  // 其余 4 个 load 保持并行;聚合错误提示:任一失败在 console 汇总(连接失败同时由
  // testConnection 落入 connStatus,UI 已有展示机制,不新增 UI),不阻塞其余加载。
  const parallelLoads = [store.loadCharacters(), store.testConnection(), store.loadModel(), store.loadModels()];
  void Promise.allSettled(parallelLoads).then((results) => {
    const failed = results.filter((r): r is PromiseRejectedResult => r.status === 'rejected');
    if (failed.length > 0) {
      console.error(`[kedai] 启动加载 ${failed.length}/${parallelLoads.length} 项失败`, failed.map((f) => f.reason));
    }
  });
  void store.loadSettings().then(() => {
    // 刷新后停留在任务模式时,补齐任务列表加载(loadSettings 已按 appMode 读对应设置),
    // 并启动任务事件 SSE 订阅(WP5:取代轮询;setAppMode 路径同样会启动,此处幂等)。
    // 实跑问题 4:列表就绪后恢复上次选中的任务,Agent 面板/调用记录才有入口加载。
    if (store.appMode === 'task') {
      void store.loadTasks().then(() => store.restoreSelectedTask());
      store.startTaskEvents();
    }
  });
  if (shouldOpenSettings()) store.settingsOpen = true;
  window.addEventListener('hashchange', onHashChange);
  void registerBackButton();
  void registerCloseRequestListener();
});

onUnmounted(() => {
  window.removeEventListener('hashchange', onHashChange);
  unregisterCloseRequestListener();
});

watch(
  () => store.settingsOpen,
  (open) => {
    if (inTauri) return;
    if (open) {
      window.location.hash = 'settings';
    } else if (window.location.hash === '#settings') {
      // 关闭时清除残留 hash(replaceState 不触发导航),避免刷新/重启后强制重开设置
      history.replaceState(null, '', window.location.pathname + window.location.search);
    }
  },
);

/** 移动端:选中角色后自动收起角色库抽屉(角色已切好,继续占屏反而挡住对话) */
watch(
  () => store.currentCharacterId,
  () => {
    if (narrowViewport?.matches) store.sidebarOpen = false;
  },
);

/** 移动端:打开任意模态弹窗时收起抽屉,避免抽屉残留在弹窗之下 */
watch(
  () => store.settingsOpen,
  (open) => {
    if (open && narrowViewport?.matches) closeDrawers();
  },
);
</script>

<template>
  <!-- 启动动画 -->
  <SplashScreen v-if="!store.splashDone" />

  <!-- 主界面(启动动画结束后淡入) -->
  <div v-else class="sv-frame-col sv-app-enter">
    <!-- 全局错误条:数据加载失败(角色/会话/历史)与弹窗懒加载失败,均可重试 -->
    <div v-if="store.dataLoadError" class="sv-global-error" role="alert">
      <span class="sv-global-error-text">{{ store.dataLoadError }}</span>
      <button class="sv-btn ghost sv-btn-sm" @click="retryDataLoad">重试</button>
      <button class="sv-btn ghost sv-btn-sm" @click="store.dataLoadError = null">关闭</button>
    </div>
    <!-- 未捕获异常兜底(计划批次 5.1):只给「关闭」——异常没有对应的重试动作,
         给重试按钮会让用户以为点一下能修复,而实际只有刷新页面可能有用 -->
    <div v-if="store.globalError" class="sv-global-error" role="alert">
      <span class="sv-global-error-text">界面出现未捕获异常:{{ store.globalError }}</span>
      <button class="sv-btn ghost sv-btn-sm" @click="store.globalError = null">关闭</button>
    </div>
    <div v-if="store.modalLoadError" class="sv-global-error" role="alert">
      <span class="sv-global-error-text">「{{ store.modalLoadError.name }}」面板加载失败(前端已更新,需刷新页面)</span>
      <button class="sv-btn ghost sv-btn-sm" @click="reloadPage">刷新页面</button>
      <button class="sv-btn ghost sv-btn-sm" @click="retryModalLoad">重试</button>
      <button class="sv-btn ghost sv-btn-sm" @click="store.modalLoadError = null">关闭</button>
    </div>
    <div class="sv-body">
      <!-- 左侧功能区(桌面常驻;移动端为覆盖式抽屉,由底部导航「角色」开合) -->
      <Sidebar />
      <!-- 中间消息区(固定,不可关闭):角色扮演模式 = 聊天,任务模式 = 任务工作台。
           用 <Transition mode="out-in"> 让两模式切换先淡出旧视图再淡入新视图。 -->
      <div class="sv-main">
        <Transition name="sv-mode" mode="out-in">
          <ChatWindow v-if="store.appMode === 'roleplay'" key="roleplay" />
          <TaskBoard v-else key="task" />
        </Transition>
      </div>
      <!-- 右侧 Agent 区(抽屉,默认收起;两种模式共用,内容按模式映射:角色扮演 = 推理链/工具,任务 = 计划/子任务;
           面板合并:原独立「调用情况」面板收编为其内部 tab,由 AgentPanel 内嵌 CallTracePanel 承载) -->
      <AgentPanel />
    </div>

    <!-- 移动端抽屉遮罩:点击空白处收起当前抽屉(桌面端由 CSS 隐藏) -->
    <div
      v-if="store.sidebarOpen || store.agentPanelOpen"
      class="sv-drawer-backdrop"
      aria-hidden="true"
      @click="closeDrawers"
    />

    <!-- 右侧边缘抽屉标识(两种模式;移动端隐藏,由底部导航「AGENT」替代) -->
    <button
      class="sv-agent-toggle"
      :class="{ active: store.agentPanelOpen }"
      :title="store.agentPanelOpen ? '收起 Agent 面板' : '展开 Agent 面板'"
      @click="store.toggleAgentPanel()"
    >
      AGENT
    </button>

    <!-- 移动端底部导航(桌面端 CSS 隐藏):角色库 / 模式切换 / Agent / 设置 -->
    <nav class="sv-mobile-nav" aria-label="主导航">
      <button
        class="sv-mobile-nav-item"
        :class="{ active: store.sidebarOpen }"
        @click="toggleDrawer('sidebar')"
      >
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 6h16M4 12h16M4 18h16" /></svg>
        <span>角色库</span>
      </button>
      <button
        class="sv-mobile-nav-item"
        :class="{ active: store.appMode === 'task' }"
        @click="toggleAppMode"
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path v-if="store.appMode === 'roleplay'" d="M4 5h16v11H8l-4 4z" />
          <path v-else d="M4 6h16M4 12h10M4 18h13" />
        </svg>
        <span>{{ store.appMode === 'roleplay' ? '角色扮演' : '任务' }}</span>
      </button>
      <button
        class="sv-mobile-nav-item"
        :class="{ active: store.agentPanelOpen }"
        @click="toggleDrawer('agent')"
      >
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3v18M3 12h18" /></svg>
        <span>AGENT</span>
      </button>
      <!-- 音频:手机上隐藏了右下角悬浮钮(会压住输入工具行),改由导航栏承载 -->
      <button
        class="sv-mobile-nav-item"
        :class="{ active: store.audioOpen }"
        @click="toggleAudio"
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M9 18V6l10-2v12" />
          <circle cx="6.5" cy="18" r="2.5" />
          <circle cx="16.5" cy="16" r="2.5" />
        </svg>
        <span>音频</span>
      </button>
      <button class="sv-mobile-nav-item" @click="openSettingsFromNav">
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <circle cx="12" cy="12" r="3" />
          <path d="M12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M19 5l-2 2M7 17l-2 2" />
        </svg>
        <span>设置</span>
      </button>
    </nav>

    <!-- 各模态弹窗:统一 <Transition name="sv-modal"> 开合过渡。
         列表/组件/顺序均来自 web/src/modals.ts 单点注册表(声明顺序 = 渲染顺序,
         后声明者叠在上层);Android 返回键按同表逆序关闭。 -->
    <Transition v-for="modal in MODALS" :key="modal.flag" name="sv-modal">
      <component :is="modal.component" v-if="isModalOpen(modal.flag)" />
    </Transition>

    <!-- 音频播放器(阶段五 5a):右下角悬浮 -->
    <template v-if="!store.audioOpen">
      <button
        class="sv-audio-toggle"
        title="音频播放器"
        @click="store.audioOpen = true"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="square" stroke-linejoin="miter" aria-hidden="true"><path d="M9 18V6l10-2v12" /><circle cx="6.5" cy="18" r="2.5" /><circle cx="16.5" cy="16" r="2.5" /></svg>
      </button>
    </template>
    <AudioPlayer v-else class="sv-audio-float" />
  </div>
</template>
