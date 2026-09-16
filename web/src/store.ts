// 全局状态兼容门面(Pinia setup store,id 保持 'app')。
// 实现已按领域拆分到 stores/ 子 store:chat(会话/消息/SSE 流)、character(角色与脚本授权)、
// modelConn(模型与连接)、genSettings(生成参数/Agent 设置/注入/流程)、task(任务模式)、
// uiPrefs(弹窗开关/渲染偏好)、resources(世界书/快速回复/音频/技能)。
// 本门面组合各子 store,导出与原单一 store 完全相同的键集合,全部调用方零改动。
// 状态/派生经 storeToRefs 转发(同一响应式源),动作按函数引用转发(与子 store 同一函数)。

import { defineStore, storeToRefs } from 'pinia';
import { useChatStore } from './stores/chat';
import { useCharacterStore } from './stores/character';
import { useModelConnStore } from './stores/modelConn';
import { useGenSettingsStore } from './stores/genSettings';
import { useTaskStore } from './stores/task';
import { useUiPrefsStore } from './stores/uiPrefs';
import { useResourcesStore } from './stores/resources';

// 类型别名:保持 store 对外的类型导出(AgentActivity / UiMessage)不变
export type { AgentActivity, UiMessage } from './sseReducer';

export const useAppStore = defineStore('app', () => {
  const chat = useChatStore();
  const character = useCharacterStore();
  const modelConn = useModelConnStore();
  const genSettings = useGenSettingsStore();
  const task = useTaskStore();
  const uiPrefs = useUiPrefsStore();
  const resources = useResourcesStore();

  return {
    // ===== 状态与派生(storeToRefs 转发,读写穿透到子 store 同一响应式源) =====
    ...storeToRefs(character),
    ...storeToRefs(chat),
    ...storeToRefs(modelConn),
    ...storeToRefs(genSettings),
    ...storeToRefs(task),
    ...storeToRefs(uiPrefs),
    ...storeToRefs(resources),

    // ===== 动作(函数引用转发) =====
    // 角色
    loadCharacters: character.loadCharacters,
    selectCharacter: character.selectCharacter,
    uploadCharacter: character.uploadCharacter,
    deleteCharacter: character.deleteCharacter,
    authorizeCurrentCharacterScripts: character.authorizeCurrentCharacterScripts,
    revokeCharacterScripts: character.revokeCharacterScripts,
    isCharacterScriptAuthorized: character.isCharacterScriptAuthorized,
    updateCharacterPrompt: character.updateCharacterPrompt,
    fetchCharacterDetail: character.fetchCharacterDetail,
    // 会话与消息
    loadSessions: chat.loadSessions,
    newSession: chat.newSession,
    switchGreeting: chat.switchGreeting,
    switchSession: chat.switchSession,
    loadHistory: chat.loadHistory,
    loadTokenTotals: chat.loadTokenTotals,
    updateContextTokens: chat.updateContextTokens,
    sendMessage: chat.sendMessage,
    startStream: chat.startStream,
    onSseEvent: chat.onSseEvent,
    clearEventLog: chat.clearEventLog,
    setAgentMode: chat.setAgentMode,
    stop: chat.stop,
    removeMessage: chat.removeMessage,
    updateMessage: chat.updateMessage,
    resendMessage: chat.resendMessage,
    regenerateMessage: chat.regenerateMessage,
    swipeMessage: chat.swipeMessage,
    clearCurrentChat: chat.clearCurrentChat,
    exportCurrentChat: chat.exportCurrentChat,
    compactCurrentChat: chat.compactCurrentChat,
    clearCurrentCompaction: chat.clearCurrentCompaction,
    // 模型与连接
    testConnection: modelConn.testConnection,
    loadModel: modelConn.loadModel,
    loadModels: modelConn.loadModels,
    switchModel: modelConn.switchModel,
    // 运行期设置 / 提示词注入 / 执行流程
    loadSettings: genSettings.loadSettings,
    saveSettings: genSettings.saveSettings,
    queueSettingsSave: genSettings.queueSettingsSave,
    setAuthorizationMode: genSettings.setAuthorizationMode,
    requestShizukuPermission: genSettings.requestShizukuPermission,
    loadPromptInject: genSettings.loadPromptInject,
    savePromptInjectConfig: genSettings.savePromptInjectConfig,
    loadAgentFlow: genSettings.loadAgentFlow,
    saveAgentFlowConfig: genSettings.saveAgentFlowConfig,
    selectAgentFlow: genSettings.selectAgentFlow,
    deleteAgentFlow: genSettings.deleteAgentFlow,
    // 任务模式
    setAppMode: task.setAppMode,
    loadTasks: task.loadTasks,
    loadGlobalTaskUsage: task.loadGlobalTaskUsage,
    loadTaskCalls: task.loadTaskCalls,
    createTask: task.createTask,
    selectTask: task.selectTask,
    clearSelectedTask: task.clearSelectedTask,
    restoreSelectedTask: task.restoreSelectedTask,
    loadTaskDetail: task.loadTaskDetail,
    runTask: task.runTask,
    stopTask: task.stopTask,
    approveTask: task.approveTask,
    followupTask: task.followupTask,
    planChatTask: task.planChatTask,
    deleteTask: task.deleteTask,
    startTaskEvents: task.startTaskEvents,
    stopTaskEvents: task.stopTaskEvents,
    startTaskPolling: task.startTaskPolling,
    stopTaskPolling: task.stopTaskPolling,
    // 界面偏好(渲染开关按角色记忆等动作)
    syncRenderHtmlToCurrent: uiPrefs.syncRenderHtmlToCurrent,
    setRenderHtml: uiPrefs.setRenderHtml,
    toggleRenderHtml: uiPrefs.toggleRenderHtml,
    removeRenderHtmlOverride: uiPrefs.removeRenderHtmlOverride,
    // Agent 面板开合(自动展开一次 + 用户主动收起后不再打扰)
    openAgentPanel: uiPrefs.openAgentPanel,
    collapseAgentPanel: uiPrefs.collapseAgentPanel,
    toggleAgentPanel: uiPrefs.toggleAgentPanel,
    autoOpenAgentPanel: uiPrefs.autoOpenAgentPanel,
    resetAgentPanelAutoSuppress: uiPrefs.resetAgentPanelAutoSuppress,
    // 世界书
    loadWorldBooks: resources.loadWorldBooks,
    uploadWorldBook: resources.uploadWorldBook,
    updateWorldBook: resources.updateWorldBook,
    deleteWorldBook: resources.deleteWorldBook,
    // 快速回复(阶段四 4b)
    loadQuickReplies: resources.loadQuickReplies,
    // 音频播放器(阶段五 5a)
    loadAudio: resources.loadAudio,
    saveAudioSettings: resources.saveAudioSettings,
    saveAudioPlaylist: resources.saveAudioPlaylist,
    // 技能库
    loadSkills: resources.loadSkills,
    importSkillsFile: resources.importSkillsFile,
    toggleSkill: resources.toggleSkill,
    removeSkill: resources.removeSkill,
  };
});
