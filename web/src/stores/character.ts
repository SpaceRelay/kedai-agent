// 角色 store:角色列表 CRUD、当前角色选择、搜索过滤、开场白派生、角色卡脚本授权。
// 从 store.ts 按领域拆分。跨 store 引用(切角色时重置 chat 会话状态 /
// 同步 uiPrefs 的 HTML 渲染开关)均在动作运行时解析,setup 阶段不实例化其他 store。
import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import * as api from '../api';
import { idleAgent } from '../sseReducer';
import { isInitVarEntry } from '../mvu/initvar';
import { collectCardScripts } from '../cardScripts';
import {
  LocalScriptAuthorizationStore,
  hashRegexScripts,
  type ScriptAuthorizationGrant,
} from '../scriptAuthorization';
import {
  readLastCharacterId,
  readLastSessionId,
  removeCharacterPosition,
  writeLastCharacterId,
  writeLastSessionId,
} from '../lastPosition';
import { useChatStore } from './chat';
import { useUiPrefsStore } from './uiPrefs';

export const useCharacterStore = defineStore('app.character', () => {
  // ===== 状态 =====
  const characters = ref<api.CharacterRecord[]>([]);
  const currentCharacterId = ref<string | null>(null);
  const searchQuery = ref('');

  const scriptAuthorizationStore = new LocalScriptAuthorizationStore(localStorage);
  /** 当前角色脚本内容哈希；角色或脚本变化时重算。 */
  const currentScriptHash = ref('');
  /** 角色详情缓存(列表项无 data_raw;卡级脚本收集/initvar 兜底/授权哈希共用) */
  const characterDetails = new Map<string, api.CharacterRecord>();
  /** 详情拉取进行中去重(角色快速切换时避免并发重复请求) */
  const detailInflight = new Map<string, Promise<api.CharacterRecord | null>>();

  /** 拉详情并缓存(失败返回 null,缓存不记失败,下次重试) */
  function fetchCharacterDetail(id: string): Promise<api.CharacterRecord | null> {
    const cached = characterDetails.get(id);
    if (cached) return Promise.resolve(cached);
    const pending = detailInflight.get(id);
    if (pending) return pending;
    const task = api
      .getCharacter(id)
      .then((detail) => {
        characterDetails.set(id, detail);
        detailInflight.delete(id);
        return detail;
      })
      .catch(() => {
        detailInflight.delete(id);
        return null;
      });
    detailInflight.set(id, task);
    return task;
  }
  /**
   * 授权变更信号:grant/revoke/删卡时递增,驱动依赖 localStorage 的
   * currentScriptAuthorized 等 computed 重算。scriptAuthorizationStore 本身
   * 不是响应式对象,若不引入此信号,授权后按钮/面板不会刷新。
   */
  const scriptAuthVersion = ref(0);
  /** 授权列表快照，供设置页查看与逐卡撤销。 */
  const scriptAuthorizations = ref<ScriptAuthorizationGrant[]>(scriptAuthorizationStore.list());

  // ===== 派生状态 =====
  const currentCharacter = computed(() =>
    characters.value.find((c) => c.id === currentCharacterId.value) ?? null,
  );
  /**
   * 当前角色全部开场(多开场切换用):主开场 first_mes + 备用开场 alternate_greetings,均去空。
   * 空数组 = 无开场;下标即 greeting_index(0=主开场,1..=备用)。
   */
  const currentGreetings = computed<string[]>(() => {
    const c = currentCharacter.value;
    if (!c) return [];
    const list: string[] = [];
    if (c.first_mes?.trim()) list.push(c.first_mes);
    for (const g of c.alternate_greetings ?? []) {
      if (g.trim()) list.push(g);
    }
    return list;
  });
  const currentCharacterName = computed(
    () => currentCharacter.value?.chara_name ?? currentCharacter.value?.name ?? '未选择角色',
  );
  const currentScriptAuthorized = computed(() => {
    const id = currentCharacterId.value;
    // 引用授权变更信号,确保 grant/revoke 后本 computed 重新求值
    void scriptAuthVersion.value;
    return !!id && !!currentScriptHash.value && scriptAuthorizationStore.isAuthorized(id, currentScriptHash.value);
  });
  /** 角色列表按搜索词过滤 */
  const filteredCharacters = computed(() => {
    const q = searchQuery.value.trim().toLowerCase();
    if (!q) return characters.value;
    return characters.value.filter((c) =>
      [c.chara_name, c.name, c.description].join(' ').toLowerCase().includes(q),
    );
  });

  // ===== 动作 =====
  async function loadCharacters(autoSelect = true): Promise<void> {
    try {
      characters.value = await api.listCharacters();
      useUiPrefsStore().dataLoadError = null;
      if (currentCharacterId.value && !characters.value.some((c) => c.id === currentCharacterId.value)) {
        currentCharacterId.value = null;
        const chat = useChatStore();
        chat.currentSessionId = null;
        chat.sessions = [];
        chat.messages = [];
      }
      if (autoSelect && !currentCharacterId.value && characters.value.length > 0) {
        // 恢复上次浏览位置:优先上次选中的角色;已删除则回退列表首位并清掉失效记忆
        const lastId = readLastCharacterId();
        const target = lastId && characters.value.some((c) => c.id === lastId)
          ? lastId
          : characters.value[0].id;
        await selectCharacter(target);
      }
    } catch (e) {
      console.error('加载角色失败', e);
      // 失败浮出水面:静默 catch 会让界面停在空数据且无感知(曾表现为「聊天记录丢失」)
      useUiPrefsStore().dataLoadError = `加载角色列表失败:${(e as Error).message ?? e}`;
    }
  }

  async function selectCharacter(id: string): Promise<void> {
    currentCharacterId.value = id;
    writeLastCharacterId(id);
    // HTML 渲染开关按角色卡记忆:有记忆用记忆,无记忆回退全局默认
    useUiPrefsStore().syncRenderHtmlToCurrent();
    const selected = characters.value.find((c) => c.id === id);
    // 无条件计算脚本哈希:无脚本卡也得到合法哈希(空数组 SHA-256),使
    // currentScriptAuthorized 与 authorizeCurrentCharacterScripts 对无脚本卡同样生效,
    // 顶栏 JS 授权按钮常驻可授权/撤销;执行层因脚本数组为空永不执行,无安全影响。
    // 先算 regex-only 哈希(立即可用);后台拉详情,若卡带酒馆助手卡级脚本则重算
    // 含卡级脚本的哈希——执行面变化使旧授权自动失效(需重新授权一次,安全语义如此)。
    currentScriptHash.value = await hashRegexScripts(selected?.regex_scripts ?? []);
    void fetchCharacterDetail(id).then(async (detail) => {
      if (!detail || currentCharacterId.value !== id) return;
      const cardScripts = collectCardScripts(detail.data_raw as Record<string, unknown> | undefined);
      if (cardScripts.length === 0) return;
      const next = await hashRegexScripts(selected?.regex_scripts ?? [], cardScripts);
      if (currentCharacterId.value === id && next !== currentScriptHash.value) {
        currentScriptHash.value = next;
      }
    });
    const chat = useChatStore();
    chat.currentSessionId = null;
    chat.sessions = [];
    chat.messages = [];
    chat.lastUsage = null;
    chat.agent = idleAgent();
    chat.mvuVariables = { stat_data: {}, display_data: {} };
    // 加载该角色 [InitVar] 初始变量条目(mvu 初始化数据)
    try {
      chat.initVarEntries = await api.fetchInitVars(id);
      // 兜底:端点为空时拉详情从 data_raw 世界书前端侧再过滤(谓词与解析全在前端,
      // 覆盖旧后端谓词大小写敏感漏掉小写 [initvar] 标签的场景——碧蓝卡状态栏全兜底值)
      if (Object.keys(chat.initVarEntries).length === 0) {
        const detail = await fetchCharacterDetail(id);
        const rawEntries = (detail?.data_raw as { character_book?: { entries?: unknown } } | undefined)
          ?.character_book?.entries;
        const list = Array.isArray(rawEntries)
          ? rawEntries
          : rawEntries && typeof rawEntries === 'object'
            ? Object.values(rawEntries)
            : [];
        const fallback: Record<string, string> = {};
        for (const e of list as Array<{ comment?: unknown; content?: unknown }>) {
          if (typeof e?.comment === 'string' && typeof e.content === 'string' && isInitVarEntry(e.comment)) {
            fallback[e.comment] = e.content;
          }
        }
        if (Object.keys(fallback).length > 0) chat.initVarEntries = fallback;
      }
    } catch {
      chat.initVarEntries = {};
    }
    try {
      await chat.loadSessions(id);
      if (chat.sessions.length > 0) {
        // 优先恢复该角色上次打开的会话;已删除则回退最近会话并刷新记忆
        const lastSid = readLastSessionId(id);
        const target = chat.sessions.find((s) => s.id === lastSid) ?? chat.sessions[0];
        chat.currentSessionId = target.id;
        writeLastSessionId(id, target.id);
        await chat.loadHistory(target.id);
      } else if (characters.value.find((c) => c.id === id)?.first_mes) {
        // 角色带开场白:自动新建会话,由后端注入开场白并显示
        await chat.newSession();
      }
    } catch (e) {
      console.error('加载会话失败', e);
      useUiPrefsStore().dataLoadError = `加载会话失败:${(e as Error).message ?? e}`;
    }
  }

  async function uploadCharacter(file: File): Promise<api.CharacterRecord> {
    const record = await api.uploadCharacter(file);
    await loadCharacters(false);
    await selectCharacter(record.id);
    return record;
  }

  async function deleteCharacter(id: string): Promise<void> {
    await api.deleteCharacter(id);
    scriptAuthorizationStore.revoke(id);
    scriptAuthorizations.value = scriptAuthorizationStore.list();
    scriptAuthVersion.value += 1;
    // 一并清理该角色卡的 HTML 渲染开关记忆与浏览位置记忆(随卡删除,不留孤儿数据)
    useUiPrefsStore().removeRenderHtmlOverride(id);
    removeCharacterPosition(id);
    await loadCharacters();
  }

  /** 授权当前角色脚本。授权前确保卡级脚本(tavern_helper)已纳入哈希——
   *  否则详情到达后哈希变化会让刚写入的授权立即失效,用户被迫点两次。 */
  async function authorizeCurrentCharacterScripts(): Promise<void> {
    const id = currentCharacterId.value;
    if (!id) return;
    const detail = await fetchCharacterDetail(id);
    const selected = characters.value.find((c) => c.id === id);
    const cardScripts = detail
      ? collectCardScripts(detail.data_raw as Record<string, unknown> | undefined)
      : [];
    const hash = await hashRegexScripts(selected?.regex_scripts ?? [], cardScripts);
    if (currentCharacterId.value === id) currentScriptHash.value = hash;
    scriptAuthorizationStore.grant(id, hash);
    scriptAuthorizations.value = scriptAuthorizationStore.list();
    scriptAuthVersion.value += 1;
  }

  function revokeCharacterScripts(characterId: string): void {
    scriptAuthorizationStore.revoke(characterId);
    scriptAuthorizations.value = scriptAuthorizationStore.list();
    scriptAuthVersion.value += 1;
  }

  function isCharacterScriptAuthorized(characterId: string, scriptHash: string): boolean {
    return scriptAuthorizationStore.isAuthorized(characterId, scriptHash);
  }

  /** 更新角色提示词/开场白(description/first_mes/alternate_greetings),并同步本地列表 */
  async function updateCharacterPrompt(
    id: string,
    patch: { description?: string; first_mes?: string; alternate_greetings?: string[] },
  ): Promise<void> {
    const updated = await api.updateCharacter(id, patch);
    const idx = characters.value.findIndex((c) => c.id === id);
    if (idx !== -1) {
      characters.value[idx].description = updated.description;
      characters.value[idx].data_raw = updated.data_raw;
      if (updated.first_mes !== undefined) characters.value[idx].first_mes = updated.first_mes;
      if (updated.alternate_greetings !== undefined) {
        characters.value[idx].alternate_greetings = updated.alternate_greetings;
      }
    }
  }

  return {
    characters,
    currentCharacterId,
    searchQuery,
    currentScriptHash,
    scriptAuthorizations,
    currentCharacter,
    currentGreetings,
    currentCharacterName,
    currentScriptAuthorized,
    filteredCharacters,
    loadCharacters,
    selectCharacter,
    uploadCharacter,
    deleteCharacter,
    authorizeCurrentCharacterScripts,
    revokeCharacterScripts,
    isCharacterScriptAuthorized,
    updateCharacterPrompt,
    fetchCharacterDetail,
  };
});
