// 资源型数据 store:世界书 / 快速回复 / 音频播放器 / 技能库 的加载与 CRUD。
// 从 store.ts 按领域拆分;均为独立资源,不引用其他 store。
// 各弹窗开关(worldBooksOpen 等)在 uiPrefs store。
import { defineStore } from 'pinia';
import { ref } from 'vue';
import * as api from '../api';

export const useResourcesStore = defineStore('app.resources', () => {
  // ===== 状态 =====
  const worldBooks = ref<api.WorldBookRecord[]>([]);
  const quickReplies = ref<api.QuickReplyRecord[]>([]);
  const audio = ref<api.AudioState | null>(null);
  const skills = ref<api.SkillRecord[]>([]);

  // ===== 世界书 =====
  async function loadWorldBooks(): Promise<void> {
    try {
      worldBooks.value = await api.listWorldBooks();
    } catch (e) {
      console.error('加载世界书失败', e);
    }
  }

  async function uploadWorldBook(file: File, characterId?: string): Promise<api.WorldBookRecord> {
    const record = await api.uploadWorldBook(file, characterId);
    await loadWorldBooks();
    return record;
  }

  async function updateWorldBook(id: string, patch: { enabled?: boolean; character_id?: string; name?: string }): Promise<void> {
    const updated = await api.updateWorldBook(id, patch);
    const idx = worldBooks.value.findIndex((b) => b.id === id);
    if (idx !== -1) worldBooks.value[idx] = updated;
  }

  async function deleteWorldBook(id: string): Promise<void> {
    await api.deleteWorldBook(id);
    await loadWorldBooks();
  }

  // ===== 快速回复(阶段四 4b) =====
  async function loadQuickReplies(): Promise<void> {
    try {
      quickReplies.value = await api.listQuickReplies(true);
    } catch (e) {
      console.error('加载快速回复失败', e);
    }
  }

  // ===== 音频播放器(阶段五 5a) =====
  async function loadAudio(): Promise<void> {
    try {
      audio.value = await api.getAudio();
    } catch (e) {
      console.error('加载音频配置失败', e);
    }
  }

  /** 保存单通道设置(部分字段合并);成功后回填全量状态 */
  async function saveAudioSettings(
    type: api.AudioChannelType,
    settings: Partial<api.AudioChannelSettings>,
  ): Promise<void> {
    try {
      audio.value = await api.updateAudioSettings(type, settings);
    } catch (e) {
      console.error('音频设置保存失败', e);
      throw e;
    }
  }

  /** 保存单通道播放列表;成功后回填全量状态 */
  async function saveAudioPlaylist(
    type: api.AudioChannelType,
    tracks: api.AudioTrack[],
  ): Promise<void> {
    try {
      audio.value = await api.updateAudioPlaylist(type, tracks);
    } catch (e) {
      console.error('音频播放列表保存失败', e);
      throw e;
    }
  }

  // ===== 技能库 =====
  async function loadSkills(): Promise<void> {
    try {
      skills.value = await api.listSkills();
    } catch (e) {
      console.error('加载技能失败', e);
    }
  }

  /** 从 JSON 文件导入技能(支持单对象 / 数组 / {skills:[...]} 包壳) */
  async function importSkillsFile(file: File): Promise<number> {
    const text = await file.text();
    const parsed: unknown = JSON.parse(text);
    let items: api.SkillImportItem[];
    if (Array.isArray(parsed)) {
      items = parsed as api.SkillImportItem[];
    } else if (parsed && typeof parsed === 'object' && Array.isArray((parsed as { skills?: unknown }).skills)) {
      items = (parsed as { skills: api.SkillImportItem[] }).skills;
    } else {
      items = [parsed as api.SkillImportItem];
    }
    const res = await api.importSkills(items);
    await loadSkills();
    return res.imported;
  }

  /** 切换技能启用状态 */
  async function toggleSkill(s: api.SkillRecord): Promise<void> {
    await api.updateSkill(s.id, { enabled: !s.enabled });
    await loadSkills();
  }

  /** 删除技能 */
  async function removeSkill(id: string): Promise<void> {
    await api.deleteSkill(id);
    await loadSkills();
  }

  return {
    worldBooks,
    quickReplies,
    audio,
    skills,
    loadWorldBooks,
    uploadWorldBook,
    updateWorldBook,
    deleteWorldBook,
    loadQuickReplies,
    loadAudio,
    saveAudioSettings,
    saveAudioPlaylist,
    loadSkills,
    importSkillsFile,
    toggleSkill,
    removeSkill,
  };
});
