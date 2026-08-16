<script setup lang="ts">
// 根组件:左侧功能区 + 中间消息区 + 右侧 Agent 抽屉 + 启动动画
import { onMounted, onUnmounted, watch } from 'vue';
import { useAppStore } from './store';
import Sidebar from './components/Sidebar.vue';
import ChatWindow from './components/ChatWindow.vue';
import AgentPanel from './components/AgentPanel.vue';
import SplashScreen from './components/SplashScreen.vue';
import SettingsHub from './components/SettingsHub.vue';
import WorldBooksModal from './components/WorldBooksModal.vue';
import ChatRecords from './components/ChatRecords.vue';
import PluginsModal from './components/PluginsModal.vue';
import SkillsModal from './components/SkillsModal.vue';
import ContractsModal from './components/ContractsModal.vue';
import PromptManager from './components/PromptManager.vue';
import ScriptsModal from './components/ScriptsModal.vue';
import MacrosModal from './components/MacrosModal.vue';
import DevToolsModal from './components/DevToolsModal.vue';
import OptimizeModal from './components/OptimizeModal.vue';
import QuickRepliesModal from './components/QuickRepliesModal.vue';
import AudioPlayer from './components/AudioPlayer.vue';

const store = useAppStore();

function shouldOpenSettings(): boolean {
  // 支持 #settings 与 ?settings=1 两种深链接
  return (
    window.location.hash === '#settings' ||
    new URLSearchParams(window.location.search).get('settings') === '1'
  );
}

function onHashChange(): void {
  if (shouldOpenSettings()) store.settingsOpen = true;
}

onMounted(() => {
  void store.loadCharacters();
  void store.testConnection();
  void store.loadModel();
  void store.loadModels();
  void store.loadSettings();
  if (shouldOpenSettings()) store.settingsOpen = true;
  window.addEventListener('hashchange', onHashChange);
});

onUnmounted(() => window.removeEventListener('hashchange', onHashChange));

watch(
  () => store.settingsOpen,
  (open) => {
    if (open) window.location.hash = 'settings';
  },
);
</script>

<template>
  <!-- 启动动画 -->
  <SplashScreen v-if="!store.splashDone" />

  <!-- 主界面(启动动画结束后淡入) -->
  <div v-else class="sv-frame-col sv-app-enter">
    <div class="sv-body">
      <!-- 左侧功能区(固定,不可关闭) -->
      <Sidebar />
      <!-- 中间消息区(固定,不可关闭) -->
      <div class="sv-main">
        <ChatWindow />
      </div>
      <!-- 右侧 Agent 区(抽屉,默认收起) -->
      <AgentPanel />
    </div>

    <!-- 右侧边缘抽屉标识 -->
    <button
      class="sv-agent-toggle"
      :class="{ active: store.agentPanelOpen }"
      :title="store.agentPanelOpen ? '收起 Agent 面板' : '展开 Agent 面板'"
      @click="store.agentPanelOpen = !store.agentPanelOpen"
    >
      AGENT
    </button>

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
      >♪</button>
    </template>
    <AudioPlayer v-else class="sv-audio-float" />
  </div>
</template>
