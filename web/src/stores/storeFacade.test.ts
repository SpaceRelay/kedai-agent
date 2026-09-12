import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useAppStore } from '../store';
import { useChatStore } from './chat';
import { useCharacterStore } from './character';
import { useModelConnStore } from './modelConn';
import { useGenSettingsStore } from './genSettings';
import { useTaskStore } from './task';
import { useUiPrefsStore } from './uiPrefs';
import { useResourcesStore } from './resources';

// store.ts 兼容门面测试:门面 useAppStore('app') 组合 stores/ 各子 store,
// 导出与原单一 store 完全相同的键集合。此处锁定三件事:
// 1) 原导出键一个不少(防漏转发);
// 2) 状态/派生字段与子 store 是同一响应式源(双向穿透);
// 3) 动作按函数引用转发(与子 store 同一函数,非包装)。

// node 环境无 localStorage,子 store 初始化即访问(appMode / renderHtml 记忆 / 脚本授权),补内存桩
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

/** 原单一 store 返回块的全部状态与派生键(拆分前 store.ts :1197-1294) */
const STATE_KEYS = [
  'characters', 'currentCharacterId', 'currentSessionId', 'sessions', 'messages', 'generating',
  'agent', 'connStatus', 'connMessage', 'lastUsage', 'contextTokens',
  'settingsOpen', 'promptsOpen', 'agentPanelOpen', 'agentMode', 'authorizationMode',
  'authorizationAlwaysRequired', 'toolAuthorizationTimeoutSecs', 'taskToolPolicy', 'taskToolAllowlist',
  'cacheZeroStreak', 'sessionTotalTokens', 'globalTotalTokens', 'splashDone',
  'temperature', 'topP', 'maxTokens', 'maxContextTokens', 'maxToolRounds',
  'compactionMode', 'compactionThreshold', 'compactionKeepRecent', 'compactionSnipBytes',
  'memoryDistillEnabled', 'memoryInjectLimit', 'memoryInjectCharBudget', 'memoryMaxEntries',
  'skillProgressiveDisclosure',
  'subagentMaxDepth', 'subagentMaxConcurrency', 'subagentResultMaxChars', 'undoEnabled',
  'model', 'models', 'searchQuery',
  'appMode', 'tasks', 'currentTaskId', 'currentTask', 'currentTaskUsage', 'globalTaskUsage',
  'taskCalls', 'callTraceOpen', 'taskRunMode',
  'worldBooks', 'worldBooksOpen', 'quickReplies', 'quickRepliesOpen',
  'audio', 'audioOpen', 'pluginsOpen', 'skillsOpen', 'skills', 'contractsOpen', 'scriptsOpen',
  'macrosOpen', 'eventLog', 'eventsOpen', 'optimizeOpen', 'memoryOpen', 'repoIndexOpen',
  'agentSystemPrompt', 'searchEndpoint', 'mvuVarsPosition', 'reflectPrompt',
  'presetTailPrompt', 'presetTailRole', 'reflectAdvicePrompt', 'reflectAdviceRole',
  'promptInject', 'agentFlowLibrary',
  'renderHtml', 'currentScriptHash', 'currentScriptAuthorized', 'scriptAuthorizations',
  'chatRecordsOpen', 'mvuVariables', 'initVarEntries',
  'currentCharacter', 'currentGreetings', 'currentCharacterName', 'filteredCharacters',
] as const;

/** 原单一 store 返回块的全部动作键(拆分前 store.ts :1295-1361) */
const ACTION_KEYS = [
  'loadCharacters', 'selectCharacter', 'loadSessions', 'newSession', 'switchGreeting',
  'switchSession', 'loadHistory', 'loadTokenTotals', 'updateContextTokens',
  'uploadCharacter', 'deleteCharacter',
  'authorizeCurrentCharacterScripts', 'revokeCharacterScripts', 'isCharacterScriptAuthorized',
  'updateCharacterPrompt',
  'sendMessage', 'onSseEvent', 'stop',
  'testConnection', 'loadModel', 'loadModels', 'switchModel',
  'loadSettings', 'saveSettings', 'compactCurrentChat', 'clearCurrentCompaction',
  'loadPromptInject', 'savePromptInjectConfig',
  'loadAgentFlow', 'saveAgentFlowConfig', 'selectAgentFlow', 'deleteAgentFlow',
  'removeMessage', 'updateMessage', 'resendMessage', 'regenerateMessage', 'swipeMessage',
  'clearCurrentChat', 'exportCurrentChat',
  'loadWorldBooks', 'uploadWorldBook', 'updateWorldBook', 'deleteWorldBook',
  'setAppMode', 'loadTasks', 'loadGlobalTaskUsage', 'createTask', 'selectTask', 'loadTaskDetail',
  'runTask', 'stopTask', 'deleteTask', 'startTaskPolling', 'stopTaskPolling', 'loadTaskCalls',
  'approveTask',
  'loadQuickReplies', 'loadAudio', 'saveAudioSettings', 'saveAudioPlaylist',
  'loadSkills', 'importSkillsFile', 'toggleSkill', 'removeSkill',
] as const;

beforeEach(() => {
  memStorage.clear();
  setActivePinia(createPinia());
});

describe('useAppStore 兼容门面', () => {
  it('原导出键一个不少:状态/派生均存在,动作均为函数', () => {
    const app = useAppStore() as unknown as Record<string, unknown>;
    for (const key of STATE_KEYS) {
      expect(app, `缺少状态键 ${key}`).toHaveProperty(key);
    }
    for (const key of ACTION_KEYS) {
      expect(typeof app[key], `动作 ${key} 应为函数`).toBe('function');
    }
  });

  it('状态字段与子 store 是同一响应式源(门面写 → 子 store 读;子 store 写 → 门面读)', () => {
    const app = useAppStore();
    const genSettings = useGenSettingsStore();
    const chat = useChatStore();
    const uiPrefs = useUiPrefsStore();

    app.temperature = 0.5;
    expect(genSettings.temperature).toBe(0.5);
    genSettings.topP = 0.33;
    expect(app.topP).toBe(0.33);

    app.messages = [{ id: 1, role: 'user', content: 'hi', extra: {}, streaming: false }];
    expect(chat.messages).toHaveLength(1);
    chat.generating = true;
    expect(app.generating).toBe(true);

    app.settingsOpen = true;
    expect(uiPrefs.settingsOpen).toBe(true);
  });

  it('派生 getter 与子 store 一致(只读穿透)', () => {
    const app = useAppStore();
    const character = useCharacterStore();
    character.characters = [
      { id: 'c1', chara_name: '小测', name: 'x', description: '', first_mes: '你好', alternate_greetings: ['  ', '二'] },
    ] as never;
    character.currentCharacterId = 'c1';
    expect(app.currentCharacterName).toBe('小测');
    expect(app.currentGreetings).toEqual(['你好', '二']);
    expect(app.currentCharacterName).toBe(character.currentCharacterName);
  });

  it('动作按函数引用转发(门面与子 store 同一函数)', () => {
    const app = useAppStore();
    expect(app.sendMessage).toBe(useChatStore().sendMessage);
    expect(app.onSseEvent).toBe(useChatStore().onSseEvent);
    expect(app.selectCharacter).toBe(useCharacterStore().selectCharacter);
    expect(app.saveSettings).toBe(useGenSettingsStore().saveSettings);
    expect(app.switchModel).toBe(useModelConnStore().switchModel);
    expect(app.setAppMode).toBe(useTaskStore().setAppMode);
    expect(app.loadWorldBooks).toBe(useResourcesStore().loadWorldBooks);
  });

  it('动态完整性:各子 store 实例的导出键在门面上全部存在(防新增动作漏转发,如 queueSettingsSave 事故)', () => {
    const app = useAppStore() as unknown as Record<string, unknown>;
    const subs: Array<[string, object]> = [
      ['chat', useChatStore()],
      ['character', useCharacterStore()],
      ['modelConn', useModelConnStore()],
      ['genSettings', useGenSettingsStore()],
      ['task', useTaskStore()],
      ['uiPrefs', useUiPrefsStore()],
      ['resources', useResourcesStore()],
    ];
    for (const [name, sub] of subs) {
      for (const key of Object.keys(sub)) {
        if (key.startsWith('$') || key.startsWith('_')) continue; // 跳过 pinia 内置 $id/$state 等
        expect(key in app, `${name} store 的「${key}」未在门面 useAppStore 暴露`).toBe(true);
      }
    }
  });
});
