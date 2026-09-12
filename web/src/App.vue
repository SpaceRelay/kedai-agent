<script setup lang="ts">
// 根组件:左侧功能区 + 中间消息区 + 右侧 Agent 抽屉 + 启动动画
// 弹窗懒加载(前端性能优化):13 个弹窗/浮层组件改 defineAsyncComponent,
// 首屏 bundle 不再包含其实现,首次打开对应弹窗时才加载 chunk;
// <Transition name="sv-modal"> 包裹与 v-if 条件保持不变(开合过渡语义不变)。
import { nextTick, onMounted, onUnmounted, watch } from 'vue';
import { useAppStore } from './store';
import { lazyModal } from './asyncModal';
import { inTauri } from './platform';
import { useExternalLinks } from './composables/useExternalLinks';
import { useKeepAlive } from './composables/useKeepAlive';
import Sidebar from './components/Sidebar.vue';
import ChatWindow from './components/ChatWindow.vue';
import TaskBoard from './components/TaskBoard.vue';
import AgentPanel from './components/AgentPanel.vue';
import SplashScreen from './components/SplashScreen.vue';

// ===== 懒加载弹窗(各自/分组拆 chunk,见 vite.config.ts manualChunks) =====
// 统一经 lazyModal 包装:chunk 加载失败自动重试一次,仍失败显示全局错误条(可手动重试),
// 不再静默「点了没反应」。第二参数为面板名,第三参数为 uiPrefs 中对应的开关 ref 名。
const SettingsHub = lazyModal(() => import('./components/SettingsHub.vue'), '综合设置', 'settingsOpen');
const WorldBooksModal = lazyModal(() => import('./components/WorldBooksModal.vue'), '世界书', 'worldBooksOpen');
const ChatRecords = lazyModal(() => import('./components/ChatRecords.vue'), '聊天记录', 'chatRecordsOpen');
const PluginsModal = lazyModal(() => import('./components/PluginsModal.vue'), '插件', 'pluginsOpen');
const SkillsModal = lazyModal(() => import('./components/SkillsModal.vue'), '技能库', 'skillsOpen');
const ContractsModal = lazyModal(() => import('./components/ContractsModal.vue'), '契约编辑', 'contractsOpen');
const PromptManager = lazyModal(() => import('./components/PromptManager.vue'), '提示词管理', 'promptsOpen');
const ScriptsModal = lazyModal(() => import('./components/ScriptsModal.vue'), '脚本管理', 'scriptsOpen');
const MacrosModal = lazyModal(() => import('./components/MacrosModal.vue'), '宏调试', 'macrosOpen');
const DevToolsModal = lazyModal(() => import('./components/DevToolsModal.vue'), '事件监控', 'eventsOpen');
const OptimizeModal = lazyModal(() => import('./components/OptimizeModal.vue'), '优化面板', 'optimizeOpen');
const MemoryModal = lazyModal(() => import('./components/MemoryModal.vue'), '记忆库', 'memoryOpen');
const RepoIndexModal = lazyModal(() => import('./components/RepoIndexModal.vue'), '仓库索引', 'repoIndexOpen');
const QuickRepliesModal = lazyModal(() => import('./components/QuickRepliesModal.vue'), '快速回复', 'quickRepliesOpen');
// 音频播放器:右下角浮层,非首屏(默认收起为开关按钮),一并懒加载
const AudioPlayer = lazyModal(() => import('./components/AudioPlayer.vue'), '音频播放器', 'audioOpen');

const store = useAppStore();

/** 窄屏(<768px)判定:抽屉与底部导航仅在窄屏有意义。
 *  用 matchMedia 而非 CSS 之外的 UA 嗅探:桌面端该查询恒为 false,行为与改动前一致。 */
const narrowViewport = typeof window !== 'undefined' && typeof window.matchMedia === 'function'
  ? window.matchMedia('(max-width: 767px)')
  : null;

/** 收起全部移动端抽屉(遮罩点击、切换模式、打开设置时调用) */
function closeDrawers(): void {
  store.sidebarOpen = false;
  store.agentPanelOpen = false;
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
 *   1) 有打开的弹窗 → 关掉最上层的那个;
 *   2) 有打开的抽屉 → 收起;
 *   3) 都没有 → 退出应用(与桌面版「关闭即退出」语义一致,后端与壳同进程一并结束)。
 *
 * 注册本监听后,壳不再自行处理返回键(见 AppPlugin 的 hasListener 分支)。
 */
const MODAL_FLAGS = [
  'settingsOpen', 'promptsOpen', 'worldBooksOpen', 'chatRecordsOpen', 'pluginsOpen',
  'skillsOpen', 'contractsOpen', 'scriptsOpen', 'macrosOpen', 'eventsOpen',
  'optimizeOpen', 'memoryOpen', 'repoIndexOpen', 'quickRepliesOpen',
] as const;

async function handleAndroidBack(): Promise<void> {
  const flags = store as unknown as Record<string, boolean | undefined>;
  for (const key of MODAL_FLAGS) {
    if (flags[key]) {
      flags[key] = false;
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
  // 无处可退 → 请求壳退出应用(后端与壳同进程,退出即整体结束,无残留服务)。
  //
  // 为什么用事件而不是命令:
  // - 页面来自 http://127.0.0.1:<port>/(remote origin),Tauri 的 IPC 会做 ACL 校验,
  //   而框架不为自定义 #[tauri::command] 生成权限条目,实测报
  //   "exit_app not allowed. Plugin not found";
  // - plugin:app|exit 亦被拒(无 Rust 侧权限声明);
  // - getCurrentWindow().close() 在 Android 上不结束 Activity(进程仍在且 Promise 悬挂)。
  // 事件系统的 emit 权限(core:event:allow-emit)已含在 core:default 中,无需额外授权。
  try {
    const { emit } = await import('@tauri-apps/api/event');
    await emit('kedai://exit-app');
  } catch (e) {
    console.warn('[kedai] 返回键退出失败', e);
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
  const flags = store as unknown as Record<string, unknown>;
  flags[err.flag] = false;
  await nextTick();
  flags[err.flag] = true;
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
});

onUnmounted(() => {
  window.removeEventListener('hashchange', onHashChange);
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

    <!-- 各模态弹窗:统一 <Transition name="sv-modal"> 开合过渡 -->
    <!-- 综合设置弹窗 -->
    <Transition name="sv-modal">
      <SettingsHub v-if="store.settingsOpen" />
    </Transition>
    <!-- 世界书模态框 -->
    <Transition name="sv-modal">
      <WorldBooksModal v-if="store.worldBooksOpen" />
    </Transition>
    <!-- 聊天记录面板 -->
    <Transition name="sv-modal">
      <ChatRecords v-if="store.chatRecordsOpen" />
    </Transition>
    <!-- 插件管理弹窗 -->
    <Transition name="sv-modal">
      <PluginsModal v-if="store.pluginsOpen" />
    </Transition>
    <!-- 技能库弹窗 -->
    <Transition name="sv-modal">
      <SkillsModal v-if="store.skillsOpen" />
    </Transition>
    <!-- 契约编辑弹窗(P6 面板) -->
    <Transition name="sv-modal">
      <ContractsModal v-if="store.contractsOpen" />
    </Transition>
    <!-- 提示词顺序管理弹窗 -->
    <Transition name="sv-modal">
      <PromptManager v-if="store.promptsOpen" />
    </Transition>
    <!-- 用户脚本管理弹窗(阶段三) -->
    <Transition name="sv-modal">
      <ScriptsModal v-if="store.scriptsOpen" />
    </Transition>
    <!-- 宏调试弹窗(阶段六 6b) -->
    <Transition name="sv-modal">
      <MacrosModal v-if="store.macrosOpen" />
    </Transition>
    <!-- 事件监控弹窗(阶段六 6c) -->
    <Transition name="sv-modal">
      <DevToolsModal v-if="store.eventsOpen" />
    </Transition>
    <!-- 优化面板弹窗(阶段六 6d) -->
    <Transition name="sv-modal">
      <OptimizeModal v-if="store.optimizeOpen" />
    </Transition>
    <!-- 记忆库弹窗(侧边栏一级入口;与优化面板内的记忆库分区同源同组件) -->
    <Transition name="sv-modal">
      <MemoryModal v-if="store.memoryOpen" />
    </Transition>
    <!-- 仓库索引面板(只读展示 .kedai-index 生成的代码索引) -->
    <Transition name="sv-modal">
      <RepoIndexModal v-if="store.repoIndexOpen" />
    </Transition>
    <!-- 快速回复管理弹窗(阶段四 4b) -->
    <Transition name="sv-modal">
      <QuickRepliesModal v-if="store.quickRepliesOpen" />
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
