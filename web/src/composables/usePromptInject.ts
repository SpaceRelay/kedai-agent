// 提示词注入设置:简单模式(禁词库/字数/转述/对话/视角)+ 复杂模式(楼层系统,仿 SillyTavern Prompt Manager)。
import { ref } from 'vue';
import { useAppStore } from '../store';
import * as api from '../api';
import { downloadBlob } from '../exportFile';

export const FLOOR_ROLE_LABELS: Record<api.FloorRole, string> = {
  system: '系统提示词',
  user: '用户',
  assistant: '角色',
};
/** 楼层注入位置:引擎实现统一归位「系统提示词内」(system 角色进系统提示词,
 *  user/assistant 角色紧随 system 按 order 排),before/after/depth 已废弃不生效,
 *  仅保留字段与解析以兼容导入的酒馆预设(见 messages/build.rs 位置4 注释)。 */
export const FLOOR_POS_LABELS: Record<api.FloorPosition, string> = {
  system: '系统提示词内',
  before: '对话开头(已废弃)',
  after: '最新消息后(已废弃)',
  depth: '深度 N(已废弃)',
};

/** 酒馆宏速查(楼层内容编辑参考) */
export const MACRO_HINTS = [
  '{{char}} 角色名', '{{user}} 用户', '{{time}} 时间', '{{datetime}} 日期时间',
  '{{random:1,100}} 随机数', '{{roll:1d20}} 骰子', '{{newline}} 换行',
  '{{lastMessage}} 上一条消息', '{{lastUserMessage}} 最近用户消息', '{{lastCharMessage}} 最近角色消息',
  '{{firstMessage}} 首条消息', '{{personality}} 个性', '{{scenario}} 情景',
  '{{setvar::键::值}} 设置变量', '{{addvar::键::追加}} 追加变量', '{{getvar::键}} 读取变量',
  '{{trim}} 修饰下一宏去空白', '{{//注释}} 注释到行尾',
];

export function usePromptInject() {
  const store = useAppStore();

  const injectDraft = ref<api.PromptInjectConfig | null>(null);
  const injectSaving = ref(false);
  const injectMsg = ref('');
  const editingFloorId = ref<string | null>(null);
  const dragFloorId = ref<string | null>(null);
  /** 酒馆预设导入(替换现有楼层;导入前弹窗确认) */
  const injectImportInput = ref<HTMLInputElement | null>(null);
  const injectImporting = ref(false);

  /** 从服务端加载配置并克隆为可编辑草稿(保存时才提交) */
  async function loadInjectConfig(): Promise<void> {
    await store.loadPromptInject();
    const cfg = store.promptInject;
    injectDraft.value = cfg ? (JSON.parse(JSON.stringify(cfg)) as api.PromptInjectConfig) : null;
  }

  async function saveInjectNow(): Promise<void> {
    if (!injectDraft.value || injectSaving.value) return;
    injectSaving.value = true;
    injectMsg.value = '';
    try {
      await store.savePromptInjectConfig(injectDraft.value);
      injectMsg.value = '注入设置已保存';
      setTimeout(() => (injectMsg.value = ''), 2500);
    } catch (e) {
      injectMsg.value = `保存失败:${(e as Error).message}`;
    } finally {
      injectSaving.value = false;
    }
  }

  function newFloorId(): string {
    return (crypto.randomUUID?.() ?? `f-${Date.now()}`) as string;
  }

  function addFloor(): void {
    if (!injectDraft.value) return;
    const id = newFloorId();
    injectDraft.value.floors.push({
      id,
      name: '新楼层',
      content: '',
      role: 'user',
      position: 'system',
      depth: 0,
      enabled: true,
      order: injectDraft.value.floors.length,
    });
    editingFloorId.value = id;
  }

  function removeFloor(id: string): void {
    if (!injectDraft.value) return;
    injectDraft.value.floors = injectDraft.value.floors.filter((f) => f.id !== id);
    injectDraft.value.floors.forEach((f, i) => (f.order = i));
    if (editingFloorId.value === id) editingFloorId.value = null;
  }

  /** ↑/↓ 移动(拖拽的兜底操作;移动后统一重排 order) */
  function moveFloor(id: string, dir: -1 | 1): void {
    const fs = injectDraft.value?.floors;
    if (!fs) return;
    const idx = fs.findIndex((f) => f.id === id);
    const to = idx + dir;
    if (idx < 0 || to < 0 || to >= fs.length) return;
    [fs[idx], fs[to]] = [fs[to], fs[idx]];
    fs.forEach((f, i) => (f.order = i));
  }

  /** 导入 JSON(智能识别格式):
   *  - 有 floors 字段且条目带 role/position → 酒馆预设(替换楼层,自动切复杂模式)
   *  - 有 mode/simple 字段 → 本工具导出 JSON(恢复完整配置含禁词库) */
  async function onImportPreset(e: Event): Promise<void> {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (!file || injectImporting.value) return;
    injectImporting.value = true;
    injectMsg.value = '';
    try {
      const text = await file.text();
      let data: Record<string, unknown>;
      try {
        data = JSON.parse(text) as Record<string, unknown>;
      } catch {
        throw new Error('文件不是有效的 JSON');
      }

      const isStPreset = Array.isArray(data.floors) && data.floors.length > 0
        && data.floors.every((f: Record<string, unknown>) => typeof f === 'object' && f !== null && 'role' in f && 'position' in f);
      const isKedaiExport = typeof data.mode === 'string' && data.simple !== undefined;

      if (isStPreset) {
        const count = injectDraft.value?.floors.length ?? 0;
        if (!window.confirm(`导入酒馆预设将替换当前 ${count} 条楼层配置,确定继续?`)) return;
        const result = await api.importPromptPreset(file);
        injectDraft.value = JSON.parse(JSON.stringify(result.config)) as api.PromptInjectConfig;
        store.promptInject = result.config;
        injectMsg.value = `已导入 ${result.imported} 条楼层(替换原配置)`;
      } else if (isKedaiExport) {
        const count = injectDraft.value?.floors.length ?? 0;
        if (!window.confirm(`导入配置将替换当前设置(含禁词库),确定继续?`)) return;
        // 迁移:若导入数据含 banned_words 且 banned_prompt 为空,自动迁移
        const cfg = data as unknown as api.PromptInjectConfig;
        if (cfg.simple?.banned_words && !cfg.simple?.banned_prompt) {
          const words = cfg.simple.banned_words
            .map((b) => b.word?.trim())
            .filter((w): w is string => !!w);
          if (words.length > 0) {
            cfg.simple.banned_prompt = `输出中禁止出现以下词语,若涉及请用含义相近、更得体的表达替换:${words.join('、')}`;
          }
        }
        const saved = await api.savePromptInject(cfg);
        injectDraft.value = JSON.parse(JSON.stringify(saved)) as api.PromptInjectConfig;
        store.promptInject = saved;
        injectMsg.value = `已导入配置(含 ${saved.simple.banned_words_enabled ? '禁词库' : '无禁词库'})`;
      } else {
        throw new Error('无法识别的 JSON 格式(非酒馆预设,也非本工具导出配置)');
      }
      setTimeout(() => (injectMsg.value = ''), 3000);
    } catch (err) {
      injectMsg.value = `导入失败:${(err as Error).message}`;
    } finally {
      injectImporting.value = false;
    }
  }

  /** 导出当前注入配置(模式 + 简单项 + 全部楼层)为 JSON 文件;导入端可原样回导 */
  function exportInjectConfig(): void {
    const cur = injectDraft.value;
    if (!cur) return;
    const payload = {
      mode: cur.mode,
      simple: cur.simple,
      floors: cur.floors,
    };
    // 直接下载,不弹保存对话框(统一走 downloadBlob,延时回收 ObjectURL)
    downloadBlob(
      `kedai-inject-${new Date().toISOString().slice(0, 10)}.json`,
      new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' }),
    );
    injectMsg.value = '注入配置已导出';
    setTimeout(() => (injectMsg.value = ''), 2500);
  }

  // ===== 楼层拖拽(HTML5 DnD;drop 后重排 order) =====
  function onDragStart(e: DragEvent, id: string): void {
    dragFloorId.value = id;
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }
  function onDragOver(e: DragEvent, id: string): void {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
  }
  function onDrop(e: DragEvent, targetId: string): void {
    e.preventDefault();
    const fromId = dragFloorId.value;
    dragFloorId.value = null;
    if (!fromId || fromId === targetId) return;
    const fs = injectDraft.value?.floors;
    if (!fs) return;
    const from = fs.findIndex((f) => f.id === fromId);
    const to = fs.findIndex((f) => f.id === targetId);
    if (from < 0 || to < 0) return;
    const [item] = fs.splice(from, 1);
    fs.splice(to, 0, item);
    fs.forEach((f, i) => (f.order = i));
  }
  function onDragEnd(): void {
    dragFloorId.value = null;
  }

  return {
    injectDraft, injectSaving, injectMsg, editingFloorId, dragFloorId, injectImportInput, injectImporting,
    loadInjectConfig, saveInjectNow, addFloor, removeFloor, moveFloor, onImportPreset, exportInjectConfig,
    onDragStart, onDragOver, onDrop, onDragEnd,
  };
}
